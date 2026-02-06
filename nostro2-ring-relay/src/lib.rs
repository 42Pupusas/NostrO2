use nostro2::NostrRelayEvent;
use nostro2_cache::Cache;
use quetzalcoatl::broadcast;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, Producer, RingBuffer};
use std::net::TcpStream;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

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
        self.producer.push(json).map_err(|json| json)
    }

    /// Send a raw pre-serialized JSON string to all relays.
    ///
    /// Use this when you've already serialized the message.
    pub fn send_raw(&self, json: String) -> Result<(), String> {
        self.producer.push(json).map_err(|json| json)
    }
}

/// Handle to a relay WebSocket connection running in its own thread
pub struct RelayConnection {
    relay_url: String,
    thread_handle: std::thread::JoinHandle<()>,
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
    ) -> Self {
        let url = relay_url.clone();
        let thread_handle = std::thread::spawn(move || {
            match Self::run_connection(&url, &mut producer, outbound) {
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
            thread_handle,
        }
    }

    /// Main connection loop — non-blocking, multiplexed read/write.
    ///
    /// 1. Connects and performs WebSocket handshake (blocking)
    /// 2. Sends the initial subscription (blocking)
    /// 3. Switches to non-blocking mode
    /// 4. Loops: try read inbound → drain outbound broadcast → sleep if idle
    fn run_connection(
        url: &str,
        producer: &mut Producer<PoolMessage>,
        mut outbound: broadcast::Consumer<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Install default crypto provider for this thread (required for rustls 0.23+)
        let _ = rustls::crypto::ring::default_provider().install_default();

        let (mut socket, _response) = connect(url)?;

        // Subscribe to kind 1 events (text notes) with limit 1000
        let subscription = nostro2::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(1000),
            ..Default::default()
        };

        // Convert to NostrClientEvent and send (still blocking at this point)
        let client_event: nostro2::NostrClientEvent = subscription.into();
        let subscription_json = serde_json::to_string(&client_event)?;
        socket.send(Message::Text(subscription_json.into()))?;

        // Switch to non-blocking for the multiplexed loop
        set_nonblocking(&socket, true)?;

        loop {
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
    pub fn add_relay(&mut self, relay_url: String) {
        let bc_consumer = self.broadcast_consumer.clone();
        let mpsc_producer = self.mpsc_producer.clone();
        let connection = RelayConnection::spawn(relay_url, mpsc_producer, bc_consumer);
        self.connections.push(connection);
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

    /// Get the number of relay connections
    pub fn connection_count(&self) -> usize {
        self.connections.len()
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
}
