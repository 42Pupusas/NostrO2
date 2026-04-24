//! Lock-free WebSocket server using io_uring and ring buffers.
//!
//! Architecture:
//! - **Listener thread**: io_uring accept loop → HTTP upgrade handshake → hands fd to reader/writer
//! - **Reader thread**: io_uring recv on all client fds, decodes WebSocket frames (Role::Server)
//! - **Writer thread**: io_uring send, encodes unmasked server→client frames
//! - Inter-thread communication via lock-free MPSC/SPSC ring buffers (quetzalcoatl)

#[cfg(not(target_arch = "x86_64"))]
compile_error!("ring-relay-server requires x86_64 (inline asm syscalls)");

mod listener;
mod reader;
mod writer;

#[cfg(feature = "ktls")]
mod kernel_tls;
#[cfg(feature = "ktls")]
mod syscall;

pub use coyoquil::{CloseCode, DeflateConfig};
pub use reader::{ReaderCore, ReaderEvent};
pub use quetzalcoatl::spsc::Consumer as AcceptedClientRx;

#[cfg(feature = "ktls")]
pub use rustls;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, Producer, RingBuffer};
use quetzalcoatl::spsc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Accepted client info handed from the listener to a reader shard.
///
/// Exposed publicly so [`ServerComponents`] consumers (e.g. a downstream
/// crate running its own per-shard reader loop) can drive [`ReaderCore`]
/// with the listener's accept stream.
#[derive(Debug, Clone)]
pub struct AcceptedClient {
    pub fd: i32,
    pub path: String,
    pub subprotocol: Option<String>,
    pub deflate: Option<DeflateConfig>,
    pub headers: Vec<(String, String)>,
}

/// Messages received from connected WebSocket clients.
#[derive(Debug, Clone)]
pub enum ClientMessage {
    /// A text message from a client.
    Text {
        /// Unique client identifier (the fd).
        client_id: i32,
        /// The text payload.
        text: String,
    },
    /// A binary message from a client.
    Binary {
        client_id: i32,
        data: Vec<u8>,
    },
    /// A new client connected.
    Connected {
        client_id: i32,
        /// The request path (e.g. "/chat").
        path: String,
        /// The negotiated subprotocol, if any.
        subprotocol: Option<String>,
        /// All HTTP headers from the upgrade request.
        headers: Vec<(String, String)>,
    },
    /// A client disconnected.
    Disconnected {
        client_id: i32,
        reason: Option<String>,
        /// The close code sent by the client, if a close frame was received.
        close_code: Option<CloseCode>,
    },
}

/// Response from an inline message handler.
///
/// Returned by the callback passed to [`WsServer::bind_with_handler`].
pub enum HandlerResult {
    /// Send a text frame back to the originating client.
    Reply(String),
    /// Message was handled inline — do not forward to the consumer.
    Consumed,
    /// Pass the message through to the consumer via `try_recv`/`recv`.
    PassThrough,
}

/// A handler function invoked inside the reader thread.
///
/// Receives the client fd and the decoded message. Runs on the IO thread,
/// so it must be fast — no blocking, no heavy computation.
pub(crate) type Handler = dyn Fn(i32, &str) -> HandlerResult + Send + Sync;

/// A non-WebSocket HTTP request seen by the listener.
///
/// The listener invokes [`HttpHandler`] when a complete HTTP request arrives
/// that lacks an `Upgrade: websocket` header. The handler returns the full
/// HTTP response (status line + headers + body) as bytes. The connection is
/// closed after the response is sent.
pub struct HttpRequest<'a> {
    /// Request path, e.g. `/` or `/info`.
    pub path: &'a str,
    /// Request method, typically `GET`.
    pub method: &'a str,
    /// All request headers.
    pub headers: &'a [(String, String)],
}

