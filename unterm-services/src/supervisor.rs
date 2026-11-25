//! The processes Unterm is made of, and what state each one is in.
//!
//! Since M1 there are three roles that can exist independently: the **Core**
//! that owns the sessions, the **GUI** that draws them, and the **MCP
//! server** the Core hosts for agents. The property that matters is that they
//! are genuinely independent — a Core with no GUI is a working Unterm, an
//! agent driving sessions with nobody logged in is the normal case, and a GUI
//! that dies must not take the work with it.
//!
//! This module is what can *say* that: discovery from the records each
//! process writes, a readiness that distinguishes alive from usable, and the
//! transitions a machine going to sleep or a process being killed put them
//! through.
//!
//! **Liveness is not readiness**, and conflating them is the bug this exists
//! to prevent. A Core that is running but still replaying scrollback is alive
//! and not ready; a client told "ready" then gets errors it cannot explain.
//! A process that is *ready* answers work now.

use serde::Serialize;
use std::path::PathBuf;

/// One of the three things Unterm runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Owns sessions, the task store and the provider registry.
    Core,
    /// Draws them. Optional, by design.
    Gui,
    /// What agents connect to. Hosted by the Core since M1, but reported
    /// separately: "the Core is up" and "agents can reach it" are different
    /// claims and only one of them is what an agent cares about.
    Mcp,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Core => "core",
            Role::Gui => "gui",
            Role::Mcp => "mcp",
        }
    }

    pub const ALL: [Role; 3] = [Role::Core, Role::Gui, Role::Mcp];
}

/// What a process is doing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Health {
    /// Nothing is running, and nothing says one should be.
    Absent,
    /// A record exists but the process behind it does not. What a crash
    /// leaves: distinct from `Absent` because it means cleanup is owed.
    Stale { pid: u32, since: Option<String> },
    /// Running, not yet answering.
    Starting { pid: u32 },
    /// Running and answering.
    Ready { pid: u32, endpoint: Option<String> },
    /// Running, finishing what it has and accepting nothing new.
    Draining { pid: u32 },
}

impl Health {
    pub fn as_str(&self) -> &'static str {
        match self {
            Health::Absent => "absent",
            Health::Stale { .. } => "stale",
            Health::Starting { .. } => "starting",
            Health::Ready { .. } => "ready",
            Health::Draining { .. } => "draining",
        }
    }

    pub fn pid(&self) -> Option<u32> {
        match self {
            Health::Absent => None,
            Health::Stale { pid, .. }
            | Health::Starting { pid }
            | Health::Ready { pid, .. }
            | Health::Draining { pid } => Some(*pid),
        }
    }

    /// Whether work can be given to it now.
    pub fn is_usable(&self) -> bool {
        matches!(self, Health::Ready { .. })
    }
}

/// One process as the supervisor sees it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Process {
    pub role: Role,
    pub health: Health,
    /// Where the claim came from, so an operator can look at the same file.
    pub source: Option<String>,
    pub version: Option<String>,
}

/// What the Core wrote about itself, if anything.
///
/// Read from `unterm-protocol`'s resolution of the Core's own directory, not
/// from the general state dir. Those are different places on every platform,
/// and asking the wrong one is how a running Core gets reported as absent —
/// found on a Linux box where the two diverge and `UNTERM_STATE_DIR`, which
/// hides the difference, was not set.
fn core_record() -> Option<(serde_json::Value, PathBuf)> {
    let path = unterm_protocol::core_discovery_path()?;
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok().map(|value| (value, path))
}

/// Whether a pid belongs to a process that is still alive.
///
/// Reuses the same check the instance registry has used since M0: a pid that
/// has been recycled is the failure mode, and having one implementation of
/// this means one place to fix it.
fn alive(pid: u32) -> bool {
    crate::server_info::pid_alive(pid)
}

