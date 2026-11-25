use super::scheduler;
use crate::{EngineHealthSnapshot, SessionActivitySnapshot, ShellSnapshot};
use anyhow::Result;

pub(in crate::next_core) fn shell_snapshot(pane_id: usize) -> Result<ShellSnapshot> {
    scheduler::shell_snapshot(pane_id)
}

pub(in crate::next_core) fn output(
    pane_id: usize,
    from: Option<u64>,
    max_bytes: usize,
) -> Result<crate::OutputSince> {
    scheduler::output(pane_id, from, max_bytes)
}

pub(in crate::next_core) fn session_activity(pane_id: usize) -> Result<SessionActivitySnapshot> {
    scheduler::session_activity(pane_id)
}

pub(in crate::next_core) fn health_snapshot() -> Result<EngineHealthSnapshot> {
    scheduler::health_snapshot()
}
