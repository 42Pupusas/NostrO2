//! The per-relay IO thread.
//!
//! One [`RelayDriver`] thread owns one relay connection outright: its socket,
//! its frame codec, and its reconnect budget. Nothing it touches is shared,
//! so nothing it touches needs a lock. It talks to the rest of the crate
//! through two lock-free rings:
//!
//! - an MPSC ring of outbound JSON frames, which any number of cloned relay
//!   handles push into and this thread alone drains;
//! - an SPMC ring of inbound [`DriverEvent`]s, which this thread alone pushes
//!   and any number of cloned handles pop from.
//!
//! Parsing happens here, on the IO thread, so a pool of relays parses in
//! parallel instead of queueing behind one consumer.

/// Ring capacities and timeouts of one driver.
#[derive(Debug, Clone)]
pub struct DriverConfig {
    /// The relay to connect to.
    pub url: crate::url::RelayUrl,
    /// The reconnection policy.
    pub reconnect: crate::reconnect::ReconnectConfig,
    /// Slots for outbound frames waiting for the socket.
    pub outbound_capacity: usize,
    /// Slots for inbound events waiting for the application.
    pub inbound_capacity: usize,
    /// Bound on the TCP handshake.
    pub connect_timeout: std::time::Duration,
    /// Pace of the IO loop.
    ///
    /// One thread owns the socket, so it blocks in `read` and does its other
    /// work between reads. This value therefore bounds two latencies:
    ///
    /// - the delay before a queued frame reaches the socket;
    /// - the delay before the thread notices a raised shutdown flag, which
    ///   is what a dropped [`crate::guard::DriverGuard`] waits for.
    ///
    /// A shorter pace cuts both. Measured on an idle connection, the price
    /// is flat: one driver costs about 1% of a core whether it wakes every
    /// 100ms or every millisecond, because an empty wakeup is only a timed
    /// `read` returning nothing. The default is therefore short.
    pub read_timeout: std::time::Duration,
    /// Bound on one socket write.
    ///
    /// A relay that accepts the connection but stops reading it is not a
    /// relay that disconnects. Both receive windows fill, and a blocking
    /// write never returns, which freezes the thread that also serves
    /// reads. This bound turns that into a disconnect and a reconnect.
    pub write_timeout: std::time::Duration,
    /// What to do with a note whose signature does not check out.
    pub verify: crate::verifier::VerifyPolicy,
    /// When to probe a quiet connection, and when to give up on it.
    ///
    /// TCP never reports a peer that stops answering, so without this a
    /// broken connection looks exactly like a quiet one and the driver
    /// waits forever instead of reconnecting.
    pub heartbeat: crate::heartbeat::HeartbeatConfig,
    /// The TLS configuration, or `None` to build the default one.
    ///
    /// Set this to choose the crypto provider, the root store, or client
    /// certificates. A `ws://` relay never reads it.
    pub tls: Option<crate::tls::RelayTls>,
}

impl DriverConfig {
    /// The default IO pace.
    ///
    /// This bounds send latency and shutdown latency alike. It is short
    /// because a wakeup that finds nothing is nearly free.
    pub const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(5);

    /// The default bound on one socket write.
    ///
    /// Long enough that a slow but working relay is never cut off, short
    /// enough that a relay which stopped reading is noticed while the
    /// service still has time to reconnect.
    pub const DEFAULT_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

    /// A configuration with the default policy and sizes.
    #[must_use]
    pub fn new(url: crate::url::RelayUrl) -> Self {
        Self {
            url,
            reconnect: crate::reconnect::ReconnectConfig::default(),
            outbound_capacity: 256,
            inbound_capacity: 1024,
            connect_timeout: std::time::Duration::from_secs(10),
            read_timeout: Self::DEFAULT_READ_TIMEOUT,
            write_timeout: Self::DEFAULT_WRITE_TIMEOUT,
            verify: crate::verifier::VerifyPolicy::default(),
            heartbeat: crate::heartbeat::HeartbeatConfig::default(),
            tls: None,
        }
    }