/// Look at what is running, without starting anything.
///
/// Deliberately read-only. A status call that started a process would make
/// "is anything running" impossible to ask.
pub fn survey() -> Vec<Process> {
    let mut processes = Vec::new();

    let core = core_record();
    let (core_health, core_source, core_version) = match &core {
        Some((record, path)) => {
            let pid = record.get("pid").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let health = if pid == 0 {
                Health::Absent
            } else if !alive(pid) {
                // The record outlived the process: a crash, not an absence.
                Health::Stale {
                    pid,
                    since: record
                        .get("started_at")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                }
            } else if record
                .get("endpoint")
                .and_then(|v| v.as_str())
                .is_some_and(|endpoint| !endpoint.is_empty())
            {
                Health::Ready {
                    pid,
                    endpoint: record
                        .get("endpoint")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                }
            } else {
                Health::Starting { pid }
            };
            (
                health,
                Some(path.display().to_string()),
                record
                    .get("product_version")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            )
        }
        None => (Health::Absent, None, None),
    };
    processes.push(Process {
        role: Role::Core,
        health: core_health.clone(),
        source: core_source.clone(),
        version: core_version.clone(),
    });

    // The MCP server rides in the Core process but is a separate claim: a
    // Core that has not opened its port yet is not somewhere an agent can go.
    let mcp_health = match (&core, &core_health) {
        (Some((record, _)), Health::Ready { pid, .. }) => {
            match record.get("mcp_port").and_then(|v| v.as_u64()) {
                Some(port) if port > 0 => Health::Ready {
                    pid: *pid,
                    endpoint: Some(format!("127.0.0.1:{port}")),
                },
                _ => Health::Starting { pid: *pid },
            }
        }
        (_, Health::Stale { pid, since }) => Health::Stale {
            pid: *pid,
            since: since.clone(),
        },
        _ => Health::Absent,
    };
    processes.push(Process {
        role: Role::Mcp,
        health: mcp_health,
        source: core_source,
        version: core_version,
    });

    // The GUI announces itself through the instance registry, which already
    // knows how to tell a live instance from a record nobody cleaned up.
    let instances = crate::server_info::list_live_instances();
    let gui = instances.first();
    processes.push(Process {
        role: Role::Gui,
        health: match gui {
            Some(instance) if alive(instance.pid) => Health::Ready {
                pid: instance.pid,
                endpoint: (instance.http_port > 0)
                    .then(|| format!("127.0.0.1:{}", instance.http_port)),
            },
            Some(instance) => Health::Stale {
                pid: instance.pid,
                since: None,
            },
            // Absent, not broken: Unterm without a window is a supported way
            // to run it, and reporting that as a fault would train people to
            // ignore the field.
            None => Health::Absent,
        },
        source: gui.map(|instance| instance.id.clone()),
        version: gui.and_then(|instance| Some(instance.product_version.clone())),
    });

    processes.sort_by_key(|process| process.role);
    processes
}

/// Whether Unterm can do work right now, with or without a window.
///
/// M7's first gate in one function: a Core and its MCP server, no GUI
/// required.
pub fn can_work_without_ui() -> bool {
    let processes = survey();
    let usable = |role: Role| {
        processes
            .iter()
            .find(|process| process.role == role)
            .is_some_and(|process| process.health.is_usable())
    };
    usable(Role::Core) && usable(Role::Mcp)
}

/// What a machine event does to the processes.
///
/// Written down as a table rather than handled where each notification
/// arrives, because the rule is the same everywhere and four copies of it
/// would be four subtly different rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Machine {
    /// The machine is going to sleep. Nothing is killed: sessions survive a
    /// closed lid, and a terminal that dropped everything on sleep would be
    /// useless on a laptop.
    Sleep,
    /// Back from sleep. Everything is re-probed rather than assumed: a
    /// provider's port, a network mount and the clock can all have moved.
    Wake,
    /// The user is logging out. Work is drained — finished, not abandoned.
    Logout,
    /// The machine is going down, with less time than a logout.
    Shutdown,
    /// A process died without saying anything.
    Crash,
}

