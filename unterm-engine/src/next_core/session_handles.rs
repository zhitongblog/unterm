use super::{activity::SessionIoActivity, runtime, NextCoreRecording, NextCoreScreen};
#[cfg(test)]
use super::{runtime::NextCoreRuntime, session_registry};
use crate::ShellSnapshot;
use anyhow::Result;
use parking_lot::Mutex;
use std::io::Write;
use std::sync::Arc;

pub(super) struct InputHandles {
    pub(super) writer: Arc<Mutex<Box<dyn Write + Send>>>,
    pub(super) activity: Arc<Mutex<SessionIoActivity>>,
    pub(super) application_cursor_keys: bool,
    pub(super) bracketed_paste: bool,
    pub(super) mouse_modes: super::mouse_encoding::MouseModes,
}

pub(super) struct RecordingHandles {
    pub(super) recording: Arc<Mutex<Option<NextCoreRecording>>>,
    pub(super) project_path: Option<String>,
}

pub(super) struct ShellHandles {
    pub(super) shell: ShellSnapshot,
    pub(super) screen: Arc<Mutex<NextCoreScreen>>,
    pub(super) root_pid: Option<u32>,
}

pub(super) struct ScrollbackHandles {
    pub(super) screen: Arc<Mutex<NextCoreScreen>>,
    pub(super) output: Arc<Mutex<super::pty_io::OutputLog>>,
    pub(super) activity: Arc<Mutex<SessionIoActivity>>,
    pub(super) cols: usize,
    pub(super) rows: usize,
}

pub(super) fn output_current(pane_id: usize) -> Result<Arc<Mutex<super::pty_io::OutputLog>>> {
    runtime::with_session(pane_id, |session| Ok(Arc::clone(&session.output)))
}

#[cfg(test)]
pub(super) fn screen(
    state: &NextCoreRuntime,
    pane_id: usize,
) -> Result<Arc<Mutex<NextCoreScreen>>> {
    Ok(Arc::clone(
        &session_registry::session(state, pane_id)?.screen,
    ))
}

pub(super) fn screen_current(pane_id: usize) -> Result<Arc<Mutex<NextCoreScreen>>> {
    runtime::with_session(pane_id, |session| Ok(Arc::clone(&session.screen)))
}

pub(super) fn activity_current(pane_id: usize) -> Result<Arc<Mutex<SessionIoActivity>>> {
    runtime::with_session(pane_id, |session| Ok(Arc::clone(&session.activity)))
}

pub(super) fn screen_activity_current(
    pane_id: usize,
) -> Result<(Arc<Mutex<NextCoreScreen>>, Arc<Mutex<SessionIoActivity>>)> {
    runtime::with_session(pane_id, |session| {
        Ok((Arc::clone(&session.screen), Arc::clone(&session.activity)))
    })
}

pub(super) fn input_current(pane_id: usize) -> Result<InputHandles> {
    runtime::with_session(pane_id, |session| {
        let screen = session.screen.lock();
        Ok(InputHandles {
            writer: Arc::clone(&session.writer),
            activity: Arc::clone(&session.activity),
            application_cursor_keys: screen.application_cursor_keys,
            bracketed_paste: screen.bracketed_paste,
            mouse_modes: screen.mouse_modes(),
        })
    })
}

pub(super) fn shell_current(pane_id: usize) -> Result<ShellHandles> {
    runtime::with_session(pane_id, |session| {
        Ok(ShellHandles {
            shell: session.snapshot.shell.clone(),
            screen: Arc::clone(&session.screen),
            root_pid: session.root_pid,
        })
    })
}

pub(super) fn scrollback_current(pane_id: usize) -> Result<ScrollbackHandles> {
    runtime::with_session(pane_id, |session| {
        Ok(ScrollbackHandles {
            screen: Arc::clone(&session.screen),
            output: Arc::clone(&session.output),
            activity: Arc::clone(&session.activity),
            cols: session.snapshot.cols,
            rows: session.snapshot.rows,
        })
    })
}

pub(super) fn recording_current(pane_id: usize) -> Result<RecordingHandles> {
    runtime::with_session(pane_id, |session| {
        Ok(RecordingHandles {
            recording: Arc::clone(&session.recording),
            project_path: session.snapshot.shell.cwd.clone(),
        })
    })
}

#[cfg(test)]
pub(super) fn recording_optional(
    state: &NextCoreRuntime,
    pane_id: usize,
) -> Option<Arc<Mutex<Option<NextCoreRecording>>>> {
    session_registry::session(state, pane_id)
        .ok()
        .map(|session| Arc::clone(&session.recording))
}

pub(super) fn recording_optional_current(
    pane_id: usize,
) -> Option<Arc<Mutex<Option<NextCoreRecording>>>> {
    runtime::with_session_optional(pane_id, |session| Arc::clone(&session.recording))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_session_reports_pane_id() {
        let err = match screen(&NextCoreRuntime::default(), 42) {
            Ok(_) => panic!("expected missing session error"),
            Err(err) => err,
        };

        assert!(err.to_string().contains("next-core session 42 not found"));
    }

    #[test]
    fn optional_recording_reports_none_for_missing_session() {
        assert!(recording_optional(&NextCoreRuntime::default(), 42).is_none());
    }
}