    /// Replaces the TLS configuration.
    ///
    /// # Example
    /// ```no_run
    /// use nostro2_relay::{DriverConfig, RelayTls, RelayUrl};
    ///
    /// # fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let url = RelayUrl::parse("wss://relay.example.com")?;
    /// let config = DriverConfig::new(url).with_tls(RelayTls::new()?);
    /// # let _ = config;
    /// # Ok(())
    /// # }
    /// ```
    #[must_use]
    pub fn with_tls(mut self, tls: crate::tls::RelayTls) -> Self {
        self.tls = Some(tls);
        self
    }

    /// The TLS configuration, building the default one when none was set.
    ///
    /// A `ws://` relay never starts a TLS session, so no configuration is
    /// built for one. Without that, a plaintext connection would demand a
    /// crypto provider it never uses.
    ///
    /// # Errors
    ///
    /// Returns [`crate::tls::RelayTlsError`] when the URL is secure, no
    /// configuration was set, and the default cannot be built, which under
    /// `rustls-custom-provider` means no provider was supplied.
    pub fn tls(&self) -> Result<Option<crate::tls::RelayTls>, crate::tls::RelayTlsError> {
        if let Some(tls) = self.tls.clone() {
            return Ok(Some(tls));
        }
        if !self.url.is_secure() {
            return Ok(None);
        }
        crate::tls::RelayTls::new().map(Some)
    }

    /// Replaces the liveness policy.
    #[must_use]
    pub const fn with_heartbeat(mut self, heartbeat: crate::heartbeat::HeartbeatConfig) -> Self {
        self.heartbeat = heartbeat;
        self
    }

    /// Replaces the signature policy for inbound notes.
    #[must_use]
    pub const fn with_verify(mut self, verify: crate::verifier::VerifyPolicy) -> Self {
        self.verify = verify;
        self
    }

    /// Replaces the reconnection policy.
    #[must_use]
    pub const fn with_reconnect(mut self, reconnect: crate::reconnect::ReconnectConfig) -> Self {
        self.reconnect = reconnect;
        self
    }

    /// Replaces the ring capacities.
    #[must_use]
    pub const fn with_capacities(mut self, outbound: usize, inbound: usize) -> Self {
        self.outbound_capacity = outbound;
        self.inbound_capacity = inbound;
        self
    }

    /// Replaces the IO loop pace, which bounds send latency.
    #[must_use]
    pub const fn with_read_timeout(mut self, read_timeout: std::time::Duration) -> Self {
        self.read_timeout = read_timeout;
        self
    }

    /// Replaces the bound on one socket write.
    #[must_use]
    pub const fn with_write_timeout(mut self, write_timeout: std::time::Duration) -> Self {
        self.write_timeout = write_timeout;
        self
    }
}

/// Something that happened on one relay connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverEvent {
    /// The socket connected and completed its upgrade.
    Connected,
    /// A relay message arrived and parsed.
    Message(Box<nostro2::NostrRelayEvent>),
    /// The connection ended, with the reason when there is one.
    Disconnected(Option<String>),
    /// The driver gave up: its retry budget is spent, and no further event
    /// will arrive on this ring.
    Exhausted,
}

/// The outcome of a driver's first connection attempt.
pub type Handshake = Result<(), String>;

/// The application's end of one driver.
///
/// Dropping this stops the thread and closes the socket, because the guard
/// joins on drop.
pub struct DriverPorts {
    /// Push outbound JSON frames here.
    pub outbound: quetzalcoatl::mpsc::Producer<String>,
    /// Pop inbound events here. Clone for another reader.
    pub inbound: quetzalcoatl::spmc::Consumer<DriverEvent>,
    /// Pop once for the first connection's outcome.
    pub handshake: quetzalcoatl::spsc::Consumer<Handshake>,
    /// Stops and joins the thread when dropped.
    pub guard: crate::guard::DriverGuard,
}

