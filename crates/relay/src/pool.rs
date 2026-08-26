#[derive(Clone)]
pub struct NostrPool {
    // _urls: std::collections::HashSet<String>,
    // relays: std::collections::HashMap<String, crate::relay::NostrRelay>,
    pub sink: tokio::sync::broadcast::Sender<nostro2::NostrClientEvent>,
    pub stream: std::sync::Arc<
        tokio::sync::RwLock<tokio::sync::mpsc::UnboundedReceiver<nostro2::NostrRelayEvent>>,
    >,
    /// Aborts every per-relay task when the last clone of this pool drops.
    /// A task still inside the initial connect-retry loop never observes the
    /// closed sink, so an unreachable relay would otherwise retry forever and
    /// keep the pool's tasks alive for the lifetime of the process.
    _relays: std::sync::Arc<Vec<crate::task_guard::TaskGuard>>,
}
impl NostrPool {
    /// Create a new relay pool with default settings.
    ///
    /// Uses a deduplication cache size of 10,000 events and default reconnection settings.
    #[must_use]
    pub fn new(relays: &[&str]) -> Self {
        Self::with_cache_size(relays, 10_000)
    }

    /// Create a new relay pool with custom cache size and reconnection settings.
    ///
    /// # Arguments
    /// * `relays` - Array of relay WebSocket URLs to connect to
    /// * `cache_size` - Maximum number of event IDs to cache for deduplication
    /// * `reconnect_config` - Configuration for automatic reconnection
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
        reconnect_config: &crate::relay::ReconnectConfig,
    ) -> Self {
        let (stream_tx, stream) =
            tokio::sync::mpsc::unbounded_channel::<nostro2::NostrRelayEvent>();
        let (sink, sink_rx) = tokio::sync::broadcast::channel(100);
        let seen = nostro2_cache::Cache::new(cache_size);
        let mut guards = Vec::with_capacity(relays.len());
        for url in relays {
            let mut sink = sink_rx.resubscribe();
            let stream_send = stream_tx.clone();
            let seen = seen.clone();
            let url = (*url).to_string();
            let reconnect_config = reconnect_config.clone();
            guards.push(crate::task_guard::TaskGuard::new(tokio::task::spawn(
                async move {
                    let relay = {
                        let mut attempt = 0;
                        loop {
                            match crate::relay::NostrRelay::with_reconnect(
                                &url,
                                reconnect_config.clone(),
                            )
                            .await
                            {
                                Ok(relay) => break relay,
                                Err(e) => {
                                    if !reconnect_config.is_enabled()
                                        || (reconnect_config.max_retries > 0
                                            && attempt >= reconnect_config.max_retries)
                                    {
                                        log::warn!("could not connect to {url}: {e}");
                                        return;
                                    }
                                    let delay = reconnect_config.next_delay(attempt);
                                    log::warn!(
                                        "connection to {url} failed ({e}), retrying in {delay:?}"
                                    );
                                    tokio::time::sleep(delay).await;
                                    attempt += 1;
                                }
                            }
                        }
                    };
                    loop {
                        tokio::select! {
                            recv = sink.recv() => match recv {
                                Ok(msg) => {
                                    if let Err(e) = relay.send(msg) {
                                        log::warn!("relay send failed: {e}");
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                                    log::warn!("pool sink lagged, skipped {skipped} messages");
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            },
                            event = relay.recv() => if let Some(msg) = event {
                                if let nostro2::NostrRelayEvent::NewNote(.., ref note) = msg {
                                    if let Some(ref id) = note.id
                                        && seen.insert(id.clone())
                                        && let Err(e) = stream_send.send(msg.clone())
                                    {
                                        log::warn!("pool stream send failed: {e}");
                                    }
                                } else if let Err(e) = stream_send.send(msg) {
                                    log::warn!("pool stream send failed: {e}");
                                }
                            } else {
                                log::warn!("relay stream ended for {url}");
                                break;
                            },
                        }
                    }
                },
            )));
        }
        Self {
            stream: std::sync::Arc::new(tokio::sync::RwLock::new(stream)),
            sink,
            _relays: std::sync::Arc::new(guards),
        }
    }

    /// Create a new relay pool with a custom deduplication cache size and
    /// default reconnection settings.
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
        Self::with_config(
            relays,
            cache_size,
            &crate::relay::ReconnectConfig::default(),
        )
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
