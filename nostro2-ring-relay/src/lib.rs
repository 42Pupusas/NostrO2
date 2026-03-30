use nostro2::NostrRelayEvent;
use nostro2_cache::Cache;
use quetzalcoatl::broadcast;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, Producer, RingBuffer};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

#[cfg(feature = "uring")]
pub mod uring;

/// Messages that flow through the ring buffer from relay threads to consumer
#[derive(Debug, Clone)]
pub enum PoolMessage {
    /// Event received from a relay
    RelayEvent {
        /// URL of the relay that sent this event
        relay_url: String,
        /// The actual relay event
        event: NostrRelayEvent,
    },
    /// Connection error or closed
    ConnectionClosed {
        relay_url: String,
        error: Option<String>,
    },
}

/// Sender handle for broadcasting messages to all connected relays.
///
/// Clone this to send from multiple threads — the broadcast Producer uses CAS
/// internally, so concurrent pushes are lock-free.
#[derive(Clone)]
pub struct PoolSender {
    producer: broadcast::Producer<String>,
}

impl PoolSender {
    /// Send a `NostrClientEvent` to all connected relays.
    ///
    /// Serializes to JSON once; each relay thread sends the pre-serialized string.
    /// Returns `Err` if the broadcast ring is full (all relay threads behind).
    pub fn send<T: Into<nostro2::NostrClientEvent>>(&self, msg: T) -> Result<(), String> {
        let client_event: nostro2::NostrClientEvent = msg.into();
        let json = serde_json::to_string(&client_event).map_err(|e| e.to_string())?;
        self.producer.push(json)
    }

    /// Send a raw pre-serialized JSON string to all relays.
    ///
    /// Use this when you've already serialized the message.
    pub fn send_raw(&self, json: String) -> Result<(), String> {
        self.producer.push(json)
    }
}

