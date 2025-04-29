#[derive(Debug, Clone)]
pub struct RelayPool {
    seen_notes: std::sync::Arc<tokio::sync::RwLock<std::collections::HashSet<String>>>,
    relays: std::collections::HashMap<String, std::sync::Arc<crate::relay::NostrRelay>>,
    events: std::sync::Arc<
        tokio::sync::Mutex<
            tokio::sync::mpsc::UnboundedReceiver<nostro2::relay_events::NostrRelayEvent>,
        >,
    >,
}
impl PartialEq for RelayPool {
    fn eq(&self, other: &Self) -> bool {
        let key_set: std::collections::HashSet<String> = self
            .relays
            .keys()
            .map(std::string::ToString::to_string)
            .collect();
        let other_key_set: std::collections::HashSet<String> = other
            .relays
            .keys()
            .map(std::string::ToString::to_string)
            .collect();
        key_set == other_key_set
            && self
                .seen_notes
                .try_read()
                .map(|s| {
                    other
                        .seen_notes
                        .try_read()
                        .map_or(true, |other_seen| s.eq(&other_seen))
                })
                .unwrap_or(true)
    }
}
impl From<&[String]> for RelayPool {
    fn from(urls: &[String]) -> Self {
        let mut relays = std::collections::HashMap::new();
        for url in urls {
            let new_relay = crate::relay::NostrRelay::new(url.as_str());
            relays.insert(url.clone(), std::sync::Arc::new(new_relay));
        }
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<nostro2::relay_events::NostrRelayEvent>();
        let new_self = Self {
            seen_notes: tokio::sync::RwLock::new(std::collections::HashSet::new()).into(),
            relays,
            events: std::sync::Arc::new(tokio::sync::Mutex::new(rx)),
        };
        new_self.relay_channel(tx);
        new_self
    }
}
impl From<&[&str]> for RelayPool {
    fn from(urls: &[&str]) -> Self {
        let mut relays = std::collections::HashMap::new();
        for url in urls {
            let new_relay = crate::relay::NostrRelay::new(url);
            relays.insert((*url).to_string(), std::sync::Arc::new(new_relay));
        }
        let (tx, rx) =
            tokio::sync::mpsc::unbounded_channel::<nostro2::relay_events::NostrRelayEvent>();

        let new_self = Self {
            seen_notes: tokio::sync::RwLock::new(std::collections::HashSet::new()).into(),
            relays,
            events: std::sync::Arc::new(tokio::sync::Mutex::new(rx)),
        };
        new_self.relay_channel(tx);
        new_self
    }
}
impl RelayPool {
    pub fn relay_channel(
        &self,
        tx: tokio::sync::mpsc::UnboundedSender<nostro2::relay_events::NostrRelayEvent>,
    ) {
        for relay in self.relays.values().cloned() {
            let tx = tx.clone();
            let seen = self.seen_notes.clone();
            wasm_bindgen_futures::spawn_local(async move {
                while let Some(event) = relay.read().await {
                    if let nostro2::relay_events::NostrRelayEvent::NewNote(.., ref note) = event {
                        let mut seen = seen.write().await;
                        if let Some(ref note_id) = note.id {
                            if seen.contains(note_id) {
                                continue;
                            }
                            seen.insert(note_id.clone());
                        }
                    }
                    if tx.send(event).is_err() {
                        break;
                    }
                }
            });
        }
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.relays.is_empty()
    }
    /// Remove a relay from the pool
    ///
    /// This function will remove the relay from the pool. If the relay is removed, it will return
    /// the relay that was removed. If the relay is not found, it will return None.
    pub fn remove_relay(&mut self, url: &str) -> Option<std::sync::Arc<crate::relay::NostrRelay>> {
        self.relays.remove(url)
    }
    /// Add a relay to the pool
    ///
    /// This function will add the relay to the pool. If the relay is already in the pool, it will
    /// return None. If the relay is added, it will return the relay that was added.
    pub fn add_relay(&mut self, url: &str) -> Option<std::sync::Arc<crate::relay::NostrRelay>> {
        self.relays.insert(
            url.to_string(),
            std::sync::Arc::new(crate::relay::NostrRelay::new(url)),
        )
    }
    pub async fn status(&self) -> Vec<nostro2::relay_events::RelayStatus> {
        futures_util::future::join_all(
            self.relays
                .values()
                .map(|relay: &std::sync::Arc<crate::relay::NostrRelay>| relay.relay_state()),
        )
        .await
    }
    /// Send an event to all relays in the pool
    ///
    /// This function will send the event to all relays in the pool. If the event is sent, it
    /// will return the event. If any relay fails to send the event, it will remove the relay from
    /// the pool.
    pub async fn send<T>(&self, event: T) -> nostro2::relay_events::NostrClientEvent
    where
        T: Into<nostro2::relay_events::NostrClientEvent>
            + Send
            + 'static
            + Sync
            + Clone
            + std::fmt::Debug,
    {
        futures_util::future::join_all(self.relays.values().map(|relay| {
            let event_clone = event.clone();
            Box::pin(async move {
                if relay.send(event_clone).await.is_err() {
                    // self.remove_relay(url).await;
                }
            })
        }))
        .await;
        event.into()
    }
    pub async fn read(&self) -> Option<nostro2::relay_events::NostrRelayEvent> {
        self.events.lock().await.recv().await
    }
}
