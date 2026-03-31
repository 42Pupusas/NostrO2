use nostro2::NostrRelayEvent;
use nostro2_cache::Cache;
use quetzalcoatl::broadcast;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, Producer, RingBuffer};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

mod ktls;
mod reader;
mod writer;

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

/// Lightweight handle to a connected relay.
///
/// Does not own any threads — the global IO threads handle all I/O.
/// Holds the fd and shutdown flag for cleanup.
pub struct RelayConnection {
    relay_url: String,
    fd: i32,
    shutdown: Arc<AtomicBool>,
}

impl RelayConnection {
    pub fn relay_url(&self) -> &str {
        &self.relay_url
    }

    pub fn is_finished(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }
}

impl Drop for RelayConnection {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        unsafe {
            libc::shutdown(self.fd, libc::SHUT_RDWR);
            libc::close(self.fd);
        }
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
    /// Returns `Some(message)` if available and not a duplicate, `None` if ring buffer is empty.
    /// Automatically deduplicates NewNote events based on event ID.
    pub fn try_recv(&mut self) -> Option<PoolMessage> {
        loop {
            match self.consumer.pop()? {
                PoolMessage::RelayEvent {
                    relay_url,
                    event: NostrRelayEvent::NewNote(tag, sub_id, note),
                } => {
                    if let Some(ref event_id) = note.id {
                        if self.dedup_cache.insert(event_id.clone()) {
                            return Some(PoolMessage::RelayEvent {
                                relay_url,
                                event: NostrRelayEvent::NewNote(tag, sub_id, note),
                            });
                        }
                        continue;
                    }
                    return Some(PoolMessage::RelayEvent {
                        relay_url,
                        event: NostrRelayEvent::NewNote(tag, sub_id, note),
                    });
                }
                other => return Some(other),
            }
        }
    }

    /// Blocking receive - spins until a message is available
    pub fn recv(&mut self) -> PoolMessage {
        loop {
            if let Some(msg) = self.try_recv() {
                return msg;
            }
            std::hint::spin_loop();
        }
    }
}

/// The relay pool — manages kTLS + io_uring WebSocket connections through
/// two global IO threads (one for reading, one for writing).
///
/// All relay fds are multiplexed through a single io_uring instance per
/// direction. Inbound events flow through an MPSC ring buffer with
/// deduplication. Outbound messages are broadcast via a lock-free ring.
pub struct RelayPool {
    connections: Vec<RelayConnection>,
    consumer: PoolConsumer,
    sender: PoolSender,
    broadcast_consumer: broadcast::Consumer<String>,

    // Command channels to the global IO threads
    reader_cmd_tx: Producer<reader::ReaderAdd>,
    writer_cmd_tx: Producer<writer::WriterAdd>,

    // IO thread handles
    reader_thread: Option<std::thread::JoinHandle<()>>,
    writer_thread: Option<std::thread::JoinHandle<()>>,
    global_shutdown: Arc<AtomicBool>,
}

impl RelayPool {
    /// Create a new relay pool and spawn the global IO threads.
    ///
    /// # Arguments
    /// * `ring_capacity` - MPSC ring buffer size for inbound event throughput
    /// * `cache_size` - Deduplication cache size (e.g. 10,000)
    /// * `broadcast_capacity` - Broadcast ring buffer size for outbound messages
    /// * `max_relays` - Maximum number of relay connections
    pub fn new(
        ring_capacity: usize,
        cache_size: usize,
        broadcast_capacity: usize,
        max_relays: usize,
    ) -> Self {
        // MPSC ring for inbound events (reader IO thread → consumer)
        let (mpsc_producer, mpsc_consumer) =
            RingBuffer::new(Capacity::at_least(ring_capacity)).split();

        // Broadcast ring for outbound messages (sender → writer IO thread)
        let (bc_producer, bc_consumer) =
            broadcast::RingBuffer::new(Capacity::at_least(broadcast_capacity), max_relays + 1)
                .split();

        // Command rings for registering new fds with the IO threads
        let (reader_cmd_tx, reader_cmd_rx) =
            RingBuffer::new(Capacity::at_least(max_relays)).split();
        let (writer_cmd_tx, writer_cmd_rx) =
            RingBuffer::new(Capacity::at_least(max_relays)).split();

        let global_shutdown = Arc::new(AtomicBool::new(false));

        // Spawn global reader IO thread
        let reader_shutdown = Arc::clone(&global_shutdown);
        let reader_thread = std::thread::spawn(move || {
            reader::reader_thread(reader_cmd_rx, mpsc_producer, reader_shutdown);
        });

        // Spawn global writer IO thread
        let writer_shutdown = Arc::clone(&global_shutdown);
        let writer_thread = std::thread::spawn(move || {
            writer::writer_thread(writer_cmd_rx, writer_shutdown);
        });

        Self {
            connections: Vec::new(),
            consumer: PoolConsumer::new(mpsc_consumer, cache_size),
            sender: PoolSender {
                producer: bc_producer,
            },
            broadcast_consumer: bc_consumer,
            reader_cmd_tx,
            writer_cmd_tx,
            reader_thread: Some(reader_thread),
            writer_thread: Some(writer_thread),
            global_shutdown,
        }
    }

