//! Multi-instance discovery for Unterm.
//!
//! Each running Unterm process owns one **instance** with a NATO-phonetic
//! name (alpha, bravo, charlie, …). On launch the instance writes its
//! metadata (ports, auth token, pid, started_at, version, platform) to
//! `~/.unterm/instances/<name>.json`. AI agents that drive multiple
//! Unterm windows enumerate that directory to discover all live
//! instances and pick which one to talk to.
//!
//! For backward compat with single-instance agents, we also maintain
//! `~/.unterm/server.json` and `~/.unterm/active.json` which mirror the
//! "active" instance (the most recently launched one whose ancestor is
//! still alive). Per the design lock-in on 2026-05-02, active.json is
//! updated only when the previous active dies — not on every focus
//! event — to keep disk IO minimal.
//!
//! Two servers (MCP JSON-RPC, HTTP web settings) cooperate within one
//! process: the MCP server starts first and seeds the instance file
//! with `mcp_port + auth_token`; the HTTP server then updates
//! `http_port` in place. Within-process writes are serialized via
//! `file_lock()`. Across-process races (two instances claiming the
//! same NATO name simultaneously) are handled with O_EXCL atomic
//! creation — see `claim_instance_name`.

use anyhow::{Context, Result};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use unterm_protocol::{BuildHandshake, ProcessRole};

pub const MCP_PREFERRED_PORT: u16 = 19876;
pub const HTTP_PREFERRED_PORT: u16 = 19877;
pub const PORT_RETRY_LIMIT: u16 = 5;
pub const SERVER_BIND: &str = "127.0.0.1";

/// NATO phonetic alphabet — 26 single-word names. Choice locked
/// 2026-05-02: easier to pronounce than Crockford Base32 IDs and
/// AI agents handle them right. When all 26 are simultaneously taken
/// we append a digit (alpha2, bravo2, …); see `claim_instance_name`.
pub const NATO_NAMES: &[&str] = &[
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
    "uniform", "victor", "whiskey", "xray", "yankee", "zulu",
];

/// On-disk metadata for one Unterm instance. Lives at
/// `~/.unterm/instances/<id>.json`. Both port fields can be 0 briefly
/// during startup before both servers have bound.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceAgentInfo {
    pub pane_id: u64,
    #[serde(default)]
    pub tab_id: Option<u64>,
    #[serde(default)]
    pub window_id: Option<u64>,
    #[serde(default)]
    pub pane_title: Option<String>,
    pub agent: String,
    pub state: String,
    pub since_unix_ms: i64,
    #[serde(default)]
    pub task_hint: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InstanceInfo {
    pub id: String,
    pub mcp_port: u16,
    pub http_port: u16,
    pub auth_token: String,
    pub pid: u32,
    pub started_at: String,
    /// User-overridable display label. None = use auto-derived
    /// `Unterm — <id> — <project>` title; Some(str) = preserve user override.
    #[serde(default)]
    pub title: Option<String>,
    /// Last-seen cwd of the active pane. Refreshed periodically
    /// by the foreground update loop. Best-effort; agents can also
    /// query it live via `session.list`.
    #[serde(default)]
    pub cwd: Option<String>,
    /// Identity profile bound to this window (window=identity model,
    /// see project_identity_profiles_design.md §1). `None` = profile
    /// system not used or no default selected yet. Set on launch from
    /// the picker (or `index.toml` default), via `profile.spawn` MCP
    /// call, or via the chip menu. Once set, every pane spawned in
    /// this instance inherits its env from this profile.
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub version: String,
    /// Complete build/protocol identity. The legacy `version` field above is
    /// retained until pre-M0 clients have aged out.
    #[serde(default)]
    pub product_version: String,
    #[serde(default)]
    pub build_commit: String,
    #[serde(default)]
    pub protocol_version: String,
    #[serde(default)]
    pub data_schema_version: u32,
    #[serde(default)]
    pub process_role: ProcessRole,
    #[serde(default)]
    pub platform: String,
    /// Serializable Cockpit snapshot for cross-instance Inbox aggregation.
    #[serde(default)]
    pub agents: Vec<InstanceAgentInfo>,
}

