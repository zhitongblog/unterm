//! Noticing that the machine slept, without asking the operating system.
//!
//! macOS announces sleep on `NSWorkspace`'s own notification centre, Windows
//! sends `WM_POWERBROADCAST`, and Linux has logind — three APIs, three sets
//! of glue, and none of them testable without putting a real machine to
//! sleep. There is a simpler signal that works everywhere and can be tested
//! by feeding it numbers: **the two clocks disagree**.
//!
//! A monotonic clock does not advance while the machine is suspended — that
//! is true of `mach_absolute_time`, `CLOCK_MONOTONIC` and
//! `QueryPerformanceCounter` alike. The wall clock does. So when wall time
//! has moved far more than monotonic time between two ticks, the gap is how
//! long the machine was away.
//!
//! What this catches is **wake**, which is the event that actually needs
//! acting on: a provider's port, a network mount and any lease's expiry have
//! all had time to change. Logout and shutdown are not detectable this way
//! and still want their platform hooks — that is a real limit, not an
//! oversight, and [`crate::supervisor::action_for`] already says what those
//! two should do when somebody wires them.

use std::time::{Duration, Instant};

/// How far the clocks must disagree before this counts as a sleep.
///
/// Small enough to catch a lid closed for a minute, large enough that NTP
/// nudging the wall clock, a timezone change or a busy scheduler does not
/// trigger a re-probe. Every one of those moves the wall clock by seconds;
/// none of them moves it by half a minute.
pub const THRESHOLD: Duration = Duration::from_secs(30);

/// Watches two clocks and reports when they disagree.
pub struct WakeWatch {
    monotonic: Instant,
    wall: chrono::DateTime<chrono::Utc>,
    threshold: Duration,
}

impl WakeWatch {
    pub fn new() -> Self {
        Self::with_threshold(THRESHOLD)
    }

    pub fn with_threshold(threshold: Duration) -> Self {
        Self {
            monotonic: Instant::now(),
            wall: chrono::Utc::now(),
            threshold,
        }
    }

    /// Sample both clocks. Returns how long the machine was away, if it was.
    ///
    /// Called from the event loop's idle tick, so it must be cheap and must
    /// never block: two clock reads and a subtraction.
    pub fn tick(&mut self) -> Option<Duration> {
        self.sample(Instant::now(), chrono::Utc::now())
    }

    /// The testable half. Both clocks are supplied, so a test can describe a
    /// machine that slept for an hour without sleeping for an hour.
    pub fn sample(
        &mut self,
        monotonic: Instant,
        wall: chrono::DateTime<chrono::Utc>,
    ) -> Option<Duration> {
        let monotonic_delta = monotonic.saturating_duration_since(self.monotonic);
        let wall_delta = wall.signed_duration_since(self.wall);
        self.monotonic = monotonic;
        self.wall = wall;

        // A wall clock that went *backwards* is somebody correcting the time,
        // not a machine waking up. Re-probing then would be harmless but
        // reporting it as sleep would be a lie, and the field is read.
        let wall_delta = wall_delta.to_std().ok()?;
        let gap = wall_delta.checked_sub(monotonic_delta)?;
        (gap >= self.threshold).then_some(gap)
    }
}

impl Default for WakeWatch {
    fn default() -> Self {
        Self::new()
    }
}

