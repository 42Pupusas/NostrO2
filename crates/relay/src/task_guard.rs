/// Owns a spawned task and aborts it when the last owner drops.
///
/// A bare `tokio::spawn` outlives the value that created it. A relay's
/// connection manager reconnects on an infinite schedule, so an orphaned
/// manager holds a TLS socket open forever: every pool rebuild leaks one
/// connection per relay until the process exhausts its file descriptors.
/// Holding the `JoinHandle` here ties the task's lifetime to its owner.
#[derive(Debug)]
pub struct TaskGuard {
    handle: tokio::task::JoinHandle<()>,
}

impl TaskGuard {
    #[must_use]
    pub const fn new(handle: tokio::task::JoinHandle<()>) -> Self {
        Self { handle }
    }

    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }
}

impl Drop for TaskGuard {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Probe {
        flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl Probe {
        fn new() -> Self {
            Self {
                flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            }
        }

        fn spawn_forever(&self) -> tokio::task::JoinHandle<()> {
            let flag = self.flag.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    flag.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            })
        }

        fn ran(&self) -> bool {
            self.flag.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn reset(&self) {
            self.flag.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn drop_aborts_the_task() {
        let probe = Probe::new();
        let guard = TaskGuard::new(probe.spawn_forever());

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(probe.ran());

        drop(guard);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        probe.reset();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        assert!(!probe.ran());
    }

    #[tokio::test]
    async fn live_guard_keeps_the_task_running() {
        let probe = Probe::new();
        let guard = TaskGuard::new(probe.spawn_forever());

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        probe.reset();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        assert!(probe.ran());
        assert!(!guard.is_finished());
    }

    #[tokio::test]
    async fn shared_guard_aborts_only_when_last_owner_drops() {
        let probe = Probe::new();
        let guard = std::sync::Arc::new(TaskGuard::new(probe.spawn_forever()));
        let clone = guard.clone();

        drop(guard);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        probe.reset();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(probe.ran());

        drop(clone);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        probe.reset();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!probe.ran());
    }
}