impl InstanceInfo {
    pub fn build_handshake(&self) -> BuildHandshake {
        BuildHandshake {
            product_version: if self.product_version.is_empty() {
                self.version.clone()
            } else {
                self.product_version.clone()
            },
            build_commit: if self.build_commit.is_empty() {
                "unknown".to_string()
            } else {
                self.build_commit.clone()
            },
            protocol_version: if self.protocol_version.is_empty() {
                "legacy".to_string()
            } else {
                self.protocol_version.clone()
            },
            data_schema_version: self.data_schema_version,
            process_role: self.process_role,
            pid: self.pid,
            started_at: self.started_at.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct InstanceRegistrySnapshot {
    pub live: Vec<InstanceInfo>,
    pub live_count: usize,
    pub stale_removed: usize,
    pub corrupt_files: usize,
    pub empty_files: usize,
    pub unreadable_files: usize,
    pub active_id: Option<String>,
    pub active_pid_alive: Option<bool>,
    pub active_source: String,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct InstanceShutdownPlan {
    pub current_id: Option<String>,
    pub registry_file_exists: bool,
    pub active_id: Option<String>,
    pub active_pid_alive: Option<bool>,
    pub would_remove_registry_file: bool,
    pub would_clear_active_pointer: bool,
    pub handoff_id: Option<String>,
    pub would_update_legacy_server: bool,
    pub close_owner: String,
    pub native_window_lifecycle: String,
    pub values_redacted: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct InstanceLifecyclePlan {
    pub current_id: Option<String>,
    pub registration_owner: String,
    pub registry_file_exists: bool,
    pub active_id: Option<String>,
    pub active_pid_alive: Option<bool>,
    pub live_count: usize,
    pub shutdown: InstanceShutdownPlan,
    pub values_redacted: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct InstanceShutdownResult {
    pub applied: bool,
    pub plan: InstanceShutdownPlan,
    pub registry_file_removed: bool,
    pub active_pointer_cleared: bool,
    pub legacy_server_removed: bool,
    pub legacy_token_updated: bool,
    pub handoff_id: Option<String>,
    pub native_window_closed: bool,
    pub errors: Vec<String>,
    pub values_redacted: bool,
}

/// Compat alias: legacy server.json schema. The current process always
/// writes the *full* InstanceInfo into server.json (extra fields are
/// ignored by older deserializers), so older agents that only read
/// {mcp_port, http_port, auth_token, pid, started_at} keep working.
pub type ServerInfo = InstanceInfo;

fn unterm_dir() -> PathBuf {
    // The same isolation contract the bridge registry and the Core's
    // discovery already honor. Without this, a test or headless GUI
    // registers itself in the real user's instance registry — and a
    // hard-killed test instance leaves a stale server.json pointing
    // at a dead port for the CLI to trip over.
    unterm_protocol::state_dir().unwrap_or_else(|| PathBuf::from(".unterm"))
}

fn instances_dir() -> PathBuf {
    unterm_dir().join("instances")
}

fn instance_file(id: &str) -> PathBuf {
    instances_dir().join(format!("{}.json", id))
}

fn instance_file_in_dir(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{}.json", id))
}

fn server_info_path() -> PathBuf {
    unterm_dir().join("server.json")
}

fn active_pointer_path() -> PathBuf {
    unterm_dir().join("active.json")
}

fn auth_token_path() -> PathBuf {
    unterm_dir().join("auth_token")
}

/// Coarse mutex serializing instance/active/server file writes within
/// this process. Cross-process atomicity handled by the O_EXCL claim
/// in `claim_instance_name` plus tmp+rename writes elsewhere.
fn file_lock() -> &'static Mutex<()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// This process's instance ID, set once at startup by `write_initial`.
fn current_id() -> &'static Mutex<Option<String>> {
    static ID: std::sync::OnceLock<Mutex<Option<String>>> = std::sync::OnceLock::new();
    ID.get_or_init(|| Mutex::new(None))
}

fn current_info() -> &'static Mutex<Option<InstanceInfo>> {
    static INFO: std::sync::OnceLock<Mutex<Option<InstanceInfo>>> = std::sync::OnceLock::new();
    INFO.get_or_init(|| Mutex::new(None))
}

fn last_written_cwd() -> &'static Mutex<Option<Option<String>>> {
    static CWD: std::sync::OnceLock<Mutex<Option<Option<String>>>> = std::sync::OnceLock::new();
    CWD.get_or_init(|| Mutex::new(None))
}

fn last_written_agents() -> &'static Mutex<Option<Vec<InstanceAgentInfo>>> {
    static AGENTS: std::sync::OnceLock<Mutex<Option<Vec<InstanceAgentInfo>>>> =
        std::sync::OnceLock::new();
    AGENTS.get_or_init(|| Mutex::new(None))
}

pub fn current_instance_id() -> Option<String> {
    current_id().lock().clone()
}

/// Try to bind to `preferred`, then `preferred+1 .. preferred+PORT_RETRY_LIMIT`.
/// Falls back to OS-assigned port (`port=0`) on persistent failure.
/// Returns the listener and the actually-bound port.
pub fn bind_with_fallback(preferred: u16) -> Result<(TcpListener, u16)> {
    for offset in 0..=PORT_RETRY_LIMIT {
        let port = preferred.saturating_add(offset);
        match TcpListener::bind((SERVER_BIND, port)) {
            Ok(listener) => {
                let port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
                return Ok((listener, port));
            }
            Err(e) => {
                log::debug!("{}:{} bind failed ({}); trying next", SERVER_BIND, port, e);
            }
        }
    }
    let listener =
        TcpListener::bind((SERVER_BIND, 0u16)).context("OS-assigned port also failed")?;
    let port = listener.local_addr()?.port();
    Ok((listener, port))
}

/// Cross-platform "is this PID alive?" check. Used to clean up stale
/// instance files left behind by crashed processes. Best-effort: if we
/// can't tell, assume alive (preferring false-positives over deleting
/// a healthy peer's file).
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // kill(pid, 0) returns 0 if process exists and we can signal.
        // ESRCH = no such process. EPERM = exists but we can't signal,
        // which still means it's running. Only ESRCH = dead.
        //
        // Read errno portably via std::io::Error::last_os_error so this
        // works on both macOS (libc::__error) and Linux (__errno_location)
        // without #[cfg(target_os)] forks.
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if rc == 0 {
            return true;
        }
        !matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(e) if e == libc::ESRCH
        )
    }
    #[cfg(windows)]
    unsafe {
        use winapi::shared::minwindef::FALSE;
        use winapi::um::errhandlingapi::GetLastError;
        use winapi::um::handleapi::CloseHandle;
        use winapi::um::processthreadsapi::{GetExitCodeProcess, OpenProcess};
        use winapi::um::winnt::PROCESS_QUERY_LIMITED_INFORMATION;

        /// There is no process with this id.
        const ERROR_INVALID_PARAMETER: u32 = 87;

        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid);
        if handle.is_null() {
            // Two different answers arrive as the same null. A process that
            // does not exist fails with "invalid parameter"; one that exists
            // but will not be opened fails with "access denied". Reading both
            // as "alive" is how the registry came to list eleven windows when
            // one was open -- every window ever opened stayed in it, and
            // routing to one of them reached a process that had exited.
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        // STILL_ACTIVE (259) means the process hasn't exited.
        !ok || code == 259
    }
}

