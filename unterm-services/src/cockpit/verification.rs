//! Asynchronous verification for Fleet worktrees.
//!
//! Verification records are kept in memory and mirrored to
//! `~/.unterm/verifications.json`.  The runner deliberately executes only a
//! command supplied by the user or one inferred from a small, auditable set of
//! project markers; it never executes scripts discovered by scanning source
//! files.

use anyhow::{anyhow, bail, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_SECS: u64 = 15 * 60;
const MAX_TIMEOUT_SECS: u64 = 2 * 60 * 60;
const MAX_LOG_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Pending,
    Running,
    Passed,
    Failed,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerificationRecord {
    pub id: String,
    pub fleet_id: String,
    /// Fleet member selector (normally the branch name).
    pub member: String,
    pub worktree: PathBuf,
    pub command: String,
    pub inferred: bool,
    pub status: VerificationStatus,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
    pub log: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct VerificationFile {
    records: Vec<VerificationRecord>,
}

fn records_path() -> Option<PathBuf> {
    unterm_protocol::state_path("verifications.json")
}

fn store() -> &'static Mutex<Vec<VerificationRecord>> {
    static STORE: OnceLock<Mutex<Vec<VerificationRecord>>> = OnceLock::new();
    STORE.get_or_init(|| {
        let mut records = records_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|body| serde_json::from_str::<VerificationFile>(&body).ok())
            .map(|file| file.records)
            .unwrap_or_default();
        // A process restart means these children no longer belong to us.
        for record in &mut records {
            if matches!(
                record.status,
                VerificationStatus::Pending | VerificationStatus::Running
            ) {
                record.status = VerificationStatus::Failed;
                record.finished_at = Some(chrono::Utc::now().to_rfc3339());
                record.log = append_log(
                    &record.log,
                    "\n[unterm] verification interrupted by restart\n",
                );
            }
        }
        Mutex::new(records)
    })
}

fn save_locked(records: &[VerificationRecord]) {
    let Some(path) = records_path() else { return };
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let Ok(body) = serde_json::to_vec_pretty(&VerificationFile {
        records: records.to_vec(),
    }) else {
        return;
    };
    let temporary = path.with_extension("json.tmp");
    if std::fs::write(&temporary, body).is_err() {
        return;
    }
    #[cfg(windows)]
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    let _ = std::fs::rename(temporary, path);
}

/// Infer a conventional verification command without executing project code.
/// Explicit commands supplied to [`verify_member`] always take precedence.
pub fn infer_command(worktree: &Path) -> Option<String> {
    if worktree.join("Cargo.toml").is_file() {
        return Some("cargo test".into());
    }
    if worktree.join("go.mod").is_file() {
        return Some("go test ./...".into());
    }
    if worktree.join("pnpm-lock.yaml").is_file() {
        return package_test_script(worktree).then(|| "pnpm test".into());
    }
    if worktree.join("yarn.lock").is_file() {
        return package_test_script(worktree).then(|| "yarn test".into());
    }
    if worktree.join("package-lock.json").is_file() || worktree.join("package.json").is_file() {
        return package_test_script(worktree).then(|| "npm test".into());
    }
    if worktree.join("uv.lock").is_file() {
        return Some("uv run pytest".into());
    }
    if worktree.join("pyproject.toml").is_file()
        || worktree.join("pytest.ini").is_file()
        || worktree.join("setup.cfg").is_file()
    {
        return Some("python -m pytest".into());
    }
    if worktree.join("pom.xml").is_file() {
        return Some("mvn test".into());
    }
    if worktree.join("gradlew").is_file() || worktree.join("gradlew.bat").is_file() {
        return Some(if cfg!(windows) {
            "gradlew.bat test".into()
        } else {
            "./gradlew test".into()
        });
    }
    if let Ok(entries) = std::fs::read_dir(worktree) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if name.ends_with(".sln") || name.ends_with(".csproj") {
                return Some("dotnet test".into());
            }
        }
    }
    None
}

fn package_test_script(worktree: &Path) -> bool {
    std::fs::read_to_string(worktree.join("package.json"))
        .ok()
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .and_then(|json| {
            json.get("scripts")?
                .get("test")?
                .as_str()
                .map(str::to_owned)
        })
        .map(|script| !script.trim().is_empty())
        .unwrap_or(false)
}