impl std::fmt::Debug for DriverPorts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriverPorts")
            .field("finished", &self.guard.is_finished())
            .finish_non_exhaustive()
    }
}

/// The IO thread of one relay.
pub struct RelayDriver {
    url: crate::url::RelayUrl,
    tls: Option<crate::tls::RelayTls>,
    schedule: crate::reconnect::ReconnectSchedule,
    shutdown: crate::guard::Shutdown,
    connect_timeout: std::time::Duration,
    read_timeout: std::time::Duration,
    write_timeout: std::time::Duration,
    outbound: quetzalcoatl::mpsc::Consumer<String>,
    inbound: quetzalcoatl::spmc::Producer<DriverEvent>,
    handshake: Option<quetzalcoatl::spsc::Producer<Handshake>>,
    verifier: crate::verifier::NoteVerifier,
    heartbeat: crate::heartbeat::HeartbeatConfig,
    session: crate::session::Session,
}

impl RelayDriver {
    /// Spawns the thread and returns the application's end of it.
    ///
    /// This does not block: the first connection happens on the new thread,
    /// and its outcome arrives on [`DriverPorts::handshake`].
    ///
    /// # Panics
    ///
    /// Panics when the operating system refuses to spawn the thread.
    #[must_use]
    pub fn spawn(config: DriverConfig, tls: Option<crate::tls::RelayTls>) -> DriverPorts {
        let (outbound_tx, outbound_rx) =
            quetzalcoatl::mpsc::RingBuffer::<String>::new(quetzalcoatl::capacity::Capacity::at_least(
                config.outbound_capacity,
            ))
            .split();
        let (inbound_tx, inbound_rx) = quetzalcoatl::spmc::RingBuffer::<DriverEvent>::new(
            quetzalcoatl::capacity::Capacity::at_least(config.inbound_capacity),
        )
        .split();
        let (handshake_tx, handshake_rx) = quetzalcoatl::spsc::RingBuffer::<Handshake>::new(
            quetzalcoatl::capacity::Capacity::at_least(1),
        )
        .split();

        let shutdown = crate::guard::Shutdown::new();
        let driver = Self {
            url: config.url,
            tls,
            schedule: config.reconnect.schedule(),
            shutdown: shutdown.clone(),
            connect_timeout: config.connect_timeout,
            read_timeout: config.read_timeout,
            write_timeout: config.write_timeout,
            outbound: outbound_rx,
            inbound: inbound_tx,
            handshake: Some(handshake_tx),
            verifier: crate::verifier::NoteVerifier::with_policy(config.verify),
            heartbeat: config.heartbeat,
            session: crate::session::Session::new(),
        };
        if !driver.verifier.is_enforcing() {
            log::warn!(
                "{}: inbound notes are not signature-checked; the relay can forge events",
                driver.url
            );
        }
        let handle = std::thread::Builder::new()
            .name("nostr-relay-driver".to_owned())
            .spawn(move || driver.run())
            .expect("the operating system can spawn a thread");

        DriverPorts {
            outbound: outbound_tx,
            inbound: inbound_rx,
            handshake: handshake_rx,
            guard: crate::guard::DriverGuard::new(shutdown, handle),
        }
    }

    fn run(mut self) {
        while !self.shutdown.is_raised() {
            match crate::socket::WsSocket::connect(
                &self.url,
                self.tls.as_ref(),
                self.connect_timeout,
                self.read_timeout,
                self.write_timeout,
            ) {
                Ok(mut socket) => {
                    self.report_handshake(Ok(()));
                    self.schedule.succeeded();
                    // A relay forgets every subscription when the connection
                    // drops, so a reconnect must restate them or the service
                    // stays connected and silent.
                    let restored = self.restore_session(&mut socket);
                    self.emit(DriverEvent::Connected);
                    let reason = match restored {
                        Ok(()) => self.serve(socket),
                        Err(e) => Some(e.to_string()),
                    };
                    self.emit(DriverEvent::Disconnected(reason));
                }
                Err(e) => {
                    self.report_handshake(Err(e.to_string()));
                    log::warn!("could not connect to {}: {e}", self.url);
                }
            }

            if self.shutdown.is_raised() {
                return;
            }
            let Some(delay) = self.schedule.next() else {
                log::warn!("giving up on {}: retry budget spent", self.url);
                self.emit(DriverEvent::Exhausted);
                return;
            };
            if !self.shutdown.sleep(delay) {
                return;
            }
        }
    }