/// Scan `instances/`, parse each `*.json`, drop entries whose PID is
/// no longer alive (and delete those files), and report what happened.
fn instance_registry_snapshot_locked() -> InstanceRegistrySnapshot {
    instance_registry_snapshot_from_paths_locked(&instances_dir(), &active_pointer_path())
}

fn instance_registry_snapshot_from_paths_locked(
    dir: &Path,
    active_path: &Path,
) -> InstanceRegistrySnapshot {
    let read = match fs::read_dir(&dir) {
        Ok(r) => r,
        Err(_) => {
            return InstanceRegistrySnapshot {
                active_source: "instances_dir_missing".to_string(),
                ..Default::default()
            }
        }
    };
    let mut snapshot = InstanceRegistrySnapshot {
        active_source: "instances_dir".to_string(),
        ..Default::default()
    };
    let mut alive = Vec::new();
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e != "json").unwrap_or(true) {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            snapshot.unreadable_files += 1;
            continue;
        };
        let trimmed = content.trim();
        if trimmed.is_empty() || trimmed == "{}" {
            snapshot.empty_files += 1;
            continue;
        }
        let Ok(info): std::result::Result<InstanceInfo, _> = serde_json::from_str(&content) else {
            // Corrupt file: leave it alone (could be a partial write
            // by a peer; deleting would be racy).
            snapshot.corrupt_files += 1;
            continue;
        };
        if info.id.is_empty() {
            snapshot.empty_files += 1;
            continue;
        }
        if pid_alive(info.pid) {
            alive.push(info);
        } else {
            // Crashed/quit: remove stale file. Best-effort.
            let _ = fs::remove_file(&path);
            snapshot.stale_removed += 1;
        }
    }
    let active = fs::read_to_string(active_path)
        .ok()
        .and_then(|content| serde_json::from_str::<InstanceInfo>(&content).ok());
    if let Some(active) = active {
        let active_alive = pid_alive(active.pid);
        snapshot.active_id = Some(active.id.clone());
        snapshot.active_pid_alive = Some(active_alive);
        snapshot.active_source = if active_alive {
            "active_pointer".to_string()
        } else {
            "active_pointer_stale".to_string()
        };
    } else if !alive.is_empty() {
        snapshot.active_source = "live_scan_fallback".to_string();
    }
    snapshot.live_count = alive.len();
    snapshot.live = alive;
    snapshot
}

/// Scan `instances/`, parse each `*.json`, drop entries whose PID is
/// no longer alive (and delete those files), return the survivors.
fn live_instances_locked() -> Vec<InstanceInfo> {
    instance_registry_snapshot_locked().live
}

/// Public: list all live instances. Used by the MCP `instance.list`
/// method and any agent that wants to enumerate.
pub fn list_live_instances() -> Vec<InstanceInfo> {
    let _g = file_lock().lock();
    live_instances_locked()
}

pub fn instance_registry_snapshot() -> InstanceRegistrySnapshot {
    let _g = file_lock().lock();
    instance_registry_snapshot_locked()
}

fn instance_lifecycle_plan_from_paths_locked(
    current_id: Option<&str>,
    dir: &Path,
    active_path: &Path,
    native_window_lifecycle: &str,
) -> InstanceLifecyclePlan {
    let snapshot = instance_registry_snapshot_from_paths_locked(dir, active_path);
    let registry_file_exists = current_id
        .map(|id| instance_file_in_dir(dir, id).exists())
        .unwrap_or(false);
    let handoff_id = current_id.and_then(|id| {
        let mut candidates: Vec<_> = snapshot
            .live
            .iter()
            .filter(|info| info.id != id)
            .cloned()
            .collect();
        candidates.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        candidates.into_iter().next().map(|info| info.id)
    });
    let would_clear_active_pointer = current_id
        .zip(snapshot.active_id.as_deref())
        .map(|(current, active)| current == active)
        .unwrap_or(false);
    let shutdown = InstanceShutdownPlan {
        current_id: current_id.map(str::to_string),
        registry_file_exists,
        active_id: snapshot.active_id.clone(),
        active_pid_alive: snapshot.active_pid_alive,
        would_remove_registry_file: registry_file_exists,
        would_clear_active_pointer,
        would_update_legacy_server: would_clear_active_pointer,
        handoff_id,
        close_owner: "server_info".to_string(),
        native_window_lifecycle: native_window_lifecycle.to_string(),
        values_redacted: true,
    };
    InstanceLifecyclePlan {
        current_id: current_id.map(str::to_string),
        registration_owner: "server_info".to_string(),
        registry_file_exists,
        active_id: snapshot.active_id,
        active_pid_alive: snapshot.active_pid_alive,
        live_count: snapshot.live_count,
        shutdown,
        values_redacted: true,
    }
}

/// The registry's side of shutting an instance down.
///
/// `native_window_lifecycle` comes from the caller because only a front end
/// knows whether the window is its own to close: the registry can say what it
/// will do to its own files and nothing more.
pub fn instance_lifecycle_plan(native_window_lifecycle: &str) -> InstanceLifecyclePlan {
    let current = current_instance_id();
    let _g = file_lock().lock();
    instance_lifecycle_plan_from_paths_locked(
        current.as_deref(),
        &instances_dir(),
        &active_pointer_path(),
        native_window_lifecycle,
    )
}

