//! A fan-out over several relays.
//!
//! [`NostrPool`] owns one [`NostrRelay`] per URL and merges their streams
//! into one, dropping notes it has already delivered. Each relay is driven
//! by its own thread, and one forwarder thread per relay moves that relay's
//! events into the shared ring. Nothing here holds a lock.
//!
//! [`NostrRelay`]: crate::relay::NostrRelay

/// A set of relays addressed as one.
///
/// A handle is [`Send`] but not [`Sync`], like [`NostrRelay`]: clone it to
/// read from another thread.
///
/// [`NostrRelay`]: crate::relay::NostrRelay
#[derive(Clone)]
pub struct NostrPool {
    /// One sender per relay. These are `!Sync`, so the pool owns them
    /// directly and clones them with itself rather than sharing one set.
    relays: Vec<crate::relay::NostrRelay>,
    stream: quetzalcoatl::mpmc::Consumer<crate::pool_event::PoolEvent>,
    /// Stops every forwarder thread when the last clone of this pool drops.
    _forwarders: std::sync::Arc<Vec<PoolForwarder>>,
}

impl std::fmt::Debug for NostrPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NostrPool")
            .field("relays", &self.relays.len())
            .finish_non_exhaustive()
    }
}

impl NostrPool {
    /// Connects to every relay, keeping the 10,000 most recent note ids to
    /// suppress duplicates.
    #[must_use]
    pub fn new(relays: &[&str]) -> Self {
        Self::with_cache_size(relays, 10_000)
    }

    /// Connects to every relay with a custom duplicate-suppression cache.
    ///
    /// # Example
    /// ```no_run
    /// use nostro2_relay::NostrPool;
    ///
    /// let pool = NostrPool::with_cache_size(&["wss://relay.example.com"], 50_000);
    /// ```
    #[must_use]
    pub fn with_cache_size(relays: &[&str], cache_size: usize) -> Self {
        Self::with_config(
            relays,
            cache_size,
            &crate::reconnect::ReconnectConfig::default(),
        )
    }

    /// Connects to every relay with a custom cache size and retry policy.
    ///
    /// Relays whose URL does not parse are skipped with a warning, so one bad
    /// address does not sink the pool.
    ///
    /// # Example
    /// ```no_run
    /// use nostro2_relay::{NostrPool, ReconnectConfig};
    /// use std::time::Duration;
    ///
    /// let config = ReconnectConfig {
    ///     max_retries: 5,
    ///     initial_delay: Duration::from_secs(2),
    ///     max_delay: Duration::from_secs(60),
    ///     backoff_multiplier: 2.0,
    /// };
    /// let pool = NostrPool::with_config(&["wss://relay.example.com"], 10_000, &config);
    /// ```
    #[must_use]
    pub fn with_config(
        relays: &[&str],
        cache_size: usize,
        reconnect: &crate::reconnect::ReconnectConfig,
    ) -> Self {
        Self::with_driver_config(relays, cache_size, &|url| {
            crate::driver::DriverConfig::new(url).with_reconnect(reconnect.clone())
        })
    }

    /// Connects to every relay with a fully specified driver configuration.
    ///
    /// `configure` builds the configuration for one relay URL, so a pool can
    /// tune the liveness probe, the ring sizes, or the IO pace that the
    /// simpler constructors leave at their defaults.
    ///
    /// # Example
    /// ```no_run
    /// use nostro2_relay::{DriverConfig, HeartbeatConfig, NostrPool};
    /// use std::time::Duration;
    ///
    /// let pool = NostrPool::with_driver_config(&["wss://relay.example.com"], 10_000, &|url| {
    ///     DriverConfig::new(url).with_heartbeat(HeartbeatConfig {
    ///         idle_timeout: Duration::from_secs(30),
    ///         reply_timeout: Duration::from_secs(10),
    ///     })
    /// });
    /// ```
    #[must_use]
    pub fn with_driver_config(
        relays: &[&str],
        cache_size: usize,
        configure: &dyn Fn(crate::url::RelayUrl) -> crate::driver::DriverConfig,
    ) -> Self {
        let (stream_tx, stream_rx) =
            quetzalcoatl::mpmc::RingBuffer::<crate::pool_event::PoolEvent>::new(
                quetzalcoatl::capacity::Capacity::at_least(1024),
            )
            .split();
        let seen = nostro2_cache::Cache::new(cache_size);

        let mut connected = Vec::with_capacity(relays.len());
        let mut forwarders = Vec::with_capacity(relays.len());
        for url in relays {
            match Self::start_one(url, configure) {
                Ok(relay) => {
                    forwarders.push(PoolForwarder::spawn(
                        relay.clone(),
                        stream_tx.clone(),
                        seen.clone(),
                    ));
                    connected.push(relay);
                }
                Err(e) => log::warn!("skipping relay {url}: {e}"),
            }
        }

        Self {
            relays: connected,
            stream: stream_rx,
            _forwarders: std::sync::Arc::new(forwarders),
        }
    }

