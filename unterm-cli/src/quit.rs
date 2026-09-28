//! `unterm-cli quit` -- end every Unterm process of this user, and check.
//!
//! In order:
//!
//! 1. Every window process in this logon session (`unterm.exe`: open
//!    windows, one parked in the tray, the elevated administrator window) is
//!    asked to quit through its `Local\UntermQuit-<pid>` event, the same
//!    orderly way its own close button would. Windows first, because a
//!    window that saw its Core vanish would start another one.
//! 2. The Core named by this state directory's discovery record is sent
//!    `core.shutdown`, which ends every session. A `--headless` Core is left
//!    running unless `--all`: the user started it to outlive windows.
//! 3. It waits up to `--wait` seconds for all of them to be gone. With
//!    `--all`, whatever is still there after that -- a window too old to
//!    listen, a hung one, a Core from another state directory -- is
//!    terminated with everything below it (a Core too old to hold a session
//!    job would otherwise leave its shells behind).
//!
//! Exits non-zero if anything it targeted is still running at the end.

use anyhow::Result;
use clap::Args;
use serde_json::json;
use std::io::{BufRead, BufReader, Write};
use std::time::{Duration, Instant};
use unterm_services::process_lifetime::{self, UntermProcessKind};
use unterm_services::server_info::pid_alive;

#[derive(Debug, Args, Clone)]
pub struct QuitCommand {
    /// Also stop a `--headless` Core and Cores from other state directories,
    /// and terminate whatever did not quit when asked.
    #[arg(long)]
    pub all: bool,

    /// Only processes started from the same directory as this unterm-cli --
    /// a development build, leaving an installed Unterm alone (or the other
    /// way round).
    #[arg(long = "this-install")]
    pub this_install: bool,

    /// Seconds to wait for everything to be gone before reporting.
    #[arg(long, value_name = "secs", default_value_t = 10)]
    pub wait: u64,
}

#[derive(serde::Deserialize)]
struct CoreRecord {
    endpoint: String,
    token: String,
    pid: u32,
    #[serde(default)]
    headless: bool,
}

fn read_core_record() -> Option<CoreRecord> {
    let path = unterm_protocol::core_discovery_path()?;
    let record: CoreRecord = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    pid_alive(record.pid).then_some(record)
}