/// What to do about a wake, in the order that matters.
///
/// Providers first: everything else that is about to fail will fail *because*
/// a provider moved, and re-probing after acting on stale state means acting
/// twice.
pub fn on_wake(away: Duration) -> serde_json::Value {
    let expired = crate::cockpit::fleet_store::tasks()
        .and_then(|store| store.expire_leases().ok())
        .unwrap_or_default();
    let providers = crate::providers::rediscover()
        .map(|registry| registry.ids())
        .unwrap_or_default();
    serde_json::json!({
        "away_seconds": away.as_secs(),
        "providers_rediscovered": providers,
        // Leases measure real time, so a machine that slept through one has a
        // key that is over. Saying which is the difference between "your
        // agent stopped working" and "your agent's permission ran out at
        // 3am".
        "leases_expired": expired,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(1_800_000_000 + seconds, 0).unwrap()
    }

    #[test]
    fn an_ordinary_tick_is_not_a_sleep() {
        let start = Instant::now();
        let mut watch = WakeWatch {
            monotonic: start,
            wall: at(0),
            threshold: THRESHOLD,
        };
        // A second of real time, a second on both clocks.
        assert_eq!(watch.sample(start + Duration::from_secs(1), at(1)), None);
    }

    #[test]
    fn clocks_that_disagree_by_a_lot_mean_the_machine_slept() {
        // The monotonic clock does not advance while suspended; the wall
        // clock does. An hour of disagreement is an hour of sleep.
        let start = Instant::now();
        let mut watch = WakeWatch {
            monotonic: start,
            wall: at(0),
            threshold: THRESHOLD,
        };
        let away = watch
            .sample(start + Duration::from_millis(50), at(3600))
            .expect("an hour of disagreement was not reported");
        assert!(away >= Duration::from_secs(3599), "{away:?}");
    }

    #[test]
    fn a_clock_correction_is_not_a_sleep() {
        // NTP nudging the wall clock by seconds must not send every provider
        // through a re-probe.
        let start = Instant::now();
        let mut watch = WakeWatch {
            monotonic: start,
            wall: at(0),
            threshold: THRESHOLD,
        };
        assert_eq!(watch.sample(start + Duration::from_secs(1), at(6)), None);
    }

    #[test]
    fn a_wall_clock_that_went_backwards_is_not_reported_as_sleep() {
        // It happens — a correction, a timezone-confused VM, a user setting
        // the date. Harmless to re-probe, but the number would be a lie and
        // the number is read.
        let start = Instant::now();
        let mut watch = WakeWatch {
            monotonic: start,
            wall: at(1000),
            threshold: THRESHOLD,
        };
        assert_eq!(watch.sample(start + Duration::from_secs(1), at(0)), None);
    }

    #[test]
    fn the_watch_moves_on_so_one_sleep_is_reported_once() {
        // Otherwise every tick after a wake would re-report the same gap, and
        // the providers would be re-probed forever.
        let start = Instant::now();
        let mut watch = WakeWatch {
            monotonic: start,
            wall: at(0),
            threshold: THRESHOLD,
        };
        assert!(watch
            .sample(start + Duration::from_millis(10), at(600))
            .is_some());
        assert_eq!(
            watch.sample(start + Duration::from_millis(20), at(600)),
            None,
            "the same sleep was reported twice"
        );
    }

    #[test]
    fn a_long_but_awake_stretch_is_not_a_sleep() {
        // A machine that was simply busy for ten minutes moves both clocks.
        let start = Instant::now();
        let mut watch = WakeWatch {
            monotonic: start,
            wall: at(0),
            threshold: THRESHOLD,
        };
        assert_eq!(watch.sample(start + Duration::from_secs(600), at(600)), None);
    }

    #[test]
    fn waking_expires_the_leases_that_ran_out_while_away() {
        // A lease measures real time, so a machine that slept through one has
        // a key that is over. "Your agent's permission ran out at 3am" is a
        // different message from "your agent stopped working".
        let _dir = crate::test_env::IsolatedEnv::new();

        let store = crate::cockpit::fleet_store::tasks().unwrap();
        let lease = store
            .issue_lease(unterm_tasks::NewLease {
                provider: "unzoo".into(),
                capability: "browser".into(),
                ttl_seconds: -5,
                ..unterm_tasks::NewLease::default()
            })
            .unwrap();
        std::thread::sleep(Duration::from_millis(1100));

        let report = on_wake(Duration::from_secs(3600));
        assert_eq!(report["away_seconds"], 3600);
        let expired = report["leases_expired"].as_array().unwrap();
        assert!(
            expired.iter().any(|id| id.as_str() == Some(lease.id.as_str())),
            "{report}"
        );
    }
}