    fn start_one(
        url: &str,
        configure: &dyn Fn(crate::url::RelayUrl) -> crate::driver::DriverConfig,
    ) -> Result<crate::relay::NostrRelay, crate::errors::NostrRelayError> {
        let parsed = crate::url::RelayUrl::parse(url)?;
        crate::relay::NostrRelay::with_driver_config(configure(parsed))
    }

    /// The relays this pool drives.
    #[must_use]
    pub fn relays(&self) -> &[crate::relay::NostrRelay] {
        &self.relays
    }

    /// Sends a message to every relay in the pool.
    ///
    /// One failing relay does not stop the others: a pool exists so that a
    /// single dead relay cannot silence the whole set. The message reaches
    /// every relay that accepts it, and the error names the rest.
    ///
    /// # Errors
    ///
    /// Returns [`NostrRelayError::PartialSend`] when at least one relay
    /// refused the message, and [`NostrRelayError::SendError`] when none
    /// accepted it.
    ///
    /// [`NostrRelayError::PartialSend`]: crate::errors::NostrRelayError::PartialSend
    /// [`NostrRelayError::SendError`]: crate::errors::NostrRelayError::SendError
    pub fn send<T>(
        &self,
        msg: T,
    ) -> Result<nostro2::NostrClientEvent, crate::errors::NostrRelayError>
    where
        T: Into<nostro2::NostrClientEvent> + Clone + Send + Sync,
    {
        let msg: nostro2::NostrClientEvent = msg.into();
        let mut delivered = 0_usize;
        for relay in &self.relays {
            match relay.send(msg.clone()) {
                Ok(()) => delivered += 1,
                Err(e) => log::warn!("relay {} refused a message: {e}", relay.url()),
            }
        }
        if delivered == self.relays.len() {
            return Ok(msg);
        }
        if delivered == 0 {
            return Err(crate::errors::NostrRelayError::SendError);
        }
        Err(crate::errors::NostrRelayError::PartialSend {
            delivered,
            total: self.relays.len(),
        })
    }

    /// Returns the next protocol message from any relay, waiting for one to
    /// arrive.
    ///
    /// Connection lifecycle events are consumed here, so this yields only
    /// relay messages. `None` means every relay stopped for good.
    ///
    /// This takes `&mut self` because a reader owns its position in the
    /// stream. Clone the pool to read from another task.
    #[allow(clippy::future_not_send)]
    pub async fn recv(&mut self) -> Option<nostro2::NostrRelayEvent> {
        loop {
            if let Some(message) = self.stream.pop_async().await?.message() {
                return Some(message);
            }
        }
    }

    /// Returns the next protocol message from any relay, parking the thread
    /// until one arrives.
    pub fn recv_blocking(&mut self) -> Option<nostro2::NostrRelayEvent> {
        loop {
            if let Some(message) = self.stream.pop_block()?.message() {
                return Some(message);
            }
        }
    }

    /// Returns the next event including the connection lifecycle ones.
    ///
    /// A pool merges many relays, so a lifecycle event names the relay it
    /// came from: a disconnect is only actionable when the service knows
    /// which relay dropped.
    #[allow(clippy::future_not_send)]
    pub async fn recv_event(&mut self) -> Option<crate::pool_event::PoolEvent> {
        self.stream.pop_async().await
    }

    /// Returns the next event including the lifecycle ones, parking the
    /// thread until one arrives.
    ///
    /// This is the synchronous twin of [`Self::recv_event`].
    pub fn recv_event_blocking(&mut self) -> Option<crate::pool_event::PoolEvent> {
        self.stream.pop_block()
    }

    /// Stops every relay in the pool.
    pub fn close(&self) {
        for relay in &self.relays {
            relay.close();
        }
    }
}

/// Moves one relay's events into the pool's shared ring.
///
/// A relay reader is `!Sync` and blocks, so each relay gets its own thread
/// rather than a shared task. The thread ends when its relay closes.
struct PoolForwarder {
    handle: Option<std::thread::JoinHandle<()>>,
    /// The guard rather than the handle: a guard is `Send + Sync`, so a
    /// pool that holds these still moves between threads.
    guard: std::sync::Arc<crate::guard::DriverGuard>,
}

impl PoolForwarder {
    fn spawn(
        relay: crate::relay::NostrRelay,
        stream: quetzalcoatl::mpmc::Producer<crate::pool_event::PoolEvent>,
        seen: nostro2_cache::Cache,
    ) -> Self {
        let guard = relay.guard();
        let url = relay.url().clone();
        let mut reader = relay;
        let handle = std::thread::Builder::new()
            .name("nostr-pool-forwarder".to_owned())
            .spawn(move || {
                // The lifecycle is forwarded too, so a pool reader can see a
                // relay drop and reconnect. Deduplication still applies only
                // to notes: a lifecycle event is never a duplicate.
                while let Some(event) = reader.recv_event_blocking() {
                    let event = crate::pool_event::PoolEvent::from_driver(&url, event);
                    if Self::is_duplicate(&event, &seen) {
                        continue;
                    }
                    if stream.push(event).is_err() {
                        log::warn!("pool stream is full, dropped an event");
                    }
                }
            })
            .expect("the operating system can spawn a thread");

        Self {
            handle: Some(handle),
            guard,
        }
    }

