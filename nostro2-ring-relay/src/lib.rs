use nostro2::NostrRelayEvent;
use nostro2_cache::Cache;
use quetzalcoatl::capacity::Capacity;
use quetzalcoatl::mpsc::{Consumer, Producer, RingBuffer};
use tungstenite::{connect, Message};

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

/// Handle to a relay WebSocket connection running in its own thread
pub struct RelayConnection {
    relay_url: String,
    thread_handle: std::thread::JoinHandle<()>,
}

impl RelayConnection {
    /// Spawn a new thread that connects to a relay and reads events into the ring buffer
    pub fn spawn(relay_url: String, mut producer: Producer<PoolMessage>) -> Self {
        let url = relay_url.clone();
        let thread_handle = std::thread::spawn(move || {
            match Self::run_connection(&url, &mut producer) {
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

    /// Main connection loop - connect to WebSocket and read messages
    fn run_connection(
        url: &str,
        producer: &mut Producer<PoolMessage>,
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

        // Convert to NostrClientEvent and send
        let client_event: nostro2::NostrClientEvent = subscription.into();
        let subscription_json = serde_json::to_string(&client_event)?;
        socket.send(Message::Text(subscription_json.into()))?;

        loop {
            let msg = socket.read()?;

            match msg {
                Message::Text(text) => {
                    // Parse the text into a NostrRelayEvent
                    if let Ok(event) = text.parse::<NostrRelayEvent>() {
                        let pool_msg = PoolMessage::RelayEvent {
                            relay_url: url.to_string(),
                            event,
                        };

                        // Try to push to ring buffer - if full, spin until space available
                        while producer.push(pool_msg.clone()).is_err() {
                            std::hint::spin_loop();
                        }
                    }
                    // Silently ignore unparseable messages
                }
                Message::Close(_) => {
                    // Connection closed by remote
                    break;
                }
                Message::Ping(data) => {
                    // Respond to ping with pong
                    socket.send(Message::Pong(data))?;
                }
                _ => {
                    // Ignore other message types (binary, pong, frame)
                }
            }
        }

        Ok(())
    }

    /// Get the relay URL
    pub fn relay_url(&self) -> &str {
        &self.relay_url
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

/// The relay pool - manages multiple WebSocket connections via a ring buffer
pub struct RelayPool {
    connections: Vec<RelayConnection>,
    consumer: PoolConsumer,
}

impl RelayPool {
    /// Create a new relay pool with the specified ring buffer and cache capacity
    ///
    /// # Arguments
    /// * `ring_capacity` - Ring buffer size for event throughput
    /// * `cache_size` - Deduplication cache size (default: 10,000)
    pub fn new(ring_capacity: usize, cache_size: usize) -> Self {
        let (producer, consumer) = RingBuffer::new(Capacity::at_least(ring_capacity)).split();
        Self {
            connections: Vec::new(),
            consumer: PoolConsumer::new(consumer, cache_size),
        }
    }

    /// Add a relay connection to the pool
    ///
    /// Spawns a new thread that connects to the relay and reads events
    pub fn add_relay(&mut self, relay_url: String, producer: Producer<PoolMessage>) {
        let connection = RelayConnection::spawn(relay_url, producer);
        self.connections.push(connection);
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
        let pool = RelayPool::new(1024, 10_000);
        assert_eq!(pool.connection_count(), 0);
    }

    #[test]
    fn test_create_pool_helper() {
        let (_consumer, _producer) = create_pool(1024, 10_000);
        // Just testing that it compiles and runs
    }
}
