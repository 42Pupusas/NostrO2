//! Liveness guarantees for long-lived services.
//!
//! The crate's main use is a daemon that holds a relay pool open for weeks
//! and reconnects through every network fault. Such a service fails in two
//! ways that a short test never sees:
//!
//! - it **hangs**: a reader waits forever for an event that cannot arrive,
//!   because the connection died and nothing woke the reader;
//! - it **leaks**: threads, sockets, or memory accumulate across reconnects
//!   until the process dies.
//!
//! Every test here states one of those guarantees and fails on a timeout
//! rather than hanging the suite.

/// Runs `body` on a thread and fails when it outlives `limit`.
///
/// A hang is the failure mode under test, so no test may simply wait for
/// one: the deadline turns a hang into a readable assertion.
struct Deadline;

impl Deadline {
    fn enforce<F>(limit: std::time::Duration, what: &str, body: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            body();
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(limit).is_ok(),
            "{what} did not finish within {limit:?}"
        );
        worker.join().expect("the worker panicked");
    }
}

/// A relay that accepts connections and can be told how to behave.
struct ScriptedRelay {
    port: u16,
    accepts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ScriptedRelay {
    fn start(script: Script) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        let counter = accepts.clone();
        let reqs = requests.clone();
        let halt = stop.clone();
        let handle = std::thread::spawn(move || {
            let mut sessions = Vec::new();
            while !halt.load(std::sync::atomic::Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let seq = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        stream.set_nonblocking(false).unwrap();
                        let halt = halt.clone();
                        let reqs = reqs.clone();
                        sessions.push(std::thread::spawn(move || {
                            script.serve(stream, &halt, seq, &reqs);
                        }));
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(2)),
                }
            }
            for session in sessions {
                let _ = session.join();
            }
        });

        Self {
            port,
            accepts,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    /// How many REQ frames every session has received in total.
    fn requests(&self) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        self.requests.clone()
    }

    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.port)
    }

    fn accepts(&self) -> usize {
        self.accepts.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Drop for ScriptedRelay {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[derive(Clone, Copy)]
enum Script {
    /// Upgrade, then answer pings but send nothing else, like a relay with
    /// no matching events. This connection is healthy and must survive.
    Silent,
    /// Upgrade, then neither read nor write, while holding the socket open.
    /// This is a half-open connection: TCP still believes it is fine, so
    /// only a liveness probe can reveal it.
    Mute,
    /// Upgrade, then drop the connection at once, forcing a reconnect.
    DropAtOnce,
    /// Drop the first connection after a moment, then serve normally.
    /// This forces exactly one reconnect on an otherwise healthy relay.
    DropOnce,
}

impl Script {
    fn serve(
        self,
        stream: std::net::TcpStream,
        halt: &std::sync::atomic::AtomicBool,
        seq: usize,
        requests: &std::sync::atomic::AtomicUsize,
    ) {
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(20)))
            .unwrap();
        let Ok(mut ws) = tungstenite::accept(stream) else {
            return;
        };
        if matches!(self, Self::DropAtOnce) {
            return;
        }
        if matches!(self, Self::DropOnce) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(150);
            while std::time::Instant::now() < deadline {
                match ws.read() {
                    Ok(tungstenite::Message::Text(text)) => {
                        if text.starts_with("[\"REQ\"") {
                            requests.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(ref e))
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(_) => return,
                }
            }
            // The first session drops to force a reconnect; later ones stay
            // up so the client's resubscription can be observed.
            if seq == 0 {
                return;
            }
            while !halt.load(std::sync::atomic::Ordering::Relaxed) {
                match ws.read() {
                    Ok(tungstenite::Message::Text(text)) => {
                        if text.starts_with("[\"REQ\"") {
                            requests.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    Ok(tungstenite::Message::Close(_)) => return,
                    Ok(_) => {}
                    Err(tungstenite::Error::Io(ref e))
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut => {}
                    Err(_) => return,
                }
            }
            return;
        }
        // Never read the socket: `tungstenite` answers a ping during `read`,
        // which would make this peer look alive.
        if matches!(self, Self::Mute) {
            while !halt.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            return;
        }
        while !halt.load(std::sync::atomic::Ordering::Relaxed) {
            match ws.read() {
                Ok(tungstenite::Message::Close(_)) => return,
                Ok(_) => {}
                Err(tungstenite::Error::Io(ref e))
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => return,
            }
        }
    }
}

// A relay that stops reading is not the same as a relay that disconnects.
// Its receive window fills, then ours, and a blocking write never returns.
// That stalls the driver thread, which also serves reads, so the whole
// connection freezes with no error anywhere and no reconnect ever starts.
//
// The driver must instead bound the write, end the connection, and try
// again. Note that the retry budget does not run out here: every attempt
// connects successfully before stalling, so the schedule counts no
// consecutive failures. The write timeout is what bounds the damage.
#[test]
fn a_relay_that_stops_reading_does_not_freeze_the_driver() {
    let relay = ScriptedRelay::start(Script::Mute);
    let url = relay.url();

    Deadline::enforce(
        std::time::Duration::from_secs(20),
        "a driver writing to a relay that never reads",
        move || {
            let mut ports = nostro2_relay::RelayDriver::spawn(
                nostro2_relay::DriverConfig::new(nostro2_relay::RelayUrl::parse(&url).unwrap())
                    .with_reconnect(nostro2_relay::ReconnectConfig::fixed(
                        std::time::Duration::from_millis(10),
                    ))
                    .with_write_timeout(std::time::Duration::from_millis(300)),
                nostro2_relay::RelayTls::new().unwrap(),
            );
            assert!(ports.handshake.pop_block().unwrap().is_ok());

            let bulk = "x".repeat(64 * 1024);
            for _ in 0..256 {
                if ports.outbound.push(bulk.clone()).is_err() {
                    break;
                }
            }

            // Without the write timeout the driver blocks inside write_all
            // and no disconnect is ever announced, so this loop hangs.
            loop {
                match ports.inbound.pop_block() {
                    Some(nostro2_relay::DriverEvent::Disconnected(reason)) => {
                        assert!(
                            reason.is_some_and(|r| r.contains("stopped reading")),
                            "the disconnect must name the stalled write"
                        );
                        return;
                    }
                    Some(_) => {}
                    None => panic!("the driver ended without reporting the stall"),
                }
            }
        },
    );
}

// The quietest failure of all. A subscription lives on the relay, not on
// the client, so a reconnect starts a connection with no subscriptions on
// it. A service that subscribed once at startup then receives nothing ever
// again, while `recv` swallows the reconnect events and reports no error.
//
// This test states the guarantee a long-lived service needs: after a
// reconnect, the subscriptions it asked for are in force again.
#[test]
fn a_reconnect_restores_the_subscriptions() {
    let relay = ScriptedRelay::start(Script::DropOnce);
    let url = relay.url();
    let seen = relay.requests();

    Deadline::enforce(
        std::time::Duration::from_secs(20),
        "a relay resubscribing after a reconnect",
        move || {
            let mut relay = nostro2_relay::NostrRelay::with_driver_config(
                nostro2_relay::DriverConfig::new(nostro2_relay::RelayUrl::parse(&url).unwrap())
                    .with_reconnect(nostro2_relay::ReconnectConfig::fixed(
                        std::time::Duration::from_millis(10),
                    )),
            )
            .unwrap();

            let filter = nostro2::NostrSubscription {
                kinds: Some(std::collections::HashSet::from([1])),
                ..Default::default()
            };
            relay.send(filter).unwrap();

            // Wait for the drop and the reconnect that follows it.
            let mut reconnects = 0;
            while reconnects < 2 {
                match relay.recv_event_blocking() {
                    Some(nostro2_relay::DriverEvent::Connected) => reconnects += 1,
                    Some(_) => {}
                    None => panic!("the driver stopped before reconnecting"),
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(300));

            assert!(
                seen.load(std::sync::atomic::Ordering::Relaxed) >= 2,
                "the relay must receive the subscription again after a reconnect, \
                 or the service goes silent forever"
            );
        },
    );
}

// A pool sends through its relays, so each driver records its own session.
// This states that the pool inherits the guarantee rather than needing a
// second mechanism of its own.
#[test]
fn a_pool_also_restores_its_subscriptions() {
    let relay = ScriptedRelay::start(Script::DropOnce);
    let url = relay.url();
    let seen = relay.requests();

    Deadline::enforce(
        std::time::Duration::from_secs(20),
        "a pool resubscribing after a reconnect",
        move || {
            let pool = nostro2_relay::NostrPool::with_config(
                &[&url],
                64,
                &nostro2_relay::ReconnectConfig::fixed(std::time::Duration::from_millis(10)),
            );
            let filter = nostro2::NostrSubscription {
                kinds: Some(std::collections::HashSet::from([1])),
                ..Default::default()
            };
            pool.send(filter).unwrap();

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if seen.load(std::sync::atomic::Ordering::Relaxed) >= 2 {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            panic!("the pool never restated its subscription after the reconnect");
        },
    );
}

/// Counts the file descriptors this process holds.
///
/// A reconnecting driver that leaks one socket per attempt exhausts the
/// process's descriptor budget within hours, which is the original bug this
/// crate was built to fix.
struct Descriptors;

impl Descriptors {
    fn count() -> usize {
        std::fs::read_dir("/proc/self/fd")
            .map(|entries| entries.count())
            .unwrap_or_default()
    }
}

// A service that gives up on a relay must not wait forever for the next
// event. When the retry budget is spent the driver stops, and every reader
// blocked on that relay has to be released.
#[test]
fn an_exhausted_driver_releases_a_blocking_reader() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    Deadline::enforce(
        std::time::Duration::from_secs(10),
        "a reader blocked on an exhausted relay",
        move || {
            let mut relay = nostro2_relay::NostrRelay::detached(
                &format!("ws://127.0.0.1:{port}"),
                nostro2_relay::ReconnectConfig {
                    max_retries: 2,
                    initial_delay: std::time::Duration::from_millis(10),
                    max_delay: std::time::Duration::from_millis(10),
                    backoff_multiplier: 1.0,
                },
            )
            .unwrap();
            assert!(
                relay.recv_blocking().is_none(),
                "an exhausted relay must end its stream, not stall"
            );
        },
    );
}

// The async twin of the guarantee above: an awaiting task must be woken
// when the driver gives up, not left pending forever.
#[test]
fn an_exhausted_driver_releases_an_awaiting_reader() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    Deadline::enforce(
        std::time::Duration::from_secs(10),
        "a task awaiting an exhausted relay",
        move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let mut relay = nostro2_relay::NostrRelay::detached(
                    &format!("ws://127.0.0.1:{port}"),
                    nostro2_relay::ReconnectConfig {
                        max_retries: 2,
                        initial_delay: std::time::Duration::from_millis(10),
                        max_delay: std::time::Duration::from_millis(10),
                        backoff_multiplier: 1.0,
                    },
                )
                .unwrap();
                assert!(
                    relay.recv().await.is_none(),
                    "an exhausted relay must end its stream, not stall"
                );
            });
        },
    );
}

