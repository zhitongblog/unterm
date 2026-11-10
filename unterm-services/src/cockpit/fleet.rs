//! Fleet — run one task across N agents in N isolated git worktrees.
//!
//! `launch` verifies the repo is clean, adds a worktree + branch per
//! member beside the repo (`../<repo>.fleet/<slug>-<n>/`), opens a tab
//! per member (its own tab so the tab badge shows that member's state),
//! and types the agent command into the fresh shell. Fleets persist in the
//! durable task engine (see `fleet_store`) so the Review page and
//! `fleet.clean` survive a restart; panes dying does NOT remove a fleet —
//! the worktrees hold the work product until every member is merged or
//! discarded.
//!
//! Persistence used to be `~/.unterm/fleets.json`, rewritten whole on every
//! change. The shapes here are unchanged; only where the bytes live moved,
//! and an existing file is imported on first use.

use anyhow::{anyhow, bail, Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Pending,
    Merged,
    Discarded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetMember {
    pub agent: String,
    pub agent_cmd: String,
    pub worktree: PathBuf,
    pub branch: String,
    pub pane_id: Option<u64>,
    /// HEAD sha the worktree started from — the review baseline.
    pub checkpoint: String,
    pub review: ReviewState,
    /// Number of times this member has been launched. Legacy fleet records
    /// deserialize as their original first attempt.
    #[serde(default = "default_attempt")]
    pub attempt: u32,
    /// Timestamp of the latest launch/retry. Optional for legacy records.
    #[serde(default)]
    pub last_started_at: Option<String>,
    /// Most recent failure to create a pane, cleared after a successful retry.
    #[serde(default)]
    pub last_launch_error: Option<String>,
}

fn default_attempt() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fleet {
    pub id: String,
    pub task: String,
    pub base_repo: PathBuf,
    pub base_branch: String,
    pub members: Vec<FleetMember>,
    pub created_at: String,
}

/// Empty the store. Behind a feature so it is not in a shipped binary, but
/// reachable from another crate's tests, which is where the fleet is exercised
/// end to end.
#[cfg(any(test, feature = "test-support"))]
pub fn reset_store_for_tests() {
    // Point the durable store at a file of this test's own before anything
    // opens one. Without this the suite writes into the developer's real
    // `~/.unterm/tasks.db` — which it did exactly once, during the change
    // that introduced it.
    let scratch = std::env::temp_dir().join(format!(
        "unterm-fleet-test-{}-{}.db",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::env::set_var("UNTERM_TASKS_DB", scratch);
    super::fleet_store::reset_for_tests();
    store().lock().clear();
}

/// The in-process view of what the durable store holds.
///
/// Since M1 every fleet operation runs in the Core, so there is one writer
/// and this is a cache of the one truth rather than a competing copy of it.
/// It is filled once, from the database, after any legacy JSON has been
/// imported.
fn store() -> &'static Mutex<Vec<Fleet>> {
    static S: OnceLock<Mutex<Vec<Fleet>>> = OnceLock::new();
    S.get_or_init(|| {
        super::fleet_store::migrate_legacy_json();
        Mutex::new(super::fleet_store::load_all())
    })
}

/// Persist the whole set.
///
/// Same meaning as the old whole-file rewrite, so every caller keeps its
/// semantics — but as transactions, which a crash cannot catch halfway.
fn save_locked(fleets: &[Fleet]) {
    super::fleet_store::save_all(fleets);
}

/// `git` with no console flash on Windows (same trick as git_panel).
fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().with_context(|| format!("run git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.first().unwrap_or(&""),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Built-in launch command per agent. The task is quoted for the pane's
/// shell (POSIX single-quote everywhere except Windows, which gets
/// double-quote escaping for pwsh/cmd).
pub fn agent_command(agent: &str, task: &str) -> String {
    let quoted = quote_for_shell(task);
    match agent {
        "claude" | "claude-code" => format!("claude {quoted}"),
        "agy" | "antigravity" | "antigravity-cli" => format!("agy -i {quoted}"),
        "codex" | "codex-cli" => format!("codex {quoted}"),
        "gemini" | "gemini-cli" => format!("gemini -i {quoted}"),
        "qwen" | "qwen-code" => format!("qwen -i {quoted}"),
        "kimi" | "kimi-code" => format!("kimi -p {quoted}"),
        "opencode" => format!("opencode --prompt {quoted}"),
        "trae" | "trae-cli" | "trae-agent" => format!("trae-cli run {quoted}"),
        "aider" => format!("aider --message {quoted}"),
        other => format!("{other} {quoted}"),
    }
}

fn quote_for_shell(s: &str) -> String {
    #[cfg(windows)]
    {
        // pwsh + cmd both accept double quotes; escape embedded quotes.
        format!("\"{}\"", s.replace('"', "`\""))
    }
    #[cfg(not(windows))]
    {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn slugify(task: &str) -> String {
    let mut slug: String = task
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        slug = "task".to_string();
    }
    slug.truncate(32);
    slug
}

pub trait FleetPaneSpawner {
    fn spawn_member(&mut self, cwd: &Path, command: &str) -> Result<u64>;
}

pub trait FleetPaneRemover {
    fn remove_member(&mut self, pane_id: u64) -> Result<()>;
}

/// Fleet members as next-core sessions.
///
/// Spawning a shell in a directory and stopping it are engine operations, not
/// window ones: a fleet launched from the settings UI, from the CLI, or from
/// an agent should get the same panes whether or not anything has a window
/// open. A front end that wants them arranged its own way still can -- that is
/// what the `*_with_driver` entry points are for.
pub struct EngineFleetPanes;

impl FleetPaneSpawner for EngineFleetPanes {
    fn spawn_member(&mut self, cwd: &Path, command: &str) -> Result<u64> {
        let mut builder = portable_pty::CommandBuilder::new_default_prog();
        builder.cwd(cwd);
        // Through the installed provider, not a bare next-core handle:
        // with the sessions living in a Core process, a fleet spawned
        // against this process's own engine lands in an empty world no
        // window is looking at.
        let engine = unterm_engine::host_engine();
        let session = unterm_engine::SessionEngine::create_session(
            &*engine,
            unterm_engine::CreateSessionRequest {
                cols: 120,
                rows: 40,
                command_dir: Some(cwd.display().to_string()),
                command: Some(builder),
                env: Vec::new(),
                launch_policy: Default::default(),
            },
        )?;
        // The agent's own command, typed into the shell that was started --
        // the same thing a person launching a fleet member would do.
        unterm_engine::InputEngine::write_input(&*engine, session.id, &format!("{command}\r"))?;
        Ok(session.id as u64)
    }
}

impl FleetPaneRemover for EngineFleetPanes {
    fn remove_member(&mut self, pane_id: u64) -> Result<()> {
        unterm_engine::SessionEngine::destroy_session(
            &*unterm_engine::host_engine(),
            pane_id as usize,
        )
    }
}

/// Retry a failed member, in a pane of its own.
pub fn retry_member(fleet_id: &str, member: &str) -> Result<FleetMember> {
    let mut panes = EngineFleetPanes;
    let mut remover = EngineFleetPanes;
    retry_member_with_driver(fleet_id, member, &mut panes, &mut remover)
}

/// Remove a fleet: stop its panes, then its worktrees and branches.
pub fn clean(fleet_id: &str, force: bool) -> Result<()> {
    let mut remover = EngineFleetPanes;
    clean_with_remover(fleet_id, force, &mut remover)
}

/// Launch a fleet, one pane per member.
pub fn launch(cwd: &Path, task: &str, agents: &[String]) -> Result<Fleet> {
    let mut panes = EngineFleetPanes;
    launch_with_spawner(cwd, task, agents, &mut panes)
}

/// Fast pre-flight check (safe on the main thread): is `cwd` inside a
/// clean git repo? Returns the i18n key of the failure for palette UI.
pub fn precheck(cwd: &Path) -> std::result::Result<(), &'static str> {
    let Ok(root) = git(cwd, &["rev-parse", "--show-toplevel"]) else {
        return Err("cockpit.fleet_not_repo");
    };
    match git(Path::new(&root), &["status", "--porcelain"]) {
        Ok(s) if s.is_empty() => Ok(()),
        _ => Err("cockpit.fleet_not_clean"),
    }
}

/// Which of the built-in agents are actually on PATH.
pub fn installed_agents() -> Vec<&'static str> {
    [
        "claude",
        "agy",
        "codex",
        "gemini",
        "kimi",
        "qwen",
        "opencode",
        "aider",
        "trae-cli",
    ]
    .iter()
    .copied()
    .filter(|bin| binary_on_path(bin))
    .collect()
}

fn binary_on_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| {
        let p = dir.join(bin);
        p.is_file() || {
            #[cfg(windows)]
            {
                dir.join(format!("{bin}.exe")).is_file() || dir.join(format!("{bin}.cmd")).is_file()
            }
            #[cfg(not(windows))]
            {
                false
            }
        }
    })
}

pub fn launch_with_spawner(
    cwd: &Path,
    task: &str,
    agents: &[String],
    spawner: &mut dyn FleetPaneSpawner,
) -> Result<Fleet> {
    if agents.is_empty() {
        bail!("fleet needs at least one agent");
    }
    if agents.len() > 8 {
        bail!("fleet supports at most 8 members");
    }
    let repo_root = PathBuf::from(
        git(cwd, &["rev-parse", "--show-toplevel"]).map_err(|_| anyhow!("not a git repository"))?,
    );
    let base_branch = git(&repo_root, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let head = git(&repo_root, &["rev-parse", "HEAD"])?;
    let dirty = git(&repo_root, &["status", "--porcelain"])?;
    if !dirty.is_empty() {
        bail!("worktree not clean — commit or stash before launching a fleet");
    }

    let slug = slugify(task);
    // Disambiguate against previous fleets for the same slug.
    let existing: usize = {
        let s = store().lock();
        s.iter().filter(|f| f.id.contains(&slug)).count()
    };
    let fleet_id = if existing > 0 {
        format!("{slug}-{}", existing + 1)
    } else {
        slug.clone()
    };

    let repo_name = repo_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());
    let fleet_dir = repo_root
        .parent()
        .unwrap_or(&repo_root)
        .join(format!("{repo_name}.fleet"));
    std::fs::create_dir_all(&fleet_dir).context("create fleet dir")?;

    let mut members = Vec::new();
    for (i, agent) in agents.iter().enumerate() {
        let n = i + 1;
        let branch = format!("fleet/{fleet_id}-{n}");
        let worktree = fleet_dir.join(format!("{fleet_id}-{n}"));
        git(
            &repo_root,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                &worktree.to_string_lossy(),
                "HEAD",
            ],
        )
        .with_context(|| format!("add worktree for member {n}"))?;
        let agent_cmd = agent_command(agent, task);
        let (pane_id, last_launch_error) = match spawner.spawn_member(&worktree, &agent_cmd) {
            Ok(id) => {
                super::status::set_fleet(id, Some(fleet_id.clone()));
                (Some(id), None)
            }
            Err(err) => {
                log::error!("fleet {fleet_id}: member {n} spawn failed: {err:#}");
                (None, Some(format!("{err:#}")))
            }
        };
        members.push(FleetMember {
            agent: agent.clone(),
            agent_cmd,
            worktree,
            branch,
            pane_id,
            checkpoint: head.clone(),
            review: ReviewState::Pending,
            attempt: 1,
            last_started_at: Some(chrono::Utc::now().to_rfc3339()),
            last_launch_error,
        });
    }

    let fleet = Fleet {
        id: fleet_id,
        task: task.to_string(),
        base_repo: repo_root,
        base_branch,
        members,
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    {
        let mut s = store().lock();
        s.push(fleet.clone());
        save_locked(&s);
    }
    Ok(fleet)
}

pub fn list() -> Vec<Fleet> {
    store().lock().clone()
}

pub fn get(id: &str) -> Option<Fleet> {
    store().lock().iter().find(|f| f.id == id).cloned()
}

/// Look up which fleet (if any) a pane belongs to.
pub fn fleet_for_pane(pane_id: u64) -> Option<String> {
    store()
        .lock()
        .iter()
        .find(|f| f.members.iter().any(|m| m.pane_id == Some(pane_id)))
        .map(|f| f.id.clone())
}

/// Update a member's review state (member = 1-based index or branch name).
pub fn set_review_state(fleet_id: &str, member: &str, state: ReviewState) -> Result<()> {
    let mut s = store().lock();
    let fleet = s
        .iter_mut()
        .find(|f| f.id == fleet_id)
        .ok_or_else(|| anyhow!("no fleet {fleet_id:?}"))?;
    let m = resolve_member_mut(fleet, member)?;
    m.review = state;
    save_locked(&s);
    Ok(())
}

fn resolve_member_mut<'a>(fleet: &'a mut Fleet, member: &str) -> Result<&'a mut FleetMember> {
    if let Ok(n) = member.parse::<usize>() {
        let len = fleet.members.len();
        return fleet
            .members
            .get_mut(n.saturating_sub(1))
            .ok_or_else(|| anyhow!("member {n} out of range (fleet has {len})"));
    }
    fleet
        .members
        .iter_mut()
        .find(|m| m.branch == member || m.branch.ends_with(member))
        .ok_or_else(|| anyhow!("no member {member:?}"))
}