fn apply_instance_shutdown_from_paths_locked(
    current_id: Option<&str>,
    dir: &Path,
    active_path: &Path,
    server_path: &Path,
    token_path: &Path,
) -> InstanceShutdownResult {
    // Applying a shutdown touches registry files only; whose window it was is
    // not this function's business, so it reports the registry's own answer.
    let lifecycle =
        instance_lifecycle_plan_from_paths_locked(current_id, dir, active_path, "unknown");
    let plan = lifecycle.shutdown;
    let mut result = InstanceShutdownResult {
        applied: true,
        handoff_id: plan.handoff_id.clone(),
        plan,
        native_window_closed: false,
        values_redacted: true,
        ..Default::default()
    };

    let Some(current_id) = current_id else {
        return result;
    };

    if result.plan.would_remove_registry_file {
        match fs::remove_file(instance_file_in_dir(dir, current_id)) {
            Ok(()) => result.registry_file_removed = true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => result
                .errors
                .push(format!("remove registry file failed: {err}")),
        }
    }

    if result.plan.would_clear_active_pointer {
        match fs::remove_file(active_path) {
            Ok(()) => result.active_pointer_cleared = true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => result
                .errors
                .push(format!("remove active pointer failed: {err}")),
        }
        match fs::remove_file(server_path) {
            Ok(()) => result.legacy_server_removed = true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => result
                .errors
                .push(format!("remove legacy server file failed: {err}")),
        }

        if let Some(handoff_id) = result.plan.handoff_id.as_deref() {
            let handoff_path = instance_file_in_dir(dir, handoff_id);
            match fs::read_to_string(&handoff_path)
                .ok()
                .and_then(|content| serde_json::from_str::<InstanceInfo>(&content).ok())
            {
                Some(next) => {
                    if let Err(err) = write_atomic(active_path, &next) {
                        result
                            .errors
                            .push(format!("write active pointer handoff failed: {err}"));
                    }
                    if let Err(err) = write_atomic(server_path, &next) {
                        result
                            .errors
                            .push(format!("write legacy server handoff failed: {err}"));
                    }
                    if let Some(parent) = token_path.parent() {
                        if let Err(err) = fs::create_dir_all(parent) {
                            result
                                .errors
                                .push(format!("create legacy auth token dir failed: {err}"));
                        }
                    }
                    if let Err(err) = write_private_file(token_path, next.auth_token.as_bytes()) {
                        result
                            .errors
                            .push(format!("write legacy auth token handoff failed: {err}"));
                    } else {
                        result.legacy_token_updated = true;
                    }
                }
                None => result.errors.push(format!(
                    "handoff instance metadata not readable: {handoff_id}"
                )),
            }
        }
    }

    result
}

pub fn unregister_current_instance() -> InstanceShutdownResult {
    let current = current_instance_id();
    let _g = file_lock().lock();
    let result = apply_instance_shutdown_from_paths_locked(
        current.as_deref(),
        &instances_dir(),
        &active_pointer_path(),
        &server_info_path(),
        &auth_token_path(),
    );
    if result.registry_file_removed {
        *current_info().lock() = None;
        *current_id().lock() = None;
    }
    result
}

/// Pick the lowest-NATO name not currently taken by a live instance,
/// then try to atomically claim it via O_EXCL create. If two instances
/// race for the same name, the second one's create_new fails and we
/// retry with the next name. Falls back to NATO+digit (alpha2, bravo2…)
/// if all 26 base names are simultaneously taken.
fn claim_instance_name() -> Result<String> {
    let dir = instances_dir();
    fs::create_dir_all(&dir).ok();

    let alive = live_instances_locked();
    let taken: std::collections::HashSet<String> = alive.iter().map(|i| i.id.clone()).collect();

    // First pass: NATO base names.
    for name in NATO_NAMES {
        if taken.contains(*name) {
            continue;
        }
        if try_o_excl_create(&instance_file(name)).is_ok() {
            return Ok(name.to_string());
        }
    }
    // Second pass: NATO+digit. Cap at 99 to bound the loop —
    // if you're really running 2,574 Untermsself something else is wrong.
    for n in 2..=99 {
        for name in NATO_NAMES {
            let candidate = format!("{}{}", name, n);
            if taken.contains(&candidate) {
                continue;
            }
            if try_o_excl_create(&instance_file(&candidate)).is_ok() {
                return Ok(candidate);
            }
        }
    }
    anyhow::bail!("no free instance name available (capped at NATO×99)")
}

fn try_o_excl_create(path: &Path) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(path)?;
    // Touch the file with an empty {} so concurrent peers see it as
    // taken. Real metadata gets written by `write_initial` immediately
    // after this returns.
    f.write_all(b"{}")?;
    Ok(())
}