/// Send one request on the Core's own IPC and return the reply.
fn core_request(record: &CoreRecord, method: &str) -> Result<serde_json::Value> {
    let mut stream = std::net::TcpStream::connect(&record.endpoint)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let frame = json!({
        "id": format!("quit-{}", std::process::id()),
        "method": method,
        "token": record.token,
        "params": {},
    });
    writeln!(stream, "{frame}")?;
    stream.flush()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

/// A directory in a form two spellings of the same place compare equal in.
fn normalized(path: &std::path::Path) -> String {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = path.to_string_lossy().to_string();
    if cfg!(windows) {
        text.to_ascii_lowercase()
    } else {
        text
    }
}

fn wait_until_gone(pids: &[u32], deadline: Instant) {
    while Instant::now() < deadline && pids.iter().any(|&pid| pid_alive(pid)) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[derive(Default)]
struct Report {
    /// Asked to quit (windows) or to shut down (Cores): pid and what it is.
    asked: Vec<(u32, &'static str)>,
    /// Found but not asked, and why.
    left: Vec<(u32, String)>,
    /// Terminated after not quitting in time (`--all`).
    terminated: Vec<u32>,
}

pub fn run(cmd: QuitCommand, json_out: bool) -> Result<()> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(cmd.wait);
    let mut report = Report::default();
    let install_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(normalized));
    let processes: Vec<_> = process_lifetime::running_unterm_processes()
        .into_iter()
        .filter(|process| {
            !cmd.this_install
                || process
                    .path
                    .as_deref()
                    .and_then(|path| path.parent())
                    .map(normalized)
                    == install_dir
        })
        .collect();
    let core = read_core_record().filter(|core| {
        !cmd.this_install
            || process_lifetime::executable_path(core.pid)
                .as_deref()
                .and_then(|path| path.parent())
                .map(normalized)
                == install_dir
    });

    // 1. Windows.
    let mut windows = Vec::new();
    // Found, but with no way to ask: terminated at once under `--all` rather
    // than waited on for a reply that cannot come.
    let mut unaskable = Vec::new();
    let mut asked_windows = Vec::new();
    for process in processes
        .iter()
        .filter(|process| process.kind == UntermProcessKind::Gui)
    {
        match process_lifetime::signal_quit(process.pid) {
            Ok(true) => {
                report.asked.push((process.pid, "window"));
                asked_windows.push(process.pid);
            }
            Ok(false) => {
                unaskable.push(process.pid);
                report.left.push((
                    process.pid,
                    "window not listening for quit (older build?)".into(),
                ))
            }
            Err(err) => {
                unaskable.push(process.pid);
                report
                    .left
                    .push((process.pid, format!("window could not be asked: {err}")))
            }
        }
        windows.push(process.pid);
    }
    // Give the windows most of the budget, but keep a little for the Core.
    let windows_deadline = deadline
        .checked_sub(Duration::from_secs(2))
        .filter(|at| *at > Instant::now())
        .unwrap_or(deadline);
    wait_until_gone(&asked_windows, windows_deadline);

    // 2. The Core of this state directory.
    let mut targets = windows.clone();
    if let Some(core) = &core {
        if core.headless && !cmd.all {
            report.left.push((
                core.pid,
                "headless Core left running (use --all to stop it)".into(),
            ));
        } else if pid_alive(core.pid) {
            match core_request(core, "core.shutdown") {
                Ok(_) => report.asked.push((core.pid, "core")),
                // It may have stopped itself as the last window left.
                Err(err) if pid_alive(core.pid) => report
                    .left
                    .push((core.pid, format!("Core could not be asked: {err:#}"))),
                Err(_) => {}
            }
            targets.push(core.pid);
        }
    }
    // Cores this record does not name: other state directories, or stale
    // ones whose record is gone. There is no token to ask them with.
    for process in processes
        .iter()
        .filter(|process| process.kind == UntermProcessKind::Core)
        .filter(|process| core.as_ref().map(|core| core.pid) != Some(process.pid))
    {
        if cmd.all {
            targets.push(process.pid);
            unaskable.push(process.pid);
        } else {
            report.left.push((
                process.pid,
                "Core of another state directory (use --all to stop it)".into(),
            ));
        }
    }
    if cmd.all {
        report.left.retain(|(pid, _)| !unaskable.contains(pid));
        for &pid in &unaskable {
            if pid_alive(pid) && process_lifetime::terminate_tree(pid) {
                report.terminated.push(pid);
            }
        }
    }
    wait_until_gone(&targets, deadline);

    // 3. Whatever did not go when asked.
    if cmd.all {
        for &pid in &targets {
            if pid_alive(pid) && process_lifetime::terminate_tree(pid) {
                report.terminated.push(pid);
            }
        }
        wait_until_gone(&targets, Instant::now() + Duration::from_secs(3));
    }

    let survivors: Vec<u32> = targets
        .iter()
        .copied()
        .filter(|&pid| pid_alive(pid))
        .collect();
    // A window that could not be asked is a survivor too, even without
    // `--all`: it was a target, and it is still there.
    let elapsed = started.elapsed().as_secs_f64();

    if json_out {
        crate::output::print_json(&json!({
            "asked": report.asked.iter().map(|(pid, kind)| json!({"pid": pid, "kind": kind})).collect::<Vec<_>>(),
            "left_running": report.left.iter().map(|(pid, why)| json!({"pid": pid, "reason": why})).collect::<Vec<_>>(),
            "terminated": report.terminated,
            "survivors": survivors,
            "elapsed_secs": elapsed,
            "ok": survivors.is_empty(),
        }));
    } else {
        if report.asked.is_empty() && report.left.is_empty() && targets.is_empty() {
            println!("no Unterm process is running");
        }
        for (pid, kind) in &report.asked {
            let state = if pid_alive(*pid) {
                "still running"
            } else {
                "gone"
            };
            println!("asked {kind} {pid} to quit: {state}");
        }
        for pid in &report.terminated {
            println!("terminated {pid} (could not be asked, or did not quit in time)");
        }
        for (pid, why) in &report.left {
            println!("left {pid}: {why}");
        }
        if survivors.is_empty() {
            println!("done in {elapsed:.1}s");
        } else {
            eprintln!(
                "still running after {elapsed:.1}s: {}",
                survivors
                    .iter()
                    .map(|pid| pid.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    if !survivors.is_empty() {
        std::process::exit(1);
    }
    Ok(())
}
