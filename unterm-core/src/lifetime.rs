//! When the Core stops.
//!
//! Two ways a Core comes to exist, and they end differently:
//!
//! - **Started by a window** (`--gui-pid <pid>`, which is how
//!   `ensure_running` spawns it). It exists to hold that window's sessions
//!   across the window closing and reopening. When every window is gone --
//!   quit, crashed or killed -- and none is parked in the tray, it has no one
//!   left to serve and goes too, after a grace period long enough for a
//!   restart to find it.
//! - **Headless** (`--headless`, or started by hand with no flags). The user
//!   asked for a Core that lives without a window. Nothing but an explicit
//!   `core.shutdown` ends it: a window quitting leaves it alone and the
//!   watchdog never fires.
//!
//! The decisions are plain functions over plain inputs so they can be tested
//! without a process, a socket or a clock.

use std::time::{Duration, Instant};

/// How this Core came to exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreMode {
    /// Runs without a window on purpose; only `core.shutdown` ends it.
    Headless,
    /// Spawned by a front end; ends when no front end is left.
    GuiOwned,
}

static MODE: std::sync::OnceLock<CoreMode> = std::sync::OnceLock::new();

/// Record the mode, once, at startup. Later calls are ignored.
pub fn set_mode(mode: CoreMode) {
    let _ = MODE.set(mode);
}

/// This process's mode. A Core that never said is headless: that is the
/// conservative answer, the one that never ends anybody's sessions early.
pub fn mode() -> CoreMode {
    MODE.get().copied().unwrap_or(CoreMode::Headless)
}

/// How long a window-owned Core waits with no window before it leaves.
///
/// Long enough to cover a window restarting (an update, a crash and a
/// relaunch), short enough that a killed window does not leave a Core and a
/// set of shells running into the next day.
pub const ORPHAN_GRACE: Duration = Duration::from_secs(30);

/// How long an orphaned Core keeps draining before giving up on the sessions
/// it was asked to let finish.
///
/// "Drain, then exit" waits for sessions to end on their own, and an idle
/// interactive shell never does. Without a ceiling that choice meant "leave a
/// Core running forever" the moment the window was gone.
pub const ORPHAN_DRAIN_DEADLINE: Duration = Duration::from_secs(10 * 60);

/// What the watchdog sees on one tick.
#[derive(Debug, Clone, Copy)]
pub struct WatchdogInput {
    pub mode: CoreMode,
    /// A front end is attached (its reverse channel is open -- a window
    /// parked in the tray keeps it open), or the window that spawned this
    /// Core is still running.
    pub front_end_alive: bool,
    /// A `core.drain {exit_when_idle: true}` is in progress.
    pub draining_to_exit: bool,
}

/// The watchdog's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Stay,
    /// Stop, ending every session, for this reason.
    Exit(&'static str),
}

/// Decides, tick by tick, whether an orphaned Core should leave.
#[derive(Debug, Default)]
pub struct Watchdog {
    orphaned_since: Option<Instant>,
}

impl Watchdog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn tick(&mut self, now: Instant, input: WatchdogInput) -> Verdict {
        if input.mode == CoreMode::Headless || input.front_end_alive {
            self.orphaned_since = None;
            return Verdict::Stay;
        }
        let since = *self.orphaned_since.get_or_insert(now);
        let orphaned_for = now.saturating_duration_since(since);
        if input.draining_to_exit {
            if orphaned_for >= ORPHAN_DRAIN_DEADLINE {
                return Verdict::Exit("drain deadline passed with no Unterm window left");
            }
            return Verdict::Stay;
        }
        if orphaned_for >= ORPHAN_GRACE {
            return Verdict::Exit("no Unterm window has been attached for 30s");
        }
        Verdict::Stay
    }
}

/// What a `core.shutdown` asked for. All false -- no params, which is what
/// every client before this one sends -- is an unconditional stop.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShutdownRequest {
    /// The front end asking, so it is not counted among the "others".
    pub requester_pid: Option<u32>,
    /// Stop only if no *other* front end is attached. What a window sends
    /// when its last window closes: other processes' windows still need the
    /// Core.
    pub unless_shared: bool,
    /// Do not stop a headless Core. What every window's quit sends: a Core
    /// the user started to live without windows is not a window's to end.
    pub unless_headless: bool,
}

impl ShutdownRequest {
    pub fn from_params(params: &serde_json::Value) -> Self {
        Self {
            requester_pid: params
                .get("requester_pid")
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok()),
            unless_shared: params
                .get("unless_shared")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            unless_headless: params
                .get("unless_headless")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        }
    }
}

/// Why a conditional shutdown was declined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    Headless,
    OtherFrontEnds(usize),
}

impl Kept {
    pub fn reason(self) -> &'static str {
        match self {
            Kept::Headless => "headless",
            Kept::OtherFrontEnds(_) => "other_front_ends",
        }
    }
}

