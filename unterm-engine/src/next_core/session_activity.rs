use super::{lifecycle, process_tree, session_registry};
#[cfg(test)]
use super::{runtime::NextCoreRuntime, NextCoreSession};
use crate::SessionActivitySnapshot;
use anyhow::Result;
use std::time::Instant;

#[cfg(test)]
pub(super) fn read_snapshot(
    state: &mut NextCoreRuntime,
    pane_id: usize,
    now: Instant,
) -> Result<SessionActivitySnapshot> {
    let session = session_registry::session_mut(state, pane_id)?;
    let (snapshot, dead_reason) = snapshot(session, now);
    if let Some(reason) = dead_reason {
        lifecycle::record_dead_reason(state, reason);
    }
    Ok(snapshot)
}

pub(in crate::next_core) fn read_snapshot_concurrent(
    pane_id: usize,
    now: Instant,
) -> Result<SessionActivitySnapshot> {
    let (is_dead, root_pid, shell_process_name, activity) =
        super::runtime::with_current_mut(|state| -> Result<_> {
            let session = session_registry::session_mut(state, pane_id)?;
            let dead_reason = lifecycle::refresh_liveness(session);
            let is_dead = session.snapshot.is_dead;
            let root_pid = session.root_pid;
            let shell_process_name = session.snapshot.shell.process_name.clone();
            let activity = session.activity.clone();
            if let Some(reason) = dead_reason {
                lifecycle::record_dead_reason(state, reason);
            }
            Ok((is_dead, root_pid, shell_process_name, activity))
        })?;
    let process = process_tree::snapshot(root_pid, &shell_process_name);
    let foreground_process = process
        .as_ref()
        .map(|process| process.foreground_process.clone())
        .unwrap_or(shell_process_name);
    let activity = activity.lock();
    Ok(SessionActivitySnapshot {
        idle: is_dead || activity.is_idle(now),
        foreground_process,
        process,
        input: activity.input.clone(),
        output: activity.output.clone(),
        paste: activity.paste.clone(),
        screen: activity.screen.clone(),
    })
}

#[cfg(test)]
pub(super) fn snapshot(
    session: &mut NextCoreSession,
    now: Instant,
) -> (SessionActivitySnapshot, Option<String>) {
    let dead_reason = lifecycle::refresh_liveness(session);
    let is_dead = session.snapshot.is_dead;
    let process = process_tree::snapshot(session.root_pid, &session.snapshot.shell.process_name);
    let foreground_process = process
        .as_ref()
        .map(|process| process.foreground_process.clone())
        .unwrap_or_else(|| session.snapshot.shell.process_name.clone());
    let activity = session.activity.lock();
    let snapshot = SessionActivitySnapshot {
        idle: is_dead || activity.is_idle(now),
        foreground_process,
        process,
        input: activity.input.clone(),
        output: activity.output.clone(),
        paste: activity.paste.clone(),
        screen: activity.screen.clone(),
    };
    (snapshot, dead_reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::next_core::{launch, session_defaults, session_runtime};
    use parking_lot::Mutex;
    use std::io::Write;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[test]
    fn dead_session_reports_idle_and_dead_reason() {
        let mut session = sample_session();
        session.dead.store(true, Ordering::Release);
        *session.dead_reason.lock() = Some("reader_eof".to_string());

        let (activity, reason) = snapshot(&mut session, Instant::now());

        assert!(activity.idle);
        assert!(!activity.foreground_process.is_empty());
        assert_eq!(reason.as_deref(), Some("reader_eof"));
    }

    #[test]
    fn read_snapshot_reports_missing_session() {
        let mut state = super::super::runtime::NextCoreRuntime::default();

        let err = read_snapshot(&mut state, 42, Instant::now()).expect_err("missing session");

        assert_eq!(err.to_string(), "next-core session 42 not found");
    }

    fn sample_session() -> NextCoreSession {
        let mut command = portable_pty::CommandBuilder::new_default_prog();
        if let Some(cwd) = std::env::current_dir()
            .ok()
            .and_then(|path| path.to_str().map(|path| path.to_string()))
        {
            command.cwd(cwd);
        }
        let label = launch::command_label(&command);
        let pair = portable_pty::native_pty_system()
            .openpty(session_runtime::pty_size(80, 24))
            .expect("open pty");
        let child = pair.slave.spawn_command(command).expect("spawn command");
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(pair.master.take_writer().expect("pty writer")));

        NextCoreSession {
            snapshot: crate::SessionSnapshot {
                split_from: None,
                split_axis: None,
                split_ratio: None,
                split_side: None,
                id: 999,
                title: "sample".to_string(),
                cols: 80,
                rows: 24,
                scrollback_rows: 0,
                cursor: session_defaults::default_cursor(),
                is_dead: false,
                dead_reason: None,
                is_active: true,
                domain_id: 0,
                shell: crate::ShellSnapshot {
                    shell_type: launch::shell_type(&label),
                    process_name: label,
                    cwd: None,
                    launch_env_keys: Vec::new(),
                    launch_context: Default::default(),
                },
            },
            root_pid: child.process_id(),
            master: Mutex::new(pair.master),
            child: Mutex::new(child),
            writer,
            output: Arc::new(Mutex::new(String::new())),
            screen: Arc::new(Mutex::new(super::super::NextCoreScreen::new(80, 24))),
            recording: Arc::new(Mutex::new(None)),
            activity: Arc::new(Mutex::new(
                crate::next_core::activity::SessionIoActivity::new(),
            )),
            dead: Arc::new(AtomicBool::new(false)),
            dead_reason: Arc::new(Mutex::new(None)),
        }
    }
}