/// Handler for non-WebSocket HTTP requests. Runs on the listener thread,
/// so it must be fast. Return the full HTTP response bytes; the listener
/// closes the connection after sending.
pub type HttpHandler = dyn Fn(HttpRequest<'_>) -> Vec<u8> + Send + Sync;

/// Commands sent to the writer thread.
#[derive(Debug, Clone)]
pub(crate) enum WriteCmd {
    /// Register a new client fd with the writer.
    Register {
        fd: i32,
        deflate: Option<DeflateConfig>,
    },
    /// Send a text frame to a specific client.
    SendText { fd: i32, text: String },
    /// Send a binary frame to a specific client.
    SendBinary { fd: i32, data: Vec<u8> },
    /// Fan-out helper: send a NIP-01 `["EVENT", sub_id, <note>]` text frame
    /// assembled directly in the writer from pre-captured components.
    ///
    /// Lets the caller amortize the note-body allocation across every
    /// matching subscriber: one `Arc<[u8]>` is produced for the event, then
    /// cloned (refcount bump, no heap traffic) into one `SendEventFrame` per
    /// sub. The writer composes the text payload directly into its WS
    /// send-buffer — no intermediate `String`.
    SendEventFrame {
        fd: i32,
        sub_id: Arc<str>,
        note_bytes: Arc<[u8]>,
    },
    /// Send a text frame to all connected clients.
    Broadcast { text: String },
    /// Send a binary frame to all connected clients.
    BroadcastBinary { data: Vec<u8> },
    /// Send a close frame to a specific client.
    Close { fd: i32, code: CloseCode },
    /// Send a pong to a specific client.
    Pong { fd: i32 },
}

/// Configuration for reader/writer thread sharding.
///
/// Defaults to 1 shard each (single-threaded, original behavior).
#[derive(Debug, Clone)]
pub struct ShardConfig {
    /// Number of reader threads (each owns an io_uring recv ring).
    pub reader_shards: usize,
    /// Number of writer threads (each owns an io_uring send ring).
    pub writer_shards: usize,
}

impl Default for ShardConfig {
    fn default() -> Self {
        Self {
            reader_shards: 1,
            writer_shards: 1,
        }
    }
}

/// Full server configuration.
#[derive(Default)]
pub struct ServerConfig {
    /// Reader/writer thread sharding.
    pub shards: ShardConfig,
    /// Subprotocols the server supports, in priority order.
    /// The first client-offered protocol that matches is selected (RFC 6455 §4.2.2).
    pub subprotocols: Vec<String>,
    /// Deflate compression policy. `None` disables permessage-deflate.
    pub deflate: Option<DeflateConfig>,
    /// Optional handler for plain HTTP requests (no `Upgrade: websocket` header).
    /// Invoked on the listener thread; must return the full response bytes.
    /// The connection is closed after sending. Used for NIP-11 and similar.
    pub http_handler: Option<Arc<HttpHandler>>,
    /// TLS config for kernel-offloaded TLS. When set, every accepted connection
    /// completes a TLS handshake on the listener thread, then the kernel's
    /// TLS engine is armed via `TCP_ULP=tls` + `TLS_TX`/`TLS_RX` setsockopts.
    /// The subsequent io_uring data path sees plaintext.
    ///
    /// The rustls config **must** have `enable_secret_extraction = true`,
    /// otherwise handshake setup will fail.
    #[cfg(feature = "ktls")]
    pub tls: Option<std::sync::Arc<rustls::ServerConfig>>,
}

/// Handle for sending messages to connected clients.
///
/// Cloneable — send from any thread.
#[derive(Clone)]
pub struct ServerSender {
    writer_txs: Vec<Producer<WriteCmd>>,
    writer_wakers: Vec<std::thread::Thread>,
}

impl ServerSender {
    fn num_writer_shards(&self) -> usize {
        self.writer_txs.len()
    }

    fn writer_shard(&self, fd: i32) -> usize {
        fd as usize % self.num_writer_shards()
    }

    /// Send a text message to a specific client.
    ///
    /// Applies backpressure if the write ring is full — blocks until space
    /// is available, never drops.
    pub fn send_text(&self, client_id: i32, text: String) {
        let shard = self.writer_shard(client_id);
        self.push_with_backpressure(shard, WriteCmd::SendText { fd: client_id, text });
    }

    /// Send a binary message to a specific client.
    pub fn send_binary(&self, client_id: i32, data: Vec<u8>) {
        let shard = self.writer_shard(client_id);
        self.push_with_backpressure(shard, WriteCmd::SendBinary { fd: client_id, data });
    }

    /// Queue a NIP-01 `["EVENT", sub_id, <note>]` text frame for delivery,
    /// composed inside the writer from pre-captured pieces.
    ///
    /// Designed for relay-style fan-out where one event is delivered to N
    /// matching subscribers: the caller produces `note_bytes` once (as an
    /// `Arc<[u8]>` sharing the event's JSON), clones it per sub (refcount
    /// bump), and pairs each clone with the subscriber's `sub_id`. The
    /// writer composes the `["EVENT", sub_id, <note>]` payload directly
    /// into its WS send-buffer — no intermediate `String` per sub.
    pub fn send_event_frame(&self, client_id: i32, sub_id: Arc<str>, note_bytes: Arc<[u8]>) {
        let shard = self.writer_shard(client_id);
        self.push_with_backpressure(
            shard,
            WriteCmd::SendEventFrame { fd: client_id, sub_id, note_bytes },
        );
    }

    /// Broadcast a text message to all connected clients.
    pub fn broadcast(&self, text: String) {
        for shard in 0..self.num_writer_shards() {
            self.push_with_backpressure(shard, WriteCmd::Broadcast { text: text.clone() });
        }
    }

    /// Broadcast a binary message to all connected clients.
    pub fn broadcast_binary(&self, data: Vec<u8>) {
        for shard in 0..self.num_writer_shards() {
            self.push_with_backpressure(shard, WriteCmd::BroadcastBinary { data: data.clone() });
        }
    }

    /// Close a specific client connection with the given close code.
    pub fn close_client(&self, client_id: i32, code: CloseCode) {
        let shard = self.writer_shard(client_id);
        self.push_with_backpressure(shard, WriteCmd::Close { fd: client_id, code });
    }

    /// Register a freshly-accepted client fd with the writer shard that owns it.
    ///
    /// Call this from a custom reader loop when a new client arrives via the
    /// listener's `AcceptedClient` stream, before any sends to that fd. The
    /// built-in [`WsServer`] reader does this automatically; external drivers
    /// using [`ServerComponents`] must do it themselves.
    pub fn register(&self, client_id: i32, deflate: Option<DeflateConfig>) {
        let shard = self.writer_shard(client_id);
        self.push_with_backpressure(shard, WriteCmd::Register { fd: client_id, deflate });
    }

    /// Queue a Pong frame to a client in response to a Ping.
    ///
    /// Used by custom reader loops that receive [`ReaderEvent::Ping`] and
    /// must respond. The built-in [`WsServer`] reader does this automatically.
    pub fn pong(&self, client_id: i32) {
        let shard = self.writer_shard(client_id);
        self.push_with_backpressure(shard, WriteCmd::Pong { fd: client_id });
    }

    /// Push a command to the write ring with backpressure.
    ///
    /// Never drops: spins briefly, then yields, then sleeps in escalating
    /// intervals while continuously waking the writer to drain. Backpressure
    /// flows naturally — the caller slows down to the writer's throughput.
    fn push_with_backpressure(&self, shard: usize, mut cmd: WriteCmd) {
        let mut spins = 0u32;
        loop {
            match self.writer_txs[shard].push(cmd) {
                Ok(()) => {
                    self.writer_wakers[shard].unpark();
                    return;
                }
                Err(returned) => {
                    cmd = returned;
                    // Wake the writer so it drains the ring
                    self.writer_wakers[shard].unpark();

                    if spins < 32 {
                        std::hint::spin_loop();
                    } else if spins < 128 {
                        std::thread::yield_now();
                    } else {
                        // Escalate to a real sleep — the writer is genuinely behind.
                        // 10µs is short enough to not add visible latency but long
                        // enough to avoid burning CPU.
                        std::thread::sleep(std::time::Duration::from_micros(10));
                    }
                    spins = spins.saturating_add(1);
                }
            }
        }
    }
}

/// Consumer side — drains client messages from the shared MPSC ring.
pub struct ServerConsumer {
    rx: Consumer<ClientMessage>,
    batch: Vec<ClientMessage>,
    batch_pos: usize,
    parker: Arc<Parker>,
}

impl ServerConsumer {
    fn new(rx: Consumer<ClientMessage>) -> Self {
        Self {
            rx,
            batch: Vec::with_capacity(1024),
            batch_pos: 0,
            parker: Arc::new(Parker::new()),
        }
    }

    pub(crate) fn waker(&self) -> Arc<Parker> {
        Arc::clone(&self.parker)
    }

    /// Non-blocking receive.
    pub fn try_recv(&mut self) -> Option<ClientMessage> {
        // Serve from local batch first
        if self.batch_pos < self.batch.len() {
            let msg = self.batch[self.batch_pos].clone();
            self.batch_pos += 1;
            return Some(msg);
        }

        // Drain more from the ring
        self.batch.clear();
        self.batch_pos = 0;
        self.rx.drain(|msg| self.batch.push(msg));
        if self.batch.is_empty() {
            return None;
        }
        let msg = self.batch[self.batch_pos].clone();
        self.batch_pos += 1;
        Some(msg)
    }

    /// Blocking receive — parks until a message arrives.
    pub fn recv(&mut self) -> ClientMessage {
        loop {
            if let Some(msg) = self.try_recv() {
                return msg;
            }
            self.parker.park();
        }
    }
}

/// Lock-free wake primitive using thread::park/unpark (futex-backed on Linux).
pub(crate) struct Parker {
    thread: std::thread::Thread,
}

impl Parker {
    pub fn new() -> Self {
        Self {
            thread: std::thread::current(),
        }
    }

    pub fn park(&self) {
        std::thread::park();
    }

    pub fn unpark(&self) {
        self.thread.unpark();
    }
}

/// The low-level server components: listener, writer threads, and a
/// [`ServerSender`]. Does **not** spawn reader threads — callers build
/// their own reader loop on top (using [`ReaderCore`] or equivalent).
///
/// Use this when you want the opinionated parts (accept loop, HTTP upgrade,
/// optional kTLS, writer threads with batching and deflate) but want to own
/// the per-message pipeline, so that parsing / application logic runs on
/// the I/O thread that read the bytes.
///
/// For a simpler all-in-one server that spawns default reader threads and
/// delivers `ClientMessage`s via an MPSC ring, use [`WsServer`].
///
/// # Lifetime
///
/// Dropping `ServerComponents` signals shutdown, closes the listener, and
/// joins the listener + writer threads. Any reader threads the caller
/// spawned must be **joined by the caller before** `ServerComponents` is
/// dropped — otherwise those readers will try to push writes into already-
/// dead writer rings.
pub struct ServerComponents {
    sender: ServerSender,
    port: u16,
    listener_fd: i32,
    listener_thread: Option<std::thread::JoinHandle<()>>,
    writer_threads: Vec<std::thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

/// Stream of accepted clients for one reader shard.
///
/// Handed out by [`ServerComponents::build`], one per configured reader shard.
/// Drive a [`ReaderCore`] with these: on each iteration, pop any new
/// `AcceptedClient`s and pass them to [`ReaderCore::accept`].
pub type AcceptStream = AcceptedClientRx<AcceptedClient>;

/// Intermediate state between writer setup and listener startup.
///
/// Returned by [`ServerComponents::prepare`]. The caller spawns its reader
/// threads using the provided [`AcceptStream`]s, then calls
/// [`ServerBuilder::start_listener`] to start accepting clients. This two-phase
/// startup guarantees readers are running before the first accept lands —
/// otherwise there is a window where the kernel has accepted a connection but
/// no reader is yet draining the `AcceptedClient` from its SPSC ring.
pub struct ServerBuilder {
    sender: ServerSender,
    port: u16,
    listener_fd: i32,
    writer_threads: Vec<std::thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
    writer_txs: Vec<Producer<WriteCmd>>,
    writer_wakers: Vec<std::thread::Thread>,
    accept_txs: Vec<spsc::Producer<AcceptedClient>>,
    subprotocols: Vec<String>,
    deflate: Option<DeflateConfig>,
    http_handler: Option<Arc<HttpHandler>>,
    #[cfg(feature = "ktls")]
    tls: Option<std::sync::Arc<rustls::ServerConfig>>,
}

impl ServerBuilder {
    /// The port the listener socket is bound to.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// A cloneable sender handle. Clone it and hand clones to reader threads
    /// so they can emit `WriteCmd`s without going through a shared state.
    pub fn sender(&self) -> ServerSender {
        self.sender.clone()
    }

    /// Shared shutdown flag. Clone it into reader threads so they exit when
    /// it flips to `true`.
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown)
    }

    /// Spawn the listener thread and return the finalized [`ServerComponents`].
    ///
    /// Call this **after** all reader threads have been spawned, so the first
    /// accept never lands on an SPSC ring with no drainer.
    pub fn start_listener(self) -> Result<ServerComponents, Box<dyn std::error::Error + Send + Sync>> {
        let listener_shutdown = Arc::clone(&self.shutdown);
        let listener_fd = self.listener_fd;
        let accept_txs = self.accept_txs;
        let subprotocols = self.subprotocols;
        let deflate = self.deflate;
        let http_handler = self.http_handler;
        #[cfg(feature = "ktls")]
        let tls = self.tls;
        let listener_thread = std::thread::Builder::new()
            .name("ws-listener".into())
            .spawn(move || {
                listener::listener_thread(
                    listener_fd,
                    accept_txs,
                    listener_shutdown,
                    subprotocols,
                    deflate,
                    http_handler,
                    #[cfg(feature = "ktls")]
                    tls,
                );
            })?;

        Ok(ServerComponents {
            sender: self.sender,
            port: self.port,
            listener_fd: self.listener_fd,
            listener_thread: Some(listener_thread),
            writer_threads: self.writer_threads,
            shutdown: self.shutdown,
        })
    }
}

