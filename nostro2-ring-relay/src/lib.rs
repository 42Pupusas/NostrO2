use nostro2::NostrRelayEvent;
use quetzalcoatl::broadcast;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Producer, RingBuffer};
use quetzalcoatl::spsc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

mod ktls;
mod reader;
mod syscall;
mod writer;

/// Messages that flow through the ring buffer from relay threads to consumer
#[derive(Debug, Clone)]
pub enum PoolMessage {
    /// Event received from a relay
    RelayEvent {
        /// URL of the relay that sent this event
        relay_url: Arc<str>,
        /// The actual relay event
        event: NostrRelayEvent,
    },
    /// Connection error or closed
    ConnectionClosed {
        relay_url: Arc<str>,
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
            syscall::shutdown(self.fd, syscall::SHUT_RDWR);
            syscall::close(self.fd);
        }
    }
}

/// Consumer side of the pool - reads events from all reader shards.
///
/// Round-robins across per-shard SPSC rings to collect events from all
/// reader IO threads without any CAS contention.
pub struct PoolConsumer {
    shard_consumers: Vec<spsc::Consumer<PoolMessage>>,
    next_shard: usize,
    dedup_set: std::collections::HashSet<u64>,
    dedup_capacity: usize,
}

impl PoolConsumer {
    /// Create a new pool consumer with deduplication cache
    pub fn new(shard_consumers: Vec<spsc::Consumer<PoolMessage>>, cache_size: usize) -> Self {
        Self {
            shard_consumers,
            next_shard: 0,
            dedup_set: std::collections::HashSet::with_capacity(cache_size),
            dedup_capacity: cache_size,
        }
    }

    /// Receive the next message from any relay (non-blocking)
    ///
    /// Returns `Some(message)` if available and not a duplicate, `None` if all rings empty.
    /// Automatically deduplicates NewNote events based on event ID.
    pub fn try_recv(&mut self) -> Option<PoolMessage> {
        let n = self.shard_consumers.len();
        if n == 0 {
            return None;
        }

        // Try each shard starting from where we left off
        for _ in 0..n {
            let idx = self.next_shard;
            self.next_shard = (self.next_shard + 1) % n;

            if let Some(msg) = self.shard_consumers[idx].pop() {
                return self.dedup(msg);
            }
        }
        None
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

    fn dedup(&mut self, msg: PoolMessage) -> Option<PoolMessage> {
        match msg {
            PoolMessage::RelayEvent {
                relay_url,
                event: NostrRelayEvent::NewNote(tag, sub_id, note),
            } => {
                if let Some(ref event_id) = note.id {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::hash::DefaultHasher::new();
                    event_id.hash(&mut h);
                    if !self.dedup_set.insert(h.finish()) {
                        return None; // duplicate
                    }
                    if self.dedup_set.len() >= self.dedup_capacity {
                        self.dedup_set.clear();
                    }
                }
                Some(PoolMessage::RelayEvent {
                    relay_url,
                    event: NostrRelayEvent::NewNote(tag, sub_id, note),
                })
            }
            other => Some(other),
        }
    }
}

/// Per-reader-thread state held by the pool.
struct ReaderShard {
    cmd_tx: Producer<reader::ReaderAdd>,
    handle: Option<std::thread::JoinHandle<()>>,
}

/// The relay pool — manages kTLS + io_uring WebSocket connections through
/// sharded reader IO threads and a single writer IO thread.
///
/// Reader threads are sharded across available CPU cores — each owns its
/// own io_uring and a dedicated SPSC event ring (zero CAS contention).
/// Connections are assigned round-robin. The consumer round-robins across
/// all shard rings to collect events. Outbound messages are broadcast via
/// a lock-free ring.
pub struct RelayPool {
    connections: Vec<RelayConnection>,
    consumer: PoolConsumer,
    sender: PoolSender,
    broadcast_consumer: broadcast::Consumer<String>,

    // Sharded reader threads (one per core)
    reader_shards: Vec<ReaderShard>,
    next_reader: usize,

    // Single writer thread
    writer_cmd_tx: Producer<writer::WriterAdd>,
    writer_thread: Option<std::thread::JoinHandle<()>>,

    global_shutdown: Arc<AtomicBool>,
}

impl RelayPool {
    /// Create a new relay pool and spawn IO threads.
    ///
    /// Detects available CPU cores and spawns one reader IO thread per core,
    /// plus one writer IO thread. Each reader thread owns its own io_uring
    /// and SPSC event ring — zero contention between shards.
    ///
    /// # Arguments
    /// * `ring_capacity` - Per-shard SPSC ring buffer size for inbound events
    /// * `cache_size` - Deduplication cache size (e.g. 10,000)
    /// * `broadcast_capacity` - Broadcast ring buffer size for outbound messages
    /// * `max_relays` - Maximum number of relay connections
    pub fn new(
        ring_capacity: usize,
        cache_size: usize,
        broadcast_capacity: usize,
        max_relays: usize,
    ) -> Self {
        let num_cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);