/// What the supervisor should do about a machine event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Carry on; nothing to do.
    Hold,
    /// Ask everything whether it is still what it was.
    Reprobe,
    /// Finish what is running, accept nothing new, then exit.
    Drain,
    /// Stop now, having written down where things were.
    StopNow,
    /// Turn what the dead process left into verdicts and take back its
    /// claims.
    Reconcile,
}

pub fn action_for(event: Machine) -> Action {
    match event {
        // Sessions survive a closed lid. This is the one people notice.
        Machine::Sleep => Action::Hold,
        Machine::Wake => Action::Reprobe,
        Machine::Logout => Action::Drain,
        // A shutdown gives seconds, not minutes: recording where things were
        // beats finishing them and being killed halfway through.
        Machine::Shutdown => Action::StopNow,
        Machine::Crash => Action::Reconcile,
    }
}

/// Clean up after processes that are no longer there.
///
/// Returns what was reconciled. Runs the task store's own recovery, which is
/// where "a step nobody is working" becomes a verdict a reader can act on.
pub fn reconcile_after_crash() -> anyhow::Result<serde_json::Value> {
    let mut stale = Vec::new();
    for process in survey() {
        if let Health::Stale { pid, .. } = process.health {
            stale.push(serde_json::json!({"role": process.role.as_str(), "pid": pid}));
        }
    }
    let recovery = match crate::cockpit::fleet_store::tasks() {
        Some(store) => {
            let recovered = store.recover()?;
            serde_json::json!({
                "steps": recovered.steps_interrupted.len(),
                "runs": recovered.runs_interrupted.len(),
                "tasks": recovered.tasks_interrupted.len(),
            })
        }
        None => serde_json::Value::Null,
    };
    Ok(serde_json::json!({"stale": stale, "recovered": recovery}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolate() -> crate::test_env::IsolatedEnv {
        crate::test_env::IsolatedEnv::new()
    }

    fn write_core(dir: &tempfile::TempDir, record: serde_json::Value) {
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("core.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn the_supervisor_reads_where_the_core_writes() {
        let _lock = crate::test_env::lock();
        let prev = std::env::var_os("UNTERM_STATE_DIR");
        // The guard for the bug this cost a release: the Core resolved its
        // discovery record one way and the supervisor another, and with
        // `UNTERM_STATE_DIR` set — as every test had it — the two agreed.
        // Without it they did not, and a running Core read as absent.
        std::env::remove_var("UNTERM_STATE_DIR");
        let core_path = unterm_protocol::core_discovery_path();
        let expected_path = unterm_protocol::core_state_dir().map(|dir| dir.join("core.json"));
        let core_dir = unterm_protocol::core_state_dir();
        let state_dir = unterm_protocol::state_dir();
        if let Some(val) = prev {
            std::env::set_var("UNTERM_STATE_DIR", val);
        }
        assert_eq!(core_path, expected_path);
        // And the two directories are genuinely different, so this is not a
        // tautology that would pass if somebody collapsed them by accident.
        assert_ne!(
            core_dir, state_dir,
            "if these ever become the same, this test is no longer proving anything"
        );
    }

    #[test]
    fn nothing_running_is_absent_rather_than_broken() {
        let _dir = isolate();
        let survey = survey();
        assert_eq!(survey.len(), 3);
        for process in survey {
            assert_eq!(process.health, Health::Absent, "{:?}", process.role);
        }
        assert!(!can_work_without_ui());
    }

    #[test]
    fn a_live_core_with_no_window_is_a_working_unterm() {
        // M7's first gate. A GUI-less Unterm is a supported way to run it,
        // and reporting that as a fault would teach people to ignore the
        // field that matters.
        let dir = isolate();
        write_core(
            &dir,
            serde_json::json!({
                "pid": std::process::id(),
                "endpoint": "127.0.0.1:1234",
                "mcp_port": 1235,
                "product_version": "0.67.0",
            }),
        );

        let survey = survey();
        let core = survey.iter().find(|p| p.role == Role::Core).unwrap();
        let mcp = survey.iter().find(|p| p.role == Role::Mcp).unwrap();
        let gui = survey.iter().find(|p| p.role == Role::Gui).unwrap();
        assert!(core.health.is_usable());
        assert!(mcp.health.is_usable());
        assert_eq!(gui.health, Health::Absent);
        assert!(can_work_without_ui());
    }

    #[test]
    fn a_core_that_has_not_opened_its_port_is_not_somewhere_an_agent_can_go() {
        // "The Core is up" and "agents can reach it" are different claims.
        let dir = isolate();
        write_core(
            &dir,
            serde_json::json!({
                "pid": std::process::id(),
                "endpoint": "127.0.0.1:1234",
                "mcp_port": 0,
            }),
        );
        let survey = survey();
        assert!(survey.iter().find(|p| p.role == Role::Core).unwrap().health.is_usable());
        assert!(!survey.iter().find(|p| p.role == Role::Mcp).unwrap().health.is_usable());
        assert!(!can_work_without_ui());
    }

    #[test]
    fn a_record_without_its_process_is_stale_not_absent() {
        // The difference is whether cleanup is owed. "Absent" says nothing
        // happened; "stale" says something died here.
        let dir = isolate();
        write_core(
            &dir,
            serde_json::json!({
                "pid": 999_999_999u32,
                "endpoint": "127.0.0.1:1234",
                "mcp_port": 1235,
                "started_at": "2026-08-17T00:00:00Z",
            }),
        );
        let survey = survey();
        let core = &survey.iter().find(|p| p.role == Role::Core).unwrap().health;
        assert!(matches!(core, Health::Stale { .. }), "{core:?}");
        assert_eq!(core.pid(), Some(999_999_999));
        // And the MCP claim inherits it rather than reporting ready on a
        // process that is gone.
        let mcp = &survey.iter().find(|p| p.role == Role::Mcp).unwrap().health;
        assert!(matches!(mcp, Health::Stale { .. }), "{mcp:?}");
        assert!(!can_work_without_ui());
    }

    #[test]
    fn a_core_still_starting_is_alive_and_not_usable() {
        let dir = isolate();
        write_core(
            &dir,
            serde_json::json!({"pid": std::process::id(), "endpoint": "", "mcp_port": 0}),
        );
        let survey = survey();
        let core = &survey.iter().find(|p| p.role == Role::Core).unwrap().health;
        assert!(matches!(core, Health::Starting { .. }), "{core:?}");
        assert!(
            !core.is_usable(),
            "a starting process was offered work it cannot do yet"
        );
    }

    #[test]
    fn sleeping_does_not_end_anybody_s_work() {
        // The one people notice: a terminal that dropped its sessions when
        // the lid closed would be unusable on a laptop.
        assert_eq!(action_for(Machine::Sleep), Action::Hold);
    }

    #[test]
    fn waking_re_probes_rather_than_assuming() {
        // A provider's port, a network mount and the clock can all have moved
        // while the machine was away.
        assert_eq!(action_for(Machine::Wake), Action::Reprobe);
    }

    #[test]
    fn logging_out_drains_and_shutting_down_does_not() {
        // A logout has minutes; a shutdown has seconds. Recording where
        // things were beats being killed halfway through finishing them.
        assert_eq!(action_for(Machine::Logout), Action::Drain);
        assert_eq!(action_for(Machine::Shutdown), Action::StopNow);
    }

    #[test]
    fn a_crash_is_reconciled() {
        let dir = isolate();
        write_core(
            &dir,
            serde_json::json!({"pid": 999_999_999u32, "endpoint": "127.0.0.1:1", "mcp_port": 2}),
        );
        assert_eq!(action_for(Machine::Crash), Action::Reconcile);

        let report = reconcile_after_crash().unwrap();
        let stale = report["stale"].as_array().unwrap();
        assert_eq!(stale.len(), 2, "{report}");
        assert!(report["recovered"].is_object());
    }

    #[test]
    fn a_survey_starts_nothing() {
        // A status call that started a process would make "is anything
        // running" an unanswerable question.
        let dir = isolate();
        let before: Vec<std::path::PathBuf> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        survey();
        let after: Vec<std::path::PathBuf> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        assert_eq!(before, after);
    }
}