impl ServerComponents {
    /// First phase of startup: bind the listener socket, allocate rings, spawn
    /// writer threads. Returns a [`ServerBuilder`] and one [`AcceptStream`] per
    /// reader shard.
    ///
    /// The caller then:
    /// 1. Spawns its own reader threads, each draining one `AcceptStream`.
    /// 2. Calls [`ServerBuilder::start_listener`] to begin accepting clients.
    ///
    /// Splitting startup in two phases avoids a race where the listener accepts
    /// a client before the reader thread for its shard is running.
    pub fn prepare(
        addr: [u8; 4],
        port: u16,
        max_clients: usize,
        config: ServerConfig,
    ) -> Result<(ServerBuilder, Vec<AcceptStream>), Box<dyn std::error::Error + Send + Sync>> {
        let n = config.shards.reader_shards.max(1);
        let m = config.shards.writer_shards.max(1);

        let listener_sock = listener::setup_listener(addr, port)?;
        let bound_addr = listener_sock.local_addr()?;
        let actual_port = u16::from_be(bound_addr.sin_port);
        let listener_fd = listener_sock.into_fd();

        let shutdown = Arc::new(AtomicBool::new(false));

        // M MPSC rings, one per writer shard. Full capacity per shard so a
        // slow shard doesn't head-of-line-block pushes to other shards.
        let mut writer_txs: Vec<Producer<WriteCmd>> = Vec::with_capacity(m);
        let mut writer_rxs = Vec::with_capacity(m);
        for _ in 0..m {
            let cap = (max_clients * 16).max(1024);
            let (tx, rx) = RingBuffer::new(Capacity::at_least(cap)).split();
            writer_txs.push(tx);
            writer_rxs.push(rx);
        }

        // N SPSC rings: listener → reader_shard[i].
        let mut accept_txs = Vec::with_capacity(n);
        let mut accept_rxs = Vec::with_capacity(n);
        for _ in 0..n {
            let cap = (max_clients / n).max(64);
            let (tx, rx) = spsc::RingBuffer::new(Capacity::at_least(cap)).split();
            accept_txs.push(tx);
            accept_rxs.push(rx);
        }

        // Spawn M writer threads.
        let mut writer_wakers: Vec<std::thread::Thread> = Vec::with_capacity(m);
        let mut writer_threads = Vec::with_capacity(m);
        for (i, write_rx) in writer_rxs.into_iter().enumerate() {
            let ws = Arc::clone(&shutdown);
            let handle = std::thread::Builder::new()
                .name(format!("ws-writer-{i}"))
                .spawn(move || {
                    writer::writer_thread(write_rx, ws);
                })?;
            writer_wakers.push(handle.thread().clone());
            writer_threads.push(handle);
        }

        let sender = ServerSender {
            writer_txs: writer_txs.clone(),
            writer_wakers: writer_wakers.clone(),
        };

        Ok((
            ServerBuilder {
                sender,
                port: actual_port,
                listener_fd,
                writer_threads,
                shutdown,
                writer_txs,
                writer_wakers,
                accept_txs,
                subprotocols: config.subprotocols,
                deflate: config.deflate,
                http_handler: config.http_handler,
                #[cfg(feature = "ktls")]
                tls: config.tls,
            },
            accept_rxs,
        ))
    }

