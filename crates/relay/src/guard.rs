//! Thread lifetime control.
//!
//! A driver thread reconnects on an infinite schedule, so it outlives its
//! owner unless somebody stops it. [`Shutdown`] is the flag the thread reads;
//! [`DriverGuard`] is the handle that raises the flag, wakes the thread, and
//! waits for it to leave.
//!
//! This is the thread-world successor of the task guard: a task can be
//! aborted at any await point, but a thread must be asked to stop and then
//! joined, so the two halves are separate types.

/// The stop flag shared between a driver thread and its guard.
///
/// Cloning shares one flag. The thread polls [`Self::is_raised`] once per
/// loop; the guard raises it once and never lowers it.
#[derive(Debug, Clone, Default)]
pub struct Shutdown {
    flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Shutdown {
    /// Creates a flag that is not yet raised.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the thread to stop.
    pub fn raise(&self) {
        self.flag.store(true, std::sync::atomic::Ordering::Release);
    }

    /// Whether the thread must stop.
    #[must_use]
    pub fn is_raised(&self) -> bool {
        self.flag.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Sleeps for `timeout`, or until the flag is raised.
    ///
    /// Returns `true` when the sleep completed and `false` when the flag
    /// interrupted it. A driver waiting out a reconnect backoff uses this so
    /// a dropped owner does not wait for a 60-second delay to elapse.
    #[must_use]
    pub fn sleep(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if self.is_raised() {
                return false;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return true;
            }
            std::thread::park_timeout(deadline - now);
        }
    }
}

/// Owns a driver thread and stops it when the last owner drops.
///
/// Drop raises the shutdown flag, unparks the thread so it observes the flag
/// at once, and joins it. The join makes the guard a real ownership boundary:
/// when it returns, the thread's socket is closed, not merely doomed.
#[derive(Debug)]
pub struct DriverGuard {
    shutdown: Shutdown,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl DriverGuard {
    /// Binds `handle` to the `shutdown` flag the thread reads.
    #[must_use]
    pub const fn new(shutdown: Shutdown, handle: std::thread::JoinHandle<()>) -> Self {
        Self {
            shutdown,
            handle: Some(handle),
        }
    }

    /// Whether the thread has left its loop.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    }

    /// Raises the shutdown flag and wakes the thread, without waiting.
    pub fn stop(&self) {
        self.shutdown.raise();
        if let Some(handle) = self.handle.as_ref() {
            handle.thread().unpark();
        }
    }

    /// Wakes the thread so it drains work it has not noticed yet.
    pub fn wake(&self) {
        if let Some(handle) = self.handle.as_ref() {
            handle.thread().unpark();
        }
    }

    /// Stops the thread and waits for it.
    ///
    /// Drop does this too; call it directly to observe a panicking thread.
    ///
    /// # Errors
    ///
    /// Returns the thread's panic payload when it ended by panicking.
    pub fn join(mut self) -> std::thread::Result<()> {
        self.stop();
        self.handle.take().map_or(Ok(()), std::thread::JoinHandle::join)
    }
}

impl Drop for DriverGuard {
    fn drop(&mut self) {
        self.shutdown.raise();
        if let Some(handle) = self.handle.take() {
            handle.thread().unpark();
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Worker {
        shutdown: Shutdown,
        laps: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Worker {
        fn new() -> Self {
            Self {
                shutdown: Shutdown::new(),
                laps: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn spawn_counting(&self) -> DriverGuard {
            let shutdown = self.shutdown.clone();
            let laps = self.laps.clone();
            let handle = std::thread::spawn(move || {
                while !shutdown.is_raised() {
                    laps.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let _ = shutdown.sleep(std::time::Duration::from_millis(1));
                }
            });
            DriverGuard::new(self.shutdown.clone(), handle)
        }

        fn spawn_sleeping(&self, nap: std::time::Duration) -> DriverGuard {
            let shutdown = self.shutdown.clone();
            let laps = self.laps.clone();
            let handle = std::thread::spawn(move || {
                while !shutdown.is_raised() {
                    if shutdown.sleep(nap) {
                        laps.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            });
            DriverGuard::new(self.shutdown.clone(), handle)
        }

        /// A thread that does slow cleanup after it sees the flag, the way a
        /// driver closes its socket. `laps` reaches 1 only at the very end.
        fn spawn_with_slow_cleanup(&self) -> DriverGuard {
            let shutdown = self.shutdown.clone();
            let laps = self.laps.clone();
            let handle = std::thread::spawn(move || {
                while !shutdown.is_raised() {
                    let _ = shutdown.sleep(std::time::Duration::from_millis(1));
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
                laps.store(1, std::sync::atomic::Ordering::SeqCst);
            });
            DriverGuard::new(self.shutdown.clone(), handle)
        }

        fn laps(&self) -> usize {
            self.laps.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn settle() {
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
    }

    #[test]
    fn a_fresh_flag_is_not_raised() {
        assert!(!Shutdown::new().is_raised());
    }

    #[test]
    fn clones_share_one_flag() {
        let shutdown = Shutdown::new();
        let clone = shutdown.clone();
        shutdown.raise();
        assert!(clone.is_raised());
    }

    #[test]
    fn sleep_reports_completion_when_it_is_not_interrupted() {
        assert!(Shutdown::new().sleep(std::time::Duration::from_millis(5)));
    }

    #[test]
    fn sleep_returns_at_once_when_the_flag_is_already_raised() {
        let shutdown = Shutdown::new();
        shutdown.raise();
        let started = std::time::Instant::now();
        assert!(!shutdown.sleep(std::time::Duration::from_secs(30)));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn a_live_guard_keeps_the_thread_running() {
        let worker = Worker::new();
        let guard = worker.spawn_counting();
        Worker::settle();
        assert!(worker.laps() > 0);
        assert!(!guard.is_finished());
    }

    #[test]
    fn drop_stops_the_thread() {
        let worker = Worker::new();
        let guard = worker.spawn_counting();
        Worker::settle();
        drop(guard);

        let after_drop = worker.laps();
        Worker::settle();
        assert_eq!(worker.laps(), after_drop);
    }

    // Drop must wait for the thread, not merely doom it: when drop returns,
    // the driver's socket is already closed rather than closing.
    #[test]
    fn drop_waits_for_the_thread_to_finish_its_cleanup() {
        let worker = Worker::new();
        let guard = worker.spawn_with_slow_cleanup();
        Worker::settle();

        drop(guard);
        assert_eq!(worker.laps(), 1);
    }

    // A driver waiting out a 30s reconnect backoff must not hold its owner's
    // drop for 30 seconds.
    #[test]
    fn drop_interrupts_a_long_backoff_sleep() {
        let worker = Worker::new();
        let guard = worker.spawn_sleeping(std::time::Duration::from_secs(30));
        Worker::settle();

        let started = std::time::Instant::now();
        drop(guard);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(worker.laps(), 0);
    }

    #[test]
    fn stop_signals_without_waiting() {
        let worker = Worker::new();
        let guard = worker.spawn_sleeping(std::time::Duration::from_secs(30));
        Worker::settle();
        guard.stop();
        Worker::settle();
        assert!(guard.is_finished());
    }

    #[test]
    fn join_reports_a_clean_exit() {
        let worker = Worker::new();
        let guard = worker.spawn_counting();
        Worker::settle();
        assert!(guard.join().is_ok());
    }

    #[test]
    fn join_surfaces_a_panicking_thread() {
        let shutdown = Shutdown::new();
        let handle = std::thread::spawn(|| panic!("driver died"));
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(DriverGuard::new(shutdown, handle).join().is_err());
    }

    #[test]
    fn wake_unparks_a_sleeping_thread_without_stopping_it() {
        let worker = Worker::new();
        let guard = worker.spawn_sleeping(std::time::Duration::from_millis(50));
        Worker::settle();
        guard.wake();
        Worker::settle();
        assert!(!guard.is_finished());
        assert!(worker.laps() > 0);
    }
}
