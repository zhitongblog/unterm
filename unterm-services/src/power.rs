//! The operating system asking a process to stop.
//!
//! Sleep and wake are noticed by watching two clocks
//! ([`crate::wake_watch`]) — no platform API needed. Logout and shutdown
//! cannot be inferred that way, and they are the two that matter most,
//! because they are when work is about to be taken away.
//!
//! Every platform *does* ask, and the asking is remarkably similar once you
//! find it:
//!
//! * **macOS and Linux** send `SIGTERM` to a process on logout and on
//!   shutdown. That is not an approximation of the hook — it *is* the hook.
//! * **Windows** has no signals, but a console process gets
//!   `CTRL_LOGOFF_EVENT` and `CTRL_SHUTDOWN_EVENT` through
//!   `SetConsoleCtrlHandler`. `unterm-core` is a console binary, which is why
//!   this is installed there rather than in the window.
//!
//! The Core is the right place for all of it: it owns the sessions and the
//! task store, so it is the process whose sudden death costs something. A
//! window closing costs a window.
//!
//! What "stop now" means is deliberately small. A shutdown gives seconds, not
//! minutes — the system will kill whatever is still running — so this writes
//! down *that* it is stopping and why, and gets out of the way. Trying to
//! finish work here is how a process gets killed halfway through finishing
//! it, which is worse than not having started.

use std::sync::atomic::{AtomicBool, Ordering};

static STOPPING: AtomicBool = AtomicBool::new(false);

/// Whether something has asked this process to stop.
///
/// The event loop reads it. A handler that tried to tear things down from
/// inside a signal context would be calling allocator and lock code from a
/// place neither is safe.
pub fn stopping() -> bool {
    STOPPING.load(Ordering::SeqCst)
}

/// Record that the system is taking this process away, and why.
///
/// Returns whether this was the first such request: a shutdown often arrives
/// as a signal *and* a window message, and writing the trail twice would put
/// two endings in a log that is supposed to say what happened.
pub fn note_stop(reason: &str) -> bool {
    if STOPPING.swap(true, Ordering::SeqCst) {
        return false;
    }
    crate::audit_store::append_correlated(
        &serde_json::json!({
            "at": chrono::Utc::now().to_rfc3339(),
            "event": "core.stopping",
            "reason": reason,
            // What the supervisor's table says to do about it, recorded here
            // so the trail and the policy cannot drift into two answers.
            "action": format!(
                "{:?}",
                crate::supervisor::action_for(machine_for(reason))
            ),
        }),
        &crate::audit_store::Correlation {
            state: Some("stopping".into()),
            ..Default::default()
        },
    );
    true
}

/// Which machine event a reason describes.
fn machine_for(reason: &str) -> crate::supervisor::Machine {
    match reason {
        "logoff" => crate::supervisor::Machine::Logout,
        _ => crate::supervisor::Machine::Shutdown,
    }
}

/// Ask the operating system to tell us before it stops this process.
///
/// Safe to call more than once; the second call does nothing.
pub fn install() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        #[cfg(unix)]
        unix::install();
        #[cfg(windows)]
        windows::install();
    });
}

#[cfg(unix)]
mod unix {
    use super::*;

    /// What the handler is allowed to do: set one flag.
    ///
    /// Everything else — allocating, locking, writing a file — is unsafe in a
    /// signal handler, and a terminal that deadlocks while shutting down is
    /// one the user force-kills, losing the very thing this exists to save.
    extern "C" fn on_signal(signal: libc::c_int) {
        let reason = match signal {
            libc::SIGHUP => 1u8,
            libc::SIGINT => 2,
            _ => 0,
        };
        PENDING.store(reason as usize + 1, Ordering::SeqCst);
    }

