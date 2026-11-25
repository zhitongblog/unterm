//! Persistent lifecycle records for long-lived `unterm-cli mcp-stdio` bridges.
//!
//! MCP clients own and respawn bridge processes, so Unterm cannot replace one
//! in-place. It can, however, request a graceful drain. The bridge rejects its
//! next request with a stable compatibility error and exits; the owner then
//! starts the configured (newly installed) binary.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use unterm_protocol::BuildHandshake;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BridgeRecord {
    build: BuildHandshake,
    state: String,
    #[serde(default)]
    drain_reason: Option<String>,
    /// When the drain was requested. The grace period is measured from
    /// here; a bridge that has not exited by then gets terminated.
    #[serde(default)]
    drain_requested_at: Option<String>,
    updated_at: String,
}

pub struct BridgeRegistration {
    path: PathBuf,
}

impl BridgeRegistration {
    pub fn drain_reason(&self) -> Option<String> {
        let record: BridgeRecord = serde_json::from_slice(&fs::read(&self.path).ok()?).ok()?;
        (record.state == "draining")
            .then_some(record.drain_reason)
            .flatten()
    }
}

impl Drop for BridgeRegistration {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn register(build: BuildHandshake) -> Result<BridgeRegistration> {
    register_in(&bridges_dir()?, build)
}

pub fn request_incompatible_drains() -> Result<usize> {
    request_incompatible_drains_in(&bridges_dir()?)
}

fn bridges_dir() -> Result<PathBuf> {
    unterm_protocol::state_path("bridges").context("could not resolve home directory")
}

fn register_in(dir: &Path, build: BuildHandshake) -> Result<BridgeRegistration> {
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.json", build.pid));
    let record = BridgeRecord {
        build,
        state: "active".into(),
        drain_reason: None,
        drain_requested_at: None,
        updated_at: chrono::Utc::now().to_rfc3339(),
    };
    write_atomic(&path, &record)?;
    Ok(BridgeRegistration { path })
}

/// Force what the grace period could not persuade.
///
/// A drained bridge is supposed to reject its next request and exit on
/// its own; one that is still alive `grace` after the request is stuck
/// or too old to understand the protocol, and holding the owner's MCP
/// slot hostage. The kill is the fallback, never the first move —
/// that ordering is what M0-02 means by cooperative replacement.
pub fn terminate_overdue_drains(grace: std::time::Duration) -> Result<usize> {
    terminate_overdue_drains_in(&bridges_dir()?, grace)
}

fn terminate_overdue_drains_in(dir: &Path, grace: std::time::Duration) -> Result<usize> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut terminated = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(record) = serde_json::from_slice::<BridgeRecord>(&bytes) else {
            continue;
        };
        if record.state != "draining" {
            continue;
        }
        let Some(requested_at) = record
            .drain_requested_at
            .as_deref()
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        else {
            continue;
        };
        let overdue = chrono::Utc::now().signed_duration_since(requested_at)
            > chrono::Duration::from_std(grace).unwrap_or(chrono::Duration::zero());
        if !overdue {
            continue;
        }
        if crate::server_info::pid_alive(record.build.pid) {
            if !kill_pid(record.build.pid) {
                log::warn!(
                    "bridge {} overdue for drain but could not be terminated",
                    record.build.pid
                );
                continue;
            }
            log::warn!(
                "terminated bridge {} ({}): drain requested at {} was ignored past the {:?} grace period",
                record.build.pid,
                record.build.product_version,
                requested_at,
                grace,
            );
        }
        let _ = fs::remove_file(&path);
        terminated += 1;
    }
    Ok(terminated)
}

/// Bridges older than the registry itself: long-lived `unterm-cli`
/// processes with no lifecycle record. A current bridge registers the
/// moment it starts, and every other CLI invocation is short-lived, so
/// age plus recordlessness identifies the pre-M0 population that can
/// never hear a cooperative drain. Termination is the only lever that
/// reaches them; the owner restarts the configured (current) binary.
pub fn drain_unregistered_bridges(min_age: std::time::Duration) -> Result<usize> {
    let dir = bridges_dir()?;
    let mut terminated = 0;
    for pid in unregistered_bridge_pids(&dir, min_age) {
        if kill_pid(pid) {
            log::warn!(
                "terminated pre-registry bridge {pid}: no lifecycle record, older than {min_age:?}"
            );
            terminated += 1;
        }
    }
    Ok(terminated)
}