    /// Runs one connection until it ends, and returns why it ended.
    fn serve(&mut self, mut socket: crate::socket::WsSocket) -> Option<String> {
        let mut heartbeat = crate::heartbeat::Heartbeat::new(self.heartbeat);
        loop {
            if self.shutdown.is_raised() {
                let _ = socket.send_close();
                return None;
            }
            if let Err(e) = self.write_pending(&mut socket) {
                return Some(e.to_string());
            }
            match socket.poll() {
                Ok(Some(crate::socket::WsMessage::Text(text))) => self.dispatch(&text),
                Ok(Some(crate::socket::WsMessage::Binary(data))) => {
                    if let Ok(text) = std::str::from_utf8(&data) {
                        self.dispatch(text);
                    }
                }
                Ok(Some(crate::socket::WsMessage::Close(reason))) => {
                    return Some(reason.unwrap_or_else(|| "relay closed the connection".to_owned()));
                }
                Ok(None) => {}
                Err(e) => return Some(e.to_string()),
            }
            if let Some(reason) = Self::check_liveness(&mut socket, &mut heartbeat) {
                return Some(reason);
            }
        }
    }

    /// Probes a quiet connection and reports one that stopped answering.
    ///
    /// Returns the reason to end the connection, or `None` to keep serving.
    fn check_liveness(
        socket: &mut crate::socket::WsSocket,
        heartbeat: &mut crate::heartbeat::Heartbeat,
    ) -> Option<String> {
        if socket.took_traffic() {
            heartbeat.saw_traffic();
        }
        match heartbeat.assess() {
            crate::heartbeat::Liveness::Healthy => None,
            crate::heartbeat::Liveness::Probe => match socket.send_ping() {
                Ok(()) => {
                    heartbeat.probed();
                    None
                }
                Err(e) => Some(e.to_string()),
            },
            crate::heartbeat::Liveness::Dead => {
                Some("the relay stopped answering".to_owned())
            }
        }
    }

    /// Replays the open subscriptions onto a fresh connection.
    fn restore_session(
        &self,
        socket: &mut crate::socket::WsSocket,
    ) -> Result<(), crate::socket::WsSocketError> {
        if self.session.is_empty() {
            return Ok(());
        }
        log::debug!(
            "{}: restoring {} subscription(s) after reconnect",
            self.url,
            self.session.len()
        );
        let frames: Vec<String> = self.session.replay().map(ToOwned::to_owned).collect();
        for frame in frames {
            socket.send_text(&frame)?;
        }
        Ok(())
    }

    fn write_pending(
        &mut self,
        socket: &mut crate::socket::WsSocket,
    ) -> Result<(), crate::socket::WsSocketError> {
        while let Some(frame) = self.outbound.pop() {
            self.session.observe(&frame);
            socket.send_text(&frame)?;
        }
        Ok(())
    }

    /// Parses one relay frame, checks its signature, and publishes it.
    ///
    /// An unparseable frame is a relay extension or junk, not a fatal error,
    /// so the connection stays up. A note that fails verification is a forgery
    /// attempt by the relay, so it never reaches the application.
    fn dispatch(&self, text: &str) {
        let Ok(event) = text.parse::<nostro2::NostrRelayEvent>() else {
            log::warn!("skipped an unparseable frame from {}", self.url);
            return;
        };
        if !self.verifier.judge(&event).is_admit() {
            log::warn!("dropped a note with a bad signature from {}", self.url);
            return;
        }
        self.emit(DriverEvent::Message(Box::new(event)));
    }