/// Read the active instance pointer (`~/.unterm/active.json`).
/// If the pointer is missing, points to a dead instance, or fails to
/// parse, returns the most recently started live instance, or default.
pub fn read() -> InstanceInfo {
    let _g = file_lock().lock();
    // Prefer active.json if it points to a live instance.
    if let Ok(content) = fs::read_to_string(active_pointer_path()) {
        if let Ok(info) = serde_json::from_str::<InstanceInfo>(&content) {
            if pid_alive(info.pid) {
                return info;
            }
        }
    }
    // Fall back to scanning instances/, picking the most recent live one.
    let mut alive = live_instances_locked();
    alive.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    if let Some(info) = alive.into_iter().next() {
        return info;
    }
    // Truly nothing alive: legacy server.json (might be from a previous run).
    fs::read_to_string(server_info_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Read this current process's instance file (or default if not yet written).
pub fn read_current() -> InstanceInfo {
    static LAST_DISK_READ: std::sync::OnceLock<Mutex<Option<(String, std::time::Instant)>>> =
        std::sync::OnceLock::new();
    let id = match current_instance_id() {
        Some(id) => id,
        None => return InstanceInfo::default(),
    };
    let last_read = LAST_DISK_READ.get_or_init(|| Mutex::new(None));
    if let Some(cached) = current_info().lock().clone() {
        if cached.id == id {
            let fresh = last_read
                .lock()
                .as_ref()
                .is_some_and(|(cached_id, at)| {
                    cached_id == &id && at.elapsed() < std::time::Duration::from_millis(1000)
                });
            if fresh {
                return cached;
            }
        }
    }
    let _g = file_lock().lock();
    let info = fs::read_to_string(instance_file(&id))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .or_else(|| current_info().lock().clone())
        .unwrap_or_default();
    if !info.id.is_empty() {
        *current_info().lock() = Some(info.clone());
        *last_read.lock() = Some((id, std::time::Instant::now()));
    }
    info
}

/// Initial write at MCP server startup: claim a NATO name, generate
/// a token, stamp pid + started_at + mcp_port + version + platform.
/// Also seeds active.json (if there's no live active currently) and
/// keeps server.json + auth_token in sync for legacy clients.
pub fn write_initial(mcp_port: u16) -> Result<InstanceInfo> {
    write_initial_with_version(mcp_port, unterm_protocol::PRODUCT_VERSION)
}

/// Register a process while reporting the version of the product binary that
/// owns it.  `unterm-services` deliberately has its own internal crate
/// version, so GUI callers must not expose that implementation detail as the
/// installed Unterm version.
pub fn write_initial_with_version(mcp_port: u16, product_version: &str) -> Result<InstanceInfo> {
    write_initial_with_version_token(mcp_port, product_version, None)
}

/// As `write_initial_with_version`, with the auth token supplied rather than
/// generated. For a window in Core mode: the agent surface it registers is
/// the Core's MCP server, so the token has to be the Core's own — a token
/// minted here would bounce off that surface, and the settings page (which
/// bootstraps its credentials from this record) would 401 on every call.
pub fn write_initial_with_version_token(
    mcp_port: u16,
    product_version: &str,
    auth_token: Option<String>,
) -> Result<InstanceInfo> {
    let _g = file_lock().lock();
    fs::create_dir_all(unterm_dir())?;
    fs::create_dir_all(instances_dir())?;

    let id = claim_instance_name().context("could not claim NATO instance name")?;
    *current_id().lock() = Some(id.clone());

    let info = InstanceInfo {
        id: id.clone(),
        mcp_port,
        http_port: 0,
        auth_token: auth_token.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        pid: std::process::id(),
        started_at: chrono::Local::now().to_rfc3339(),
        title: None,
        cwd: None,
        // Profile binding is resolved later — either by the GUI when
        // the window finishes initializing (reading `index.toml` for
        // the default profile, or showing the picker) or by an MCP
        // `profile.spawn` call that names a profile up front.
        profile: None,
        version: product_version.to_string(),
        product_version: product_version.to_string(),
        build_commit: unterm_protocol::BUILD_COMMIT.to_string(),
        protocol_version: unterm_protocol::PROTOCOL_VERSION.to_string(),
        data_schema_version: unterm_protocol::DATA_SCHEMA_VERSION,
        process_role: ProcessRole::Gui,
        platform: std::env::consts::OS.to_string(),
        agents: Vec::new(),
    };
    write_atomic(&instance_file(&id), &info)?;
    *current_info().lock() = Some(info.clone());

    claim_compat_files_if_needed(&info)?;
    Ok(info)
}

/// Update this instance's file to record the HTTP server's port.
/// Called after the HTTP server successfully binds. Also updates
/// active.json + server.json if we're the active instance.
pub fn set_http_port(port: u16) -> Result<InstanceInfo> {
    let id = match current_instance_id() {
        Some(id) => id,
        None => return Ok(InstanceInfo::default()),
    };
    let _g = file_lock().lock();
    let mut info: InstanceInfo = fs::read_to_string(instance_file(&id))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .or_else(|| current_info().lock().clone())
        .unwrap_or_default();
    info.http_port = port;
    write_atomic(&instance_file(&id), &info)?;
    *current_info().lock() = Some(info.clone());
    claim_compat_files_if_needed(&info)?;
    Ok(info)
}

/// Update this instance's `cwd` field. Called periodically by the
/// foreground update loop. Cheap (one file write); skipped if the
/// value hasn't changed since last write.
pub fn set_cwd(cwd: Option<String>) -> Result<()> {
    {
        let last = last_written_cwd().lock();
        if last.as_ref() == Some(&cwd) {
            return Ok(());
        }
    }

    let id = match current_instance_id() {
        Some(id) => id,
        None => return Ok(()),
    };
    let _g = file_lock().lock();
    let path = instance_file(&id);
    let file_missing = !path.exists();
    let mut info: InstanceInfo = match fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => current_info().lock().clone().unwrap_or_default(),
    };
    if info.id.is_empty() {
        info.id = id.clone();
    }
    if info.pid == 0 {
        info.pid = std::process::id();
    }
    if info.started_at.is_empty() {
        info.started_at = chrono::Local::now().to_rfc3339();
    }
    if info.version.is_empty() {
        info.version = unterm_protocol::PRODUCT_VERSION.to_string();
    }
    if info.product_version.is_empty() {
        info.product_version = info.version.clone();
    }
    if info.build_commit.is_empty() {
        info.build_commit = unterm_protocol::BUILD_COMMIT.to_string();
    }
    if info.protocol_version.is_empty() {
        info.protocol_version = unterm_protocol::PROTOCOL_VERSION.to_string();
    }
    if info.data_schema_version == 0 {
        info.data_schema_version = unterm_protocol::DATA_SCHEMA_VERSION;
    }
    if info.platform.is_empty() {
        info.platform = std::env::consts::OS.to_string();
    }
    if info.cwd == cwd && !file_missing {
        *last_written_cwd().lock() = Some(cwd);
        claim_compat_files_if_needed(&info)?;
        return Ok(()); // no change
    }
    info.cwd = cwd.clone();
    write_atomic(&path, &info)?;
    *current_info().lock() = Some(info.clone());
    claim_compat_files_if_needed(&info)?;
    *last_written_cwd().lock() = Some(cwd);
    Ok(())
}

/// Publish this window's Cockpit rows for peer windows.
///
/// The caller prepares a bounded snapshot off the UI hot path. Identical
/// snapshots do not rewrite the instance file.
pub fn set_agents(agents: Vec<InstanceAgentInfo>) -> Result<()> {
    {
        let last = last_written_agents().lock();
        if last.as_ref() == Some(&agents) {
            return Ok(());
        }
    }
    let id = match current_instance_id() {
        Some(id) => id,
        None => return Ok(()),
    };
    let _g = file_lock().lock();
    let path = instance_file(&id);
    let mut info: InstanceInfo = fs::read_to_string(&path)
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok())
        .or_else(|| current_info().lock().clone())
        .unwrap_or_default();
    info.id = id;
    info.agents = agents.clone();
    write_atomic(&path, &info)?;
    *current_info().lock() = Some(info.clone());
    claim_compat_files_if_needed(&info)?;
    *last_written_agents().lock() = Some(agents);
    Ok(())
}