/// Queue verification of one Fleet member and return its persistent record.
///
/// The command runs asynchronously in the member's isolated worktree.  Pass
/// `None` to use project inference and `None` for the 15 minute timeout.
pub fn verify_member(
    fleet_id: &str,
    member: &str,
    explicit_command: Option<&str>,
    timeout_secs: Option<u64>,
) -> Result<VerificationRecord> {
    let fleet = super::fleet::get(fleet_id).ok_or_else(|| anyhow!("no fleet {fleet_id:?}"))?;
    let fleet_member = super::fleet::resolve_member(&fleet, member)?;
    if !fleet_member.worktree.is_dir() {
        bail!("fleet member worktree no longer exists");
    }
    let (command, inferred) = match explicit_command.map(str::trim) {
        Some("") => bail!("verification command cannot be empty"),
        Some(command) => (command.to_owned(), false),
        None => (
            infer_command(&fleet_member.worktree).ok_or_else(|| {
                anyhow!("could not infer a verification command; provide one explicitly")
            })?,
            true,
        ),
    };
    let timeout_secs = timeout_secs
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(1, MAX_TIMEOUT_SECS);
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let id = format!(
        "verify-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let record = VerificationRecord {
        id: id.clone(),
        fleet_id: fleet_id.to_owned(),
        member: fleet_member.branch,
        worktree: fleet_member.worktree,
        command: command.clone(),
        inferred,
        status: VerificationStatus::Pending,
        exit_code: None,
        duration_ms: None,
        log: String::new(),
        started_at: None,
        finished_at: None,
    };
    {
        let mut records = store().lock();
        records.push(record.clone());
        save_locked(&records);
    }
    let worktree = record.worktree.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(format!("fleet-verification-{id}"))
        .spawn(move || run_and_update(&id, &worktree, &command, timeout_secs))
    {
        update(&record.id, |stored| {
            stored.status = VerificationStatus::Failed;
            stored.finished_at = Some(chrono::Utc::now().to_rfc3339());
            stored.log = format!("[unterm] could not spawn verification worker: {error}\n");
        });
        return Err(error).context("spawn verification worker");
    }
    Ok(record)
}

pub fn list() -> Vec<VerificationRecord> {
    store().lock().clone()
}

pub fn get(id: &str) -> Option<VerificationRecord> {
    store()
        .lock()
        .iter()
        .find(|record| record.id == id)
        .cloned()
}

pub fn latest_for_member(fleet_id: &str, member: &str) -> Option<VerificationRecord> {
    let resolved = super::fleet::get(fleet_id)
        .and_then(|fleet| super::fleet::resolve_member(&fleet, member).ok())
        .map(|member| member.branch)
        .unwrap_or_else(|| member.to_owned());
    store()
        .lock()
        .iter()
        .rev()
        .find(|record| record.fleet_id == fleet_id && record.member == resolved)
        .cloned()
}

pub fn for_fleet(fleet_id: &str) -> Vec<VerificationRecord> {
    store()
        .lock()
        .iter()
        .filter(|record| record.fleet_id == fleet_id)
        .cloned()
        .collect()
}

/// Require a successful latest verification before a normal merge.
pub fn ensure_passed(fleet_id: &str, member: &str) -> Result<VerificationRecord> {
    let record = latest_for_member(fleet_id, member).ok_or_else(|| {
        anyhow!(
            "member has not been verified; run review.verify first or explicitly force the merge"
        )
    })?;
    if record.status != VerificationStatus::Passed {
        bail!(
            "latest verification is {:?}, not passed; rerun verification or explicitly force the merge",
            record.status
        );
    }
    Ok(record)
}

/// Attach the latest verification plus a deterministic candidate score/rank
/// to each member in a Review overview response.
pub fn enrich_overview(mut overview: Value) -> Value {
    let Some(fleets) = overview.get_mut("fleets").and_then(Value::as_array_mut) else {
        return overview;
    };
    for fleet in fleets {
        let Some(fleet_id) = fleet.get("id").and_then(Value::as_str).map(str::to_owned) else {
            continue;
        };
        let Some(members) = fleet.get_mut("members").and_then(Value::as_array_mut) else {
            continue;
        };
        let mut scored = Vec::new();
        for (index, member) in members.iter_mut().enumerate() {
            let selector = member
                .get("branch")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| (index + 1).to_string());
            let verification = latest_for_member(&fleet_id, &selector);
            let base = match verification.as_ref().map(|record| record.status) {
                Some(VerificationStatus::Passed) => 1000i64,
                Some(VerificationStatus::Running) => 250,
                Some(VerificationStatus::Pending) => 200,
                Some(VerificationStatus::Failed | VerificationStatus::TimedOut) => 0,
                None => 100,
            };
            let changed = member
                .get("changed_files")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let churn = member.get("additions").and_then(Value::as_u64).unwrap_or(0)
                + member.get("deletions").and_then(Value::as_u64).unwrap_or(0);
            let score =
                (base - (changed.min(100) as i64 * 2) - (churn.min(5000) as i64 / 100)).max(0);
            if let Some(object) = member.as_object_mut() {
                object.insert("score".into(), score.into());
                object.insert(
                    "verification".into(),
                    verification
                        .and_then(|record| serde_json::to_value(record).ok())
                        .unwrap_or(Value::Null),
                );
            }
            scored.push((index, score));
        }
        scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for (rank, (index, _)) in scored.into_iter().enumerate() {
            if let Some(object) = members[index].as_object_mut() {
                object.insert("rank".into(), (rank + 1).into());
            }
        }
    }
    overview
}

