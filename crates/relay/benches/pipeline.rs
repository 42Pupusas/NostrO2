//! End-to-end comparison of this crate's runtime-free pipeline against
//! `nostr-sdk`, the reference tokio implementation.
//!
//! Both arms do the same work, against the same server, over a real
//! loopback socket, on the same pre-signed bytes:
//!
//! - open a WebSocket connection;
//! - send one REQ frame;
//! - receive `COUNT` EVENT frames;
//! - parse each frame and verify its Schnorr signature.
//!
//! Signature checking happens on both sides: this crate checks in
//! `NoteVerifier` on the driver thread, `nostr-sdk` checks in
//! `SharedState::verify_and_cache` on its inbound path.
//!
//! # Reading the numbers
//!
//! The two crates use different curve backends: `nostro2` verifies with
//! pure-Rust `k256`, `nostr-sdk` with the `secp256k1` C library. On this
//! fixture that alone is a ~11ms difference over 500 notes, which is
//! larger than the whole transport cost. The `verification` benchmark
//! below measures it directly, and the `transport_only` arm removes it,
//! so the three numbers separate crypto from pipeline.
//!
//! `nostr-sdk` also keeps a verification LRU, an event database, and an
//! admission policy on this path, so it does strictly more per event than
//! a transport comparison needs.
//!
//! Each arm owns its own relay and driver, and stops both before it
//! returns. A leaked driver keeps its socket and its loop alive and
//! competes with whatever runs next, which produced 20x swings between
//! runs before the harness took care to shut down.

fn main() {
    divan::main();
}

/// How many events one iteration carries.
const COUNT: usize = 500;