/// Update this instance's bound identity profile. `None` clears the
/// binding (panes spawned afterward run with the unscoped global env).
/// `Some(id)` pins this window to a profile — subsequent pane spawns
/// will resolve secrets from that profile's keychain entries and
/// inject `UNTERM_PROFILE` + `GIT_AUTHOR_*` + `[env]` + `[secrets]`
/// into their environment. Per the locked design we do NOT respawn
/// already-running panes: the window's mental model is "this whole
/// window is identity X", and existing panes keep whatever env they
/// were spawned with.
pub fn set_profile(profile: Option<String>) -> Result<()> {
    let id = match current_instance_id() {
        Some(id) => id,
        None => return Ok(()),
    };
    let _g = file_lock().lock();
    let mut info: InstanceInfo = match fs::read_to_string(instance_file(&id)) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => current_info().lock().clone().unwrap_or_default(),
    };
    info.profile = profile;
    write_atomic(&instance_file(&id), &info)?;
    *current_info().lock() = Some(info);
    Ok(())
}

/// Update this instance's user-overridable title. None clears the
/// override (auto-title resumes); Some(str) pins a custom title.
pub fn set_title(title: Option<String>) -> Result<()> {
    let id = match current_instance_id() {
        Some(id) => id,
        None => return Ok(()),
    };
    let _g = file_lock().lock();
    let mut info: InstanceInfo = match fs::read_to_string(instance_file(&id)) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => current_info().lock().clone().unwrap_or_default(),
    };
    info.title = title;
    write_atomic(&instance_file(&id), &info)?;
    *current_info().lock() = Some(info);
    Ok(())
}

/// Best-effort cleanup on graceful exit: delete this instance's file
/// and, if we were the active pointer, drop active.json so the next
/// launching peer can claim it. Called from drop / signal / atexit.
///
/// Dead-code-allow is intentional: callers that want explicit cleanup
/// invoke this; on hard crashes, the next-launching instance handles
/// stale files via its PID-liveness scan in `live_instances_locked()`.
/// Both paths are correct, so wiring shutdown into every code-path
/// isn't load-bearing.
#[allow(dead_code)]
pub fn shutdown() {
    let Some(id) = current_instance_id() else {
        return;
    };
    let _g = file_lock().lock();
    *current_info().lock() = None;
    let _ = fs::remove_file(instance_file(&id));
    // Was I the active? If so, hand off — clear the pointer and pick
    // the next live instance, if any. Single-instance agents will
    // re-resolve on next read.
    if let Ok(content) = fs::read_to_string(active_pointer_path()) {
        if let Ok(active) = serde_json::from_str::<InstanceInfo>(&content) {
            if active.id == id {
                let _ = fs::remove_file(active_pointer_path());
                let _ = fs::remove_file(server_info_path());
                let mut alive = live_instances_locked();
                alive.sort_by(|a, b| b.started_at.cmp(&a.started_at));
                if let Some(next) = alive.into_iter().next() {
                    let _ = write_atomic(&active_pointer_path(), &next);
                    let _ = write_atomic(&server_info_path(), &next);
                    let _ = write_legacy_token(&next.auth_token);
                }
            }
        }
    }
}

