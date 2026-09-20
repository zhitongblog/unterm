use super::{session_registry, with_current, with_current_mut, with_session, NextCoreRuntime};
use anyhow::Result;
use parking_lot::Mutex;
use std::sync::{atomic::AtomicBool, Arc};

/// Serialises every test that touches the process-wide runtime.
static RUNTIME_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Put the runtime back to empty, and keep other tests out until you are done.
///
/// The runtime is one global. A test that resets it while another is midway
/// through pulls the floor out from under that one, which is why the guard
/// comes back with the reset rather than being a separate thing to remember:
/// you cannot reset without holding it.
#[must_use = "hold the guard for the length of the test, or another test will reset the runtime under you"]
pub(in crate::next_core) fn reset() -> RuntimeTestGuard {
    let guard = RuntimeTestGuard(RUNTIME_TEST_LOCK.lock());
    guard.reset();
    guard
}

/// Proof that this test has the runtime to itself.
///
/// The guard is never read, and must not be removed for that reason: holding
/// it *is* the point, and dropping it at the end of the test is what lets the
/// next one in. `-D warnings` counts an unread field as dead code, which is
/// usually right and here would delete the exclusion this whole module exists
/// to provide.
pub(in crate::next_core) struct RuntimeTestGuard(
    #[allow(dead_code)] parking_lot::MutexGuard<'static, ()>,
);

impl RuntimeTestGuard {
    /// Start over without giving up the lock.
    ///
    /// A test that checks several independent cases needs a clean runtime for
    /// each; calling `reset()` again would deadlock on the lock it already
    /// holds, so it asks the guard instead.
    pub(in crate::next_core) fn reset(&self) {
        with_current_mut(|state| *state = NextCoreRuntime::default());
    }
}

pub(in crate::next_core) struct TestSessionHandles {
    pub(in crate::next_core) output: Arc<Mutex<String>>,
    pub(in crate::next_core) screen: Arc<Mutex<super::super::NextCoreScreen>>,
    pub(in crate::next_core) recording: Arc<Mutex<Option<super::super::NextCoreRecording>>>,
    pub(in crate::next_core) activity: Arc<Mutex<super::super::activity::SessionIoActivity>>,
    pub(in crate::next_core) dead: Arc<AtomicBool>,
    pub(in crate::next_core) dead_reason: Arc<Mutex<Option<String>>>,
    pub(in crate::next_core) cols: usize,
    pub(in crate::next_core) rows: usize,
}

pub(in crate::next_core) fn session_handles(pane_id: usize) -> Result<TestSessionHandles> {
    with_session(pane_id, |session| {
        Ok(TestSessionHandles {
            output: Arc::clone(&session.output),
            screen: Arc::clone(&session.screen),
            recording: Arc::clone(&session.recording),
            activity: Arc::clone(&session.activity),
            dead: Arc::clone(&session.dead),
            dead_reason: Arc::clone(&session.dead_reason),
            cols: session.snapshot.cols,
            rows: session.snapshot.rows,
        })
    })
}

pub(in crate::next_core) fn pane_count() -> usize {
    with_current(session_registry::pane_count)
}

pub(in crate::next_core) fn next_session_id() -> usize {
    with_current_mut(session_registry::next_session_id)
}
