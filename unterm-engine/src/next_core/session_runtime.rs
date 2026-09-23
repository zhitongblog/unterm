#[cfg(test)]
use super::runtime::NextCoreRuntime;
#[cfg(test)]
use super::session_registry;
use super::{
    activity::SessionIoActivity, launch, pty_io, session_defaults, session_output,
    NextCoreRecording, NextCoreScreen, NextCoreSession,
};
use crate::{SessionSnapshot, ShellSnapshot};
use anyhow::Result;
use parking_lot::Mutex;
use portable_pty::{native_pty_system, PtySize};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

pub(super) fn pty_size(cols: usize, rows: usize) -> PtySize {
    PtySize {
        rows: rows.clamp(1, u16::MAX as usize) as u16,
        cols: cols.clamp(1, u16::MAX as usize) as u16,
        pixel_width: 0,
        pixel_height: 0,
    }
}

#[cfg(test)]
pub(super) fn resize(
    state: &mut NextCoreRuntime,
    pane_id: usize,
    cols: usize,
    rows: usize,
) -> Result<()> {
    let session = session_registry::session_mut(state, pane_id)?;
    resize_session(session, cols, rows)
}

/// Refuse a size no pane should be given.
///
/// A limit on believing the caller, not a rendering limit: a pane keeping a
/// size it is no longer drawn at wraps oddly until the next real resize,
/// where a pane resized to one column has lost its output for good.
pub(super) fn check_grid(pane_id: usize, cols: usize, rows: usize) -> Result<()> {
    let floor = crate::MIN_SESSION_GRID;
    if cols < floor || rows < floor {
        anyhow::bail!(
            "refusing to resize pane {pane_id} to {cols}x{rows}: a grid below {floor}x{floor} discards the pane's lines and scrollback rather than reflowing them"
        );
    }
    Ok(())
}

pub(super) fn resize_session(
    session: &mut NextCoreSession,
    cols: usize,
    rows: usize,
) -> Result<()> {
    check_grid(session.snapshot.id, cols, rows)?;
    // Our own model first, the kernel second.
    //
    // `master.resize` raises SIGWINCH, and a full-screen program answers it
    // by repainting immediately: absolute cursor moves, a new scroll region,
    // rows addressed by numbers that only make sense at the new size. The
    // reader thread hands all of that to the screen as it arrives, on its own
    // thread -- so telling the kernel before the screen knows its new shape
    // leaves a window in which the repaint is parsed against the old one.
    // The cursor lands wherever the stale geometry puts it, which is what a
    // TUI's caret sitting at the bottom edge while the program types
    // somewhere else actually is.
    //
    // Reversed, the worst case is a repaint that arrives a moment late
    // against a screen already the right size -- which is just a repaint.
    session.screen.lock().resize(cols, rows);
    session.snapshot.cols = cols;
    session.snapshot.rows = rows;
    session.master.lock().resize(pty_size(cols, rows))?;
    Ok(())
}