    /// The port the listener is bound to.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// A cloneable sender handle.
    pub fn sender(&self) -> ServerSender {
        self.sender.clone()
    }

    /// Shared shutdown flag. The listener + writer threads exit when this is
    /// set to `true`. Callers should set it before joining their own reader
    /// threads, then drop `ServerComponents` to join listener + writers.
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown)
    }
}

impl Drop for ServerComponents {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);

        // Shutdown the listener socket so its in-flight accept SQE completes.
        {
            let sock = unsafe { ququmatz::Socket::from_fd(self.listener_fd) };
            let _ = sock.shutdown(ququmatz::types::ShutdownHow::Both);
        }

        if let Some(h) = self.listener_thread.as_ref() {
            h.thread().unpark();
        }
        for h in &self.writer_threads {
            h.thread().unpark();
        }

        if let Some(h) = self.listener_thread.take() {
            let _ = h.join();
        }
        for h in self.writer_threads.drain(..) {
            let _ = h.join();
        }
    }
}

/// The WebSocket server.
///
/// Spawns an accept loop, reader IO thread(s), and writer IO thread(s).
/// All lock-free, all io_uring. Use [`ShardConfig`] to spread load
/// across multiple cores. For callers that want to own the per-message
/// pipeline (parse + verify + dispatch on the I/O thread), use
/// [`ServerComponents`] instead.
pub struct WsServer {
    consumer: ServerConsumer,
    components: ServerComponents,
    reader_threads: Vec<std::thread::JoinHandle<()>>,
}