// Closing a relay from another thread must release a reader that is already
// parked, which is how a service shuts down cleanly.
#[test]
fn closing_a_relay_releases_a_parked_reader() {
    let server = ScriptedRelay::start(Script::Silent);
    let relay = nostro2_relay::NostrRelay::detached(
        &server.url(),
        nostro2_relay::ReconnectConfig::disabled(),
    )
    .unwrap();
    let mut reader = relay.clone();

    let closer = relay.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        closer.close();
    });

    Deadline::enforce(
        std::time::Duration::from_secs(10),
        "a parked reader released by close",
        move || {
            assert!(reader.recv_blocking().is_none());
        },
    );
}

// The async twin: a task awaiting a relay that is closed elsewhere must be
// woken rather than left pending.
#[test]
fn closing_a_relay_releases_an_awaiting_reader() {
    let server = ScriptedRelay::start(Script::Silent);
    let url = server.url();

    Deadline::enforce(
        std::time::Duration::from_secs(10),
        "a task released by close",
        move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let relay = nostro2_relay::NostrRelay::detached(
                    &url,
                    nostro2_relay::ReconnectConfig::disabled(),
                )
                .unwrap();
                let mut reader = relay.clone();
                let closer = relay.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    closer.close();
                });
                assert!(reader.recv().await.is_none());
            });
        },
    );
}

