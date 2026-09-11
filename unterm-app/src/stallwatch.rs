//! Notices when the GUI thread stops answering, and writes it down.
//!
//! On 2026-08-06 the GUI froze for stretches of up to seventeen minutes under
//! background build load, and the only reason anybody could say so afterwards
//! was the previous front end's stall log. This is that watchdog, ported to
//! the new window: the event loop beats on every tick, a watcher thread
//! measures the gaps, and a gap no user would forgive lands in
//! `<state>/stall.log` with its duration. It cannot prevent a stall; it makes
//! one impossible to mistake for "the terminal was fine".

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What the GUI thread is inside right now.
///
/// `SlowGuard` reports on drop, and a thread that never comes back never
/// drops anything: the twenty-minute freeze of 2026-09-11 left three
/// thousand lines of "stalled (ongoing)" and not one word about where. The
/// watcher thread reads this instead, so a stall that is still going names
/// the call it is stuck in -- which is the whole difference between a log
/// that proves a freeze happened and one that says why.
static DOING: Mutex<&'static str> = Mutex::new("");

/// Mark the GUI thread as being inside `what`, and return what it was inside
/// before so a nested guard can put it back.
pub fn enter(what: &'static str) -> &'static str {
    match DOING.try_lock() {
        Ok(mut slot) => std::mem::replace(&mut slot, what),
        Err(_) => "",
    }
}

/// Put back what `enter` displaced.
pub fn leave(previous: &'static str) {
    if let Ok(mut slot) = DOING.try_lock() {
        *slot = previous;
    }
}

/// Read by the watcher, never by the GUI thread. `try_lock` rather than
/// `lock`: a watchdog that can block is one more thread to hang, and the
/// only holder is a stalled thread whose name is what we came for anyway.
fn doing() -> &'static str {
    DOING.try_lock().map(|slot| *slot).unwrap_or("")
}

/// `, in feed_cockpit` -- or nothing, when the stall is somewhere unnamed.
fn whereabouts() -> String {
    match doing() {
        "" => String::new(),
        what => format!(", in {what}"),
    }
}

/// Milliseconds since `origin()`, last time the GUI thread beat.
static BEAT_MS: AtomicU64 = AtomicU64::new(0);

/// A stall shorter than this is a busy frame, not an event. Two seconds is
/// far past any redraw and far below anything a person would call frozen.
const STALL_MS: u64 = 2000;

/// While a stall is still going, note it at this interval so a hang that
/// never recovers still leaves a trace beyond its first line.
const ONGOING_MS: u64 = 30_000;

fn origin() -> Instant {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    origin().elapsed().as_millis() as u64
}

/// The GUI thread is alive. Called from the event loop's tick; cheap enough
/// to call every frame.
pub fn beat() {
    BEAT_MS.store(now_ms(), Ordering::Release);
}

/// Start the watcher thread. Call once, from the GUI thread, before the
/// event loop runs.
pub fn start() {
    beat();
    remember_beat(BEAT_MS.load(Ordering::Acquire));
    let spawned = std::thread::Builder::new()
        .name("gui-stall-watch".into())
        .spawn(|| {
            let mut last_reported: u64 = 0;
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let beat = BEAT_MS.load(Ordering::Acquire);
                let gap = now_ms().saturating_sub(beat);
                if gap >= STALL_MS {
                    // Still stalled. Note it now and then — a hang that never
                    // ends would otherwise never be written down at all.
                    if gap.saturating_sub(last_reported) >= ONGOING_MS {
                        log_line(&format!(
                            "GUI thread stalled for ~{gap}ms (ongoing{})",
                            whereabouts()
                        ));
                        last_reported = gap;
                    }
                } else {
                    if last_reported > 0 {
                        // Recovered: the ongoing lines told the story's
                        // middle; this is its end.
                        log_line(&format!(
                            "GUI thread recovered after ~{last_reported}ms stall"
                        ));
                    } else if beat > 0 && previous_beat() > 0 {
                        // A stall that began and ended between our looks --
                        // possible when the whole process was paused and the
                        // watcher slept through it. The gap between
                        // consecutive beats is its length.
                        let missed = beat.saturating_sub(previous_beat());
                        if missed >= STALL_MS {
                            log_line(&format!("GUI thread stalled for ~{missed}ms"));
                        }
                    }
                    last_reported = 0;
                }
                remember_beat(beat);
            }
        });
    if let Err(err) = spawned {
        log::warn!("could not start the GUI stall watcher: {err}");
    }
}

/// The beat value the watcher saw on its previous look, for catching stalls
/// that both began and ended between two looks.
fn previous_beat() -> u64 {
    PREV_BEAT.load(Ordering::Relaxed)
}

fn remember_beat(beat: u64) {
    PREV_BEAT.store(beat, Ordering::Relaxed);
}

static PREV_BEAT: AtomicU64 = AtomicU64::new(0);

fn log_line(message: &str) {
    log::warn!("{message}");
    let Some(dir) = unterm_protocol::state_dir() else {
        return;
    };
    let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let line = format!("{stamp} {message}\n");
    let _ = std::fs::create_dir_all(&dir);
    use std::io::Write as _;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("stall.log"))
    {
        let _ = file.write_all(line.as_bytes());
    }
}

/// Name a slow moment. For wrapping suspects on the GUI thread: anything
/// that held it past `threshold` lands in the log with its name, so a
/// "clicking X is laggy" report comes with the function that was lagging.
pub fn note_if_slow(what: &str, since: std::time::Instant, threshold_ms: u64) {
    let took = since.elapsed().as_millis() as u64;
    if took >= threshold_ms {
        log_line(&format!("{what} held the GUI thread for {took}ms"));
    }
}