impl WsServer {
    /// Start a WebSocket server on the given address and port.
    ///
    /// Use port `0` to let the OS pick an ephemeral port, then call
    /// [`port()`](Self::port) to retrieve it.
    ///
    /// # Arguments
    /// * `addr` - IPv4 address bytes, e.g. `[0, 0, 0, 0]` for all interfaces
    /// * `port` - TCP port number (0 for OS-assigned)
    /// * `max_clients` - Maximum number of concurrent client connections
    pub fn bind(
        addr: [u8; 4],
        port: u16,
        max_clients: usize,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::bind_sharded(addr, port, max_clients, ShardConfig::default())
    }

    /// Start a sharded WebSocket server.
    ///
    /// Like [`bind`](Self::bind), but spreads client load across multiple
    /// reader and writer threads according to `config`.
    pub fn bind_sharded(
        addr: [u8; 4],
        port: u16,
        max_clients: usize,
        config: ShardConfig,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::bind_inner(
            addr,
            port,
            max_clients,
            ServerConfig { shards: config, ..ServerConfig::default() },
            None,
        )
    }

    /// Start a sharded server with an inline message handler.
    ///
    /// The handler runs inside each reader IO thread — messages that it
    /// handles never cross a thread boundary.  For echo-like workloads
    /// this eliminates the consumer-thread bottleneck entirely.
    ///
    /// The handler receives `(client_id, text)` for every text frame.
    /// Return [`HandlerResult::Reply`] to echo/respond inline,
    /// [`HandlerResult::Consumed`] to swallow the message, or
    /// [`HandlerResult::PassThrough`] to forward it to `try_recv`/`recv`.
    pub fn bind_with_handler(
        addr: [u8; 4],
        port: u16,
        max_clients: usize,
        config: ShardConfig,
        handler: impl Fn(i32, &str) -> HandlerResult + Send + Sync + 'static,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::bind_inner(
            addr,
            port,
            max_clients,
            ServerConfig { shards: config, ..ServerConfig::default() },
            Some(Arc::new(handler)),
        )
    }

    /// Start a server with full configuration.
    ///
    /// Combines sharding, subprotocol negotiation, and deflate compression.
    pub fn bind_with_config(
        addr: [u8; 4],
        port: u16,
        max_clients: usize,
        config: ServerConfig,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::bind_inner(addr, port, max_clients, config, None)
    }

    /// Start a server with full configuration and an inline handler.
    pub fn bind_with_config_and_handler(
        addr: [u8; 4],
        port: u16,
        max_clients: usize,
        config: ServerConfig,
        handler: impl Fn(i32, &str) -> HandlerResult + Send + Sync + 'static,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        Self::bind_inner(addr, port, max_clients, config, Some(Arc::new(handler)))
    }

    fn bind_inner(
        addr: [u8; 4],
        port: u16,
        max_clients: usize,
        config: ServerConfig,
        handler: Option<Arc<Handler>>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (builder, accept_rxs) = ServerComponents::prepare(addr, port, max_clients, config)?;

        // MPSC ring: all reader shards push ClientMessages, consumer pops.
        let event_capacity = max_clients * 64;
        let (event_tx, event_rx) = RingBuffer::new(Capacity::at_least(event_capacity)).split();
        let consumer = ServerConsumer::new(event_rx);
        let consumer_waker = consumer.waker();

        let shutdown = builder.shutdown_flag();
        let writer_txs = builder.writer_txs.clone();
        let writer_wakers = builder.writer_wakers.clone();

        // Spawn readers BEFORE starting the listener, so no accept can land on
        // an empty SPSC ring.
        let mut reader_threads = Vec::with_capacity(accept_rxs.len());
        for (i, accept_rx) in accept_rxs.into_iter().enumerate() {
            let etx = event_tx.clone();
            let wtxs = writer_txs.clone();
            let wwakers = writer_wakers.clone();
            let cw = Arc::clone(&consumer_waker);
            let rs = Arc::clone(&shutdown);
            let h = handler.clone();

            let handle = std::thread::Builder::new()
                .name(format!("ws-reader-{i}"))
                .spawn(move || {
                    reader::reader_thread(accept_rx, etx, wtxs, wwakers, cw, rs, h);
                })?;
            reader_threads.push(handle);
        }

        let components = builder.start_listener()?;

        Ok(Self {
            consumer,
            components,
            reader_threads,
        })
    }

    /// The port the server is listening on.
    pub fn port(&self) -> u16 {
        self.components.port()
    }

    /// Get a cloneable sender handle.
    pub fn sender(&self) -> ServerSender {
        self.components.sender()
    }

    /// Blocking receive — wait for the next client message.
    pub fn recv(&mut self) -> ClientMessage {
        self.consumer.recv()
    }

    /// Non-blocking receive.
    pub fn try_recv(&mut self) -> Option<ClientMessage> {
        self.consumer.try_recv()
    }
}

impl Drop for WsServer {
    fn drop(&mut self) {
        // Set the shared shutdown flag first so readers notice when we unpark.
        self.components.shutdown.store(true, Ordering::Release);

        // Wake parked reader threads so they notice the shutdown flag, then
        // join them. Readers must exit before ServerComponents drops, since
        // they push into writer rings that the components' Drop tears down.
        for h in &self.reader_threads {
            h.thread().unpark();
        }
        for h in self.reader_threads.drain(..) {
            let _ = h.join();
        }
        // ServerComponents' Drop handles listener + writer shutdown/join.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_starts_and_stops() {
        let server = WsServer::bind([127, 0, 0, 1], 0, 64);
        assert!(server.is_ok());
        drop(server);
    }
}