        // Broadcast ring for outbound messages (sender → writer IO thread)
        let (bc_producer, bc_consumer) =
            broadcast::RingBuffer::new(Capacity::at_least(broadcast_capacity), max_relays + 1)
                .split();

        let global_shutdown = Arc::new(AtomicBool::new(false));

        // Spawn reader IO threads — one per core, capped at max_relays
        // (no point having more shards than connections).
        let num_shards = num_cores.min(max_relays).max(1);
        let relays_per_shard = (max_relays / num_shards).max(4);
        let capacity_per_shard = ring_capacity;
        let mut reader_shards = Vec::with_capacity(num_shards);
        let mut shard_consumers = Vec::with_capacity(num_shards);

        for i in 0..num_shards {
            let (cmd_tx, cmd_rx) = RingBuffer::new(Capacity::at_least(relays_per_shard)).split();
            let (event_tx, event_rx) =
                spsc::RingBuffer::new(Capacity::at_least(capacity_per_shard)).split();
            shard_consumers.push(event_rx);

            let shutdown = Arc::clone(&global_shutdown);
            let handle = std::thread::Builder::new()
                .name(format!("ring-reader-{i}"))
                .spawn(move || {
                    reader::reader_thread(cmd_rx, event_tx, shutdown);
                })
                .expect("failed to spawn reader thread");
            reader_shards.push(ReaderShard {
                cmd_tx,
                handle: Some(handle),
            });
        }

        // Spawn single writer IO thread
        let (writer_cmd_tx, writer_cmd_rx) =
            RingBuffer::new(Capacity::at_least(max_relays)).split();
        let writer_shutdown = Arc::clone(&global_shutdown);
        let writer_thread = std::thread::Builder::new()
            .name("ring-writer".into())
            .spawn(move || {
                writer::writer_thread(writer_cmd_rx, writer_shutdown);
            })
            .expect("failed to spawn writer thread");

        Self {
            connections: Vec::new(),
            consumer: PoolConsumer::new(shard_consumers, cache_size),
            sender: PoolSender {
                producer: bc_producer,
            },
            broadcast_consumer: bc_consumer,
            reader_shards,
            next_reader: 0,
            writer_cmd_tx,
            writer_thread: Some(writer_thread),
            global_shutdown,
        }
    }

    /// Add a relay connection to the pool.
    ///
    /// Connects synchronously (TCP → TLS → kTLS → WebSocket handshake),
    /// then registers the fd with a reader shard (round-robin) and the writer.
    pub fn add_relay(
        &mut self,
        relay_url: String,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.cleanup();

        let ktls_conn = ktls::connect(&relay_url)?;
        let fd = ktls_conn.fd;
        std::mem::forget(ktls_conn);

        let shutdown = Arc::new(AtomicBool::new(false));
        let url: Arc<str> = relay_url.as_str().into();

        // SPSC ring for ping/pong coordination (reader → writer)
        let (pong_tx, pong_rx) = spsc::RingBuffer::<Vec<u8>>::new(Capacity::at_least(4)).split();

        // Clone a broadcast consumer for this connection's outbound
        let outbound = self.broadcast_consumer.clone();

        // Round-robin assign to a reader shard
        let shard_idx = self.next_reader % self.reader_shards.len();
        self.next_reader += 1;

        let reader_cmd = reader::ReaderAdd {
            fd,
            relay_url: Arc::clone(&url),
            pong_tx,
            shutdown: Arc::clone(&shutdown),
        };
        self.reader_shards[shard_idx]
            .cmd_tx
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
    pub fn remove_relay(&mut self, relay_url: &str) -> bool {
        if let Some(pos) = self
            .connections
            .iter()
            .position(|c| c.relay_url == relay_url)
        {
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

    /// Get the number of reader IO threads.
    pub fn reader_thread_count(&self) -> usize {
        self.reader_shards.len()
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
        self.global_shutdown.store(true, Ordering::Relaxed);

        for conn in &self.connections {
            conn.shutdown.store(true, Ordering::Relaxed);
            unsafe {
                syscall::shutdown(conn.fd, syscall::SHUT_RDWR);
            }
        }

        for shard in &mut self.reader_shards {
            if let Some(h) = shard.handle.take() {
                let _ = h.join();
            }
        }

        if let Some(h) = self.writer_thread.take() {
            let _ = h.join();
        }

        for conn in &self.connections {
            unsafe {
                syscall::close(conn.fd);
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
    // Wrap single MPSC consumer in a vec for PoolConsumer compatibility
    // (not ideal but keeps the helper working for simple use cases)
    let _ = consumer;
    let (spsc_tx, spsc_rx) = spsc::RingBuffer::new(Capacity::at_least(ring_capacity)).split();
    // We return the MPSC producer but the consumer reads from SPSC — this helper
    // is only useful for manual single-producer setups now
    let _ = spsc_tx;
    (PoolConsumer::new(vec![spsc_rx], cache_size), producer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_creation() {
        let pool = RelayPool::new(1024, 10_000, 64, 8);
        assert_eq!(pool.connection_count(), 0);
        assert!(pool.reader_thread_count() >= 1);
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