/// Handle to a relay WebSocket connection running in its own thread.
///
/// Each connection runs in a dedicated OS thread with non-blocking I/O.
/// The thread can be signaled to shut down via an atomic flag.
pub struct RelayConnection {
    relay_url: String,
    thread_handle: Option<std::thread::JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl RelayConnection {
    /// Spawn a new thread that connects to a relay with bidirectional messaging.
    ///
    /// The thread reads inbound events into the MPSC ring buffer and sends
    /// outbound messages from the broadcast consumer to the WebSocket.
    pub fn spawn(
        relay_url: String,
        mut producer: Producer<PoolMessage>,
        outbound: broadcast::Consumer<String>,
        shutdown: Arc<AtomicBool>,
    ) -> Self {
        let url = relay_url.clone();
        let shutdown_clone = Arc::clone(&shutdown);
        let thread_handle = std::thread::spawn(move || {
            match Self::run_connection(&url, &mut producer, outbound, &shutdown_clone) {
                Ok(()) => {
                    let _ = producer.push(PoolMessage::ConnectionClosed {
                        relay_url: url.clone(),
                        error: None,
                    });
                }
                Err(e) => {
                    let _ = producer.push(PoolMessage::ConnectionClosed {
                        relay_url: url.clone(),
                        error: Some(e.to_string()),
                    });
                }
            }
        });

        Self {
            relay_url,
            thread_handle: Some(thread_handle),
            shutdown,
        }
    }

    /// Returns `true` if the connection thread has exited.
    pub fn is_finished(&self) -> bool {
        self.thread_handle
            .as_ref()
            .is_some_and(|h| h.is_finished())
    }

    /// Signal the connection thread to shut down gracefully.
    ///
    /// The thread will send a WebSocket Close frame and exit within one
    /// poll cycle (~1ms). Does not block.
    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    /// Signal shutdown and block until the thread exits.
    fn shutdown_and_join(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }

    /// Main connection loop — non-blocking, multiplexed read/write.
    ///
    /// 1. Connects and performs WebSocket handshake (blocking)
    /// 2. Switches to non-blocking mode
    /// 3. Loops: check shutdown → try read inbound → drain outbound → sleep if idle
    fn run_connection(
        url: &str,
        producer: &mut Producer<PoolMessage>,
        mut outbound: broadcast::Consumer<String>,
        shutdown: &AtomicBool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Install default crypto provider for this thread (required for rustls 0.23+)
        let _ = rustls::crypto::ring::default_provider().install_default();

        let (mut socket, _response) = connect(url)?;

        // Switch to non-blocking for the multiplexed loop
        set_nonblocking(&socket, true)?;

        loop {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            let mut had_work = false;

            // 1. Try reading inbound (returns WouldBlock instantly if empty)
            match socket.read() {
                Ok(Message::Text(text)) => {
                    if let Ok(event) = text.parse::<NostrRelayEvent>() {
                        let mut pool_msg = PoolMessage::RelayEvent {
                            relay_url: url.to_string(),
                            event,
                        };
                        loop {
                            match producer.push(pool_msg) {
                                Ok(()) => break,
                                Err(returned) => {
                                    pool_msg = returned;
                                    std::hint::spin_loop();
                                }
                            }
                        }
                    }
                    had_work = true;
                }
                Ok(Message::Close(_)) => break,
                Ok(Message::Ping(data)) => {
                    // Pong may WouldBlock — data is buffered internally by tungstenite
                    // and will flush on the next successful I/O operation
                    let _ = socket.send(Message::Pong(data));
                    had_work = true;
                }
                Ok(_) => {
                    had_work = true;
                }
                Err(tungstenite::Error::Io(ref e))
                    if e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    // No data available — fall through to check outbound
                }
                Err(e) => return Err(e.into()),
            }

            // 2. Drain outbound broadcast messages
            while let Some(json) = outbound.pop() {
                match socket.send(Message::Text(json.into())) {
                    Ok(()) => {
                        had_work = true;
                    }
                    Err(tungstenite::Error::Io(ref e))
                        if e.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        // Write buffer full — frame is in tungstenite's internal buffer,
                        // will flush on next successful I/O. Stop draining to avoid
                        // growing the buffer unboundedly.
                        had_work = true;
                        break;
                    }
                    Err(e) => return Err(e.into()),
                }
            }

            // 3. Avoid burning CPU when idle
            if !had_work {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }

        Ok(())
    }

    /// Get the relay URL
    pub fn relay_url(&self) -> &str {
        &self.relay_url
    }
}

impl Drop for RelayConnection {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

/// Set non-blocking mode on the underlying TCP stream through tungstenite's layers.
fn set_nonblocking(
    socket: &WebSocket<MaybeTlsStream<TcpStream>>,
    nonblocking: bool,
) -> std::io::Result<()> {
    match socket.get_ref() {
        MaybeTlsStream::Plain(tcp) => tcp.set_nonblocking(nonblocking),
        MaybeTlsStream::Rustls(tls) => tls.get_ref().set_nonblocking(nonblocking),
        _ => Ok(()),
    }
}

/// Consumer side of the pool - reads events from all relays in a single thread
pub struct PoolConsumer {
    consumer: Consumer<PoolMessage>,
    dedup_cache: Cache,
}

impl PoolConsumer {
    /// Create a new pool consumer with deduplication cache
    pub fn new(consumer: Consumer<PoolMessage>, cache_size: usize) -> Self {
        Self {
            consumer,
            dedup_cache: Cache::new(cache_size),
        }
    }

