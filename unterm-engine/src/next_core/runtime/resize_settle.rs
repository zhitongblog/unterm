//! Shrinks that wait until they are sure.
//!
//! A shrink cuts every row to the new width and drops the rows that no longer
//! fit; growing back does not restore them. Claude Code repaints only when the
//! size it reads differs from the last one it drew at, so a pane that went
//! 159x43 -> 69x20 -> 159x43 faster than it read its size kept the 69x20
//! remnant, with the program writing timer ticks into rows now blank.
//!
//! Those sizes are passing ones -- a window made at a provisional size, a
//! backlog of resizes after a GUI stall. So a window's shrink is held until the
//! size has stayed put for [`SHRINK_SETTLE`], and one taken back sooner never
//! happens. Grows apply at once. An agent's explicit `session.resize` does not
//! come through here.

use super::{scheduler, with_session};
use anyhow::Result;
use parking_lot::{Condvar, Mutex};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long a smaller size has to hold before the pane is shrunk to it.
pub(in crate::next_core) const SHRINK_SETTLE: Duration = Duration::from_millis(120);

struct Pending {
    cols: usize,
    rows: usize,
    due: Instant,
}

struct Settler {
    pending: Mutex<HashMap<usize, Pending>>,
    wake: Condvar,
    /// Held across deciding and applying by both sides, so a shrink the
    /// worker just took cannot land after the grow that superseded it.
    apply: Mutex<()>,
    /// Without the worker a shrink would wait forever; resize at once then.
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
    // Refused now, not when the timer fires: the caller can act on it.
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
        // A grow, or taking back a shrink still waiting here.
        settler.pending.lock().remove(&pane_id);
        if (cols, rows) == current {
            return Ok(());
        }
        return scheduler::resize_session(pane_id, cols, rows);
    }
    let due = Instant::now() + SHRINK_SETTLE;
    settler.pending.lock().insert(pane_id, Pending { cols, rows, due });
    settler.wake.notify_one();
    Ok(())
}

#[cfg(test)]
pub(in crate::next_core) fn shrink_pending(pane_id: usize) -> bool {
    settler().pending.lock().contains_key(&pane_id)
}

fn worker() {
    // Blocks on the `OnceLock` until the spawning call has finished it.
    let settler = settler();
    loop {
        let due = {
            let mut pending = settler.pending.lock();
            loop {
                let Some(next) = pending.values().map(|p| p.due).min() else {
                    settler.wake.wait(&mut pending);
                    continue;
                };
                if next <= Instant::now() {
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
            // Again under `apply`: a grow may have taken it back since.
            let ready = {
                let mut pending = settler.pending.lock();
                match pending.get(&pane_id) {
                    Some(p) if p.due <= Instant::now() => pending.remove(&pane_id),
                    _ => None,
                }
            };
            // Nobody left to report to; almost always a pane that closed.
            if let Some(size) = ready {
                let _ = scheduler::resize_session(pane_id, size.cols, size.rows);
            }
        }
    }
}