pub(super) fn spawn(
    id: usize,
    title: String,
    cols: usize,
    rows: usize,
    command: portable_pty::CommandBuilder,
    cwd: Option<String>,
    launch_env_keys: Vec<String>,
    split_from: Option<usize>,
) -> Result<NextCoreSession> {
    // Tell the shell which pane it is.
    //
    // Nothing did, until now. `unterm-cli agent signal` has always read this
    // to attribute a hook to the pane that raised it, and the MCP bridge
    // needs it to stop falling back to whichever pane the user happens to be
    // looking at -- an agent in a background pane that omitted a target was
    // running its commands in the foreground one. Both read it; neither
    // could, because it was never written.
    //
    // Both spellings, matching `unterm_services::env_names::both("PANE")`
    // -- spelled out here because the engine sits below that crate. A test
    // over there holds the two in step. The old name stays because a user's
    // prompt may read `$WEZTERM_PANE`, and those prompts have been showing
    // nothing at all.
    let mut command = command;
    command.env("UNTERM_PANE", id.to_string());
    command.env("WEZTERM_PANE", id.to_string());
    let label = launch::command_label(&command);
    let pair = native_pty_system().openpty(pty_size(cols, rows))?;
    let child = pair.slave.spawn_command(command)?;
    let root_pid = child.process_id();
    let reader = pair.master.try_clone_reader()?;
    let writer = Arc::new(Mutex::new(pair.master.take_writer()?));
    let output = Arc::new(Mutex::new(String::new()));
    let screen = Arc::new(Mutex::new(NextCoreScreen::new(cols, rows)));
    let recording = Arc::new(Mutex::new(None));
    let activity = Arc::new(Mutex::new(SessionIoActivity::new()));
    let dead = Arc::new(AtomicBool::new(false));
    let dead_reason = Arc::new(Mutex::new(None));
    spawn_reader_thread(
        id,
        Arc::clone(&output),
        Arc::clone(&screen),
        Arc::clone(&recording),
        Arc::clone(&activity),
        Arc::clone(&writer),
        Arc::clone(&dead),
        Arc::clone(&dead_reason),
        reader,
    );
    let shell = ShellSnapshot {
        shell_type: launch::shell_type(&label),
        process_name: label,
        cwd,
        launch_env_keys,
        launch_context: Default::default(),
    };

    Ok(NextCoreSession {
        snapshot: SessionSnapshot {
            // Filled in by the split path; a pane that was not split
            // from anything has no arrangement of its own to describe.
            split_axis: None,
            split_ratio: None,
            split_side: None,
            split_from,
            id,
            title,
            cols,
            rows,
            scrollback_rows: 0,
            cursor: session_defaults::default_cursor(),
            is_dead: false,
            dead_reason: None,
            is_active: true,
            domain_id: 0,
            shell,
        },
        root_pid,
        master: Mutex::new(pair.master),
        child: Mutex::new(child),
        writer,
        output,
        screen,
        recording,
        activity,
        dead,
        dead_reason,
    })
}

fn spawn_reader_thread(
    pane_id: usize,
    output: Arc<Mutex<String>>,
    screen: Arc<Mutex<NextCoreScreen>>,
    recording: Arc<Mutex<Option<NextCoreRecording>>>,
    activity: Arc<Mutex<SessionIoActivity>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    dead: Arc<AtomicBool>,
    dead_reason: Arc<Mutex<Option<String>>>,
    mut reader: Box<dyn Read + Send>,
) {
    thread::Builder::new()
        .name(format!("next-core-pty-reader-{pane_id}"))
        .spawn(move || {
            let mut buf = [0u8; 8192];
            let mut pending_utf8 = Vec::new();
            let mut pending_terminal_query = String::new();
            let mut startup_output_filter = pty_io::StartupOutputFilter::default();
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => {
                        *dead_reason.lock() = Some("pty_reader_eof".to_string());
                        break;
                    }
                    Ok(n) => {
                        let Some(chunk) = pty_io::decode_pty_chunk(&mut pending_utf8, &buf[..n])
                        else {
                            continue;
                        };
                        let Some(chunk) = startup_output_filter.filter(chunk) else {
                            continue;
                        };
                        session_output::apply_chunk(
                            session_output::OutputHandles {
                                output: &output,
                                screen: &screen,
                                recording: &recording,
                                activity: &activity,
                                writer: &writer,
                            },
                            chunk.as_str(),
                            &mut pending_terminal_query,
                        );
                    }
                    Err(err) => {
                        *dead_reason.lock() = Some(format!("pty_reader_error:{err}"));
                        break;
                    }
                }
            }
            dead.store(true, Ordering::Release);
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pty_size_clamps_to_conpty_safe_range() {
        let size = pty_size(0, usize::MAX);

        assert_eq!(size.cols, 1);
        assert_eq!(size.rows, u16::MAX);
        assert_eq!(size.pixel_width, 0);
        assert_eq!(size.pixel_height, 0);
    }

    #[test]
    fn resize_reports_missing_session() {
        let mut state = NextCoreRuntime::default();
        let err = resize(&mut state, 42, 80, 24).expect_err("missing session should fail");

        assert!(err.to_string().contains("next-core session 42 not found"));
    }
}
