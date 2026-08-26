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
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CountingRelay {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let counter = accepts.clone();
        let halt = stop.clone();
        let handle = std::thread::spawn(move || {
            while !halt.load(std::sync::atomic::Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        drop(stream);
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
                }
            }
        });

        Self {
            port,
            accepts,
            stop,
            handle: Some(handle),
        }
    }

    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.port)
    }

    fn accepts(&self) -> usize {
        self.accepts.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for CountingRelay {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Drives the scenario that leaked: build a pool, drop it, then watch whether
/// anything still reconnects to the relay behind its back.
struct LeakProbe {
    relay: CountingRelay,
}

impl LeakProbe {
    fn new() -> Self {
        Self {
            relay: CountingRelay::start(),
        }
    }

    /// Retry at a flat 100ms cadence, so an orphaned task betrays itself many
    /// times inside the observation window. The production default doubles up
    /// to 60s, which would need a minutes-long test to observe.
    fn eager_reconnect() -> nostro2_relay::ReconnectConfig {
        nostro2_relay::ReconnectConfig::fixed(std::time::Duration::from_millis(100))
    }

    fn build_and_drop_pool(&self) {
        let url = self.relay.url();
        let pool =
            nostro2_relay::NostrPool::with_config(&[url.as_str()], 128, &Self::eager_reconnect());
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(pool);
    }

    fn build_and_drop_default_pool(&self) {
        let url = self.relay.url();
        let pool = nostro2_relay::NostrPool::new(&[url.as_str()]);
        std::thread::sleep(std::time::Duration::from_millis(300));
        drop(pool);
    }

    fn accepts_after_drop(&self, settle: std::time::Duration) -> usize {
        std::thread::sleep(settle);
        let baseline = self.relay.accepts();
        std::thread::sleep(settle);
        self.relay.accepts() - baseline
    }
}

#[test]
fn dropped_pool_stops_reconnecting() {
    let probe = LeakProbe::new();
    probe.build_and_drop_pool();

    let reconnects = probe.accepts_after_drop(std::time::Duration::from_secs(3));

    assert_eq!(
        reconnects, 0,
        "a dropped pool reconnected {reconnects} more times; its task outlived it and leaked a socket",
    );
}

#[test]
fn repeated_pool_rebuilds_do_not_accumulate_connections() {
    let probe = LeakProbe::new();
    for _ in 0..4 {
        probe.build_and_drop_default_pool();
    }

    let reconnects = probe.accepts_after_drop(std::time::Duration::from_secs(3));

    assert_eq!(
        reconnects, 0,
        "{reconnects} orphaned reconnects survived four pool rebuilds",
    );
}