pub fn resolve_member(fleet: &Fleet, member: &str) -> Result<FleetMember> {
    if let Ok(n) = member.parse::<usize>() {
        return fleet
            .members
            .get(n.saturating_sub(1))
            .cloned()
            .ok_or_else(|| anyhow!("member {n} out of range"));
    }
    fleet
        .members
        .iter()
        .find(|m| m.branch == member || m.branch.ends_with(member))
        .cloned()
        .ok_or_else(|| anyhow!("no member {member:?}"))
}

pub fn retry_member_with_driver(
    fleet_id: &str,
    member: &str,
    spawner: &mut dyn FleetPaneSpawner,
    remover: &mut dyn FleetPaneRemover,
) -> Result<FleetMember> {
    let (old_pane_id, worktree, branch, agent_cmd) = {
        let s = store().lock();
        let fleet = s
            .iter()
            .find(|f| f.id == fleet_id)
            .ok_or_else(|| anyhow!("no fleet {fleet_id:?}"))?;
        let m = resolve_member(fleet, member)?;
        if m.review != ReviewState::Pending {
            bail!("cannot retry a {:?} fleet member", m.review);
        }
        validate_retry_worktree(&m)?;
        (m.pane_id, m.worktree, m.branch, m.agent_cmd)
    };

    // Persist the new attempt before touching panes. If spawning fails, Review
    // accurately shows that the member has no active pane and may be retried.
    {
        let mut s = store().lock();
        let fleet = s
            .iter_mut()
            .find(|f| f.id == fleet_id)
            .ok_or_else(|| anyhow!("no fleet {fleet_id:?}"))?;
        let m = resolve_member_mut(fleet, member)?;
        m.pane_id = None;
        m.attempt = m.attempt.saturating_add(1);
        m.last_started_at = Some(chrono::Utc::now().to_rfc3339());
        m.last_launch_error = None;
        save_locked(&s);
    }

    if let Some(pane_id) = old_pane_id {
        remover.remove_member(pane_id).ok();
    }

    let pane_id = match spawner.spawn_member(&worktree, &agent_cmd) {
        Ok(pane_id) => pane_id,
        Err(err) => {
            let message = format!("{err:#}");
            let mut s = store().lock();
            if let Some(fleet) = s.iter_mut().find(|f| f.id == fleet_id) {
                if let Ok(m) = resolve_member_mut(fleet, &branch) {
                    m.last_launch_error = Some(message.clone());
                }
                save_locked(&s);
            }
            return Err(err).context("retry fleet member");
        }
    };
    super::status::set_fleet(pane_id, Some(fleet_id.to_string()));

    let mut s = store().lock();
    let Some(fleet) = s.iter_mut().find(|f| f.id == fleet_id) else {
        remover.remove_member(pane_id).ok();
        bail!("fleet {fleet_id:?} was removed while retrying");
    };
    let m = resolve_member_mut(fleet, &branch)?;
    m.pane_id = Some(pane_id);
    m.last_launch_error = None;
    let result = m.clone();
    save_locked(&s);
    Ok(result)
}

