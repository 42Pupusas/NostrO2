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

use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, Producer, RingBuffer};
use quetzalcoatl::spsc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    },
    /// A client disconnected.
    Disconnected {
        client_id: i32,
        reason: Option<String>,
    },
}

/// Response from an inline message handler.
///
/// Returned by the callback passed to [`WsServer::set_handler`].
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

/// Commands sent to the writer thread.
#[derive(Debug, Clone)]
pub(crate) enum WriteCmd {
    /// Register a new client fd with the writer.
    Register { fd: i32 },
    /// Send a text frame to a specific client.
    SendText { fd: i32, text: String },
    /// Send a binary frame to a specific client.
    SendBinary { fd: i32, data: Vec<u8> },
    /// Send a text frame to all connected clients.
    Broadcast { text: String },
    /// Send a close frame to a specific client.
    Close { fd: i32 },
    /// Send a pong to a specific client.
    Pong { fd: i32 },
}

/// Configuration for reader/writer thread sharding.
///
/// Defaults to 1 shard each (single-threaded, original behavior).
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

    /// Broadcast a text message to all connected clients.
    pub fn broadcast(&self, text: String) {
        for shard in 0..self.num_writer_shards() {
            self.push_with_backpressure(shard, WriteCmd::Broadcast { text: text.clone() });
        }
    }

    /// Close a specific client connection.
    pub fn close_client(&self, client_id: i32) {
        let shard = self.writer_shard(client_id);
        self.push_with_backpressure(shard, WriteCmd::Close { fd: client_id });
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

/// The WebSocket server.
///
/// Spawns an accept loop, reader IO thread(s), and writer IO thread(s).
/// All lock-free, all io_uring. Use [`ShardConfig`] to spread load
/// across multiple cores.
pub struct WsServer {
    consumer: ServerConsumer,
    sender: ServerSender,
    port: u16,
    listener_fd: i32,
    listener_thread: Option<std::thread::JoinHandle<()>>,
    reader_threads: Vec<std::thread::JoinHandle<()>>,
    writer_threads: Vec<std::thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
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
        Self::bind_inner(addr, port, max_clients, config, None)
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
        Self::bind_inner(addr, port, max_clients, config, Some(Arc::new(handler)))
    }

    fn bind_inner(
        addr: [u8; 4],
        port: u16,
        max_clients: usize,
        config: ShardConfig,
        handler: Option<Arc<Handler>>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let n = config.reader_shards.max(1);
        let m = config.writer_shards.max(1);

        // Create listener socket on the main thread so we can query the port
        let listener_sock = listener::setup_listener(addr, port)?;
        let bound_addr = listener_sock.local_addr()?;
        let actual_port = u16::from_be(bound_addr.sin_port);
        let listener_fd = listener_sock.into_fd();

        let shutdown = Arc::new(AtomicBool::new(false));

        // MPSC ring: all reader shards push ClientMessages, consumer pops.
        // MPSC Producer is Clone, so each reader shard gets a clone.
        let event_capacity = max_clients * 64;
        let (event_tx, event_rx) = RingBuffer::new(Capacity::at_least(event_capacity)).split();
        let consumer = ServerConsumer::new(event_rx);
        let consumer_waker = consumer.waker();

        // M MPSC rings: one per writer shard.
        // ServerSender + reader shards push WriteCmds, each writer shard pops its own ring.
        // Each shard gets the FULL capacity — don't divide by M, because the
        // echo thread (or any single-threaded consumer) pushes to shards
        // sequentially and any one shard blocking causes head-of-line blocking
        // for all shards.
        let mut writer_txs: Vec<Producer<WriteCmd>> = Vec::with_capacity(m);
        let mut writer_rxs = Vec::with_capacity(m);
        for _ in 0..m {
            let cap = (max_clients * 16).max(1024);
            let (tx, rx) = RingBuffer::new(Capacity::at_least(cap)).split();
            writer_txs.push(tx);
            writer_rxs.push(rx);
        }

        // N SPSC rings: listener → reader_shard[i]
        let mut accept_txs = Vec::with_capacity(n);
        let mut accept_rxs = Vec::with_capacity(n);
        for _ in 0..n {
            let cap = (max_clients / n).max(64);
            let (tx, rx) = spsc::RingBuffer::new(Capacity::at_least(cap)).split();
            accept_txs.push(tx);
            accept_rxs.push(rx);
        }

        // Spawn M writer threads
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

        // Spawn N reader threads
        let mut reader_threads = Vec::with_capacity(n);
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

        // Spawn listener/accept thread
        let listener_shutdown = Arc::clone(&shutdown);
        let listener_thread = std::thread::Builder::new()
            .name("ws-listener".into())
            .spawn(move || {
                listener::listener_thread(listener_fd, accept_txs, listener_shutdown);
            })?;

        Ok(Self {
            consumer,
            sender,
            port: actual_port,
            listener_fd,
            listener_thread: Some(listener_thread),
            reader_threads,
            writer_threads,
            shutdown,
        })
    }

    /// The port the server is listening on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Get a cloneable sender handle.
    pub fn sender(&self) -> ServerSender {
        self.sender.clone()
    }

    /// Blocking receive — wait for the next client message.
    ///
    /// When a `Connected` event is returned, the client is already registered
    /// with the writer thread, so `send_text` / `broadcast` will reach it.
    pub fn recv(&mut self) -> ClientMessage {
        let msg = self.consumer.recv();
        self.register_if_connected(&msg);
        msg
    }

    /// Non-blocking receive.
    pub fn try_recv(&mut self) -> Option<ClientMessage> {
        let msg = self.consumer.try_recv();
        if let Some(ref m) = msg {
            self.register_if_connected(m);
        }
        msg
    }

    /// When we see a Connected event, register the fd with the correct
    /// writer shard.  Because this runs on the consumer thread (the same
    /// thread that calls send_text/broadcast), the Register command is
    /// guaranteed to be in the writer's ring before any subsequent
    /// SendText/Broadcast for this fd.
    fn register_if_connected(&self, msg: &ClientMessage) {
        if let ClientMessage::Connected { client_id } = msg {
            let shard = self.sender.writer_shard(*client_id);
            let _ = self.sender.writer_txs[shard].push(WriteCmd::Register { fd: *client_id });
            self.sender.writer_wakers[shard].unpark();
        }
    }
}

impl Drop for WsServer {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);

        // Shutdown the listener socket — the in-flight accept SQE completes
        // with an error, the listener thread sees the shutdown flag and exits.
        // The Socket drop then closes the fd.
        {
            let sock = unsafe { ququmatz::Socket::from_fd(self.listener_fd) };
            let _ = sock.shutdown(ququmatz::types::ShutdownHow::Both);
        }

        // Wake parked threads so they notice the shutdown flag.
        if let Some(h) = self.listener_thread.as_ref() {
            h.thread().unpark();
        }
        for h in &self.reader_threads {
            h.thread().unpark();
        }
        for h in &self.writer_threads {
            h.thread().unpark();
        }

        if let Some(h) = self.listener_thread.take() {
            let _ = h.join();
        }
        for h in self.reader_threads.drain(..) {
            let _ = h.join();
        }
        for h in self.writer_threads.drain(..) {
            let _ = h.join();
        }
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