fn write_atomic<T: Serialize>(path: &Path, info: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(info)?;
    let tmp = path.with_extension("json.tmp");
    write_private_file(&tmp, body.as_bytes())?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn write_private_file(path: &Path, body: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(body)?;
    file.sync_all()?;
    Ok(())
}

fn write_legacy_token(token: &str) -> Result<()> {
    let path = auth_token_path();
    write_private_file(&path, token.as_bytes())
}

fn claim_compat_files_if_needed(info: &InstanceInfo) -> Result<()> {
    let should_claim_active = match fs::read_to_string(active_pointer_path()) {
        Ok(content) => match serde_json::from_str::<InstanceInfo>(&content) {
            Ok(prev) => prev.id == info.id || !pid_alive(prev.pid),
            Err(_) => true,
        },
        Err(_) => true,
    };

    if should_claim_active {
        write_atomic(&active_pointer_path(), info)?;
        // Mirror to legacy server.json for old CLI / agent clients.
        write_atomic(&server_info_path(), info)?;
        write_legacy_token(&info.auth_token)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap()
    }

    struct StateDirGuard {
        previous: Option<std::ffi::OsString>,
    }

    impl StateDirGuard {
        fn set(path: &std::path::Path) -> Self {
            let previous = std::env::var_os("UNTERM_STATE_DIR");
            std::env::set_var("UNTERM_STATE_DIR", path);
            Self { previous }
        }
    }

    impl Drop for StateDirGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var("UNTERM_STATE_DIR", value),
                None => std::env::remove_var("UNTERM_STATE_DIR"),
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn auth_bearing_files_are_user_only() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir()?;
        let direct = root.path().join("auth_token");
        write_private_file(&direct, b"secret")?;
        assert_eq!(fs::metadata(&direct)?.permissions().mode() & 0o777, 0o600);

        let atomic = root.path().join("instance.json");
        write_atomic(&atomic, &serde_json::json!({"auth_token": "secret"}))?;
        assert_eq!(fs::metadata(&atomic)?.permissions().mode() & 0o777, 0o600);
        Ok(())
    }

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("unterm-server-info-{name}-{}", std::process::id()))
    }

    #[test]
    fn legacy_instance_files_default_to_no_published_agents() {
        let info: InstanceInfo = serde_json::from_str(
            r#"{"id":"alpha","mcp_port":1,"http_port":2,"auth_token":"x","pid":3,"started_at":"now"}"#,
        )
        .unwrap();
        assert!(info.agents.is_empty());
    }

    #[test]
    fn compat_claim_writes_active_server_and_legacy_token_schema() -> Result<()> {
        let _lock = env_lock();
        let root = tempfile::tempdir()?;
        let _guard = StateDirGuard::set(root.path());
        let info = InstanceInfo {
            id: "alpha".to_string(),
            mcp_port: 19876,
            http_port: 19877,
            auth_token: "secret-token".to_string(),
            pid: std::process::id(),
            started_at: "2026-08-12T00:00:00+08:00".to_string(),
            title: Some("Home - Powershell".to_string()),
            cwd: Some("C:\\Users\\Alex".to_string()),
            profile: Some("default".to_string()),
            version: unterm_protocol::PRODUCT_VERSION.to_string(),
            product_version: unterm_protocol::PRODUCT_VERSION.to_string(),
            build_commit: "abc123".to_string(),
            protocol_version: unterm_protocol::PROTOCOL_VERSION.to_string(),
            data_schema_version: unterm_protocol::DATA_SCHEMA_VERSION,
            process_role: ProcessRole::Gui,
            platform: std::env::consts::OS.to_string(),
            agents: Vec::new(),
        };

        claim_compat_files_if_needed(&info)?;

        let active: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(root.path().join("active.json"))?)?;
        let server: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(root.path().join("server.json"))?)?;
        for written in [&active, &server] {
            assert_eq!(written["id"], "alpha");
            assert_eq!(written["mcp_port"], 19876);
            assert_eq!(written["http_port"], 19877);
            assert_eq!(written["auth_token"], "secret-token");
            assert_eq!(written["pid"], std::process::id());
            assert_eq!(written["started_at"], "2026-08-12T00:00:00+08:00");
            assert_eq!(written["title"], "Home - Powershell");
            assert_eq!(written["cwd"], "C:\\Users\\Alex");
            assert_eq!(written["version"], unterm_protocol::PRODUCT_VERSION);
            assert_eq!(written["product_version"], unterm_protocol::PRODUCT_VERSION);
            assert_eq!(written["build_commit"], "abc123");
            assert_eq!(
                written["protocol_version"],
                unterm_protocol::PROTOCOL_VERSION
            );
            assert_eq!(
                written["data_schema_version"],
                unterm_protocol::DATA_SCHEMA_VERSION
            );
            assert_eq!(written["process_role"], "gui");
            assert_eq!(written["platform"], std::env::consts::OS);
            assert!(written["agents"].as_array().is_some());
        }
        assert_eq!(
            fs::read_to_string(root.path().join("auth_token"))?,
            "secret-token"
        );
        Ok(())
    }

    #[test]
    fn registry_snapshot_reports_cleanup_and_parse_diagnostics() -> Result<()> {
        let root = test_dir("registry-snapshot");
        let instances = root.join("instances");
        let active = root.join("active.json");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&instances)?;

        let live = InstanceInfo {
            id: "alpha".to_string(),
            pid: std::process::id(),
            started_at: "2026-07-27T00:00:00+08:00".to_string(),
            ..Default::default()
        };
        let stale = InstanceInfo {
            id: "bravo".to_string(),
            pid: 0,
            started_at: "2026-07-27T00:00:01+08:00".to_string(),
            ..Default::default()
        };
        write_atomic(&instances.join("alpha.json"), &live)?;
        write_atomic(&instances.join("bravo.json"), &stale)?;
        fs::write(instances.join("charlie.json"), "{not-json")?;
        fs::write(instances.join("delta.json"), "{}")?;
        write_atomic(&active, &live)?;

        let snapshot = instance_registry_snapshot_from_paths_locked(&instances, &active);

        assert_eq!(snapshot.live_count, 1);
        assert_eq!(snapshot.live[0].id, "alpha");
        assert_eq!(snapshot.stale_removed, 1);
        assert_eq!(snapshot.corrupt_files, 1);
        assert_eq!(snapshot.empty_files, 1);
        assert_eq!(snapshot.active_id.as_deref(), Some("alpha"));
        assert_eq!(snapshot.active_pid_alive, Some(true));
        assert_eq!(snapshot.active_source, "active_pointer");
        assert!(!instances.join("bravo.json").exists());

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn lifecycle_plan_reports_shutdown_handoff_without_closing() -> Result<()> {
        let root = test_dir("lifecycle-plan");
        let instances = root.join("instances");
        let active = root.join("active.json");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&instances)?;

        let current = InstanceInfo {
            id: "alpha".to_string(),
            pid: std::process::id(),
            started_at: "2026-07-27T00:00:00+08:00".to_string(),
            ..Default::default()
        };
        let next = InstanceInfo {
            id: "bravo".to_string(),
            pid: std::process::id(),
            started_at: "2026-07-27T00:00:01+08:00".to_string(),
            ..Default::default()
        };
        write_atomic(&instances.join("alpha.json"), &current)?;
        write_atomic(&instances.join("bravo.json"), &next)?;
        write_atomic(&active, &current)?;

        let plan = instance_lifecycle_plan_from_paths_locked(
            Some("alpha"),
            &instances,
            &active,
            "host_owned",
        );

        assert_eq!(plan.current_id.as_deref(), Some("alpha"));
        assert_eq!(plan.registration_owner, "server_info");
        assert_eq!(plan.registry_file_exists, true);
        assert_eq!(plan.active_id.as_deref(), Some("alpha"));
        assert_eq!(plan.live_count, 2);
        assert_eq!(plan.shutdown.would_remove_registry_file, true);
        assert_eq!(plan.shutdown.would_clear_active_pointer, true);
        assert_eq!(plan.shutdown.would_update_legacy_server, true);
        assert_eq!(plan.shutdown.handoff_id.as_deref(), Some("bravo"));
        assert_eq!(plan.shutdown.native_window_lifecycle, "host_owned");
        assert_eq!(plan.shutdown.values_redacted, true);
        assert!(instances.join("alpha.json").exists());
        assert!(active.exists());

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn shutdown_apply_unregisters_current_and_hands_off_active_pointer() -> Result<()> {
        let root = test_dir("shutdown-apply");
        let instances = root.join("instances");
        let active = root.join("active.json");
        let server = root.join("server.json");
        let token = root.join("auth_token");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&instances)?;

        let current = InstanceInfo {
            id: "alpha".to_string(),
            pid: std::process::id(),
            started_at: "2026-07-27T00:00:00+08:00".to_string(),
            auth_token: "alpha-token".to_string(),
            ..Default::default()
        };
        let next = InstanceInfo {
            id: "bravo".to_string(),
            pid: std::process::id(),
            started_at: "2026-07-27T00:00:01+08:00".to_string(),
            auth_token: "bravo-token".to_string(),
            ..Default::default()
        };
        write_atomic(&instances.join("alpha.json"), &current)?;
        write_atomic(&instances.join("bravo.json"), &next)?;
        write_atomic(&active, &current)?;
        write_atomic(&server, &current)?;
        fs::write(&token, &current.auth_token)?;

        let result = apply_instance_shutdown_from_paths_locked(
            Some("alpha"),
            &instances,
            &active,
            &server,
            &token,
        );

        assert_eq!(result.applied, true);
        assert!(result.errors.is_empty(), "{:?}", result.errors);
        assert_eq!(result.registry_file_removed, true);
        assert_eq!(result.active_pointer_cleared, true);
        assert_eq!(result.legacy_server_removed, true);
        assert_eq!(result.legacy_token_updated, true);
        assert_eq!(result.handoff_id.as_deref(), Some("bravo"));
        assert_eq!(result.native_window_closed, false);
        assert!(!instances.join("alpha.json").exists());
        assert!(instances.join("bravo.json").exists());

        let active_after: InstanceInfo = serde_json::from_str(&fs::read_to_string(&active)?)?;
        let server_after: InstanceInfo = serde_json::from_str(&fs::read_to_string(&server)?)?;
        assert_eq!(active_after.id, "bravo");
        assert_eq!(server_after.id, "bravo");
        assert_eq!(fs::read_to_string(&token)?, "bravo-token");

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn bind_with_fallback_skips_an_occupied_preferred_port() -> Result<()> {
        let occupied = TcpListener::bind((SERVER_BIND, 0u16))?;
        let preferred = occupied.local_addr()?.port();

        let (fallback, port) = bind_with_fallback(preferred)?;
        let addr = fallback.local_addr()?;

        assert_ne!(port, preferred);
        assert_ne!(addr.port(), preferred);
        assert!(addr.ip().is_loopback());
        Ok(())
    }
}