    /// Add a relay connection to the pool.
    ///
    /// Connects synchronously (TCP → TLS → kTLS → WebSocket handshake),
    /// then registers the fd with the global IO threads.
    pub fn add_relay(
        &mut self,
        relay_url: String,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.cleanup();

        let ktls_conn = ktls::connect(&relay_url)?;

        let fd = ktls_conn.fd;
        std::mem::forget(ktls_conn); // we manage the fd now

        let shutdown = Arc::new(AtomicBool::new(false));
        let url: Arc<str> = relay_url.as_str().into();

        // SPSC ring for ping/pong coordination (reader → writer)
        let (pong_tx, pong_rx) = RingBuffer::<Vec<u8>>::new(Capacity::at_least(4)).split();

        // Clone a broadcast consumer for this connection's outbound
        let outbound = self.broadcast_consumer.clone();

        // Register with reader IO thread
        let reader_cmd = reader::ReaderAdd {
            fd,
            relay_url: Arc::clone(&url),
            pong_tx,
            shutdown: Arc::clone(&shutdown),
        };
        self.reader_cmd_tx
            .push(reader_cmd)
            .map_err(|_| "reader command ring full")?;

        // Register with writer IO thread
        let writer_cmd = writer::WriterAdd {
            fd,
            outbound,
            pong_rx,
            shutdown: Arc::clone(&shutdown),
        };
        self.writer_cmd_tx
            .push(writer_cmd)
            .map_err(|_| "writer command ring full")?;

        self.connections.push(RelayConnection {
            relay_url,
            fd,
            shutdown,
        });

        Ok(())
    }

    /// Remove a relay from the pool by URL.
    ///
    /// Signals shutdown and closes the fd. The IO threads will see the
    /// error on next recv/send and clean up their slot.
    ///
    /// Returns `true` if the relay was found and removed.
    pub fn remove_relay(&mut self, relay_url: &str) -> bool {
        if let Some(pos) = self
            .connections
            .iter()
            .position(|c| c.relay_url == relay_url)
        {
            // Drop triggers shutdown + fd close
            self.connections.swap_remove(pos);
            true
        } else {
            false
        }
    }

    /// Remove dead connections from the pool.
    pub fn cleanup(&mut self) {
        self.connections.retain(|conn| !conn.is_finished());
    }

    /// Get a cloneable sender handle for broadcasting to all relays.
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

    /// Get the number of live connections.
    pub fn active_connection_count(&self) -> usize {
        self.connections.iter().filter(|c| !c.is_finished()).count()
    }

    /// Get the relay URLs of all connections in the pool.
    pub fn relay_urls(&self) -> Vec<&str> {
        self.connections
            .iter()
            .map(|c| c.relay_url.as_str())
            .collect()
    }

    /// Get the relay URLs of only active connections.
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
        // Signal global shutdown
        self.global_shutdown.store(true, Ordering::Relaxed);

        // Shut down all connection fds to unblock pending io_uring ops
        for conn in &self.connections {
            conn.shutdown.store(true, Ordering::Relaxed);
            unsafe {
                libc::shutdown(conn.fd, libc::SHUT_RDWR);
            }
        }

        // Join IO threads
        if let Some(h) = self.reader_thread.take() {
            let _ = h.join();
        }
        if let Some(h) = self.writer_thread.take() {
            let _ = h.join();
        }

        // Close fds (connections are dropped after this, but we close explicitly
        // here since the IO threads are done and won't touch them)
        for conn in &self.connections {
            unsafe {
                libc::close(conn.fd);
            }
        }
    }
}

/// Helper function to create a new pool and producer for spawning connections
pub fn create_pool(
    ring_capacity: usize,
    cache_size: usize,
) -> (PoolConsumer, Producer<PoolMessage>) {
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

        sender.send_raw("hello".to_string()).unwrap();
        sender2.send_raw("world".to_string()).unwrap();

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
}