/// Whether to honour a shutdown request.
///
/// `attached` is the pids of the front ends whose reverse channel is open
/// right now -- verifiably alive, since a dead process's socket is closed by
/// the kernel. `None` is a front end too old to have said who it is; it is
/// counted as another front end, because guessing it is the requester would
/// end a window's sessions under it.
pub fn decide_shutdown(
    request: ShutdownRequest,
    mode: CoreMode,
    attached: &[Option<u32>],
) -> Result<(), Kept> {
    if request.unless_headless && mode == CoreMode::Headless {
        return Err(Kept::Headless);
    }
    if request.unless_shared {
        let others = attached
            .iter()
            .filter(|pid| match (pid, request.requester_pid) {
                (Some(pid), Some(requester)) => *pid != requester,
                _ => true,
            })
            .count();
        if others > 0 {
            return Err(Kept::OtherFrontEnds(others));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(mode: CoreMode, alive: bool, draining: bool) -> WatchdogInput {
        WatchdogInput {
            mode,
            front_end_alive: alive,
            draining_to_exit: draining,
        }
    }

    #[test]
    fn a_headless_core_is_never_ended_by_the_watchdog() {
        let mut dog = Watchdog::new();
        let start = Instant::now();
        for seconds in [0, 31, 3600, 86_400] {
            let now = start + Duration::from_secs(seconds);
            assert_eq!(
                dog.tick(now, input(CoreMode::Headless, false, false)),
                Verdict::Stay
            );
        }
    }

    #[test]
    fn an_orphaned_window_core_leaves_after_the_grace_period() {
        let mut dog = Watchdog::new();
        let start = Instant::now();
        assert_eq!(
            dog.tick(start, input(CoreMode::GuiOwned, false, false)),
            Verdict::Stay
        );
        assert_eq!(
            dog.tick(
                start + Duration::from_secs(29),
                input(CoreMode::GuiOwned, false, false)
            ),
            Verdict::Stay
        );
        assert!(matches!(
            dog.tick(
                start + Duration::from_secs(30),
                input(CoreMode::GuiOwned, false, false)
            ),
            Verdict::Exit(_)
        ));
    }

    #[test]
    fn a_window_coming_back_resets_the_grace_period() {
        let mut dog = Watchdog::new();
        let start = Instant::now();
        dog.tick(start, input(CoreMode::GuiOwned, false, false));
        // A restart: the window is back at 20s...
        assert_eq!(
            dog.tick(
                start + Duration::from_secs(20),
                input(CoreMode::GuiOwned, true, false)
            ),
            Verdict::Stay
        );
        // ...and gone again at 25s. The clock starts over from there.
        assert_eq!(
            dog.tick(
                start + Duration::from_secs(25),
                input(CoreMode::GuiOwned, false, false)
            ),
            Verdict::Stay
        );
        assert_eq!(
            dog.tick(
                start + Duration::from_secs(54),
                input(CoreMode::GuiOwned, false, false)
            ),
            Verdict::Stay
        );
        assert!(matches!(
            dog.tick(
                start + Duration::from_secs(55),
                input(CoreMode::GuiOwned, false, false)
            ),
            Verdict::Exit(_)
        ));
    }

    #[test]
    fn an_attached_window_keeps_the_core_indefinitely() {
        let mut dog = Watchdog::new();
        let start = Instant::now();
        for seconds in [0, 30, 600, 86_400] {
            assert_eq!(
                dog.tick(
                    start + Duration::from_secs(seconds),
                    input(CoreMode::GuiOwned, true, false)
                ),
                Verdict::Stay
            );
        }
    }

    #[test]
    fn draining_gets_longer_than_the_grace_period_but_not_forever() {
        let mut dog = Watchdog::new();
        let start = Instant::now();
        dog.tick(start, input(CoreMode::GuiOwned, false, true));
        assert_eq!(
            dog.tick(
                start + Duration::from_secs(300),
                input(CoreMode::GuiOwned, false, true)
            ),
            Verdict::Stay
        );
        assert!(matches!(
            dog.tick(
                start + ORPHAN_DRAIN_DEADLINE,
                input(CoreMode::GuiOwned, false, true)
            ),
            Verdict::Exit(_)
        ));
    }

    #[test]
    fn an_unconditional_shutdown_is_always_honoured() {
        let request = ShutdownRequest::from_params(&serde_json::Value::Null);
        assert_eq!(request, ShutdownRequest::default());
        assert_eq!(
            decide_shutdown(request, CoreMode::Headless, &[Some(1), Some(2), None]),
            Ok(())
        );
    }

    #[test]
    fn a_window_quitting_leaves_a_headless_core_alone() {
        let request = ShutdownRequest::from_params(&serde_json::json!({
            "requester_pid": 10, "unless_shared": true, "unless_headless": true,
        }));
        assert_eq!(
            decide_shutdown(request, CoreMode::Headless, &[Some(10)]),
            Err(Kept::Headless)
        );
        assert_eq!(
            decide_shutdown(request, CoreMode::GuiOwned, &[Some(10)]),
            Ok(())
        );
    }

    #[test]
    fn only_live_other_front_ends_veto_a_last_window_quit() {
        let request = ShutdownRequest {
            requester_pid: Some(10),
            unless_shared: true,
            unless_headless: true,
        };
        // Nobody else attached -- whatever a stale registry file might say.
        assert_eq!(decide_shutdown(request, CoreMode::GuiOwned, &[]), Ok(()));
        assert_eq!(
            decide_shutdown(request, CoreMode::GuiOwned, &[Some(10)]),
            Ok(())
        );
        assert_eq!(
            decide_shutdown(request, CoreMode::GuiOwned, &[Some(10), Some(11)]),
            Err(Kept::OtherFrontEnds(1))
        );
        // A front end that never said who it is counts as someone else.
        assert_eq!(
            decide_shutdown(request, CoreMode::GuiOwned, &[None]),
            Err(Kept::OtherFrontEnds(1))
        );
        // "Quit everything" does not ask about others.
        let everything = ShutdownRequest {
            unless_shared: false,
            ..request
        };
        assert_eq!(
            decide_shutdown(everything, CoreMode::GuiOwned, &[Some(10), Some(11)]),
            Ok(())
        );
    }
}
