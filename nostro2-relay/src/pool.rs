/// LRU cache for deduplicating event IDs across multiple relays.
///
/// Events are automatically evicted when the cache reaches capacity,
/// removing the least recently used entries. This prevents unbounded
/// memory growth in long-running relay pools.
#[derive(Debug, Clone)]
struct SeenNotes(std::sync::Arc<tokio::sync::Mutex<lru::LruCache<Option<String>, ()>>>);

impl SeenNotes {
    /// Create a new SeenNotes cache with the specified capacity.
    ///
    /// # Arguments
    /// * `capacity` - Maximum number of event IDs to cache (default: 10,000)
    pub fn new(capacity: usize) -> Self {
        Self(std::sync::Arc::new(tokio::sync::Mutex::new(
            lru::LruCache::new(std::num::NonZeroUsize::new(capacity).unwrap())
        )))
    }

    /// Add an event ID to the cache.
    ///
    /// Returns `true` if this is a new event (not seen before),
    /// `false` if the event was already in the cache.
    pub async fn add(&self, id: Option<String>) -> bool {
        let mut cache = self.0.lock().await;
        // put() returns None if key didn't exist, Some(old_value) if it did
        cache.put(id, ()).is_none()
    }
}

impl Default for SeenNotes {
    fn default() -> Self {
        Self::new(10_000)
    }
}
#[derive(Clone)]
pub struct NostrPool {
    // _urls: std::collections::HashSet<String>,
    // relays: std::collections::HashMap<String, crate::relay::NostrRelay>,
    pub sink: tokio::sync::broadcast::Sender<nostro2::NostrClientEvent>,
    pub stream: std::sync::Arc<
        tokio::sync::RwLock<tokio::sync::mpsc::UnboundedReceiver<nostro2::NostrRelayEvent>>,
    >,
}
impl NostrPool {
    /// Create a new relay pool with default settings.
    ///
    /// Uses a deduplication cache size of 10,000 events.
    #[must_use]
    pub fn new(relays: &[&str]) -> Self {
        Self::with_cache_size(relays, 10_000)
    }

    /// Create a new relay pool with a custom deduplication cache size.
    ///
    /// # Arguments
    /// * `relays` - Array of relay WebSocket URLs to connect to
    /// * `cache_size` - Maximum number of event IDs to cache for deduplication
    ///
    /// # Example
    /// ```no_run
    /// use nostro2_relay::NostrPool;
    ///
    /// // Pool with 50K event cache (higher memory, fewer duplicates)
    /// let pool = NostrPool::with_cache_size(&["wss://relay.example.com"], 50_000);
    /// ```
    #[must_use]
    pub fn with_cache_size(relays: &[&str], cache_size: usize) -> Self {
        let (stream_tx, stream) =
            tokio::sync::mpsc::unbounded_channel::<nostro2::NostrRelayEvent>();
        let (sink, sink_rx) = tokio::sync::broadcast::channel(100);
        let seen = SeenNotes::new(cache_size);
        for url in relays {
            let mut sink = sink_rx.resubscribe();
            let stream_send = stream_tx.clone();
            let seen = seen.clone();
            let url = (*url).to_string();
            tokio::task::spawn(async move {
                if let Ok(relay) = crate::relay::NostrRelay::new(&url).await {
                    loop {
                        tokio::select! {
                            Ok(msg) = sink.recv() => {
                                if let Err(e) = relay.send(msg) {
                                    eprintln!("Failed to send message: {e}");
                                }
                            },
                            Some(msg) = relay.recv() => {
                                if let nostro2::NostrRelayEvent::NewNote(.., ref note) =
                                    msg
                                {
                                    if seen.add(note.id.clone()).await {
                                        if let Err(e) = stream_send.send(msg.clone()) {
                                            eprintln!("Failed to send message: {e}");
                                        }
                                    }
                                    continue;
                                }
                                if let Err(e) = stream_send.send(msg) {
                                    eprintln!("Failed to send message: {e}");
                                }
                            },
                            else => {
                                eprintln!("Relay connection closed");
                                break;
                            }

                        }
                    }
                }
            });
        }
        Self {
            stream: std::sync::Arc::new(tokio::sync::RwLock::new(stream)),
            sink,
        }
    }
    /// Sends a message to all relays in the pool.
    ///
    /// # Errors
    ///
    /// Returns an error if the message fails to send, which might happen if all broadcast
    /// channels are closed.
    pub fn send<T>(
        &self,
        msg: T,
    ) -> Result<nostro2::NostrClientEvent, crate::errors::NostrRelayError>
    where
        T: Into<nostro2::NostrClientEvent> + Clone + Send + Sync,
    {
        let msg: nostro2::NostrClientEvent = msg.into();
        self.sink.send(msg.clone())?;
        Ok(msg)
    }
    pub async fn recv(&self) -> Option<nostro2::NostrRelayEvent> {
        let mut stream = self.stream.write().await;
        stream.recv().await
    }
}
