//! The application-facing handle for one relay.
//!
//! [`NostrRelay`] is a thin facade over the rings of one [`RelayDriver`]
//! thread. It holds no lock and no runtime: `send` writes into a lock-free
//! MPSC ring, and `recv` pops a lock-free SPMC ring. Cloning the handle
//! clones the ring endpoints, so every clone reads the same stream and
//! writes to the same socket.
//!
//! A handle is [`Send`] but not [`Sync`]: the ring endpoints keep private
//! cursors that one thread advances at a time. Sharing therefore happens by
//! cloning the handle, not by sharing a reference to it, which is what lets
//! the whole path stay lock-free.
//!
//! Readers **compete**: each message reaches exactly one handle, as with the
//! single receiver this replaced. Clone a handle to spread work over several
//! readers, not to give each reader a copy of the stream.
//!
//! [`RelayDriver`]: crate::driver::RelayDriver

pub use crate::reconnect::ReconnectConfig;

/// A connection to one relay.
///
/// The connection lives on its own thread. Dropping the last clone stops
/// that thread and closes the socket.
#[derive(Clone)]
pub struct NostrRelay {
    outbound: quetzalcoatl::mpsc::Producer<String>,
    inbound: quetzalcoatl::spmc::Consumer<crate::driver::DriverEvent>,
    url: crate::url::RelayUrl,
    guard: std::sync::Arc<crate::guard::DriverGuard>,
}

impl std::fmt::Debug for NostrRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NostrRelay")
            .field("url", &self.url)
            .field("connected", &!self.guard.is_finished())
            .finish_non_exhaustive()
    }
}

impl NostrRelay {
    /// Connects to `url` with the default reconnection policy.
    ///
    /// # Errors
    ///
    /// Returns [`NostrRelayError::Url`] when `url` is not a relay URL, and
    /// [`NostrRelayError::Connect`] when the first connection fails.
    ///
    /// [`NostrRelayError::Url`]: crate::errors::NostrRelayError::Url
    /// [`NostrRelayError::Connect`]: crate::errors::NostrRelayError::Connect
    pub async fn new(url: &str) -> Result<Self, crate::errors::NostrRelayError> {
        Self::with_reconnect(url, ReconnectConfig::default()).await
    }

    /// Connects to `url` with a custom reconnection policy.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use nostro2_relay::{NostrRelay, ReconnectConfig};
    /// use std::time::Duration;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let config = ReconnectConfig {
    ///     max_retries: 10,
    ///     initial_delay: Duration::from_secs(1),
    ///     max_delay: Duration::from_secs(30),
    ///     backoff_multiplier: 2.0,
    /// };
    /// let relay = NostrRelay::with_reconnect("wss://relay.example.com", config).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`NostrRelayError::Url`] when `url` is not a relay URL, and
    /// [`NostrRelayError::Connect`] when the first connection fails.
    ///
    /// [`NostrRelayError::Url`]: crate::errors::NostrRelayError::Url
    /// [`NostrRelayError::Connect`]: crate::errors::NostrRelayError::Connect
    pub async fn with_reconnect(
        url: &str,
        reconnect: ReconnectConfig,
    ) -> Result<Self, crate::errors::NostrRelayError> {
        let url = crate::url::RelayUrl::parse(url)?;
        let config = crate::driver::DriverConfig::new(url.clone()).with_reconnect(reconnect);
        let (relay, handshake) = Self::spawn(config)?;
        Self::settled(handshake, &url).await?;
        Ok(relay)
    }

    /// Connects to `url` without waiting for the first attempt to settle.
    ///
    /// The connection proceeds on its own thread, so this returns at once.
    /// A pool uses this to start every relay in parallel.
    ///
    /// # Errors
    ///
    /// Returns [`NostrRelayError::Url`] when `url` is not a relay URL.
    ///
    /// [`NostrRelayError::Url`]: crate::errors::NostrRelayError::Url
    pub fn detached(
        url: &str,
        reconnect: ReconnectConfig,
    ) -> Result<Self, crate::errors::NostrRelayError> {
        let url = crate::url::RelayUrl::parse(url)?;
        let config = crate::driver::DriverConfig::new(url).with_reconnect(reconnect);
        Ok(Self::spawn(config)?.0)
    }