fn run_and_update(id: &str, worktree: &Path, command: &str, timeout_secs: u64) {
    update(id, |record| {
        record.status = VerificationStatus::Running;
        record.started_at = Some(chrono::Utc::now().to_rfc3339());
    });
    let result = execute(worktree, command, Duration::from_secs(timeout_secs));
    update(id, |record| {
        record.finished_at = Some(chrono::Utc::now().to_rfc3339());
        match result {
            Ok(output) => {
                record.status = if output.timed_out {
                    VerificationStatus::TimedOut
                } else if output.exit_code == Some(0) {
                    VerificationStatus::Passed
                } else {
                    VerificationStatus::Failed
                };
                record.exit_code = output.exit_code;
                record.duration_ms = Some(output.duration_ms);
                record.log = output.log;
            }
            Err(ref error) => {
                record.status = VerificationStatus::Failed;
                record.log = append_log(&record.log, &format!("[unterm] {error:#}\n"));
            }
        }
    });
}

fn update(id: &str, change: impl FnOnce(&mut VerificationRecord)) {
    let mut records = store().lock();
    if let Some(record) = records.iter_mut().find(|record| record.id == id) {
        change(record);
        save_locked(&records);
    }
}

struct ExecutionOutput {
    exit_code: Option<i32>,
    duration_ms: u64,
    timed_out: bool,
    log: String,
}

fn execute(worktree: &Path, command: &str, timeout: Duration) -> Result<ExecutionOutput> {
    let mut process = shell_command(command);
    process
        .current_dir(worktree)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        process.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        process.process_group(0);
    }
    let started = Instant::now();
    let mut child = process
        .spawn()
        .with_context(|| format!("start verification command {command:?}"))?;
    #[cfg(windows)]
    let verification_job = WindowsJob::assign(&child);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || read_pipe(stdout));
    let stderr_reader = std::thread::spawn(move || read_pipe(stderr));
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().context("poll verification command")? {
            break (status, false);
        }
        if started.elapsed() >= timeout {
            #[cfg(windows)]
            if let Some(job) = verification_job.as_ref() {
                job.terminate();
            } else {
                terminate_process_tree(&mut child);
            }
            #[cfg(not(windows))]
            terminate_process_tree(&mut child);
            let _ = child.kill();
            break (
                child
                    .wait()
                    .context("reap timed out verification command")?,
                true,
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut bytes = stdout_reader.join().unwrap_or_default();
    bytes.extend_from_slice(&stderr_reader.join().unwrap_or_default());
    let log = truncate_log(String::from_utf8_lossy(&bytes).into_owned());
    Ok(ExecutionOutput {
        exit_code: status.code(),
        duration_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        timed_out,
        log,
    })
}

#[cfg(windows)]
struct WindowsJob {
    handle: winapi::shared::ntdef::HANDLE,
}

#[cfg(windows)]
impl WindowsJob {
    /// Put the verifier shell in a kill-on-close Job Object as soon as it is
    /// spawned. Descendants inherit job membership, so timeout cleanup does
    /// not depend on starting and waiting for the comparatively slow
    /// `taskkill.exe` helper.
    fn assign(child: &std::process::Child) -> Option<Self> {
        use std::os::windows::io::AsRawHandle;
        use std::ptr::{null_mut, NonNull};
        use winapi::shared::minwindef::FALSE;
        use winapi::um::handleapi::CloseHandle;
        use winapi::um::jobapi2::{
            AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
        };
        use winapi::um::winnt::{
            JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        unsafe {
            let handle = CreateJobObjectW(null_mut(), null_mut());
            NonNull::new(handle)?;

            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &mut limits as *mut _ as *mut _,
                std::mem::size_of_val(&limits) as u32,
            ) != FALSE;
            let assigned =
                configured && AssignProcessToJobObject(handle, child.as_raw_handle() as _) != FALSE;
            if !assigned {
                CloseHandle(handle);
                return None;
            }
            Some(Self { handle })
        }
    }

    fn terminate(&self) {
        unsafe {
            winapi::um::jobapi2::TerminateJobObject(self.handle, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe {
            winapi::um::handleapi::CloseHandle(self.handle);
        }
    }
}

fn terminate_process_tree(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut taskkill = Command::new("taskkill.exe");
        taskkill
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000);
        let _ = taskkill.status();
    }
    #[cfg(unix)]
    unsafe {
        // The shell was put in its own process group above, so a negative pid
        // terminates it and every test/lint process it launched.
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
}

fn shell_command(command: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut process = Command::new("cmd.exe");
        process
            .args(["/D", "/S", "/C", command])
            .creation_flags(0x0800_0000);
        process
    }
    #[cfg(not(windows))]
    {
        let mut process = Command::new("sh");
        process.args(["-c", command]);
        process
    }
}

fn read_pipe(pipe: Option<impl Read>) -> Vec<u8> {
    let mut bytes = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut bytes);
    }
    bytes
}