/// A blocking loopback relay that replays a fixed set of pre-signed notes.
///
/// It waits for a REQ, reads the subscription id out of it, then writes
/// every fixture note under that id and an EOSE. The id must be echoed:
/// `nostr-sdk` rejects an event whose subscription id it did not issue,
/// and generates a random one per subscription.
struct FixtureRelay {
    port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl FixtureRelay {
    fn start(notes: std::sync::Arc<Vec<String>>) -> Self {
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
                        stream.set_nodelay(true).unwrap();
                        let notes = notes.clone();
                        let halt = halt.clone();
                        sessions.push(std::thread::spawn(move || {
                            Self::serve(stream, &notes, &halt);
                        }));
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(1)),
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

    fn serve(stream: std::net::TcpStream, notes: &[String], halt: &std::sync::atomic::AtomicBool) {
        stream
            .set_read_timeout(Some(std::time::Duration::from_millis(20)))
            .unwrap();
        let Ok(mut ws) = tungstenite::accept(stream) else {
            return;
        };
        while !halt.load(std::sync::atomic::Ordering::Relaxed) {
            match ws.read() {
                Ok(tungstenite::Message::Text(text)) => {
                    let Some(id) = Self::subscription_id(&text) else {
                        continue;
                    };
                    for note in notes {
                        let frame = format!("[\"EVENT\",\"{id}\",{note}]");
                        if ws.send(tungstenite::Message::Text(frame.into())).is_err() {
                            return;
                        }
                    }
                    let _ = ws.send(tungstenite::Message::Text(
                        format!("[\"EOSE\",\"{id}\"]").into(),
                    ));
                    let _ = ws.flush();
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

    /// Reads the subscription id out of a `["REQ","<id>",{...}]` frame.
    fn subscription_id(frame: &str) -> Option<String> {
        let rest = frame.strip_prefix("[\"REQ\",\"")?;
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    }

    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.port)
    }
}

impl Drop for FixtureRelay {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The pre-signed notes both arms consume.
///
/// Signing is expensive and unrelated to transport, so it happens once
/// here rather than inside a measured iteration.
struct Fixture;

impl Fixture {
    fn notes(count: usize) -> std::sync::Arc<Vec<String>> {
        use nostro2::{NostrKeypair as _, NostrSigner as _};
        let keypair = nostro2_signer::NostrKeypair::generate();
        let notes = (0..count)
            .map(|i| {
                let mut note = nostro2::NostrNote {
                    kind: 1,
                    content: format!("benchmark note {i}"),
                    pubkey: keypair.public_key(),
                    created_at: 1_700_000_000 + i64::try_from(i).unwrap(),
                    ..Default::default()
                };
                note.sign_with(&keypair).unwrap();
                Self::encode(&note)
            })
            .collect();
        std::sync::Arc::new(notes)
    }

    #[cfg(feature = "bourne")]
    fn encode(note: &nostro2::NostrNote) -> String {
        json_bourne::to_string(note).unwrap()
    }

    #[cfg(feature = "serde")]
    fn encode(note: &nostro2::NostrNote) -> String {
        serde_json::to_string(note).unwrap()
    }

    #[cfg(feature = "bourne")]
    fn encode_client(event: &nostro2::NostrClientEvent) -> String {
        json_bourne::to_string(event).unwrap()
    }

    #[cfg(feature = "serde")]
    fn encode_client(event: &nostro2::NostrClientEvent) -> String {
        serde_json::to_string(event).unwrap()
    }

    #[cfg(feature = "bourne")]
    fn decode(frame: &str) -> nostro2::NostrNote {
        json_bourne::from_str(frame).unwrap()
    }

    #[cfg(feature = "serde")]
    fn decode(frame: &str) -> nostro2::NostrNote {
        serde_json::from_str(frame).unwrap()
    }

    /// The REQ frame this crate's arm sends.
    fn request_frame() -> String {
        Self::encode_client(&nostro2::NostrClientEvent::Subscribe(
            nostro2::RelayEventTag::Req,
            "bench".to_string(),
            nostro2::NostrSubscription {
                kinds: Some([1].into_iter().collect()),
                ..Default::default()
            },
        ))
    }
}

/// One run of this crate's pipeline, from spawn to shutdown.
///
/// Owning the driver and dropping it here is what keeps the arms from
/// interfering: the guard stops the thread and closes the socket.
struct RuntimeFreeRun;

impl RuntimeFreeRun {
    fn drain(url: &str, verify: nostro2_relay::VerifyPolicy) -> usize {
        let parsed = nostro2_relay::RelayUrl::parse(url).unwrap();
        let config = nostro2_relay::DriverConfig::new(parsed).with_verify(verify);
        let ports =
            nostro2_relay::RelayDriver::spawn(config, nostro2_relay::RelayTls::new().unwrap());
        ports.outbound.push(Fixture::request_frame()).unwrap();

        let nostro2_relay::DriverPorts { inbound, guard, .. } = ports;
        let mut seen = 0_usize;
        while seen < COUNT {
            match inbound.pop_block() {
                Some(nostro2_relay::DriverEvent::Message(event)) => {
                    if matches!(*event, nostro2::NostrRelayEvent::NewNote(..)) {
                        seen += 1;
                    }
                }
                Some(_) => {}
                None => break,
            }
        }
        drop(guard);
        assert_eq!(seen, COUNT, "the run lost events");
        seen
    }
}

/// One run of `nostr-sdk`'s pipeline, from connect to disconnect.
struct TokioRun;

impl TokioRun {
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap()
    }

    fn drain(runtime: &tokio::runtime::Runtime, url: &str) -> usize {
        runtime.block_on(async {
            use nostr_sdk::prelude::*;

            let parsed = RelayUrl::parse(url).unwrap();
            let client = nostr_sdk::relay::Relay::new(parsed);
            client.try_connect().await.unwrap();

            let filter = Filter::new().kind(Kind::TextNote);
            let mut stream = client.stream_events(filter).await.unwrap();

            let mut seen = 0_usize;
            while seen < COUNT {
                match futures_util::StreamExt::next(&mut stream).await {
                    Some(_) => seen += 1,
                    None => break,
                }
            }
            client.disconnect();
            assert_eq!(seen, COUNT, "the run lost events");
            seen
        })
    }
}

/// The full pipeline: socket, parse, and signature check.
#[divan::bench(name = "full: nostro2-relay (thread + rings)", sample_count = 20)]
fn full_nostro2(bencher: divan::Bencher) {
    let relay = FixtureRelay::start(Fixture::notes(COUNT));
    let url = relay.url();
    bencher.bench_local(|| {
        divan::black_box(RuntimeFreeRun::drain(
            &url,
            nostro2_relay::VerifyPolicy::Reject,
        ))
    });
}

/// The full pipeline through `nostr-sdk`.
#[divan::bench(name = "full: nostr-sdk (tokio tasks)", sample_count = 20)]
fn full_nostr_sdk(bencher: divan::Bencher) {
    let relay = FixtureRelay::start(Fixture::notes(COUNT));
    let url = relay.url();
    let runtime = TokioRun::runtime();
    bencher.bench_local(|| divan::black_box(TokioRun::drain(&runtime, &url)));
}

/// Socket and parse with the signature check off.
///
/// The curve backends differ, and on this fixture the difference is larger
/// than the transport cost. This arm isolates the pipeline from it.
#[divan::bench(name = "transport only: nostro2-relay (no verify)", sample_count = 20)]
fn transport_nostro2(bencher: divan::Bencher) {
    let relay = FixtureRelay::start(Fixture::notes(COUNT));
    let url = relay.url();
    bencher.bench_local(|| {
        divan::black_box(RuntimeFreeRun::drain(
            &url,
            nostro2_relay::VerifyPolicy::Trust,
        ))
    });
}

/// The signature check alone, off the socket, on the same notes.
///
/// This is the crypto term of the full runs above.
#[divan::bench(name = "verification: nostro2 (k256)", sample_count = 20)]
fn verify_nostro2(bencher: divan::Bencher) {
    let notes: Vec<nostro2::NostrNote> = Fixture::notes(COUNT)
        .iter()
        .map(|frame| Fixture::decode(frame))
        .collect();
    bencher.bench_local(|| {
        for note in &notes {
            assert!(nostro2::NostrEvent::verify(divan::black_box(note)));
        }
    });
}

/// The same check through `nostr-sdk`'s curve backend.
#[divan::bench(name = "verification: nostr-sdk (secp256k1)", sample_count = 20)]
fn verify_nostr_sdk(bencher: divan::Bencher) {
    use nostr_sdk::prelude::*;
    let notes: Vec<Event> = Fixture::notes(COUNT)
        .iter()
        .map(|frame| Event::from_json(frame).unwrap())
        .collect();
    bencher.bench_local(|| {
        for note in &notes {
            divan::black_box(note).verify().unwrap();
        }
    });
}
