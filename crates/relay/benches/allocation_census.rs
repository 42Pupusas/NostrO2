//! How many heap allocations one message costs, end to end.
//!
//! The timing benches put transport at ~2.95ms per 500 messages, level
//! with `nostr-sdk`. Timing hides allocation, though: an allocator hitting
//! a warm thread-local free list is fast enough that a wasteful path can
//! measure fine and still fall over under a real workload, where the heap
//! is fragmented and the free lists are cold.
//!
//! So this counts instead of timing. A global allocator tallies every
//! `alloc` while a gate is open, and the gate is open only around the
//! drain, so the numbers describe the receive path rather than the setup.
//!
//! The counter is process-wide on purpose. Divan's `AllocProfiler`
//! attributes per thread, and the driver runs on its own thread, so a
//! per-thread count would miss the very allocations in question.

fn main() {
    Census::run();
}

/// How many messages one run carries.
const COUNT: usize = 500;

/// A global allocator that tallies allocations while a gate is open.
///
/// `Relaxed` ordering throughout: the counters are statistics, not
/// synchronisation, and nothing reads them until every producing thread
/// has been joined.
struct CountingAllocator;

/// Whether allocations are currently being counted.
static COUNTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// How many times `alloc` has been called while counting.
static ALLOCS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// How many times `realloc` has been called while counting.
static REALLOCS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// How many bytes have been requested while counting.
static BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        if COUNTING.load(std::sync::atomic::Ordering::Relaxed) {
            ALLOCS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            BYTES.fetch_add(layout.size() as u64, std::sync::atomic::Ordering::Relaxed);
        }
        unsafe { std::alloc::System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
        unsafe { std::alloc::System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(
        &self,
        ptr: *mut u8,
        layout: std::alloc::Layout,
        new_size: usize,
    ) -> *mut u8 {
        if COUNTING.load(std::sync::atomic::Ordering::Relaxed) {
            REALLOCS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            BYTES.fetch_add(
                new_size.saturating_sub(layout.size()) as u64,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
        unsafe { std::alloc::System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// One tally of allocation activity over a measured window.
struct Tally {
    allocs: u64,
    reallocs: u64,
    bytes: u64,
}

impl Tally {
    /// Runs `body` with the counters open and returns what it cost.
    ///
    /// Not reentrant and not thread-scoped: the gate is process-wide, so
    /// only one window may be open at a time and every thread's
    /// allocations land in it. That is the point, but it means callers
    /// must quiesce anything they do not want counted.
    fn around<T>(body: impl FnOnce() -> T) -> (T, Self) {
        ALLOCS.store(0, std::sync::atomic::Ordering::Relaxed);
        REALLOCS.store(0, std::sync::atomic::Ordering::Relaxed);
        BYTES.store(0, std::sync::atomic::Ordering::Relaxed);

        COUNTING.store(true, std::sync::atomic::Ordering::SeqCst);
        let out = body();
        COUNTING.store(false, std::sync::atomic::Ordering::SeqCst);

        let tally = Self {
            allocs: ALLOCS.load(std::sync::atomic::Ordering::Relaxed),
            reallocs: REALLOCS.load(std::sync::atomic::Ordering::Relaxed),
            bytes: BYTES.load(std::sync::atomic::Ordering::Relaxed),
        };
        (out, tally)
    }

    fn report(&self, label: &str, messages: u64) {
        let per = f64::from(u32::try_from(self.allocs).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(messages).unwrap_or(1));
        println!(
            "  {label:<34} {:>7} allocs  {:>6} reallocs  {:>9} B  {per:>6.2} allocs/msg",
            self.allocs, self.reallocs, self.bytes,
        );
    }
}

/// The end-to-end census: drives the real relay and counts the receive path.
struct Census;

impl Census {
    fn run() {
        let notes = Fixture::notes();
        let relay = FixtureRelay::start(notes.clone());
        let url = relay.url();

        println!("\nallocation census: {COUNT} messages, end to end\n");

        Self::whole_path(&url);
        Self::parse_only(&notes);
        Self::parse_note_only(&notes);
        #[cfg(feature = "serde")]
        Self::parse_tag_only();
        Self::event_only(&notes);

        println!();
    }

    /// Counts every allocation the driver makes while draining a burst.
    fn whole_path(url: &str) {
        let mut relay = nostro2_relay::NostrRelay::connect_blocking(url).unwrap();
        relay
            .send(nostro2::NostrClientEvent::Subscribe(
                nostro2::RelayEventTag::Req,
                "bench".to_string(),
                nostro2::NostrSubscription {
                    kinds: Some([1].into_iter().collect()),
                    ..Default::default()
                },
            ))
            .unwrap();

        let (seen, tally) = Tally::around(|| {
            let mut seen = 0_usize;
            while seen < COUNT {
                match relay.recv_blocking() {
                    Some(nostro2::NostrRelayEvent::NewNote(..)) => seen += 1,
                    Some(_) => {}
                    None => break,
                }
            }
            seen
        });

        assert_eq!(seen, COUNT, "drained {seen} of {COUNT}");
        tally.report("whole receive path", COUNT as u64);
    }

    /// Counts only the JSON parse, for the same frames.
    fn parse_only(notes: &[String]) {
        let frames: Vec<String> = notes
            .iter()
            .map(|note| format!("[\"EVENT\",\"bench\",{note}]"))
            .collect();

        let (parsed, tally) = Tally::around(|| {
            let mut parsed = 0_usize;
            for frame in &frames {
                let event: nostro2::NostrRelayEvent = frame.parse().unwrap();
                std::hint::black_box(&event);
                parsed += 1;
            }
            parsed
        });

        assert_eq!(parsed, COUNT);
        tally.report("parse only (no io, no ring)", COUNT as u64);
    }

    /// Counts parsing the bare note, without the enclosing frame.
    ///
    /// The difference against the full frame is what the `["EVENT",id,..]`
    /// wrapper costs, as opposed to the note's own fields.
    fn parse_note_only(notes: &[String]) {
        let (parsed, tally) = Tally::around(|| {
            let mut parsed = 0_usize;
            for note in notes {
                let note: nostro2::NostrNote = note.parse().unwrap();
                std::hint::black_box(&note);
                parsed += 1;
            }
            parsed
        });

        assert_eq!(parsed, COUNT);
        tally.report("  of which: bare note", COUNT as u64);
    }

    /// Counts deserializing the frame tag on its own.
    ///
    /// `RelayEventTag` is a closed set of short literals, so recognising
    /// one should not need the heap at all.
    ///
    /// Serde only: the bourne backend lexes the tag inline through
    /// `WireFrameExt` rather than deserializing it as a standalone value,
    /// so there is no equivalent call to make.
    #[cfg(feature = "serde")]
    fn parse_tag_only() {
        let tags: Vec<String> = (0..COUNT).map(|_| "\"EVENT\"".to_string()).collect();

        let (parsed, tally) = Tally::around(|| {
            let mut parsed = 0_usize;
            for tag in &tags {
                let tag: nostro2::RelayEventTag = serde_json::from_str(tag).unwrap();
                std::hint::black_box(&tag);
                parsed += 1;
            }
            parsed
        });

        assert_eq!(parsed, COUNT);
        tally.report("  of which: frame tag", COUNT as u64);
    }

    /// Counts boxing a parsed event, the step the pool adds.
    fn event_only(notes: &[String]) {
        let events: Vec<nostro2::NostrRelayEvent> = notes
            .iter()
            .map(|note| {
                format!("[\"EVENT\",\"bench\",{note}]")
                    .parse()
                    .unwrap()
            })
            .collect();
        let url: std::sync::Arc<nostro2_relay::RelayUrl> =
            std::sync::Arc::new("ws://127.0.0.1:1".parse().unwrap());

        let (built, tally) = Tally::around(|| {
            let mut built = 0_usize;
            for event in &events {
                let pooled = nostro2_relay::PoolEvent::Message(
                    url.clone(),
                    Box::new(nostro2::NostrRelayEvent::NewNote(
                        nostro2::RelayEventTag::Event,
                        "bench".to_string(),
                        match event {
                            nostro2::NostrRelayEvent::NewNote(.., note) => note.clone(),
                            _ => unreachable!(),
                        },
                    )),
                );
                std::hint::black_box(&pooled);
                built += 1;
            }
            built
        });

        assert_eq!(built, COUNT);
        tally.report("pool event construction", COUNT as u64);
    }
}

/// A loopback relay that replays pre-signed notes on demand.
///
/// Writes are buffered and flushed once, matching `pipeline`: an
/// unbuffered send per note on a nodelay socket puts every frame in its
/// own segment and makes the fixture, not the crate, set the pace.
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
        let Ok(mut ws) = tungstenite::accept(BufferedStream::wrap(stream)) else {
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
                        if ws.write(tungstenite::Message::Text(frame.into())).is_err() {
                            return;
                        }
                    }
                    let _ = ws.write(tungstenite::Message::Text(
                        format!("[\"EOSE\",\"{id}\"]").into(),
                    ));
                    let _ = ws.flush();
                }
                Ok(tungstenite::Message::Close(_)) => return,
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
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

/// A `TcpStream` whose writes are buffered until flushed.
struct BufferedStream {
    reader: std::net::TcpStream,
    writer: std::io::BufWriter<std::net::TcpStream>,
}

impl BufferedStream {
    fn wrap(stream: std::net::TcpStream) -> Self {
        let writer = std::io::BufWriter::with_capacity(1 << 20, stream.try_clone().unwrap());
        Self {
            reader: stream,
            writer,
        }
    }
}

impl std::io::Read for BufferedStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.reader, buf)
    }
}

impl std::io::Write for BufferedStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(&mut self.writer, buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::Write::flush(&mut self.writer)
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
}