fn append_log(current: &str, addition: &str) -> String {
    truncate_log(format!("{current}{addition}"))
}

fn truncate_log(log: String) -> String {
    if log.len() <= MAX_LOG_BYTES {
        return log;
    }
    let mut start = log.len() - MAX_LOG_BYTES;
    while !log.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "[unterm] earlier verification output truncated\n{}",
        &log[start..]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_common_projects_and_requires_real_npm_test_script() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        assert_eq!(infer_command(dir.path()).as_deref(), Some("cargo test"));

        let js = tempfile::tempdir().unwrap();
        std::fs::write(
            js.path().join("package.json"),
            r#"{"scripts":{"build":"x"}}"#,
        )
        .unwrap();
        assert_eq!(infer_command(js.path()), None);
        std::fs::write(
            js.path().join("package.json"),
            r#"{"scripts":{"test":"vitest"}}"#,
        )
        .unwrap();
        assert_eq!(infer_command(js.path()).as_deref(), Some("npm test"));
    }

    #[test]
    fn infers_every_documented_project_marker() {
        for (marker, expected) in [
            ("go.mod", "go test ./..."),
            ("uv.lock", "uv run pytest"),
            ("pyproject.toml", "python -m pytest"),
            ("pom.xml", "mvn test"),
            (
                if cfg!(windows) {
                    "gradlew.bat"
                } else {
                    "gradlew"
                },
                if cfg!(windows) {
                    "gradlew.bat test"
                } else {
                    "./gradlew test"
                },
            ),
            ("example.sln", "dotnet test"),
            ("example.csproj", "dotnet test"),
        ] {
            let project = tempfile::tempdir().unwrap();
            std::fs::write(project.path().join(marker), "").unwrap();
            assert_eq!(
                infer_command(project.path()).as_deref(),
                Some(expected),
                "marker {marker}"
            );
        }
    }

    #[test]
    fn executes_and_captures_output_and_failure() {
        let dir = tempfile::tempdir().unwrap();
        let success = execute(dir.path(), "echo verified", Duration::from_secs(5)).unwrap();
        assert_eq!(success.exit_code, Some(0));
        assert!(!success.timed_out);
        assert!(success.log.contains("verified"));

        let failure_command = if cfg!(windows) { "exit /b 7" } else { "exit 7" };
        let failure = execute(dir.path(), failure_command, Duration::from_secs(5)).unwrap();
        assert_eq!(failure.exit_code, Some(7));
    }

    #[test]
    fn stops_a_command_at_its_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let slow_command = if cfg!(windows) {
            "ping -n 6 127.0.0.1 >nul"
        } else {
            "sleep 5"
        };
        let output = execute(dir.path(), slow_command, Duration::from_millis(25)).unwrap();
        assert!(output.timed_out);
        // Windows launches taskkill.exe to terminate the complete descendant
        // tree. Process startup and tree teardown can occasionally exceed one
        // second on a busy CI/desktop host, but must still finish well before
        // the command's natural five-second duration.
        let cleanup_deadline_ms = if cfg!(windows) { 3_000 } else { 1_000 };
        assert!(
            output.duration_ms < cleanup_deadline_ms,
            "timed-out command cleanup took {}ms",
            output.duration_ms
        );
    }

    #[test]
    fn log_truncation_preserves_utf8_tail() {
        let log = "界".repeat(MAX_LOG_BYTES);
        let truncated = truncate_log(log);
        assert!(truncated.starts_with("[unterm]"));
        assert!(truncated.ends_with('界'));
        assert!(truncated.len() <= MAX_LOG_BYTES + 64);
    }

    #[test]
    fn overview_members_receive_stable_scores_and_ranks() {
        let overview = serde_json::json!({
            "fleets": [{
                "id": "fleet-score-test",
                "members": [
                    { "branch": "one", "changed_files": 8, "additions": 500, "deletions": 0 },
                    { "branch": "two", "changed_files": 1, "additions": 5, "deletions": 1 }
                ]
            }]
        });
        let enriched = enrich_overview(overview);
        let members = enriched["fleets"][0]["members"].as_array().unwrap();
        assert_eq!(members[1]["rank"], 1);
        assert_eq!(members[0]["rank"], 2);
        assert!(members[1]["score"].as_i64() > members[0]["score"].as_i64());
        assert!(members[0]["verification"].is_null());
    }
}
