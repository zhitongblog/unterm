//! Shrinks that wait until they are sure.
//!
//! A shrink is the one resize that destroys anything: the screen cuts every
//! row to the new width and drops the rows that no longer fit, and nothing
//! brings them back when the pane grows again. A program that repaints on
//! every size it is told about survives that. Claude Code does not have to:
//! it repaints when the size it reads *differs from the last one it drew
//! at*, and otherwise rewrites only the cells it believes changed. So a pane
//! that went 159x43 -> 69x20 -> 159x43 faster than the program read its size
//! was left holding the 69x20 remnant of the old frame, with the program
//! writing timer ticks into rows it believed were full and were blank -- a
//! screen that no longer matches what the program thinks is on it, and a
//! cursor that looks misplaced because the frame around it is gone.
//!
//! The sizes that do this are passing ones: a window created at a provisional
//! size before it is restored, a backlog of resize events handled in one go
//! after the GUI thread stalls. So a shrink is held until the size has stayed
//! put for [`SHRINK_SETTLE`]; one that is taken back sooner never happens, and
//! the program never sees it. A grow costs nothing and still applies at once,
//! as does any size a pane really settles at.
//!
//! This is the path for a front end laying out windows. An agent calling
//! `session.resize` asked for one size on purpose and gets it immediately.

use super::{scheduler, with_session};
use anyhow::Result;
use parking_lot::{Condvar, Mutex};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a smaller size has to hold before the pane is shrunk to it.
///
/// Long enough to cover a window restored a frame or two after it was made
/// and a burst of queued events, short enough that dragging a window smaller
/// still feels like the text follows the edge.
pub(in crate::next_core) const SHRINK_SETTLE: Duration = Duration::from_millis(120);

struct Pending {
    cols: usize,
    rows: usize,
    due: Instant,
}

struct Settler {
    pending: Mutex<HashMap<usize, Pending>>,
    wake: Condvar,
    /// Held across deciding and applying, by both the caller and the worker.
    ///
    /// Without it a shrink the worker had just taken off the map could land
    /// *after* a grow that superseded it, and the pane would end up small.
    apply: Mutex<()>,
    /// Whether the thread that applies waiting shrinks started. Without it a
    /// shrink would wait forever, so every resize goes straight through.
    running: bool,
}

fn settler() -> &'static Settler {
    static SETTLER: std::sync::OnceLock<Settler> = std::sync::OnceLock::new();
    SETTLER.get_or_init(|| {
        let running = std::thread::Builder::new()
            .name("resize-settle".into())
            .spawn(worker)
            .is_ok();
        Settler {
            pending: Mutex::new(HashMap::new()),
            wake: Condvar::new(),
            apply: Mutex::new(()),
            running,
        }
    })
}

/// Resize a pane to the size a window is drawing it at.
pub(in crate::next_core) fn resize_settled(pane_id: usize, cols: usize, rows: usize) -> Result<()> {
    // Refused now rather than when the timer fires: the caller is the one
    // who can do something about a size that will never be accepted.
    super::super::session_runtime::check_grid(pane_id, cols, rows)?;
    let settler = settler();
    if !settler.running {
        return scheduler::resize_session(pane_id, cols, rows);
    }
    let _apply = settler.apply.lock();
    let current = with_session(pane_id, |session| {
        Ok((session.snapshot.cols, session.snapshot.rows))
    })?;
    if cols >= current.0 && rows >= current.1 {
        // A grow, or the size it already has -- which is what taking back a
        // shrink still waiting here looks like. Either way nothing smaller
        // is wanted any more.
        settler.pending.lock().remove(&pane_id);
        if (cols, rows) == current {
            return Ok(());
        }
        return scheduler::resize_session(pane_id, cols, rows);
    }
    settler.pending.lock().insert(
        pane_id,
        Pending {
            cols,
            rows,
            due: Instant::now() + SHRINK_SETTLE,
        },
    );
    settler.wake.notify_one();
    Ok(())
}

/// Whether a shrink is still waiting for this pane, for tests.
#[cfg(test)]
pub(in crate::next_core) fn shrink_pending(pane_id: usize) -> bool {
    settler().pending.lock().contains_key(&pane_id)
}

fn worker() {
    // `settler()` is still being built when this thread starts; the first
    // call blocks on the `OnceLock` until it is ready, which is all that is
    // needed.
    let settler = settler();
    loop {
        let due = {
            let mut pending = settler.pending.lock();
            loop {
                let Some(next) = pending.values().map(|p| p.due).min() else {
                    settler.wake.wait(&mut pending);
                    continue;
                };
                let now = Instant::now();
                if next <= now {
                    break;
                }
                settler.wake.wait_until(&mut pending, next);
            }
            let now = Instant::now();
            pending
                .iter()
                .filter(|(_, p)| p.due <= now)
                .map(|(pane, _)| *pane)
                .collect::<Vec<_>>()
        };
        let _apply = settler.apply.lock();
        for pane_id in due {
            // Looked up again under `apply`: a grow may have taken it back,
            // or a newer shrink moved its deadline, since the list was made.
            let ready = {
                let mut pending = settler.pending.lock();
                match pending.get(&pane_id) {
                    Some(p) if p.due <= Instant::now() => pending.remove(&pane_id),
                    _ => None,
                }
            };
            let Some(size) = ready else { continue };
            // An error here has nobody to go to: the caller was answered when
            // the shrink was queued. It is almost always a pane that closed
            // while its shrink was waiting, which has no size left to fix.
            let _ = scheduler::resize_session(pane_id, size.cols, size.rows);
        }
    }
}