// A pool reader must be released the same way, or a service shutting down
// hangs on its last await.
#[test]
fn closing_a_pool_releases_a_parked_reader() {
    let server = ScriptedRelay::start(Script::Silent);
    let pool = nostro2_relay::NostrPool::with_config(
        &[&server.url()],
        16,
        &nostro2_relay::ReconnectConfig::disabled(),
    );
    let mut reader = pool.clone();

    let closer = pool.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        closer.close();
    });

    Deadline::enforce(
        std::time::Duration::from_secs(10),
        "a parked pool reader released by close",
        move || {
            assert!(reader.recv_blocking().is_none());
        },
    );
}

// Reconnecting must not leak a descriptor per attempt. This is the failure
// that motivated the rewrite: a service reconnecting every few seconds hit
// the process limit and died after hours.
#[test]
fn many_reconnects_do_not_leak_descriptors() {
    let server = ScriptedRelay::start(Script::DropAtOnce);
    let relay = nostro2_relay::NostrRelay::detached(
        &server.url(),
        nostro2_relay::ReconnectConfig::fixed(std::time::Duration::from_millis(5)),
    )
    .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(300));
    let settled = Descriptors::count();
    std::thread::sleep(std::time::Duration::from_secs(2));
    let later = Descriptors::count();

    let attempts = server.accepts();
    relay.close();

    assert!(
        attempts > 10,
        "the relay reconnected {attempts} times, too few to prove anything"
    );
    assert!(
        later <= settled + 4,
        "descriptors grew from {settled} to {later} over {attempts} reconnects"
    );
}