    /// Receive the next message from any relay (non-blocking)
    ///
    /// Returns `Some(message)` if available and not a duplicate, `None` if ring buffer is empty
    /// Automatically deduplicates NewNote events based on event ID
    pub fn try_recv(&mut self) -> Option<PoolMessage> {
        loop {
            match self.consumer.pop()? {
                PoolMessage::RelayEvent {
                    relay_url,
                    event: NostrRelayEvent::NewNote(tag, sub_id, note),
                } => {
                    // Check for duplicate event ID
                    if let Some(ref event_id) = note.id {
                        if self.dedup_cache.insert(event_id.clone()) {
                            // New event, return it
                            return Some(PoolMessage::RelayEvent {
                                relay_url,
                                event: NostrRelayEvent::NewNote(tag, sub_id, note),
                            });
                        }
                        // Duplicate, continue to next message
                        continue;
                    }
                    // No ID, pass through
                    return Some(PoolMessage::RelayEvent {
                        relay_url,
                        event: NostrRelayEvent::NewNote(tag, sub_id, note),
                    });
                }
                other => {
                    // Pass through non-NewNote messages
                    return Some(other);
                }
            }
        }
    }

    /// Blocking receive - spins until a message is available
    ///
    /// This is the main event loop for the consumer thread
    /// Automatically deduplicates NewNote events based on event ID
    pub fn recv(&mut self) -> PoolMessage {
        loop {
            if let Some(msg) = self.try_recv() {
                return msg;
            }
            std::hint::spin_loop();
        }
    }
}

/// The relay pool — manages multiple WebSocket connections with bidirectional messaging.
///
/// Inbound events flow through an MPSC ring buffer with deduplication.
/// Outbound messages are broadcast to all relay threads via a lock-free broadcast ring.
pub struct RelayPool {
    connections: Vec<RelayConnection>,
    consumer: PoolConsumer,
    sender: PoolSender,
    broadcast_consumer: broadcast::Consumer<String>,
    mpsc_producer: Producer<PoolMessage>,
}

impl RelayPool {
    /// Create a new relay pool with bidirectional messaging.
    ///
    /// # Arguments
    /// * `ring_capacity` - MPSC ring buffer size for inbound event throughput
    /// * `cache_size` - Deduplication cache size (e.g. 10,000)
    /// * `broadcast_capacity` - Broadcast ring buffer size for outbound messages
    /// * `max_relays` - Maximum number of relay connections (broadcast consumer slots)
    pub fn new(
        ring_capacity: usize,
        cache_size: usize,
        broadcast_capacity: usize,
        max_relays: usize,
    ) -> Self {
        let (mpsc_producer, mpsc_consumer) =
            RingBuffer::new(Capacity::at_least(ring_capacity)).split();
        // +1 because split() creates the template consumer that we clone per relay
        let (bc_producer, bc_consumer) =
            broadcast::RingBuffer::new(Capacity::at_least(broadcast_capacity), max_relays + 1)
                .split();
        Self {
            connections: Vec::new(),
            consumer: PoolConsumer::new(mpsc_consumer, cache_size),
            sender: PoolSender {
                producer: bc_producer,
            },
            broadcast_consumer: bc_consumer,
            mpsc_producer,
        }
    }

    /// Add a relay connection to the pool.
    ///
    /// Spawns a new thread that connects to the relay, reads inbound events,
    /// and sends outbound messages from the broadcast ring.
    ///
    /// Automatically cleans up dead connections first to free broadcast slots.
    pub fn add_relay(&mut self, relay_url: String) {
        self.cleanup();
        let shutdown = Arc::new(AtomicBool::new(false));
        let bc_consumer = self.broadcast_consumer.clone();
        let mpsc_producer = self.mpsc_producer.clone();
        let connection =
            RelayConnection::spawn(relay_url, mpsc_producer, bc_consumer, shutdown);
        self.connections.push(connection);
    }

    /// Remove a relay from the pool by URL.
    ///
    /// Signals the relay thread to shut down and blocks until it exits (~1-2ms).
    /// The broadcast consumer slot is freed immediately.
    ///
    /// Returns `true` if the relay was found and removed.
    pub fn remove_relay(&mut self, relay_url: &str) -> bool {
        if let Some(pos) = self
            .connections
            .iter()
            .position(|c| c.relay_url == relay_url)
        {
            let mut conn = self.connections.swap_remove(pos);
            conn.shutdown_and_join();
            true
        } else {
            false
        }
    }