    /// Connects with a fully specified driver configuration.
    ///
    /// Use this to tune the liveness probe, the ring sizes, or the IO pace,
    /// which the simpler constructors leave at their defaults. Like
    /// [`Self::detached`], this returns before the first attempt settles.
    ///
    /// # Errors
    ///
    /// Returns [`NostrRelayError::Tls`] when the TLS backend refuses to
    /// build a configuration.
    ///
    /// [`NostrRelayError::Tls`]: crate::errors::NostrRelayError::Tls
    pub fn with_driver_config(
        config: crate::driver::DriverConfig,
    ) -> Result<Self, crate::errors::NostrRelayError> {
        Ok(Self::spawn(config)?.0)
    }

    /// Spawns the driver thread without waiting for its first connection.
    ///
    /// The handshake port comes back separately, so waiting for the first
    /// connection never touches the inbound ring. A reader claims inbound
    /// events in batches, so a pop here would strand later messages in this
    /// handle's private cursor where a clone could not reach them.
    fn spawn(
        config: crate::driver::DriverConfig,
    ) -> Result<
        (Self, quetzalcoatl::spsc::Consumer<crate::driver::Handshake>),
        crate::errors::NostrRelayError,
    > {
        let url = config.url.clone();
        let tls = crate::tls::RelayTls::new()?;
        let ports = crate::driver::RelayDriver::spawn(config, tls);
        Ok((
            Self {
                outbound: ports.outbound,
                inbound: ports.inbound,
                url,
                guard: std::sync::Arc::new(ports.guard),
            },
            ports.handshake,
        ))
    }

    /// Waits for the first connection attempt to settle.
    #[allow(clippy::future_not_send)]
    async fn settled(
        mut handshake: quetzalcoatl::spsc::Consumer<crate::driver::Handshake>,
        url: &crate::url::RelayUrl,
    ) -> Result<(), crate::errors::NostrRelayError> {
        match handshake.pop_async().await {
            Some(Ok(())) => Ok(()),
            Some(Err(reason)) => Err(crate::errors::NostrRelayError::Connect(reason)),
            None => Err(crate::errors::NostrRelayError::Connect(format!(
                "could not connect to {url}"
            ))),
        }
    }

    /// The relay this handle is connected to.
    #[must_use]
    pub const fn url(&self) -> &crate::url::RelayUrl {
        &self.url
    }

