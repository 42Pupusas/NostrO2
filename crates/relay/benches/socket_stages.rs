//! Per-stage timing inside the real driver path, measured rather than
//! inferred.
//!
//! The `pipeline` bench says the driver spends ~3.66ms of transport per
//! 500 messages with verification off. `transport_breakdown` accounts for
//! only ~0.5ms of that (parse 0.38ms, ring 0.13ms), which leaves ~3.1ms
//! unexplained. Guessing at the remainder is how the last three
//! hypotheses died, so this walks the real [`WsSocket`] against a real
//! relay and times each stage with the clock.
//!
//! [`WsSocket`]: nostro2_relay::WsSocket

fn main() {
    divan::main();
}

/// How many messages one run carries.
const COUNT: usize = 500;

/// A loopback relay that replays pre-signed notes on demand.
///
/// Deliberately the same shape as the one in `pipeline`, so the numbers
/// here describe that benchmark's conditions and not a different setup.
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

/// The pre-signed notes the relay replays.
struct Fixture;

impl Fixture {
    fn notes() -> std::sync::Arc<Vec<String>> {
        use nostro2::{NostrKeypair as _, NostrSigner as _};
        let keypair = nostro2_signer::NostrKeypair::generate();
        let notes = (0..COUNT)
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

/// Reads the socket the way the driver does, counting syscalls and the
/// bytes each one returns.
///
/// `poll` costs ~4.5us per message while the decode measures ~0.007us,
/// so the time is in the `read` syscall. The open question is whether a
/// read returns one frame or many: if the relay's burst arrives
/// coalesced, one syscall should serve many messages and a per-message
/// syscall cost should not exist.
struct RawReads {
    reads: usize,
    bytes: usize,
    elapsed: std::time::Duration,
}

impl RawReads {
    /// Upgrades by hand, sends the REQ, then reads raw until `expect`
    /// bytes have arrived.
    fn measure(url: &str, expect: usize) -> Self {
        let port = url.rsplit(':').next().unwrap().to_owned();
        let mut stream = std::net::TcpStream::connect(format!("127.0.0.1:{port}")).unwrap();
        stream.set_nodelay(true).unwrap();

        let key = coyoquil::WsKey::new();
        let request = key
            .upgrade_request(&format!("127.0.0.1:{port}"), "/")
            .unwrap();
        std::io::Write::write_all(&mut stream, request.as_bytes()).unwrap();
        std::io::Write::flush(&mut stream).unwrap();

        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            std::io::Read::read(&mut stream, &mut byte).unwrap();
            head.push(byte[0]);
        }

        let mut encoded = Vec::new();
        coyoquil::Frame::Text(&Fixture::request_frame())
            .encode_masked(coyoquil::MaskKey::new(), &mut encoded);
        std::io::Write::write_all(&mut stream, &encoded).unwrap();
        std::io::Write::flush(&mut stream).unwrap();

        let mut buf = vec![0_u8; 64 * 1024];
        let mut reads = 0_usize;
        let mut bytes = 0_usize;
        let started = std::time::Instant::now();
        while bytes < expect {
            match std::io::Read::read(&mut stream, &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    reads += 1;
                    bytes += n;
                }
                Err(_) => break,
            }
        }
        Self {
            reads,
            bytes,
            elapsed: started.elapsed(),
        }
    }

    fn report(&self) {
        println!("    reads         {}", self.reads);
        println!("    bytes         {}", self.bytes);
        println!("    bytes/read    {}", self.bytes / self.reads.max(1));
        println!("    elapsed       {:>9.3?}", self.elapsed);
    }
}

/// Wall-clock time attributed to each stage of one drain.
#[derive(Default)]
struct Stages {
    poll: std::time::Duration,
    parse: std::time::Duration,
    messages: usize,
    polls: usize,
    empty_polls: usize,
}

impl Stages {
    /// Drives the real socket to `COUNT` messages, timing each stage.
    ///
    /// `poll` covers the syscall and the frame decode; `parse` covers the
    /// JSON. An empty poll is a `read_timeout` that elapsed with no
    /// complete message, which costs a syscall and yields nothing.
    fn measure(url: &str) -> Self {
        let mut socket = nostro2_relay::WsSocket::connect(
            &nostro2_relay::RelayUrl::parse(url).unwrap(),
            None,
            std::time::Duration::from_secs(5),
            std::time::Duration::from_millis(5),
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        socket.send_text(&Fixture::request_frame()).unwrap();

        let mut stages = Self::default();
        while stages.messages < COUNT {
            let started = std::time::Instant::now();
            let polled = socket.poll();
            stages.poll += started.elapsed();
            stages.polls += 1;

            match polled {
                Ok(Some(nostro2_relay::WsMessage::Text(text))) => {
                    let started = std::time::Instant::now();
                    let parsed = text.parse::<nostro2::NostrRelayEvent>();
                    stages.parse += started.elapsed();
                    if matches!(parsed, Ok(nostro2::NostrRelayEvent::NewNote(..))) {
                        stages.messages += 1;
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => stages.empty_polls += 1,
                Err(_) => break,
            }
        }
        stages
    }

    fn report(&self) {
        let total = self.poll + self.parse;
        println!("    messages      {}", self.messages);
        println!(
            "    polls         {} ({} empty)",
            self.polls, self.empty_polls
        );
        println!(
            "    poll (io+decode) {:>9.3?}  {:>5.1}%",
            self.poll,
            Self::share(self.poll, total)
        );
        println!(
            "    parse (json)     {:>9.3?}  {:>5.1}%",
            self.parse,
            Self::share(self.parse, total)
        );
        println!("    accounted        {total:>9.3?}");
        println!(
            "    per message      {:>9.3?}",
            total / u32::try_from(self.messages.max(1)).unwrap()
        );
    }

    fn share(part: std::time::Duration, total: std::time::Duration) -> f64 {
        if total.is_zero() {
            return 0.0;
        }
        part.as_secs_f64() / total.as_secs_f64() * 100.0
    }
}

/// Times the real socket path and prints the split.
///
/// Reported through divan so it runs with the other benches, but the
/// per-stage numbers land on stdout: the point is the breakdown, not the
/// single total.
#[divan::bench(name = "socket: per-stage breakdown", sample_count = 5)]
fn socket_stages(bencher: divan::Bencher) {
    let relay = FixtureRelay::start(Fixture::notes());
    let url = relay.url();

    let stages = Stages::measure(&url);
    println!("\n  --- one instrumented drain of {COUNT} messages ---");
    stages.report();

    let expect: usize = Fixture::notes().iter().map(String::len).sum();
    let raw = RawReads::measure(&url, expect);
    println!("\n  --- raw socket reads for the same burst ---");
    raw.report();
    println!();

    bencher.bench_local(|| divan::black_box(Stages::measure(&url).messages));
}
