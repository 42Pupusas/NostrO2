//! The crate is usable with no executor at all.
//!
//! "Depends on no async runtime" is a weaker claim than it sounds: a crate
//! whose only constructor is `async` still forces the caller to find an
//! executor to poll it, even though nothing about connecting is
//! asynchronous. This file is written from the point of view of a consumer
//! that owns threads and refuses to add one, and it never writes `.await`,
//! never names a runtime, and never spawns a task.
//!
//! Every test here must therefore compile and pass without `tokio` doing
//! any work. If a future is ever required on a path a synchronous service
//! needs, one of these stops compiling.

/// Fails the test when `body` outlives `limit`.
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

/// A minimal relay that completes the upgrade and then answers.
struct SyncRelay {
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl SyncRelay {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let halt = stop.clone();

        let handle = std::thread::spawn(move || {
            let mut sessions = Vec::new();
            while !halt.load(std::sync::atomic::Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(std::time::Duration::from_millis(20)))
                            .unwrap();
                        let halt = halt.clone();
                        sessions.push(std::thread::spawn(move || Self::serve(stream, &halt)));
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
            stop,
            handle: Some(handle),
        }
    }

    fn serve(stream: std::net::TcpStream, halt: &std::sync::atomic::AtomicBool) {
        let Ok(mut ws) = tungstenite::accept(stream) else {
            return;
        };
        while !halt.load(std::sync::atomic::Ordering::Relaxed) {
            match ws.read() {
                Ok(tungstenite::Message::Text(text)) if text.starts_with("[\"REQ\"") => {
                    let _ = ws.send(tungstenite::Message::Text(
                        r#"["EOSE","sub"]"#.to_string().into(),
                    ));
                }
                Ok(tungstenite::Message::Close(_)) => return,
                Ok(_) => {}
                Err(tungstenite::Error::Io(ref e))
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => return,
            }
        }
    }

    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.port)
    }
}

impl Drop for SyncRelay {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// A subscription for kind-1 notes.
struct Filter;

impl Filter {
    fn kind_one() -> nostro2::NostrSubscription {
        nostro2::NostrSubscription {
            kinds: Some(std::collections::HashSet::from([1])),
            ..Default::default()
        }
    }
}

// Connecting is not an asynchronous operation: it dials a socket on another
// thread and waits for the outcome. A synchronous service must be able to
// wait for that outcome without an executor.
#[test]
fn a_relay_connects_without_an_executor() {
    let relay = SyncRelay::start();
    let url = relay.url();

    Deadline::enforce(
        std::time::Duration::from_secs(20),
        "a blocking connect",
        move || {
            let mut client = nostro2_relay::NostrRelay::connect_blocking(&url)
                .expect("a blocking connect must reach a live relay");

            client.send(Filter::kind_one()).unwrap();

            let event = client
                .recv_blocking()
                .expect("a subscribed relay must answer");
            assert!(matches!(
                event,
                nostro2::NostrRelayEvent::EndOfSubscription(..)
            ));
        },
    );
}

// A failed connection must report the failure through the same synchronous
// path, rather than forcing the caller into a future to learn about it.
#[test]
fn a_refused_connection_reports_itself_without_an_executor() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);

    Deadline::enforce(
        std::time::Duration::from_secs(20),
        "a blocking connect to a dead port",
        move || {
            let outcome = nostro2_relay::NostrRelay::connect_blocking_with(
                &format!("ws://127.0.0.1:{port}"),
                nostro2_relay::ReconnectConfig::disabled(),
            );
            assert!(
                matches!(
                    outcome,
                    Err(nostro2_relay::errors::NostrRelayError::Connect(_))
                ),
                "a dead port must surface as a connect error"
            );
        },
    );
}

// The reconnect policy must be reachable from the synchronous path too, or
// a service that wants both is pushed back into async for no reason.
#[test]
fn a_reconnect_policy_is_available_without_an_executor() {
    let relay = SyncRelay::start();
    let url = relay.url();

    Deadline::enforce(
        std::time::Duration::from_secs(20),
        "a blocking connect with a policy",
        move || {
            let client = nostro2_relay::NostrRelay::connect_blocking_with(
                &url,
                nostro2_relay::ReconnectConfig::fixed(std::time::Duration::from_millis(50)),
            )
            .expect("a blocking connect must accept a policy");
            assert!(!client.is_closed());
        },
    );
}