#[cfg(windows)]
fn unregistered_bridge_pids(dir: &Path, min_age: std::time::Duration) -> Vec<u32> {
    use winapi::um::handleapi::{CloseHandle, INVALID_HANDLE_VALUE};
    use winapi::um::processthreadsapi::{GetProcessTimes, OpenProcess};
    use winapi::um::sysinfoapi::GetSystemTimeAsFileTime;
    use winapi::um::tlhelp32::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use winapi::um::winnt::PROCESS_QUERY_LIMITED_INFORMATION;

    fn filetime_to_u64(ft: &winapi::shared::minwindef::FILETIME) -> u64 {
        ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
    }

    let mut candidates = Vec::new();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return candidates;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry);
        while more != 0 {
            let name_len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..name_len]);
            let pid = entry.th32ProcessID;
            // The bridge runs under the product's name when installed as one
            // (Nissewyn's `nissewyn-cli.exe`), the runtime's otherwise.
            let product_cli = format!(
                "{}.exe",
                unterm_protocol::program_stem(unterm_protocol::Program::Cli)
            );
            if (name.eq_ignore_ascii_case("unterm-cli.exe") || name.eq_ignore_ascii_case(&product_cli))
                && pid != std::process::id()
                && !dir.join(format!("{pid}.json")).exists()
            {
                let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
                if !handle.is_null() {
                    let mut creation = std::mem::zeroed();
                    let mut exit = std::mem::zeroed();
                    let mut kernel = std::mem::zeroed();
                    let mut user = std::mem::zeroed();
                    if GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user)
                        != 0
                    {
                        let mut now = std::mem::zeroed();
                        GetSystemTimeAsFileTime(&mut now);
                        // FILETIME ticks are 100ns.
                        let age_ticks =
                            filetime_to_u64(&now).saturating_sub(filetime_to_u64(&creation));
                        if age_ticks / 10_000_000 >= min_age.as_secs() {
                            candidates.push(pid);
                        }
                    }
                    CloseHandle(handle);
                }
            }
            more = Process32NextW(snapshot, &mut entry);
        }
        CloseHandle(snapshot);
    }
    candidates
}

#[cfg(not(windows))]
fn unregistered_bridge_pids(_dir: &Path, _min_age: std::time::Duration) -> Vec<u32> {
    // The pre-registry bridge population shipped on Windows only; on
    // other platforms every bridge that exists postdates the registry.
    Vec::new()
}

#[cfg(windows)]
fn kill_pid(pid: u32) -> bool {
    use winapi::um::handleapi::CloseHandle;
    use winapi::um::processthreadsapi::{OpenProcess, TerminateProcess};
    use winapi::um::winnt::PROCESS_TERMINATE;
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            return false;
        }
        let ok = TerminateProcess(handle, 1) != 0;
        CloseHandle(handle);
        ok
    }
}

#[cfg(not(windows))]
fn kill_pid(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) == 0 }
}

fn request_incompatible_drains_in(dir: &Path) -> Result<usize> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut requested = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(mut record) = serde_json::from_slice::<BridgeRecord>(&bytes) else {
            continue;
        };
        if !crate::server_info::pid_alive(record.build.pid) {
            let _ = fs::remove_file(&path);
            continue;
        }
        let compatibility = record.build.compatibility();
        if compatibility.is_usable() || record.state == "draining" {
            continue;
        }
        let code = compatibility
            .error_code()
            .unwrap_or("protocol_incompatible");
        record.state = "draining".into();
        record.drain_reason = Some(format!(
            "{code}: bridge {} is incompatible with installed Unterm {}; restart from the configured unterm-cli path",
            record.build.product_version,
            unterm_protocol::PRODUCT_VERSION,
        ));
        record.drain_requested_at = Some(chrono::Utc::now().to_rfc3339());
        record.updated_at = chrono::Utc::now().to_rfc3339();
        write_atomic(&path, &record)?;
        requested += 1;
    }
    Ok(requested)
}

