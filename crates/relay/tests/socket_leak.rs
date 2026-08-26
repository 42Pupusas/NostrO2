//! Regression test for the connection leak that exhausted the issuer's file
//! descriptors in production.
//!
//! A dropped pool used to leave its per-relay tasks running. Each task kept
//! reconnecting on an infinite schedule, so every pool rebuild orphaned one
//! live socket per relay until the process ran out of descriptors.

/// A TCP listener that accepts connections and counts them, never completing a
/// WebSocket handshake. A relay client therefore fails to connect and retries,
/// which makes the retry traffic observable as an accept count.
struct CountingRelay {
    port: u16,
    accepts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    _task: nostro2_relay::TaskGuard,
}

impl CountingRelay {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let counter = accepts.clone();
        let task = tokio::spawn(async move {
            loop {
                if let Ok((stream, _)) = listener.accept().await {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    drop(stream);
                }
            }
        });

        Self {
            port,
            accepts,
            _task: nostro2_relay::TaskGuard::new(task),
        }
    }

    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.port)
    }

    fn accepts(&self) -> usize {
        self.accepts.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Drives the scenario that leaked: build a pool, drop it, then watch whether
/// anything still reconnects to the relay behind its back.
struct LeakProbe {
    relay: CountingRelay,
}

impl LeakProbe {
    async fn new() -> Self {
        Self {
            relay: CountingRelay::start().await,
        }
    }

    /// Retry at a flat one-second cadence, so an orphaned task betrays itself
    /// within a short window. The production default doubles up to 60s, which
    /// would need a minutes-long test to observe.
    ///
    /// Delays are second-granular by contract: `is_enabled` and the manager's
    /// own guard both test `as_secs()`, so a sub-second `max_delay` disables
    /// reconnection instead of speeding it up.
    fn eager_reconnect() -> nostro2_relay::ReconnectConfig {
        nostro2_relay::ReconnectConfig {
            max_retries: 0,
            initial_delay: std::time::Duration::from_secs(1),
            max_delay: std::time::Duration::from_secs(2),
            backoff_multiplier: 1.0,
        }
    }

    async fn build_and_drop_pool(&self) {
        let url = self.relay.url();
        let pool =
            nostro2_relay::NostrPool::with_config(&[url.as_str()], 128, &Self::eager_reconnect());
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        drop(pool);
    }

    async fn build_and_drop_default_pool(&self) {
        let url = self.relay.url();
        let pool = nostro2_relay::NostrPool::new(&[url.as_str()]);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        drop(pool);
    }

    async fn accepts_after_drop(&self, settle: std::time::Duration) -> usize {
        tokio::time::sleep(settle).await;
        let baseline = self.relay.accepts();
        tokio::time::sleep(settle).await;
        self.relay.accepts() - baseline
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dropped_pool_stops_reconnecting() {
    let probe = LeakProbe::new().await;
    probe.build_and_drop_pool().await;

    let reconnects = probe
        .accepts_after_drop(std::time::Duration::from_secs(3))
        .await;

    assert_eq!(
        reconnects, 0,
        "a dropped pool reconnected {reconnects} more times; its task outlived it and leaked a socket",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_pool_rebuilds_do_not_accumulate_connections() {
    let probe = LeakProbe::new().await;
    for _ in 0..4 {
        probe.build_and_drop_default_pool().await;
    }

    let reconnects = probe
        .accepts_after_drop(std::time::Duration::from_secs(3))
        .await;

    assert_eq!(
        reconnects, 0,
        "{reconnects} orphaned reconnects survived four pool rebuilds",
    );
}
