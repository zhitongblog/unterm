use super::{runtime::NextCoreRuntime, session_registry, NextCoreSession};
use std::sync::atomic::Ordering;

pub(super) fn refresh_liveness(session: &mut NextCoreSession) -> Option<String> {
    if session.snapshot.is_dead {
        return None;
    }

    if session.dead.load(Ordering::Acquire) {
        session.snapshot.is_dead = true;
        if session.snapshot.dead_reason.is_none() {
            session.snapshot.dead_reason = session
                .dead_reason
                .lock()
                .clone()
                .or_else(|| Some("unknown".to_string()));
        }
        return session.snapshot.dead_reason.clone();
    }

    if let Ok(Some(status)) = session.child.lock().try_wait() {
        let reason = format!("process_exited:{status}");
        session.snapshot.is_dead = true;
        session.snapshot.dead_reason = Some(reason.clone());
        *session.dead_reason.lock() = Some(reason);
        session.dead.store(true, Ordering::Release);
        return session.snapshot.dead_reason.clone();
    }

    None
}

pub(super) fn record_dead_reason(state: &mut NextCoreRuntime, reason: String) {
    session_registry::record_dead_reason(state, reason);
}

pub(super) fn mark_destroyed(session: &mut NextCoreSession) -> (bool, String) {
    let previous_dead = session.snapshot.is_dead;
    session.snapshot.is_dead = true;
    let reason = session
        .snapshot
        .dead_reason
        .clone()
        .or_else(|| session.dead_reason.lock().clone())
        .unwrap_or_else(|| "destroyed".to_string());
    session.snapshot.dead_reason = Some(reason.clone());
    *session.dead_reason.lock() = Some(reason.clone());
    session.dead.store(true, Ordering::Release);
    session.child.lock().kill().ok();
    (previous_dead, reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::next_core::{launch, session_defaults, session_runtime};
    use std::sync::{atomic::AtomicBool, Arc};

    #[test]
    fn records_dead_reason_counters() {
        let mut state = NextCoreRuntime::default();

        record_dead_reason(&mut state, "reader_eof".to_string());

        let stats = session_registry::stats(&state);

        assert_eq!(stats.total_marked_dead, 1);
        assert_eq!(stats.last_dead_reason.as_deref(), Some("reader_eof"));
    }

    #[test]
    fn mark_destroyed_preserves_existing_dead_reason() {
        let mut session = sample_session();
        session.snapshot.is_dead = true;
        session.snapshot.dead_reason = Some("pty_reader_eof".to_string());
        *session.dead_reason.lock() = Some("pty_reader_eof".to_string());

        let (previous_dead, reason) = mark_destroyed(&mut session);

        assert!(previous_dead);
        assert_eq!(reason, "pty_reader_eof");
        assert!(session.dead.load(Ordering::Acquire));
        assert_eq!(
            session.snapshot.dead_reason.as_deref(),
            Some("pty_reader_eof")
        );
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
        let writer = Arc::new(parking_lot::Mutex::new(
            pair.master.take_writer().expect("pty writer"),
        ));

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
            master: parking_lot::Mutex::new(pair.master),
            child: parking_lot::Mutex::new(child),
            writer,
            output: Arc::new(parking_lot::Mutex::new(super::super::pty_io::OutputLog::default())),
            screen: Arc::new(parking_lot::Mutex::new(super::super::NextCoreScreen::new(
                80, 24,
            ))),
            recording: Arc::new(parking_lot::Mutex::new(None)),
            activity: Arc::new(parking_lot::Mutex::new(
                crate::next_core::activity::SessionIoActivity::new(),
            )),
            dead: Arc::new(AtomicBool::new(false)),
            dead_reason: Arc::new(parking_lot::Mutex::new(None)),
        }
    }
}