    static PENDING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    pub fn install() {
        unsafe {
            for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
                libc::signal(signal, on_signal as *const () as libc::sighandler_t);
            }
        }
    }

    /// Drain what a handler flagged, from a context where work is allowed.
    ///
    /// Called from the event loop. This split is the whole reason the handler
    /// is one atomic store.
    pub fn drain() -> Option<&'static str> {
        match PENDING.swap(0, Ordering::SeqCst) {
            0 => None,
            1 => Some("shutdown"),
            2 => Some("hangup"),
            _ => Some("interrupt"),
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;

    const CTRL_C_EVENT: u32 = 0;
    const CTRL_CLOSE_EVENT: u32 = 2;
    const CTRL_LOGOFF_EVENT: u32 = 5;
    const CTRL_SHUTDOWN_EVENT: u32 = 6;

    static PENDING: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleCtrlHandler(
            handler: Option<unsafe extern "system" fn(u32) -> i32>,
            add: i32,
        ) -> i32;
    }

    unsafe extern "system" fn on_ctrl(event: u32) -> i32 {
        PENDING.store(event as usize + 1, Ordering::SeqCst);
        // Windows gives a console process a few seconds after a logoff or
        // shutdown event and then ends it regardless. Returning TRUE says the
        // event was handled; the process still has to be quick.
        1
    }

    pub fn install() {
        unsafe {
            SetConsoleCtrlHandler(Some(on_ctrl), 1);
        }
    }

    pub fn drain() -> Option<&'static str> {
        match PENDING.swap(0, Ordering::SeqCst).checked_sub(1) {
            None => None,
            Some(event) => Some(match event as u32 {
                CTRL_LOGOFF_EVENT => "logoff",
                CTRL_SHUTDOWN_EVENT => "shutdown",
                CTRL_CLOSE_EVENT => "closed",
                CTRL_C_EVENT => "interrupt",
                _ => "shutdown",
            }),
        }
    }
}

/// Whatever the platform flagged since the last call, as a reason string.
pub fn drain() -> Option<&'static str> {
    #[cfg(unix)]
    {
        unix::drain()
    }
    #[cfg(windows)]
    {
        windows::drain()
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

/// The whole of it, for a loop that just wants to know whether to stop.
///
/// Drains what the platform reported, writes the trail once, and answers
/// whether this process should be leaving.
pub fn should_stop() -> Option<&'static str> {
    let reason = drain()?;
    note_stop(reason);
    Some(reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn isolate() -> crate::test_env::IsolatedEnv {
        let dir = crate::test_env::IsolatedEnv::new();
        STOPPING.store(false, Ordering::SeqCst);
        dir
    }

    #[test]
    fn stopping_is_written_down_once() {
        // A shutdown can arrive as a signal and a window message. Two endings
        // in a trail that is supposed to say what happened is worse than one.
        let _dir = isolate();
        assert!(note_stop("shutdown"));
        assert!(!note_stop("shutdown"));
        assert!(stopping());

        let entries: Vec<serde_json::Value> = crate::audit_store::recent(10)
            .into_iter()
            .filter(|entry| entry["event"] == "core.stopping")
            .collect();
        assert_eq!(entries.len(), 1, "{entries:#?}");
        assert_eq!(entries[0]["reason"], "shutdown");
        assert_eq!(entries[0]["state"], "stopping");
    }

    #[test]
    fn the_trail_records_what_the_policy_said_to_do() {
        // The audit line and `supervisor::action_for` must not drift into two
        // answers about the same event.
        let _dir = isolate();
        note_stop("logoff");
        let entry = crate::audit_store::recent(10)
            .into_iter()
            .find(|entry| entry["event"] == "core.stopping")
            .unwrap();
        assert_eq!(entry["action"], "Drain", "a logout should drain: {entry}");

        STOPPING.store(false, Ordering::SeqCst);
        note_stop("shutdown");
        let entry = crate::audit_store::recent(10)
            .into_iter()
            .rev()
            .find(|entry| entry["event"] == "core.stopping")
            .unwrap();
        assert_eq!(entry["action"], "StopNow", "a shutdown has seconds: {entry}");
    }

    #[test]
    fn nothing_pending_means_nothing_to_do() {
        let _dir = isolate();
        assert_eq!(drain(), None);
        assert_eq!(should_stop(), None);
        assert!(!stopping());
    }

    #[test]
    #[cfg(unix)]
    fn a_real_signal_is_noticed() {
        // The handler itself, driven by the kernel rather than by a mock:
        // this is the one part of the path that cannot be checked any other
        // way short of logging out.
        let _dir = isolate();
        install();
        unsafe {
            libc::raise(libc::SIGTERM);
        }
        // The handler only sets a flag; the work happens here, where
        // allocating and locking are allowed.
        assert_eq!(should_stop(), Some("shutdown"));
        assert!(stopping());
        assert!(crate::audit_store::recent(10)
            .into_iter()
            .any(|entry| entry["event"] == "core.stopping"));
    }

    #[test]
    #[cfg(unix)]
    fn a_hangup_is_told_apart_from_a_shutdown() {
        // Different reasons in the trail, because "the terminal went away"
        // and "the machine is going down" are different mornings-after.
        let _dir = isolate();
        install();
        unsafe {
            libc::raise(libc::SIGHUP);
        }
        assert_eq!(drain(), Some("hangup"));
    }
}