    /// Whether the driver thread has stopped for good.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.guard.is_finished()
    }

    /// The thread guard, for owners that must outlive this handle.
    ///
    /// A guard is `Send + Sync`, unlike the handle itself, so a pool can hold
    /// one per relay and still move between threads.
    pub(crate) fn guard(&self) -> std::sync::Arc<crate::guard::DriverGuard> {
        std::sync::Arc::clone(&self.guard)
    }

    /// Queues a message for the relay.
    ///
    /// This does not block. The driver thread writes the frame to the socket
    /// on its next pass.
    ///
    /// # Errors
    ///
    /// Returns [`NostrRelayError::Serde`] when the message does not
    /// serialize, and [`NostrRelayError::SendError`] when the outbound ring
    /// is full or the driver has stopped.
    ///
    /// [`NostrRelayError::Serde`]: crate::errors::NostrRelayError::Serde
    /// [`NostrRelayError::SendError`]: crate::errors::NostrRelayError::SendError
    pub fn send<T>(&self, msg: T) -> Result<(), crate::errors::NostrRelayError>
    where
        T: Into<nostro2::NostrClientEvent> + Send + Sync,
    {
        let msg: nostro2::NostrClientEvent = msg.into();
        let frame = crate::json::RelayJson::to_string(&msg)?;
        self.outbound
            .push(frame)
            .map_err(|_| crate::errors::NostrRelayError::SendError)
    }

    /// Queues every message the stream yields.
    ///
    /// # Errors
    ///
    /// Returns the first error [`Self::send`] produces.
    // The ring endpoints are `!Sync` by design, so a future holding `&self`
    // is not `Send`. Drive it on the thread that owns the handle, or clone
    // the handle for another thread.
    #[allow(clippy::future_not_send)]
    pub async fn send_all<St, T>(&self, stream: St) -> Result<(), crate::errors::NostrRelayError>
    where
        T: Into<nostro2::NostrClientEvent> + Send + Sync + std::fmt::Debug,
        St: futures_util::Stream<Item = T> + Unpin + Sized,
    {
        let mut stream = stream;
        while let Some(msg) = futures_util::StreamExt::next(&mut stream).await {
            self.send(msg)?;
        }
        Ok(())
    }

    /// Returns the next relay message, waiting for one to arrive.
    ///
    /// Connection lifecycle events are consumed here, so this yields only
    /// relay messages. `None` means the driver stopped for good and no
    /// further message will arrive.
    ///
    /// This takes `&mut self` because a reader owns its position in the
    /// stream. Clone the handle to read from another task.
    #[allow(clippy::future_not_send)]
    pub async fn recv(&mut self) -> Option<nostro2::NostrRelayEvent> {
        loop {
            match self.inbound.pop_async().await? {
                crate::driver::DriverEvent::Message(event) => return Some(*event),
                crate::driver::DriverEvent::Exhausted => return None,
                crate::driver::DriverEvent::Connected
                | crate::driver::DriverEvent::Disconnected(_) => {}
            }
        }
    }

    /// Returns the next event, including the connection lifecycle ones.
    ///
    /// `None` means the driver stopped for good.
    #[allow(clippy::future_not_send)]
    pub async fn recv_event(&mut self) -> Option<crate::driver::DriverEvent> {
        self.inbound.pop_async().await
    }

    /// Returns the next relay message, parking the thread until one arrives.
    ///
    /// This is the synchronous twin of [`Self::recv`], for callers that own a
    /// thread instead of a task. [`Self::close`] unblocks it.
    pub fn recv_blocking(&mut self) -> Option<nostro2::NostrRelayEvent> {
        loop {
            match self.inbound.pop_block()? {
                crate::driver::DriverEvent::Message(event) => return Some(*event),
                crate::driver::DriverEvent::Exhausted => return None,
                crate::driver::DriverEvent::Connected
                | crate::driver::DriverEvent::Disconnected(_) => {}
            }
        }
    }

    /// Returns the next event including the lifecycle ones, parking the
    /// thread until one arrives.
    ///
    /// This is the synchronous twin of [`Self::recv_event`]. A service that
    /// must react to a reconnect needs this rather than [`Self::recv_blocking`],
    /// which hides the lifecycle.
    pub fn recv_event_blocking(&mut self) -> Option<crate::driver::DriverEvent> {
        self.inbound.pop_block()
    }

    /// Stops the connection and its thread.
    ///
    /// Every clone of this handle stops with it, and a reader parked in
    /// [`Self::recv_blocking`] returns `None`.
    pub fn close(&self) {
        self.guard.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    /// A relay on a real socket, built on an independent WebSocket
    /// implementation. It greets every connection with a NOTICE and records
    /// the frames it receives, so both directions are observable.
    struct EchoRelay {
        port: u16,
        received: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl EchoRelay {
        const GREETING: &'static str = "[\"NOTICE\",\"welcome\"]";

        fn start() -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let received = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

            let halt = stop.clone();
            let log = received.clone();
            let handle = std::thread::spawn(move || {
                while !halt.load(std::sync::atomic::Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).unwrap();
                            Self::serve(stream, &halt, &log);
                        }
                        Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
                    }
                }
            });

            Self {
                port,
                received,
                stop,
                handle: Some(handle),
            }
        }

        fn serve(
            stream: std::net::TcpStream,
            halt: &std::sync::atomic::AtomicBool,
            log: &std::sync::Mutex<Vec<String>>,
        ) {
            stream
                .set_read_timeout(Some(std::time::Duration::from_millis(50)))
                .unwrap();
            let Ok(mut ws) = tungstenite::accept(stream) else {
                return;
            };
            if ws
                .send(tungstenite::Message::Text(Self::GREETING.into()))
                .is_err()
            {
                return;
            }
            while !halt.load(std::sync::atomic::Ordering::SeqCst) {
                match ws.read() {
                    Ok(tungstenite::Message::Text(text)) => {
                        log.lock().unwrap().push(text.to_string());
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

        /// Waits for a received frame containing `needle`.
        fn saw(&self, needle: &str) -> bool {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if self
                    .received
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|frame| frame.contains(needle))
                {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            false
        }
    }

    impl Drop for EchoRelay {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    /// A relay that accepts a connection and never upgrades it.
    struct DeafRelay {
        port: u16,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        handle: Option<std::thread::JoinHandle<()>>,
    }

    impl DeafRelay {
        fn start() -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let port = listener.local_addr().unwrap().port();
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

            let halt = stop.clone();
            let handle = std::thread::spawn(move || {
                while !halt.load(std::sync::atomic::Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let mut scratch = [0_u8; 1024];
                            let _ = stream.read(&mut scratch);
                            let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
                        }
                        Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
                    }
                }
            });

            Self {
                port,
                stop,
                handle: Some(handle),
            }
        }

        fn url(&self) -> String {
            format!("ws://127.0.0.1:{}", self.port)
        }
    }

    impl Drop for DeafRelay {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    #[tokio::test]
    async fn a_connected_relay_receives_a_relay_message() {
        let server = EchoRelay::start();
        let mut relay = NostrRelay::new(&server.url()).await.unwrap();

        match relay.recv().await.expect("the greeting arrived") {
            nostro2::NostrRelayEvent::Notice(_, text) => assert_eq!(text, "welcome"),
            other => panic!("expected the greeting notice, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_sent_message_reaches_the_relay() {
        let server = EchoRelay::start();
        let relay = NostrRelay::new(&server.url()).await.unwrap();

        relay
            .send(nostro2::NostrClientEvent::close_subscription("sub-alpha"))
            .unwrap();
        assert!(server.saw("sub-alpha"), "the relay never saw the frame");
    }

    // Readers compete rather than each seeing every message, and a reader
    // claims a batch at a time. A clone therefore continues the stream from
    // wherever the shared cursor is, and must not be expected to replay what
    // another reader already claimed.
    #[tokio::test]
    async fn readers_share_one_stream_without_duplicating_it() {
        let server = EchoRelay::start();
        let relay = NostrRelay::new(&server.url()).await.unwrap();
        let mut first = relay.clone();
        let mut second = relay.clone();

        let from_first = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            first.recv(),
        )
        .await
        .ok()
        .flatten();
        let from_second = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            second.recv(),
        )
        .await
        .ok()
        .flatten();

        let delivered = usize::from(from_first.is_some()) + usize::from(from_second.is_some());
        assert_eq!(
            delivered, 1,
            "the greeting must reach exactly one reader, not both and not neither"
        );
    }

    #[tokio::test]
    async fn a_refused_upgrade_fails_the_connection() {
        let server = DeafRelay::start();
        let outcome = NostrRelay::with_reconnect(
            &server.url(),
            ReconnectConfig::disabled(),
        )
        .await;
        assert!(matches!(
            outcome,
            Err(crate::errors::NostrRelayError::Connect(_))
        ));
    }

    #[tokio::test]
    async fn a_bad_url_is_rejected_before_any_thread_starts() {
        let outcome = NostrRelay::new("not-a-relay-url").await;
        assert!(matches!(outcome, Err(crate::errors::NostrRelayError::Url(_))));
    }

    #[test]
    fn a_closed_relay_stops_its_reader() {
        let server = EchoRelay::start();
        let relay = NostrRelay::detached(&server.url(), ReconnectConfig::disabled()).unwrap();
        let mut reader = relay.clone();

        let joined = std::thread::spawn(move || while reader.recv_blocking().is_some() {});
        std::thread::sleep(std::time::Duration::from_millis(200));
        relay.close();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !joined.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "close did not release a parked reader"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        joined.join().unwrap();
    }

    #[test]
    fn a_dropped_relay_stops_its_thread() {
        let server = EchoRelay::start();
        let relay = NostrRelay::detached(&server.url(), ReconnectConfig::disabled()).unwrap();
        let observer = relay.clone();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!observer.is_closed());

        drop(relay);
        observer.close();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !observer.is_closed() {
            assert!(
                std::time::Instant::now() < deadline,
                "the driver thread outlived every handle"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    // Live-relay smoke test. Connects to a real wss endpoint and
    // streams an open subscription with no termination condition, so
    // it hangs `cargo test` indefinitely. Kept around for manual
    // verification (`cargo test -p nostro2-relay -- --ignored`); not
    // part of the default suite.
    #[tokio::test]
    #[ignore = "live relay; manual run only"]
    async fn test_relay() {
        let time = std::time::Instant::now();
        println!("Connecting to relay...");
        let mut relay = NostrRelay::new("wss://relay.illuminodes.com")
            .await
            .unwrap();
        let subscription = nostro2::NostrSubscription {
            kinds: Some(vec![20001].into_iter().collect()),
            ..Default::default()
        };
        relay.send(subscription).unwrap();
        println!("Connected in {:?}", time.elapsed());
        while let Some(msg) = relay.recv().await {
            println!("{msg:?}");
        }
        println!("Done in {:?}", time.elapsed());
    }

    #[test]
    fn error_display_and_source() {
        use std::error::Error as _;
        let e = crate::errors::NostrRelayError::SendError;
        assert_eq!(e.to_string(), "the relay connection is not accepting messages");
        assert!(e.source().is_none());

        let inner = crate::json::RelayJson::dummy_err();
        let e = crate::errors::NostrRelayError::Serde(inner);
        assert!(e.to_string().contains("serialization error"));
        assert!(e.source().is_some());
    }
}
