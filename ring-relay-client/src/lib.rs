use nostro2::{NostrClientEvent, NostrRelayEvent, NostrSubscription, RelayEventTag};
use quetzalcoatl::broadcast;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Producer, RingBuffer};
use quetzalcoatl::spsc;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Lock-free wake primitive: reader threads unpark the consumer via its
/// thread handle after pushing events.  Uses `thread::park` / `unpark`
/// which is backed by a futex on Linux — no mutex in the hot path.
pub(crate) struct Parker {
    thread: std::thread::Thread,
}

impl Parker {
    /// Capture the current thread as the one that will be parked.
    pub fn new() -> Self {
        Self {
            thread: std::thread::current(),
        }
    }

    /// Park the consumer thread until an `unpark()` is called.
    /// Spurious wakeups are fine — the caller re-checks the ring.
    pub fn park(&self) {
        std::thread::park();
    }

    /// Wake the consumer thread.  Cheap no-op if it isn't parked.
    pub fn unpark(&self) {
        self.thread.unpark();
    }
}

mod ktls;
mod reader;
mod reconnect;
mod syscall;
mod writer;

use reconnect::{ReconnectCmd, ReconnectContext, ReconnectResult};

/// Default initial reconnect delay (1 second).
const DEFAULT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Default maximum reconnect delay (60 seconds).
const DEFAULT_MAX_BACKOFF: Duration = Duration::from_secs(60);

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
    pub fn send<T: Into<NostrClientEvent>>(&self, msg: T) -> Result<(), String> {
        let client_event: NostrClientEvent = msg.into();
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
    parker: Arc<Parker>,
}

impl PoolConsumer {
    /// Create a new pool consumer with deduplication cache.
    ///
    /// Must be called on the thread that will call `recv()`, since
    /// `Parker` captures the current thread handle for `thread::park`.
    pub fn new(shard_consumers: Vec<spsc::Consumer<PoolMessage>>, cache_size: usize) -> Self {
        Self {
            shard_consumers,
            next_shard: 0,
            dedup_set: std::collections::HashSet::with_capacity(cache_size),
            dedup_capacity: cache_size,
            parker: Arc::new(Parker::new()),
        }
    }

    /// Get a cloneable handle that reader threads use to wake this consumer.
    pub(crate) fn waker(&self) -> Arc<Parker> {
        Arc::clone(&self.parker)
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

        // Sticky drain: stay on the current shard until empty, then rotate.
        // Avoids touching N-1 cold cache lines per event.
        if let Some(msg) = self.shard_consumers[self.next_shard].pop() {
            return self.dedup(msg);
        }
        // Current shard empty — scan others
        for _ in 1..n {
            self.next_shard = (self.next_shard + 1) % n;
            if let Some(msg) = self.shard_consumers[self.next_shard].pop() {
                return self.dedup(msg);
            }
        }
        // All empty — advance so next call starts at a different shard
        self.next_shard = (self.next_shard + 1) % n;
        None
    }