    /// Publishes an event, dropping it when the application is not reading.
    ///
    /// A full inbound ring means the reader fell behind. The driver must not
    /// block on it: the socket would stop being read and the relay would drop
    /// the connection for a missed heartbeat.
    fn emit(&self, event: DriverEvent) {
        if self.inbound.push(event).is_err() {
            log::warn!("inbound ring is full for {}, dropped an event", self.url);
        }
    }

    fn report_handshake(&mut self, outcome: Handshake) {
        if let Some(port) = self.handshake.take() {
            let _ = port.push(outcome);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    /// A relay built on an independent WebSocket implementation. It counts
    /// accepted connections, so an orphaned driver betrays itself, and it can
    /// be told to drop connections to force the reconnect path.
    struct FakeRelay {
        port: u16,
        accepts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl FakeRelay {
        fn serving(script: Script) -> Self {
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
                            stream.set_nonblocking(false).unwrap();
                            script.serve(stream, &halt);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => return,
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

        fn url(&self) -> crate::url::RelayUrl {
            crate::url::RelayUrl::parse(&format!("ws://127.0.0.1:{}", self.port)).unwrap()
        }

        fn accepts(&self) -> usize {
            self.accepts.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn config(&self) -> DriverConfig {
            DriverConfig::new(self.url())
                .with_reconnect(crate::reconnect::ReconnectConfig::fixed(
                    std::time::Duration::from_millis(50),
                ))
        }

        fn spawn_driver(&self) -> DriverPorts {
            RelayDriver::spawn(self.config(), Some(crate::tls::RelayTls::testing()))
        }
    }

    impl Drop for FakeRelay {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    #[derive(Clone, Copy)]
    enum Script {
        /// Complete the upgrade, then never send or answer anything, while
        /// holding the socket open. This is a half-open connection: TCP
        /// still believes it is fine.
        Mute,
        /// Echo every client frame back verbatim.
        Echo,
        /// Send one properly signed note, then serve normally.
        SendNote,
        /// Send a note whose signature does not match its contents.
        SendForgedNote,
        /// Complete the upgrade, then drop the connection at once.
        DropAfterUpgrade,
        /// Refuse the upgrade.
        Refuse,
    }

    impl Script {
        fn serve(
            self,
            stream: std::net::TcpStream,
            halt: &std::sync::atomic::AtomicBool,
        ) {
            match self {
                Self::Refuse => Self::refuse(stream),
                Self::DropAfterUpgrade => Self::drop_after_upgrade(stream),
                Self::Mute => Self::mute(stream, halt),
                Self::Echo => Self::echo(stream, halt, None),
                Self::SendNote => Self::echo(stream, halt, Some(Self::note())),
                Self::SendForgedNote => Self::echo(stream, halt, Some(Self::forged_note())),
            }
        }

        fn signed_note(content: &str) -> nostro2::NostrNote {
            use nostro2::{NostrKeypair as _, NostrSigner as _};
            let keypair = nostro2_signer::NostrKeypair::generate();
            let mut note = nostro2::NostrNote {
                kind: 1,
                content: content.to_owned(),
                pubkey: keypair.public_key(),
                ..Default::default()
            };
            note.sign_with(&keypair).unwrap();
            note
        }

        fn frame(note: &nostro2::NostrNote) -> String {
            format!(
                "[\"EVENT\",\"sub\",{}]",
                crate::json::RelayJson::to_string(note).unwrap()
            )
        }

        fn note() -> String {
            Self::frame(&Self::signed_note("hi"))
        }

        /// A note signed correctly, then edited. The signature no longer
        /// covers the content, which is what a malicious relay would send.
        fn forged_note() -> String {
            let mut note = Self::signed_note("hi");
            note.content = "tampered after signing".to_owned();
            Self::frame(&note)
        }

        fn refuse(mut stream: std::net::TcpStream) {
            let mut scratch = [0_u8; 1024];
            let _ = stream.read(&mut scratch);
            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
        }

        /// Upgrades, then reads nothing and writes nothing. `tungstenite`
        /// would answer a ping by itself, so this never reads the socket.
        fn mute(stream: std::net::TcpStream, halt: &std::sync::atomic::AtomicBool) {
            let Ok(ws) = tungstenite::accept(stream) else {
                return;
            };
            while !halt.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            drop(ws);
        }

        fn drop_after_upgrade(stream: std::net::TcpStream) {
            let Ok(mut ws) = tungstenite::accept(stream) else {
                return;
            };
            let _ = ws.flush();
        }

        fn echo(
            stream: std::net::TcpStream,
            halt: &std::sync::atomic::AtomicBool,
            greeting: Option<String>,
        ) {
            stream
                .set_read_timeout(Some(std::time::Duration::from_millis(50)))
                .unwrap();
            let Ok(mut ws) = tungstenite::accept(stream) else {
                return;
            };
            if let Some(text) = greeting
                && ws.send(tungstenite::Message::Text(text.into())).is_err()
            {
                return;
            }
            while !halt.load(std::sync::atomic::Ordering::SeqCst) {
                match ws.read() {
                    Ok(tungstenite::Message::Text(text)) => {
                        if ws.send(tungstenite::Message::Text(text)).is_err() {
                            return;
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
        }
    }

    /// Polls a ring until it yields, so tests state a deadline instead of a
    /// sleep.
    struct Awaited;

    impl Awaited {
        fn event(ports: &DriverPorts) -> DriverEvent {
            Self::poll(|| ports.inbound.pop(), "an inbound event")
        }

        fn handshake(ports: &mut DriverPorts) -> Handshake {
            Self::poll(|| ports.handshake.pop(), "the handshake outcome")
        }

        fn message(ports: &DriverPorts) -> nostro2::NostrRelayEvent {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if let Some(DriverEvent::Message(event)) = ports.inbound.pop() {
                    return *event;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            panic!("no relay message arrived before the deadline");
        }

        fn poll<T>(mut source: impl FnMut() -> Option<T>, what: &str) -> T {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if let Some(value) = source() {
                    return value;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            panic!("{what} did not arrive before the deadline");
        }

        fn settle() {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        /// Clears events already queued, so a later wait times a fresh one.
        fn drain(ports: &DriverPorts) {
            while ports.inbound.pop().is_some() {}
        }
    }

    #[test]
    fn a_successful_handshake_is_reported() {
        let relay = FakeRelay::serving(Script::Echo);
        let mut ports = relay.spawn_driver();
        assert_eq!(Awaited::handshake(&mut ports), Ok(()));
        assert_eq!(Awaited::event(&ports), DriverEvent::Connected);
    }

    #[test]
    fn a_failed_handshake_carries_the_reason() {
        let relay = FakeRelay::serving(Script::Refuse);
        let mut ports = relay.spawn_driver();
        let outcome = Awaited::handshake(&mut ports);
        assert!(outcome.is_err());
        assert!(outcome.unwrap_err().contains("upgrade failed"));
    }

    #[test]
    fn a_relay_message_is_parsed_on_the_driver_thread() {
        let relay = FakeRelay::serving(Script::SendNote);
        let ports = relay.spawn_driver();
        let event = Awaited::message(&ports);
        assert!(matches!(event, nostro2::NostrRelayEvent::NewNote(..)));
    }

    // A relay is untrusted: it can invent a note and attribute it to any
    // pubkey. The driver checks the signature, so the forgery never reaches
    // the application.
    #[test]
    fn a_forged_note_never_reaches_the_reader() {
        let relay = FakeRelay::serving(Script::SendForgedNote);
        let mut ports = relay.spawn_driver();
        assert_eq!(Awaited::handshake(&mut ports), Ok(()));

        ports
            .outbound
            .push("[\"NOTICE\",\"after the forgery\"]".to_owned())
            .unwrap();

        // The echo arrives after the forged note, so receiving it proves the
        // forgery was dropped rather than merely delayed.
        match Awaited::message(&ports) {
            nostro2::NostrRelayEvent::Notice(_, text) => assert_eq!(text, "after the forgery"),
            other => panic!("a forged note reached the reader: {other:?}"),
        }
    }

    #[test]
    fn a_trusting_driver_admits_a_forged_note() {
        let relay = FakeRelay::serving(Script::SendForgedNote);
        let config = relay.config().with_verify(crate::verifier::VerifyPolicy::Trust);
        let ports = RelayDriver::spawn(config, Some(crate::tls::RelayTls::testing()));
        assert!(matches!(
            Awaited::message(&ports),
            nostro2::NostrRelayEvent::NewNote(..)
        ));
    }

    // The echo server returns whatever it receives, so an outbound frame that
    // is shaped like a relay message proves the whole loop: ring, socket
    // write, socket read, parse, ring.
    #[test]
    fn an_outbound_frame_reaches_the_relay_and_echoes_back() {
        let relay = FakeRelay::serving(Script::Echo);
        let mut ports = relay.spawn_driver();
        assert_eq!(Awaited::handshake(&mut ports), Ok(()));

        ports
            .outbound
            .push("[\"NOTICE\",\"echoed\"]".to_owned())
            .unwrap();
        ports.guard.wake();

        match Awaited::message(&ports) {
            nostro2::NostrRelayEvent::Notice(_, text) => assert_eq!(text, "echoed"),
            other => panic!("expected the echoed notice, got {other:?}"),
        }
    }

    // One thread owns the socket, so a queued frame waits at most one read
    // timeout before it is written. This pins that bound: it is the price of
    // the single-owner design, and a regression here is a latency bug.
    #[test]
    fn a_send_reaches_the_relay_within_one_read_timeout() {
        let relay = FakeRelay::serving(Script::Echo);
        let config = relay
            .config()
            .with_read_timeout(std::time::Duration::from_millis(20));
        let mut ports = RelayDriver::spawn(config, Some(crate::tls::RelayTls::testing()));
        assert_eq!(Awaited::handshake(&mut ports), Ok(()));
        Awaited::drain(&ports);

        let sent = std::time::Instant::now();
        ports
            .outbound
            .push("[\"NOTICE\",\"paced\"]".to_owned())
            .unwrap();
        match Awaited::message(&ports) {
            nostro2::NostrRelayEvent::Notice(_, text) => assert_eq!(text, "paced"),
            other => panic!("expected the echoed notice, got {other:?}"),
        }

        let elapsed = sent.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "a send took {elapsed:?}, far past the 20ms pace"
        );
    }

    // A driver blocked in `read` notices a raised shutdown flag only when
    // that read returns, so stopping one costs at most a single IO pace.
    // This measured ~100ms per connection before the pace shrank, which
    // dominated every short-lived connection.
    #[test]
    fn dropping_the_ports_costs_at_most_one_read_timeout() {
        let relay = FakeRelay::serving(Script::Echo);
        let config = relay
            .config()
            .with_read_timeout(std::time::Duration::from_millis(10));
        let mut ports = RelayDriver::spawn(config, Some(crate::tls::RelayTls::testing()));
        assert_eq!(Awaited::handshake(&mut ports), Ok(()));

        let stopping = std::time::Instant::now();
        drop(ports);
        let elapsed = stopping.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "shutdown took {elapsed:?} with a 10ms pace"
        );
    }

    // A relay that holds the socket open and answers nothing must be
    // detected, or a reader waits forever on a connection that is already
    // gone. TCP reports nothing in this case: only a probe reveals it.
    #[test]
    fn a_silent_relay_ends_the_connection() {
        let relay = FakeRelay::serving(Script::Mute);
        let config = relay
            .config()
            .with_reconnect(crate::reconnect::ReconnectConfig::disabled())
            .with_heartbeat(crate::heartbeat::HeartbeatConfig {
                idle_timeout: std::time::Duration::from_millis(100),
                reply_timeout: std::time::Duration::from_millis(100),
            });
        let ports = RelayDriver::spawn(config, Some(crate::tls::RelayTls::testing()));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if let Some(DriverEvent::Disconnected(reason)) = ports.inbound.pop() {
                assert_eq!(reason.as_deref(), Some("the relay stopped answering"));
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("a silent relay was never detected");
    }

    // A relay that answers the probe is alive. The connection must survive,
    // or every quiet subscription would be torn down on a timer.
    #[test]
    fn a_relay_that_answers_pings_keeps_its_connection() {
        let relay = FakeRelay::serving(Script::Echo);
        let config = relay.config().with_heartbeat(crate::heartbeat::HeartbeatConfig {
            idle_timeout: std::time::Duration::from_millis(20),
            reply_timeout: std::time::Duration::from_millis(100),
        });
        let mut ports = RelayDriver::spawn(config, Some(crate::tls::RelayTls::testing()));
        assert_eq!(Awaited::handshake(&mut ports), Ok(()));
        Awaited::drain(&ports);

        std::thread::sleep(std::time::Duration::from_millis(500));
        while let Some(event) = ports.inbound.pop() {
            assert!(
                !matches!(event, DriverEvent::Disconnected(_)),
                "a responsive relay was dropped by the heartbeat"
            );
        }
    }

    #[test]
    fn a_dropped_connection_reconnects() {
        let relay = FakeRelay::serving(Script::DropAfterUpgrade);
        let ports = relay.spawn_driver();
        Awaited::settle();
        drop(ports);
        assert!(
            relay.accepts() > 1,
            "the driver reconnected {} times, expected repeated attempts",
            relay.accepts()
        );
    }

    #[test]
    fn a_disconnect_is_announced() {
        let relay = FakeRelay::serving(Script::DropAfterUpgrade);
        let ports = relay.spawn_driver();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if let Some(DriverEvent::Disconnected(_)) = ports.inbound.pop() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("no disconnect was announced");
    }

    // The bug that started this work: a dropped owner used to leave the IO
    // side reconnecting forever, one leaked socket per rebuild.
    #[test]
    fn dropping_the_ports_stops_the_reconnect_loop() {
        let relay = FakeRelay::serving(Script::DropAfterUpgrade);
        let ports = relay.spawn_driver();
        Awaited::settle();
        drop(ports);

        Awaited::settle();
        let baseline = relay.accepts();
        Awaited::settle();
        assert_eq!(
            relay.accepts(),
            baseline,
            "the driver kept reconnecting after its ports were dropped"
        );
    }

    #[test]
    fn a_spent_retry_budget_ends_the_driver() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let config = DriverConfig::new(
            crate::url::RelayUrl::parse(&format!("ws://127.0.0.1:{port}")).unwrap(),
        )
        .with_reconnect(crate::reconnect::ReconnectConfig {
            max_retries: 2,
            initial_delay: std::time::Duration::from_millis(10),
            max_delay: std::time::Duration::from_millis(10),
            backoff_multiplier: 1.0,
        });
        let ports = RelayDriver::spawn(config, Some(crate::tls::RelayTls::testing()));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if ports.inbound.pop() == Some(DriverEvent::Exhausted) {
                assert!(ports.guard.is_finished());
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("the driver never gave up");
    }

    #[test]
    fn a_disabled_policy_never_retries() {
        let relay = FakeRelay::serving(Script::DropAfterUpgrade);
        let config = DriverConfig::new(relay.url())
            .with_reconnect(crate::reconnect::ReconnectConfig::disabled());
        let ports = RelayDriver::spawn(config, Some(crate::tls::RelayTls::testing()));
        Awaited::settle();
        Awaited::settle();
        drop(ports);
        assert_eq!(relay.accepts(), 1);
    }

    #[test]
    fn cloned_readers_share_one_stream() {
        let relay = FakeRelay::serving(Script::Echo);
        let mut ports = relay.spawn_driver();
        let second = ports.inbound.clone();
        assert_eq!(Awaited::handshake(&mut ports), Ok(()));

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if ports.inbound.pop().is_some() || second.pop().is_some() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("neither reader saw the connection event");
    }
}
