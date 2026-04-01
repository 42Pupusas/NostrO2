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

/// Handle for sending messages to connected clients.
///
/// Cloneable — send from any thread.
#[derive(Clone)]
pub struct ServerSender {
    tx: Producer<WriteCmd>,
    writer_thread: std::thread::Thread,
}

impl ServerSender {
    /// Send a text message to a specific client.
    pub fn send_text(&self, client_id: i32, text: String) -> Result<(), String> {
        self.tx
            .push(WriteCmd::SendText { fd: client_id, text })
            .map_err(|_| "write command ring full".to_string())?;
        self.writer_thread.unpark();
        Ok(())
    }

    /// Send a binary message to a specific client.
    pub fn send_binary(&self, client_id: i32, data: Vec<u8>) -> Result<(), String> {
        self.tx
            .push(WriteCmd::SendBinary { fd: client_id, data })
            .map_err(|_| "write command ring full".to_string())?;
        self.writer_thread.unpark();
        Ok(())
    }

    /// Broadcast a text message to all connected clients.
    pub fn broadcast(&self, text: String) -> Result<(), String> {
        self.tx
            .push(WriteCmd::Broadcast { text })
            .map_err(|_| "write command ring full".to_string())?;
        self.writer_thread.unpark();
        Ok(())
    }

    /// Close a specific client connection.
    pub fn close_client(&self, client_id: i32) -> Result<(), String> {
        self.tx
            .push(WriteCmd::Close { fd: client_id })
            .map_err(|_| "write command ring full".to_string())?;
        self.writer_thread.unpark();
        Ok(())
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
/// Spawns an accept loop, reader IO thread, and writer IO thread.
/// All lock-free, all io_uring.
pub struct WsServer {
    consumer: ServerConsumer,
    sender: ServerSender,
    port: u16,
    listener_fd: i32,
    listener_thread: Option<std::thread::JoinHandle<()>>,
    reader_thread: Option<std::thread::JoinHandle<()>>,
    writer_thread: Option<std::thread::JoinHandle<()>>,
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
        // Create listener socket on the main thread so we can query the port
        let listener_sock = listener::setup_listener(addr, port)?;
        let bound_addr = listener_sock.local_addr()?;
        let actual_port = u16::from_be(bound_addr.sin_port);
        let listener_fd = listener_sock.into_fd();

        let shutdown = Arc::new(AtomicBool::new(false));

        // MPSC ring: listener + reader threads push ClientMessages, consumer pops
        let event_capacity = max_clients * 64;
        let (event_tx, event_rx) = RingBuffer::new(Capacity::at_least(event_capacity)).split();
        let consumer = ServerConsumer::new(event_rx);
        let consumer_waker = consumer.waker();

        // MPSC ring: ServerSender + reader push WriteCmds, writer pops.
        // Registration and send commands share one ring for FIFO ordering.
        let (write_tx, write_rx) =
            RingBuffer::new(Capacity::at_least(max_clients * 16)).split();

        // SPSC ring: listener → reader (new client fds after handshake)
        let (accept_tx, accept_rx) =
            spsc::RingBuffer::new(Capacity::at_least(max_clients)).split();

        // Spawn writer thread
        let writer_shutdown = Arc::clone(&shutdown);

        let writer_thread = std::thread::Builder::new()
            .name("ws-writer".into())
            .spawn(move || {
                writer::writer_thread(write_rx, writer_shutdown);
            })?;

        let writer_thread_handle = writer_thread.thread().clone();

        let sender = ServerSender {
            tx: write_tx.clone(),
            writer_thread: writer_thread_handle.clone(),
        };

        // Spawn reader thread — shares write_tx with ServerSender
        let reader_event_tx = event_tx;
        let reader_write_tx = write_tx;
        let reader_shutdown = Arc::clone(&shutdown);
        let reader_consumer_waker = Arc::clone(&consumer_waker);

        let reader_thread = std::thread::Builder::new()
            .name("ws-reader".into())
            .spawn(move || {
                reader::reader_thread(
                    accept_rx,
                    reader_event_tx,
                    reader_write_tx,
                    reader_consumer_waker,
                    writer_thread_handle,
                    reader_shutdown,
                );
            })?;

        // Spawn listener/accept thread
        let listener_shutdown = Arc::clone(&shutdown);

        let listener_thread = std::thread::Builder::new()
            .name("ws-listener".into())
            .spawn(move || {
                listener::listener_thread(
                    listener_fd,
                    accept_tx,
                    listener_shutdown,
                );
            })?;

        Ok(Self {
            consumer,
            sender,
            port: actual_port,
            listener_fd,
            listener_thread: Some(listener_thread),
            reader_thread: Some(reader_thread),
            writer_thread: Some(writer_thread),
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

    /// When we see a Connected event, register the fd with the writer.
    /// Because this runs on the consumer thread (the same thread that
    /// calls send_text/broadcast), the Register command is guaranteed
    /// to be in the writer's ring before any subsequent SendText/Broadcast.
    fn register_if_connected(&self, msg: &ClientMessage) {
        if let ClientMessage::Connected { client_id } = msg {
            let _ = self.sender.tx.push(WriteCmd::Register { fd: *client_id });
            self.sender.writer_thread.unpark();
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
        // IO threads blocked in submit_and_wait are woken by the socket
        // shutdown (listener) or timeout SQE (reader).
        if let Some(h) = self.listener_thread.as_ref() {
            h.thread().unpark();
        }
        if let Some(h) = self.reader_thread.as_ref() {
            h.thread().unpark();
        }
        if let Some(h) = self.writer_thread.as_ref() {
            h.thread().unpark();
        }

        if let Some(h) = self.listener_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.reader_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.writer_thread.take() {
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