    /// Blocking receive — parks the thread until a reader pushes an event.
    pub fn recv(&mut self) -> PoolMessage {
        loop {
            if let Some(msg) = self.try_recv() {
                return msg;
            }
            self.parker.park();
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

/// Bookkeeping for a relay tracked by the pool.
struct RelayEntry {
    url: String,
    fd: i32,
    shutdown: Arc<AtomicBool>,
    #[allow(dead_code)] // kept for relay identification in reconnect commands
    shard_idx: usize,
}

/// The relay pool — manages kTLS + io_uring WebSocket connections through
/// sharded reader IO threads and a single writer IO thread.
///
/// Reader threads are sharded across available CPU cores — each owns its
/// own io_uring and a dedicated SPSC event ring (zero CAS contention).
/// Connections are assigned round-robin. The consumer round-robins across
/// all shard rings to collect events. Outbound messages are broadcast via
/// a lock-free ring.
///
/// Includes event-driven auto-reconnection with exponential backoff and
/// subscription tracking — when a relay reconnects, all active subscriptions
/// are re-sent. The reconnect thread parks until woken; zero CPU cost when
/// all relays are healthy.
pub struct RelayPool {
    relay_entries: Vec<RelayEntry>,
    subscriptions: HashMap<String, String>,
    consumer: PoolConsumer,
    sender: PoolSender,
    broadcast_consumer: broadcast::Consumer<String>,

    // Sharded reader threads (one per core)
    reader_shards: Vec<ReaderShard>,
    next_reader: usize,

    // Single writer thread
    writer_cmd_tx: Producer<writer::WriterAdd>,
    writer_thread: Option<std::thread::JoinHandle<()>>,

    // Reconnect thread — event-driven, parks until needed
    reconnect_cmd_tx: Producer<ReconnectCmd>,
    reconnect_result_rx: spsc::Consumer<ReconnectResult>,
    reconnect_thread: Option<std::thread::JoinHandle<()>>,

    global_shutdown: Arc<AtomicBool>,
}

impl RelayPool {
    /// Create a new relay pool and spawn IO threads.
    ///
    /// Detects available CPU cores and spawns one reader IO thread per core,
    /// plus one writer IO thread and one event-driven reconnect thread.
    /// Each reader thread owns its own io_uring and SPSC event ring — zero
    /// contention between shards.
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
        // Budget: max_relays initial + max_relays reconnections + 1 reconnect thread
        let (bc_producer, bc_consumer) =
            broadcast::RingBuffer::new(Capacity::at_least(broadcast_capacity), max_relays * 2 + 1)
                .split();

        let global_shutdown = Arc::new(AtomicBool::new(false));

        // Spawn reader IO threads — one per core, capped at max_relays
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

        let consumer = PoolConsumer::new(shard_consumers, cache_size);

        // Reconnect thread rings — cmd (MPSC) and results (SPSC)
        let (reconnect_cmd_tx, reconnect_cmd_rx) =
            RingBuffer::new(Capacity::at_least(max_relays * 2)).split();
        let (reconnect_result_tx, reconnect_result_rx) =
            spsc::RingBuffer::new(Capacity::at_least(max_relays)).split();

        // Clone handles for the reconnect thread
        let reconnect_reader_txs: Vec<_> =
            reader_shards.iter().map(|s| s.cmd_tx.clone()).collect();
        let reconnect_writer_tx = writer_cmd_tx.clone();
        let reconnect_bc_consumer = bc_consumer.clone();
        let reconnect_bc_producer = bc_producer.clone();
        let reconnect_waker = consumer.waker();
        let reconnect_shutdown = Arc::clone(&global_shutdown);

        let reconnect_thread = std::thread::Builder::new()
            .name("ring-reconnect".into())
            .spawn(move || {
                reconnect::reconnect_thread(ReconnectContext {
                    cmd_rx: reconnect_cmd_rx,
                    result_tx: reconnect_result_tx,
                    reader_txs: reconnect_reader_txs,
                    writer_tx: reconnect_writer_tx,
                    broadcast_consumer: reconnect_bc_consumer,
                    broadcast_producer: reconnect_bc_producer,
                    waker: reconnect_waker,
                    global_shutdown: reconnect_shutdown,
                    initial_backoff: DEFAULT_INITIAL_BACKOFF,
                    max_backoff: DEFAULT_MAX_BACKOFF,
                });
            })
            .expect("failed to spawn reconnect thread");

        Self {
            relay_entries: Vec::new(),
            subscriptions: HashMap::new(),
            consumer,
            sender: PoolSender {
                producer: bc_producer,
            },
            broadcast_consumer: bc_consumer,
            reader_shards,
            next_reader: 0,
            writer_cmd_tx,
            writer_thread: Some(writer_thread),
            reconnect_cmd_tx,
            reconnect_result_rx,
            reconnect_thread: Some(reconnect_thread),
            global_shutdown,
        }
    }

    /// Wake the reconnect thread via its JoinHandle (lock-free futex unpark).
    fn wake_reconnect(&self) {
        if let Some(h) = self.reconnect_thread.as_ref() {
            h.thread().unpark();
        }
    }

    /// Drain reconnect results and update relay entries.
    /// Called internally from `recv()` / `try_recv()`.
    fn process_reconnections(&mut self) {
        while let Some(result) = self.reconnect_result_rx.pop() {
            if let Some(entry) = self.relay_entries.iter_mut().find(|e| e.url == result.url) {
                entry.fd = result.fd;
                entry.shutdown = result.shutdown;
            }
        }
    }

    /// Add a relay connection to the pool.
    ///
    /// Connects synchronously (TCP → TLS → kTLS → WebSocket handshake),
    /// then registers the fd with a reader shard (round-robin) and the writer.
    /// The relay is automatically managed for reconnection — if it disconnects,
    /// the reconnect thread will re-establish it with exponential backoff.
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
            waker: self.consumer.waker(),
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

        // Track locally
        self.relay_entries.push(RelayEntry {
            url: relay_url.clone(),
            fd,
            shutdown: Arc::clone(&shutdown),
            shard_idx,
        });

        // Tell reconnect thread to manage this relay
        let _ = self.reconnect_cmd_tx.push(ReconnectCmd::Add {
            url: relay_url,
            fd,
            shutdown,
            shard_idx,
        });

        Ok(())
    }

    /// Remove a relay from the pool by URL.
    ///
    /// Shuts down the connection and stops reconnection attempts.
    pub fn remove_relay(&mut self, relay_url: &str) -> bool {
        if let Some(pos) = self.relay_entries.iter().position(|e| e.url == relay_url) {
            let entry = self.relay_entries.swap_remove(pos);
            entry.shutdown.store(true, Ordering::Relaxed);
            unsafe {
                syscall::shutdown(entry.fd, syscall::SHUT_RDWR);
            }
            // Tell reconnect thread to forget this relay
            let _ = self.reconnect_cmd_tx.push(ReconnectCmd::Remove {
                url: relay_url.to_string(),
            });
            self.wake_reconnect();
            true
        } else {
            false
        }
    }

    /// Remove dead connections that have been replaced by the reconnect thread.
    pub fn cleanup(&mut self) {
        self.process_reconnections();
    }

    /// Subscribe to events matching `filter` with the given subscription ID.
    ///
    /// Sends the REQ message to all relays and tracks the subscription so it
    /// is automatically re-sent when a relay reconnects.
    pub fn subscribe(
        &mut self,
        sub_id: String,
        filter: NostrSubscription,
    ) -> Result<(), String> {
        let event = NostrClientEvent::Subscribe(RelayEventTag::Req, sub_id.clone(), filter);
        let json = serde_json::to_string(&event).map_err(|e| e.to_string())?;
        self.sender.send_raw(json.clone())?;
        self.subscriptions.insert(sub_id.clone(), json.clone());
        let _ = self.reconnect_cmd_tx.push(ReconnectCmd::TrackSub {
            sub_id,
            json,
        });
        self.wake_reconnect();
        Ok(())
    }

    /// Close a subscription by ID.
    ///
    /// Sends the CLOSE message and removes the subscription from tracking
    /// so it is not re-sent on reconnection.
    pub fn unsubscribe(&mut self, sub_id: &str) -> Result<(), String> {
        self.subscriptions.remove(sub_id);
        let event = NostrClientEvent::close_subscription(sub_id);
        let json = serde_json::to_string(&event).map_err(|e| e.to_string())?;
        self.sender.send_raw(json)?;
        let _ = self.reconnect_cmd_tx.push(ReconnectCmd::UntrackSub {
            sub_id: sub_id.to_string(),
        });
        self.wake_reconnect();
        Ok(())
    }

    /// Return the IDs of all tracked subscriptions.
    pub fn active_subscriptions(&self) -> Vec<String> {
        self.subscriptions.keys().cloned().collect()
    }

    /// Get a cloneable sender handle for broadcasting to all relays.
    pub fn sender(&self) -> PoolSender {
        self.sender.clone()
    }

    /// Receive the next event from any relay (blocking).
    ///
    /// Also processes reconnection results and wakes the reconnect
    /// thread when a `ConnectionClosed` is observed.
    pub fn recv(&mut self) -> PoolMessage {
        self.process_reconnections();
        let msg = self.consumer.recv();
        if matches!(&msg, PoolMessage::ConnectionClosed { .. }) {
            self.wake_reconnect();
        }
        msg
    }

    /// Receive the next event from any relay (non-blocking).
    pub fn try_recv(&mut self) -> Option<PoolMessage> {
        self.process_reconnections();
        let msg = self.consumer.try_recv();
        if matches!(&msg, Some(PoolMessage::ConnectionClosed { .. })) {
            self.wake_reconnect();
        }
        msg
    }

    /// Get the total number of managed relays (including dead ones pending reconnection).
    pub fn connection_count(&self) -> usize {
        self.relay_entries.len()
    }

    /// Get the number of live connections.
    pub fn active_connection_count(&self) -> usize {
        self.relay_entries
            .iter()
            .filter(|e| !e.shutdown.load(Ordering::Relaxed))
            .count()
    }

    /// Get the number of reader IO threads.
    pub fn reader_thread_count(&self) -> usize {
        self.reader_shards.len()
    }

    /// Get the relay URLs of all managed relays.
    pub fn relay_urls(&self) -> Vec<String> {
        self.relay_entries.iter().map(|e| e.url.clone()).collect()
    }

    /// Get the relay URLs of only active (connected) relays.
    pub fn active_relay_urls(&self) -> Vec<String> {
        self.relay_entries
            .iter()
            .filter(|e| !e.shutdown.load(Ordering::Relaxed))
            .map(|e| e.url.clone())
            .collect()
    }
}

impl Drop for RelayPool {
    fn drop(&mut self) {
        self.global_shutdown.store(true, Ordering::Relaxed);

        // Shut down all known connections
        for entry in &self.relay_entries {
            entry.shutdown.store(true, Ordering::Relaxed);
            unsafe {
                syscall::shutdown(entry.fd, syscall::SHUT_RDWR);
            }
        }

        // Join reconnect thread (it checks global_shutdown)
        self.wake_reconnect();
        if let Some(h) = self.reconnect_thread.take() {
            let _ = h.join();
        }

        // Pick up any final reconnect results
        self.process_reconnections();

        // Join IO threads
        for shard in &mut self.reader_shards {
            if let Some(h) = shard.handle.take() {
                let _ = h.join();
            }
        }
        if let Some(h) = self.writer_thread.take() {
            let _ = h.join();
        }

        // Close all fds — shutdown is idempotent, close handles EBADF
        for entry in &self.relay_entries {
            unsafe {
                syscall::shutdown(entry.fd, syscall::SHUT_RDWR);
                syscall::close(entry.fd);
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
    let _ = consumer;
    let (spsc_tx, spsc_rx) = spsc::RingBuffer::new(Capacity::at_least(ring_capacity)).split();
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

    #[test]
    fn test_subscription_tracking() {
        let mut pool = RelayPool::new(1024, 10_000, 64, 8);

        let filter = NostrSubscription::default();
        pool.subscribe("sub1".to_string(), filter).unwrap();

        let subs = pool.active_subscriptions();
        assert_eq!(subs.len(), 1);
        assert!(subs.contains(&"sub1".to_string()));

        pool.unsubscribe("sub1").unwrap();
        assert!(pool.active_subscriptions().is_empty());
    }
}
