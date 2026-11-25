use super::{
    activity::SessionIoActivity, recording_output, runtime, CellAttributes, NextCoreScreen,
};
use anyhow::Result;
use std::{sync::atomic::Ordering, sync::Arc, time::Instant};

/// Put the runtime back to empty for a test, keeping other tests out.
///
/// Hold the returned guard for the length of the test -- see
/// `runtime::test_facade::reset`.
#[must_use = "hold the guard for the length of the test"]
pub(super) fn reset_state_for_test() -> runtime::test_facade::RuntimeTestGuard {
    runtime::test_facade::reset()
}

/// Wait until the shell has finished announcing itself.
///
/// A session runs a real shell, and a shell says who it is: cmd.exe sets its
/// console title within the first moments of starting, and that arrives on the
/// same screen a test is about to write to. Injecting before it lands means
/// the shell overwrites the test's output a moment later -- which failed about
/// one run in ten, with a title nobody in the test had written.
///
/// Settling on the output going quiet rather than on a fixed sleep: the wait
/// is then as short as the machine allows, and still correct on a slow one.
fn wait_for_the_shell_to_settle(handles: &runtime::test_facade::TestSessionHandles) {
    use std::time::Duration;

    const QUIET: Duration = Duration::from_millis(120);
    const GIVE_UP: Duration = Duration::from_millis(1_000);

    let deadline = Instant::now() + GIVE_UP;
    let mut seen = usize::MAX;
    let mut unchanged_since = Instant::now();
    while Instant::now() < deadline {
        let produced = handles.output.lock().len();
        if produced != seen {
            seen = produced;
            unchanged_since = Instant::now();
        } else if produced > 0 && unchanged_since.elapsed() >= QUIET {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(super) fn set_output_for_test(pane_id: usize, text: &str) -> Result<()> {
    let handles = runtime::test_facade::session_handles(pane_id)?;
    let started_at = Instant::now();
    *handles.output.lock() = super::pty_io::OutputLog { text: text.to_string(), dropped: 0 };
    let mut screen = handles.screen.lock();
    let revision = screen.revision();
    *screen = NextCoreScreen::new(handles.cols, handles.rows);
    screen.render_state.set_revision(revision);
    screen.feed(text);
    let recorded = if let Some(recording) = handles.recording.lock().as_mut() {
        recording_output::append_now(recording, text);
        true
    } else {
        false
    };
    handles
        .activity
        .lock()
        .mark_output(text.len(), 0, recorded, started_at.elapsed());
    Ok(())
}

pub(super) fn mark_dead_for_test(pane_id: usize) -> Result<()> {
    let handles = runtime::test_facade::session_handles(pane_id)?;

    *handles.dead_reason.lock() = Some("test_dead_marker".to_string());
    handles.dead.store(true, Ordering::Release);
    Ok(())
}

fn make_activity_stale_for_test(pane_id: usize) -> Result<()> {
    let handles = runtime::test_facade::session_handles(pane_id)?;

    handles.activity.lock().mark_stale_for_test();
    Ok(())
}

/// Wait until the shell has finished announcing itself.
///
/// Only for tests that assert on the *title*. Everything else can run against
/// a session that is still starting up, and waiting there would only make the
/// suite slower -- and, for the two tests that assert exact I/O counts or
/// recent activity, wrong.
pub(super) fn settle_session_for_test(pane_id: usize) -> Result<()> {
    let handles = runtime::test_facade::session_handles(pane_id)?;
    wait_for_the_shell_to_settle(&handles);
    Ok(())
}

pub(super) fn reset_activity_for_test(pane_id: usize) -> Result<()> {
    let handles = runtime::test_facade::session_handles(pane_id)?;

    *handles.activity.lock() = SessionIoActivity::new();
    make_activity_stale_for_test(pane_id)
}

pub(super) fn viewport_attrs_for_test(pane_id: usize) -> Result<Vec<Vec<CellAttributes>>> {
    let handles = runtime::test_facade::session_handles(pane_id)?;

    let attrs = handles.screen.lock().attrs_for_viewport();
    Ok(attrs)
}

pub(super) fn screen_for_test(pane_id: usize) -> Result<Arc<parking_lot::Mutex<NextCoreScreen>>> {
    runtime::test_facade::session_handles(pane_id).map(|handles| handles.screen)
}