fn validate_retry_worktree(member: &FleetMember) -> Result<()> {
    if !member.worktree.is_dir() {
        bail!("fleet worktree {:?} no longer exists", member.worktree);
    }
    let actual_branch = git(&member.worktree, &["rev-parse", "--abbrev-ref", "HEAD"])
        .context("validate fleet worktree")?;
    if actual_branch != member.branch {
        bail!(
            "fleet worktree is on branch {actual_branch:?}, expected {:?}",
            member.branch
        );
    }
    Ok(())
}

pub fn clean_with_remover(
    fleet_id: &str,
    force: bool,
    remover: &mut dyn FleetPaneRemover,
) -> Result<()> {
    let fleet = get(fleet_id).ok_or_else(|| anyhow!("no fleet {fleet_id:?}"))?;
    if !force {
        let pending = fleet
            .members
            .iter()
            .filter(|m| m.review == ReviewState::Pending)
            .count();
        if pending > 0 {
            bail!("{pending} member(s) still pending review — merge/discard them or use force");
        }
    }
    for m in &fleet.members {
        if let Some(pane_id) = m.pane_id {
            remover.remove_member(pane_id).ok();
        }
        if m.worktree.exists() {
            git(
                &fleet.base_repo,
                &[
                    "worktree",
                    "remove",
                    "--force",
                    &m.worktree.to_string_lossy(),
                ],
            )
            .with_context(|| format!("remove worktree {:?}", m.worktree))?;
        } else {
            let _ = git(&fleet.base_repo, &["worktree", "prune"]);
        }
        let _ = git(&fleet.base_repo, &["branch", "-D", &m.branch]);
    }
    // Remove the fleet dir if now empty.
    if let Some(parent) = fleet.members.first().and_then(|m| m.worktree.parent()) {
        let _ = std::fs::remove_dir(parent);
    }
    let mut s = store().lock();
    s.retain(|f| f.id != fleet_id);
    save_locked(&s);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_member(worktree: PathBuf, branch: &str) -> FleetMember {
        FleetMember {
            agent: "codex".to_string(),
            agent_cmd: "codex test".to_string(),
            worktree,
            branch: branch.to_string(),
            pane_id: Some(42),
            checkpoint: "abc123".to_string(),
            review: ReviewState::Pending,
            attempt: 1,
            last_started_at: None,
            last_launch_error: None,
        }
    }

    #[test]
    fn slug_basics() {
        assert_eq!(slugify("Fix the login bug now please"), "fix-the-login-bug");
        assert_eq!(slugify("修复登录"), "task");
        assert_eq!(slugify("a  b"), "a-b");
    }

    #[test]
    fn agent_commands_quote_tasks() {
        #[cfg(not(windows))]
        let cmd = agent_command("claude", "fix it's bug");
        #[cfg(not(windows))]
        assert_eq!(cmd, "claude 'fix it'\\''s bug'");
        assert!(agent_command("codex", "t").starts_with("codex "));
        assert!(agent_command("agy", "t").starts_with("agy -i "));
        assert!(agent_command("gemini", "t").starts_with("gemini -i "));
        assert!(agent_command("qwen", "t").starts_with("qwen -i "));
        assert!(agent_command("kimi", "t").starts_with("kimi -p "));
        assert!(agent_command("aider", "t").starts_with("aider --message "));
    }

    #[test]
    fn legacy_member_defaults_to_first_attempt() {
        let json = r#"{
            "agent":"codex",
            "agent_cmd":"codex test",
            "worktree":"repo.fleet/task-1",
            "branch":"fleet/task-1",
            "pane_id":null,
            "checkpoint":"abc123",
            "review":"pending"
        }"#;
        let member: FleetMember = serde_json::from_str(json).unwrap();
        assert_eq!(member.attempt, 1);
        assert_eq!(member.last_started_at, None);
        assert_eq!(member.last_launch_error, None);
    }

    #[test]
    fn retry_validation_keeps_dirty_worktree_and_checks_branch() {
        let temp = tempfile::tempdir().unwrap();
        git(temp.path(), &["init", "-b", "fleet/test-1"]).unwrap();
        git(
            temp.path(),
            &["config", "user.email", "test@unterm.invalid"],
        )
        .unwrap();
        git(temp.path(), &["config", "user.name", "Unterm Test"]).unwrap();
        std::fs::write(temp.path().join("result.txt"), "first\n").unwrap();
        git(temp.path(), &["add", "result.txt"]).unwrap();
        git(temp.path(), &["commit", "-m", "initial"]).unwrap();

        // A failed agent may leave valuable uncommitted and untracked work.
        // Validation must accept it and must not modify it.
        std::fs::write(temp.path().join("result.txt"), "first\nsecond\n").unwrap();
        std::fs::write(temp.path().join("untracked.txt"), "keep me\n").unwrap();
        let member = test_member(temp.path().to_path_buf(), "fleet/test-1");
        validate_retry_worktree(&member).unwrap();
        let status = git(temp.path(), &["status", "--porcelain"]).unwrap();
        assert!(status.contains("result.txt"));
        assert!(status.contains("untracked.txt"));

        let wrong_branch = test_member(temp.path().to_path_buf(), "fleet/other");
        assert!(validate_retry_worktree(&wrong_branch)
            .unwrap_err()
            .to_string()
            .contains("expected"));
    }
}

