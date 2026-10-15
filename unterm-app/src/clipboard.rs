//! Clipboard work that must never hold the winit event thread.

/// Run one platform clipboard operation away from the UI and return its result
/// through the app's channel.
pub fn run<T, F>(tx: std::sync::mpsc::Sender<T>, operation: F)
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    std::thread::spawn(move || {
        let _ = tx.send(operation());
        crate::mcp_host::wake_loop();
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_slow_platform_retry_does_not_hold_the_calling_thread() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let started = Instant::now();
        run(tx, move || {
            release_rx.recv().unwrap();
            42
        });
        assert!(started.elapsed() < Duration::from_millis(50));
        assert!(rx.try_recv().is_err());
        release_tx.send(()).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), 42);
    }
}
