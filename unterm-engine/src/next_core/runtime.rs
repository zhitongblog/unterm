use super::session_registry;
use parking_lot::RwLock;
use std::sync::OnceLock;

pub(in crate::next_core) mod command;
mod consumer;
mod dispatch;
mod input_executor;
mod io_facade;
mod pump;
pub(in crate::next_core) mod queue;
mod recording_executor;
mod recording_facade;
mod resize_settle;
mod response;
mod scheduler;
mod scheduling;
mod screen_executor;
mod session_executor;
mod session_facade;
mod session_query_executor;
mod status_executor;
mod status_facade;
#[cfg(test)]
pub(super) mod test_facade;

pub(super) use io_facade::{
    cursor, erase_scrollback, pane_modes, paste_input, read_lines, read_render_frame, read_screen,
    read_scrollback, read_scrollback_text, read_styled_screen, read_styled_scrollback,
    read_visible_text, report_mouse, screen_revision, scroll_viewport_by, scroll_viewport_to,
    scroll_viewport_to_prompt, search_screen, write_input,
};
pub(super) use recording_facade::{
    attach_recording_trace, export_recording_markdown, recording_status, start_recording,
    stop_recording,
};
pub(super) use session_facade::{
    clone_session_base, create_session, destroy, focus, get_session, insert_created, list_sessions,
    next_session_id, resize, set_split_ratio, split_session, with_session, with_session_optional,
};
pub(super) use resize_settle::resize_settled;
#[cfg(test)]
pub(super) use resize_settle::{shrink_pending, SHRINK_SETTLE};
pub(super) use status_facade::{health_snapshot, output, session_activity, shell_snapshot};

#[derive(Default)]
pub(super) struct NextCoreRuntime {
    pub(super) command_queue: queue::RuntimeCommandQueue,
    pub(super) pump_stats: pump::RuntimePumpStats,
    pub(super) registry: session_registry::SessionRegistry,
}

pub(super) fn current() -> &'static RwLock<NextCoreRuntime> {
    static RUNTIME: OnceLock<RwLock<NextCoreRuntime>> = OnceLock::new();
    RUNTIME.get_or_init(|| RwLock::new(NextCoreRuntime::default()))
}

pub(super) fn with_current<T>(visit: impl FnOnce(&NextCoreRuntime) -> T) -> T {
    let state = current().read();
    visit(&state)
}

pub(super) fn with_current_mut<T>(visit: impl FnOnce(&mut NextCoreRuntime) -> T) -> T {
    let mut state = current().write();
    visit(&mut state)
}