    /// Remove dead connections from the pool.
    ///
    /// Joins finished threads and frees their broadcast consumer slots.
    /// Called automatically by [`add_relay`], but can be called explicitly
    /// to update [`connection_count`].
    pub fn cleanup(&mut self) {
        self.connections.retain_mut(|conn| {
            if conn.is_finished() {
                if let Some(handle) = conn.thread_handle.take() {
                    let _ = handle.join();
                }
                false
            } else {
                true
            }
        });
    }

    /// Get a cloneable sender handle for broadcasting to all relays.
    ///
    /// Multiple threads can hold a `PoolSender` and send concurrently.
    pub fn sender(&self) -> PoolSender {
        self.sender.clone()
    }

    /// Receive the next event from any relay (blocking)
    pub fn recv(&mut self) -> PoolMessage {
        self.consumer.recv()
    }

    /// Receive the next event from any relay (non-blocking)
    pub fn try_recv(&mut self) -> Option<PoolMessage> {
        self.consumer.try_recv()
    }

    /// Get the total number of connections (including dead ones not yet cleaned up).
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Get the number of connections whose threads are still running.
    pub fn active_connection_count(&self) -> usize {
        self.connections.iter().filter(|c| !c.is_finished()).count()
    }

    /// Get the relay URLs of all connections in the pool.
    pub fn relay_urls(&self) -> Vec<&str> {
        self.connections.iter().map(|c| c.relay_url.as_str()).collect()
    }