#[cfg(test)]
mod pid_alive_tests {
    use super::*;

    #[test]
    fn this_process_is_alive() {
        assert!(pid_alive(std::process::id()));
    }

    /// A process id nobody has been given reads as dead.
    ///
    /// Without this the registry keeps every window ever opened: entries are
    /// only removed when their process is known to have gone, and a check that
    /// never says so removes nothing. `instance.list` then reports windows
    /// that are not there and routing to one reaches a process that exited.
    #[test]
    fn a_process_id_nobody_has_reads_as_dead() {
        // Above the range the kernels allocate from, and not a multiple of
        // four, which Windows process ids always are.
        assert!(!pid_alive(0xFFFF_FFFE));
        assert!(!pid_alive(0xFFFF_FFFD));
        assert!(!pid_alive(0));
    }

    /// A process that has exited reads as dead, which is the case the registry
    /// actually meets: a window that was open a moment ago and is not now.
    #[test]
    fn a_process_that_has_exited_reads_as_dead() {
        let program = if cfg!(windows) { "cmd" } else { "true" };
        let mut command = std::process::Command::new(program);
        if cfg!(windows) {
            command.args(["/C", "exit"]);
        }
        let Ok(mut child) = command.spawn() else {
            // No shell on PATH is this machine's problem, not the check's.
            return;
        };
        let pid = child.id();
        assert!(pid_alive(pid), "a running child read as dead");
        let _ = child.wait();
        // The handle is closed by `wait`, so nothing is keeping the id alive.
        assert!(!pid_alive(pid), "an exited child read as alive");
    }
}