// A pool is the shape most services actually use, so it needs the same
// treatment: construction, sending, and reading with no executor.
#[test]
fn a_pool_is_usable_without_an_executor() {
    let relay = SyncRelay::start();
    let url = relay.url();

    Deadline::enforce(
        std::time::Duration::from_secs(20),
        "a blocking pool",
        move || {
            let mut pool = nostro2_relay::NostrPool::new(&[&url]);
            pool.send(Filter::kind_one()).unwrap();
            assert!(
                pool.recv_blocking().is_some(),
                "a pool must deliver events to a blocking reader"
            );
        },
    );
}

// `recv_blocking` hides the connection lifecycle, so a synchronous service
// that must react to a reconnect needs the event-level twin on the pool as
// well as on a single relay.
#[test]
fn a_pool_reports_its_lifecycle_without_an_executor() {
    let relay = SyncRelay::start();
    let url = relay.url();

    Deadline::enforce(
        std::time::Duration::from_secs(20),
        "a blocking pool lifecycle read",
        move || {
            let mut pool = nostro2_relay::NostrPool::new(&[&url]);
            pool.send(Filter::kind_one()).unwrap();

            let mut saw_message = false;
            for _ in 0..8 {
                match pool.recv_event_blocking() {
                    Some(nostro2_relay::PoolEvent::Message(from, _)) => {
                        assert_eq!(
                            from,
                            nostro2_relay::RelayUrl::parse(&url).unwrap(),
                            "a pooled message must name the relay that served it"
                        );
                        saw_message = true;
                        break;
                    }
                    Some(_) => {}
                    None => panic!("the pool ended before delivering anything"),
                }
            }
            assert!(saw_message, "a pool must surface its messages");
        },
    );
}

// Batch sending must not require a stream. A synchronous service has
// iterators, and sending never blocks anyway.
#[test]
fn many_messages_are_sent_without_an_executor() {
    let relay = SyncRelay::start();
    let url = relay.url();

    Deadline::enforce(
        std::time::Duration::from_secs(20),
        "a blocking batch send",
        move || {
            let mut client = nostro2_relay::NostrRelay::connect_blocking(&url).unwrap();
            client
                .send_all_blocking([Filter::kind_one(), Filter::kind_one()])
                .expect("an iterator of messages must be sendable");
            assert!(client.recv_blocking().is_some());
        },
    );
}

// The whole point of the exercise: a service can hold a pool open, react to
// its lifecycle, and shut down cleanly using only threads. This mirrors the
// shape of a real daemon rather than testing one method.
#[test]
fn a_service_runs_a_full_session_without_an_executor() {
    let relay = SyncRelay::start();
    let url = relay.url();

    Deadline::enforce(
        std::time::Duration::from_secs(20),
        "a whole blocking service session",
        move || {
            let pool = nostro2_relay::NostrPool::new(&[&url]);
            pool.send(Filter::kind_one()).unwrap();

            let worker = {
                let mut reader = pool.clone();
                std::thread::spawn(move || {
                    let mut seen = 0_usize;
                    while let Some(event) = reader.recv_event_blocking() {
                        if matches!(event, nostro2_relay::PoolEvent::Message(..)) {
                            seen += 1;
                        }
                    }
                    seen
                })
            };

            std::thread::sleep(std::time::Duration::from_millis(200));
            pool.close();

            let seen = worker.join().expect("the reader thread panicked");
            assert!(seen > 0, "the service must have received something");
        },
    );
}

// A blocking reader must be released when the connection is closed, or a
// synchronous service cannot shut down.
#[test]
fn a_blocking_reader_is_released_on_close() {
    let relay = SyncRelay::start();
    let url = relay.url();

    Deadline::enforce(
        std::time::Duration::from_secs(20),
        "a blocking reader released by close",
        move || {
            let mut client = nostro2_relay::NostrRelay::connect_blocking(&url).unwrap();
            let closer = client.clone();
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(100));
                closer.close();
            });
            assert!(
                client.recv_blocking().is_none(),
                "close must release a parked reader"
            );
        },
    );
}