#[cfg(test)]
mod precheck_tests {
    use super::*;

    fn git_in(dir: &Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    /// A repository is what a fleet needs: every member gets a worktree, and
    /// there are no worktrees without one.
    #[test]
    fn a_folder_that_is_not_a_repository_is_refused() {
        let plain = tempfile::tempdir().expect("a temporary directory");
        assert_eq!(precheck(plain.path()), Err("cockpit.fleet_not_repo"));
    }

    /// And a clean one, because every member branches from what is committed.
    /// Launching from a dirty tree silently leaves the uncommitted work behind
    /// in the original checkout while several agents rewrite the same files
    /// from an older state -- which is how a fleet loses somebody's work.
    #[test]
    fn a_dirty_repository_is_refused_and_a_clean_one_is_not() {
        let repo = tempfile::tempdir().expect("a temporary directory");
        if !git_in(repo.path(), &["init"]) {
            // No git on PATH is this machine's problem, not the check's.
            return;
        }
        git_in(repo.path(), &["config", "user.email", "test@example.com"]);
        git_in(repo.path(), &["config", "user.name", "Test"]);
        std::fs::write(repo.path().join("a.txt"), b"first").expect("write a file");
        git_in(repo.path(), &["add", "-A"]);
        assert!(
            git_in(repo.path(), &["commit", "-m", "first"]),
            "could not make a commit"
        );

        assert_eq!(
            precheck(repo.path()),
            Ok(()),
            "a clean repository was refused"
        );

        std::fs::write(repo.path().join("a.txt"), b"changed").expect("change the file");
        assert_eq!(
            precheck(repo.path()),
            Err("cockpit.fleet_not_clean"),
            "a dirty repository was allowed"
        );

        // An untracked file counts too: it is work that would be left behind.
        git_in(repo.path(), &["checkout", "--", "a.txt"]);
        assert_eq!(precheck(repo.path()), Ok(()));
        std::fs::write(repo.path().join("b.txt"), b"new").expect("add a file");
        assert_eq!(precheck(repo.path()), Err("cockpit.fleet_not_clean"));
    }

    /// The check answers for the repository, not for the directory: standing
    /// in a subdirectory of a dirty repository is still standing in a dirty
    /// repository.
    #[test]
    fn a_subdirectory_answers_for_its_repository() {
        let repo = tempfile::tempdir().expect("a temporary directory");
        if !git_in(repo.path(), &["init"]) {
            return;
        }
        git_in(repo.path(), &["config", "user.email", "test@example.com"]);
        git_in(repo.path(), &["config", "user.name", "Test"]);
        let inner = repo.path().join("nested");
        std::fs::create_dir_all(&inner).expect("make a subdirectory");
        std::fs::write(inner.join("a.txt"), b"first").expect("write a file");
        git_in(repo.path(), &["add", "-A"]);
        assert!(git_in(repo.path(), &["commit", "-m", "first"]));

        assert_eq!(precheck(&inner), Ok(()));
        std::fs::write(repo.path().join("elsewhere.txt"), b"dirty").expect("dirty it");
        assert_eq!(precheck(&inner), Err("cockpit.fleet_not_clean"));
    }
}