    /// Get the relay URLs of only active (thread still running) connections.
    pub fn active_relay_urls(&self) -> Vec<&str> {
        self.connections
            .iter()
            .filter(|c| !c.is_finished())
            .map(|c| c.relay_url.as_str())
            .collect()
    }
}

impl Drop for RelayPool {
    fn drop(&mut self) {
        // Phase 1: Signal all threads to shut down (they exit in parallel)
        for conn in &self.connections {
            conn.request_shutdown();
        }
        // Phase 2: Join all threads
        for conn in &mut self.connections {
            if let Some(handle) = conn.thread_handle.take() {
                let _ = handle.join();
            }
        }
    }
}

/// Helper function to create a new pool and producer for spawning connections
///
/// # Arguments
/// * `ring_capacity` - Ring buffer size for event throughput
/// * `cache_size` - Deduplication cache size
pub fn create_pool(ring_capacity: usize, cache_size: usize) -> (PoolConsumer, Producer<PoolMessage>) {
    let (producer, consumer) = RingBuffer::new(Capacity::at_least(ring_capacity)).split();
    (PoolConsumer::new(consumer, cache_size), producer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_creation() {
        let pool = RelayPool::new(1024, 10_000, 64, 8);
        assert_eq!(pool.connection_count(), 0);
    }

    #[test]
    fn test_create_pool_helper() {
        let (_consumer, _producer) = create_pool(1024, 10_000);
        // Just testing that it compiles and runs
    }

    #[test]
    fn test_pool_sender_clone_and_broadcast() {
        let (bc_producer, mut c1) =
            broadcast::RingBuffer::<String>::new(Capacity::exact(16), 4).split();
        let mut c2 = c1.clone();

        let sender = PoolSender {
            producer: bc_producer,
        };
        let sender2 = sender.clone();

        // Send from two different senders
        sender.send_raw("hello".to_string()).unwrap();
        sender2.send_raw("world".to_string()).unwrap();

        // Both consumers see both messages
        assert_eq!(c1.pop(), Some("hello".to_string()));
        assert_eq!(c1.pop(), Some("world".to_string()));
        assert_eq!(c2.pop(), Some("hello".to_string()));
        assert_eq!(c2.pop(), Some("world".to_string()));
    }

    #[test]
    fn test_pool_sender_via_relay_pool() {
        let pool = RelayPool::new(1024, 10_000, 64, 8);
        let sender = pool.sender();
        let sender2 = pool.sender();

        // Both senders are valid clones
        assert!(!sender.producer.is_full());
        assert!(!sender2.producer.is_full());
    }

    #[test]
    fn test_shutdown_flag_stops_thread() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);
        let handle = std::thread::spawn(move || {
            while !shutdown_clone.load(Ordering::Relaxed) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        assert!(!handle.is_finished());
        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();
    }

    #[test]
    fn test_cleanup_removes_dead_connections() {
        // Connect to an invalid address — thread will fail fast
        let mut pool = RelayPool::new(1024, 10_000, 64, 8);
        pool.add_relay("ws://127.0.0.1:1".to_string());
        assert_eq!(pool.connection_count(), 1);

        // Wait for the thread to fail and exit
        std::thread::sleep(std::time::Duration::from_millis(500));

        pool.cleanup();
        assert_eq!(pool.connection_count(), 0);
    }

    #[test]
    fn test_remove_relay() {
        let mut pool = RelayPool::new(1024, 10_000, 64, 8);
        pool.add_relay("ws://127.0.0.1:1".to_string());
        assert_eq!(pool.connection_count(), 1);

        assert!(pool.remove_relay("ws://127.0.0.1:1"));
        assert_eq!(pool.connection_count(), 0);

        // Removing a non-existent relay returns false
        assert!(!pool.remove_relay("ws://127.0.0.1:2"));
    }

    #[test]
    fn test_active_connection_count() {
        let mut pool = RelayPool::new(1024, 10_000, 64, 8);
        // Invalid address — thread will die quickly
        pool.add_relay("ws://127.0.0.1:1".to_string());
        pool.add_relay("ws://127.0.0.1:2".to_string());
        assert_eq!(pool.connection_count(), 2);

        // Wait for threads to fail
        std::thread::sleep(std::time::Duration::from_millis(500));

        // connection_count still 2 (stale), active_connection_count is 0
        assert_eq!(pool.connection_count(), 2);
        assert_eq!(pool.active_connection_count(), 0);

        // cleanup brings connection_count in sync
        pool.cleanup();
        assert_eq!(pool.connection_count(), 0);
    }

    #[test]
    fn test_relay_urls() {
        let mut pool = RelayPool::new(1024, 10_000, 64, 8);
        pool.add_relay("ws://127.0.0.1:1".to_string());
        pool.add_relay("ws://127.0.0.1:2".to_string());

        let urls = pool.relay_urls();
        assert_eq!(urls.len(), 2);
        assert!(urls.contains(&"ws://127.0.0.1:1"));
        assert!(urls.contains(&"ws://127.0.0.1:2"));
    }

    #[test]
    fn test_pool_drop_joins_threads() {
        let mut pool = RelayPool::new(1024, 10_000, 64, 8);
        pool.add_relay("ws://127.0.0.1:1".to_string());
        pool.add_relay("ws://127.0.0.1:2".to_string());
        // Drop should signal shutdown and join — no panic
        drop(pool);
    }

    #[test]
    fn test_add_after_remove_reuses_slots() {
        // max_relays=2 means only 2 broadcast consumer slots available
        let mut pool = RelayPool::new(1024, 10_000, 64, 2);
        pool.add_relay("ws://127.0.0.1:1".to_string());
        pool.add_relay("ws://127.0.0.1:2".to_string());

        // Remove one — frees a broadcast consumer slot via blocking join
        pool.remove_relay("ws://127.0.0.1:1");
        assert_eq!(pool.connection_count(), 1);

        // Add a new relay — should reuse the freed slot without panic
        pool.add_relay("ws://127.0.0.1:3".to_string());
        assert!(pool.relay_urls().contains(&"ws://127.0.0.1:3"));
    }
}