// A driver that reconnects forever must not accumulate threads either.
#[test]
fn many_reconnects_do_not_leak_threads() {
    let server = ScriptedRelay::start(Script::DropAtOnce);
    let relay = nostro2_relay::NostrRelay::detached(
        &server.url(),
        nostro2_relay::ReconnectConfig::fixed(std::time::Duration::from_millis(5)),
    )
    .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(300));
    let settled = Threads::count();
    std::thread::sleep(std::time::Duration::from_secs(2));
    let later = Threads::count();

    let attempts = server.accepts();
    relay.close();

    assert!(attempts > 10, "only {attempts} reconnects happened");
    assert!(
        later <= settled + 2,
        "threads grew from {settled} to {later} over {attempts} reconnects"
    );
}

// The worst failure for a long-lived service: the peer stops answering
// without closing the socket. A NAT drops the mapping, a laptop suspends,
// a middlebox times the flow out. TCP alone never reports this, so a reader
// waits for events that can never arrive, and reconnection never triggers
// because the driver believes it is still connected.
//
// The connection must therefore be probed. This test holds the socket open
// but answers nothing, and requires the driver to notice within a bounded
// time and report the connection as gone.
#[test]
fn a_silent_peer_is_detected_rather_than_waited_on_forever() {
    let server = ScriptedRelay::start(Script::Mute);
    let config = nostro2_relay::DriverConfig::new(
        nostro2_relay::RelayUrl::parse(&server.url()).unwrap(),
    )
    .with_reconnect(nostro2_relay::ReconnectConfig::disabled())
    .with_heartbeat(nostro2_relay::HeartbeatConfig {
        idle_timeout: std::time::Duration::from_millis(200),
        reply_timeout: std::time::Duration::from_millis(200),
    });
    let mut relay = nostro2_relay::NostrRelay::with_driver_config(config).unwrap();

    Deadline::enforce(
        std::time::Duration::from_secs(30),
        "a driver noticing a silent peer",
        move || {
            assert!(
                relay.recv_blocking().is_none(),
                "a silent connection must end, not stall forever"
            );
        },
    );
}

// A half-open connection must also be reconnected through, not merely
// noticed: a service that loses its relay to a NAT timeout has to come
// back by itself.
#[test]
fn a_silent_peer_triggers_a_reconnect() {
    let server = ScriptedRelay::start(Script::Mute);
    let config = nostro2_relay::DriverConfig::new(
        nostro2_relay::RelayUrl::parse(&server.url()).unwrap(),
    )
    .with_reconnect(nostro2_relay::ReconnectConfig::fixed(
        std::time::Duration::from_millis(20),
    ))
    .with_heartbeat(nostro2_relay::HeartbeatConfig {
        idle_timeout: std::time::Duration::from_millis(100),
        reply_timeout: std::time::Duration::from_millis(100),
    });
    let relay = nostro2_relay::NostrRelay::with_driver_config(config).unwrap();

    std::thread::sleep(std::time::Duration::from_secs(2));
    let attempts = server.accepts();
    relay.close();

    assert!(
        attempts > 2,
        "a half-open connection produced only {attempts} attempts, so the \
         driver stalled instead of reconnecting"
    );
}

// Every exit from a driver must release its readers, including the one
// nobody plans for. A panic on the IO thread must not leave a service
// waiting forever on a stream that can never produce another event.
#[test]
fn a_panicking_driver_releases_its_readers() {
    let (tx, rx) = quetzalcoatl::spmc::RingBuffer::<u8>::new(
        quetzalcoatl::capacity::Capacity::at_least(8),
    )
    .split();

    let worker = std::thread::spawn(move || {
        let _tx = tx;
        panic!("the driver died");
    });
    let _ = worker.join();

    Deadline::enforce(
        std::time::Duration::from_secs(10),
        "a reader after its producer panicked",
        move || {
            assert!(
                rx.pop_block().is_none(),
                "a panicked producer must end the stream"
            );
        },
    );
}

/// Counts the threads this process runs.
struct Threads;

impl Threads {
    fn count() -> usize {
        std::fs::read_dir("/proc/self/task")
            .map(|entries| entries.count())
            .unwrap_or_default()
    }
}