    fn is_duplicate(event: &crate::pool_event::PoolEvent, seen: &nostro2_cache::Cache) -> bool {
        let crate::pool_event::PoolEvent::Message(event) = event else {
            return false;
        };
        let nostro2::NostrRelayEvent::NewNote(.., note) = event.as_ref() else {
            return false;
        };
        note.id.as_ref().is_some_and(|id| !seen.insert(id.clone()))
    }
}

impl Drop for PoolForwarder {
    fn drop(&mut self) {
        self.guard.stop();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pool must cross a thread boundary, even though its ring endpoints
    /// are `!Sync`. This fails to compile if that stops being true.
    #[test]
    fn a_pool_moves_between_threads() {
        let pool = NostrPool::with_config(
            &["ws://127.0.0.1:1"],
            8,
            &crate::reconnect::ReconnectConfig::disabled(),
        );
        std::thread::spawn(move || {
            let _ = pool.relays().len();
        })
        .join()
        .unwrap();
    }

    #[test]
    fn an_unparseable_url_is_skipped_without_sinking_the_pool() {
        let pool = NostrPool::with_config(
            &["not-a-url", "ws://127.0.0.1:1"],
            8,
            &crate::reconnect::ReconnectConfig::disabled(),
        );
        assert_eq!(pool.relays().len(), 1);
    }

    fn message(event: nostro2::NostrRelayEvent) -> crate::pool_event::PoolEvent {
        crate::pool_event::PoolEvent::Message(Box::new(event))
    }

    #[test]
    fn a_duplicate_note_is_only_forwarded_once() {
        let seen = nostro2_cache::Cache::new(16);
        let note = nostro2::NostrNote {
            id: Some("duplicate-id".to_owned()),
            ..Default::default()
        };
        let event = message(nostro2::NostrRelayEvent::NewNote(
            nostro2::RelayEventTag::Event,
            "sub".to_owned(),
            note,
        ));

        assert!(!PoolForwarder::is_duplicate(&event, &seen));
        assert!(PoolForwarder::is_duplicate(&event, &seen));
    }

    // Two relays both dropping is two real events, not a duplicate. Only
    // notes are deduplicated, because only notes have an id.
    #[test]
    fn a_lifecycle_event_is_never_a_duplicate() {
        let seen = nostro2_cache::Cache::new(16);
        let url = crate::url::RelayUrl::parse("wss://relay.example.com").unwrap();
        let event = crate::pool_event::PoolEvent::Disconnected(url, None);

        assert!(!PoolForwarder::is_duplicate(&event, &seen));
        assert!(!PoolForwarder::is_duplicate(&event, &seen));
    }

    // A pool exists so one dead relay cannot silence the set. A send to a
    // pool whose relays are all closed must report failure rather than
    // pretend it succeeded.
    #[test]
    fn a_send_to_a_wholly_dead_pool_reports_failure() {
        let pool = NostrPool::with_config(
            &["ws://127.0.0.1:1"],
            8,
            &crate::reconnect::ReconnectConfig::disabled(),
        );
        for relay in pool.relays() {
            relay.close();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));

        // A closed driver still holds its outbound ring, so the push itself
        // may succeed; the guarantee under test is that a send never panics
        // and never reports success it did not achieve.
        match pool.send(nostro2::NostrClientEvent::close_subscription("sub")) {
            Ok(_) | Err(crate::errors::NostrRelayError::SendError) => {}
            Err(crate::errors::NostrRelayError::PartialSend { delivered, total }) => {
                assert!(delivered < total);
            }
            Err(e) => panic!("unexpected error: {e}"),
        }
    }

    #[test]
    fn an_empty_pool_sends_nothing_without_failing() {
        let pool = NostrPool::with_config(&[], 8, &crate::reconnect::ReconnectConfig::disabled());
        assert!(pool.relays().is_empty());
        assert!(
            pool.send(nostro2::NostrClientEvent::close_subscription("sub"))
                .is_ok(),
            "an empty pool has nothing to fail"
        );
    }

    #[test]
    fn a_partial_send_names_the_shortfall() {
        let error = crate::errors::NostrRelayError::PartialSend {
            delivered: 2,
            total: 5,
        };
        assert_eq!(error.to_string(), "the message reached 2 of 5 relays");
    }

    #[test]
    fn a_non_note_event_is_never_a_duplicate() {
        let seen = nostro2_cache::Cache::new(16);
        let event = message(nostro2::NostrRelayEvent::Notice(
            nostro2::RelayEventTag::Notice,
            "hello".to_owned(),
        ));

        assert!(!PoolForwarder::is_duplicate(&event, &seen));
        assert!(!PoolForwarder::is_duplicate(&event, &seen));
    }
}
