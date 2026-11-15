use super::{lifecycle, process_tree, runtime::NextCoreRuntime, session_registry, NextCoreSession};
use crate::SessionSnapshot;
use anyhow::Result;

pub(super) fn list(state: &mut NextCoreRuntime) -> Vec<SessionSnapshot> {
    let mut snapshots = Vec::with_capacity(session_registry::pane_count(state));
    let mut dead_reasons = Vec::new();
    let mut active_went_dead = false;
    session_registry::for_each_session_mut(state, |session| {
        let (snapshot, dead_reason) = snapshot(session);
        if snapshot.is_active && snapshot.is_dead {
            active_went_dead = true;
        }
        if let Some(reason) = dead_reason {
            dead_reasons.push(reason);
        }
        snapshots.push(snapshot);
    });
    for reason in dead_reasons {
        lifecycle::record_dead_reason(state, reason);
    }
    if active_went_dead {
        let active_id = session_registry::promote_active_to_live(state);
        for snapshot in &mut snapshots {
            snapshot.is_active = Some(snapshot.id) == active_id;
        }
    }
    snapshots
}

pub(super) fn get(state: &mut NextCoreRuntime, pane_id: usize) -> Result<SessionSnapshot> {
    list(state)
        .into_iter()
        .find(|session| session.id == pane_id)
        .ok_or_else(|| anyhow::anyhow!("next-core session {pane_id} not found"))
}

#[cfg(test)]
pub(super) fn clone_base(state: &NextCoreRuntime, pane_id: usize) -> Result<SessionSnapshot> {
    Ok(session_registry::session(state, pane_id)?.snapshot.clone())
}

fn snapshot(session: &mut NextCoreSession) -> (SessionSnapshot, Option<String>) {
    let dead_reason = lifecycle::refresh_liveness(session);
    let mut snapshot = session.snapshot.clone();
    let (cursor, scrollback_rows, title, reported) = {
        let screen = session.screen.lock();
        (
            screen.cursor_snapshot(),
            screen.scrollback_rows(),
            screen.title(),
            screen.current_dir(),
        )
    };
    snapshot.cursor = cursor;
    snapshot.scrollback_rows = scrollback_rows;
    if let Some(title) = title {
        snapshot.title = title;
    }
    let existing = snapshot.shell.cwd.take();
    snapshot.shell.cwd = process_tree::resolve_cwd(reported, existing, session.root_pid);
    (snapshot, dead_reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_empty_state_has_no_snapshots() {
        let mut state = NextCoreRuntime::default();

        assert!(list(&mut state).is_empty());
    }

    #[test]
    fn get_reports_missing_session() {
        let mut state = NextCoreRuntime::default();
        let err = get(&mut state, 88).expect_err("missing session should fail");

        assert!(err.to_string().contains("next-core session 88 not found"));
    }

    #[test]
    fn clone_base_reports_missing_session() {
        let state = NextCoreRuntime::default();
        let err = clone_base(&state, 77).expect_err("missing source session should fail");

        assert!(err.to_string().contains("next-core session 77 not found"));
    }

    #[test]
    fn list_moves_active_flag_off_a_session_that_died() {
        let mut state = NextCoreRuntime::default();
        let first = sample_session(1);
        session_registry::insert_created(&mut state, first);
        let mut second = sample_session(2);
        second.snapshot.is_dead = true;
        session_registry::insert_created(&mut state, second);

        let snapshots = list(&mut state);
        let live = snapshots.iter().find(|session| !session.is_dead).unwrap();
        let dead = snapshots.iter().find(|session| session.id == 2).unwrap();

        assert!(dead.is_dead);
        assert!(!dead.is_active);
        assert!(live.is_active);
    }

    fn sample_session(id: usize) -> NextCoreSession {
        let command = portable_pty::CommandBuilder::new_default_prog();
        crate::next_core::session_runtime::spawn(
            id,
            format!("sample-{id}"),
            80,
            24,
            command,
            None,
            Vec::new(),
            None,
        )
        .expect("spawn session")
    }
}