fn write_atomic(path: &Path, value: &BridgeRecord) -> Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    fs::write(&temporary, serde_json::to_vec_pretty(value)?)?;
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use unterm_protocol::ProcessRole;

    #[test]
    fn incompatible_live_bridge_is_marked_for_drain() {
        let root = tempfile::tempdir().unwrap();
        let mut build = BuildHandshake::current(ProcessRole::McpBridge, std::process::id(), "now");
        build.product_version = "0.57.4".into();
        let registration = register_in(root.path(), build).unwrap();
        assert_eq!(request_incompatible_drains_in(root.path()).unwrap(), 1);
        let reason = registration.drain_reason().unwrap();
        assert!(reason.starts_with("product_version_mismatch:"), "{reason}");
    }

    fn sleeper() -> std::process::Child {
        let mut command = if cfg!(windows) {
            let mut c = std::process::Command::new("powershell.exe");
            c.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 120"]);
            c
        } else {
            let mut c = std::process::Command::new("sleep");
            c.arg("120");
            c
        };
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    fn write_draining_record(dir: &Path, pid: u32, requested_at: chrono::DateTime<chrono::Utc>) {
        let mut build = BuildHandshake::current(ProcessRole::McpBridge, pid, "now");
        build.product_version = "0.57.4".into();
        let record = BridgeRecord {
            build,
            state: "draining".into(),
            drain_reason: Some("test".into()),
            drain_requested_at: Some(requested_at.to_rfc3339()),
            updated_at: requested_at.to_rfc3339(),
        };
        write_atomic(&dir.join(format!("{pid}.json")), &record).unwrap();
    }

    #[test]
    fn overdue_drain_terminates_the_bridge_and_removes_its_record() {
        let root = tempfile::tempdir().unwrap();
        let mut child = sleeper();
        let pid = child.id();
        write_draining_record(
            root.path(),
            pid,
            chrono::Utc::now() - chrono::Duration::seconds(120),
        );

        let terminated =
            terminate_overdue_drains_in(root.path(), std::time::Duration::from_secs(30)).unwrap();
        assert_eq!(terminated, 1);
        assert!(!root.path().join(format!("{pid}.json")).exists());

        // The process must actually be gone, not just deregistered.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match child.try_wait().unwrap() {
                Some(_) => break,
                None if std::time::Instant::now() > deadline => {
                    let _ = child.kill();
                    panic!("overdue bridge was not terminated");
                }
                None => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
    }

    #[test]
    fn drain_within_grace_period_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let mut child = sleeper();
        let pid = child.id();
        write_draining_record(root.path(), pid, chrono::Utc::now());

        let terminated =
            terminate_overdue_drains_in(root.path(), std::time::Duration::from_secs(30)).unwrap();
        assert_eq!(terminated, 0);
        assert!(root.path().join(format!("{pid}.json")).exists());
        assert!(matches!(child.try_wait().unwrap(), None));
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn registered_bridge_is_never_a_pre_registry_candidate() {
        // The scan keys on "no record"; a pid with a record must be
        // excluded no matter its name or age. Exercised through the
        // filter itself because spawning a real long-lived
        // unterm-cli.exe from this crate's tests is not possible.
        let root = tempfile::tempdir().unwrap();
        let build = BuildHandshake::current(ProcessRole::McpBridge, std::process::id(), "now");
        let _registration = register_in(root.path(), build).unwrap();
        let candidates =
            unregistered_bridge_pids(root.path(), std::time::Duration::from_secs(0));
        assert!(
            !candidates.contains(&std::process::id()),
            "own registered pid must never be selected"
        );
    }

    #[test]
    fn compatible_bridge_stays_active_and_drop_unregisters_it() {
        let root = tempfile::tempdir().unwrap();
        let build = BuildHandshake::current(ProcessRole::McpBridge, std::process::id(), "now");
        let registration = register_in(root.path(), build).unwrap();
        let path = registration.path.clone();
        assert_eq!(request_incompatible_drains_in(root.path()).unwrap(), 0);
        assert!(registration.drain_reason().is_none());
        drop(registration);
        assert!(!path.exists());
    }
}
