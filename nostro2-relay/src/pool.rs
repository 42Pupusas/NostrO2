#[derive(Debug, Clone, Default)]
struct SeenNotes(std::sync::Arc<tokio::sync::Mutex<std::collections::HashSet<String>>>);
impl SeenNotes {
    pub async fn add(&self, id: &str) -> bool {
        let mut seen = self.0.lock().await;
        if seen.contains(id) {
            false
        } else {
            seen.insert(id.to_string());
            true
        }
    }
}

#[derive(Default, Clone)]
pub struct NostrPool {
    _urls: std::collections::HashSet<String>,
    relays: std::collections::HashMap<String, crate::relay::NostrRelay>,
    seen: SeenNotes,
}
impl NostrPool {
    pub async fn new(relays: &[&str]) -> Self {
        let mut new_relays = std::collections::HashMap::new();
        for url in relays {
            if let Ok(relay) = crate::relay::NostrRelay::new(url).await {
                new_relays.insert((*url).to_string(), relay);
            }
        }
        Self {
            _urls: relays
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
            relays: new_relays,
            seen: SeenNotes(std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashSet::new(),
            ))),
        }
    }
    /// Sends a message to all relays in the pool.
    ///
    /// # Errors
    ///
    /// Returns an error if the message fails to send.
    pub async fn send<T>(
        &self,
        msg: T,
    ) -> Result<nostro2::relay_events::NostrClientEvent, std::io::Error>
    where
        T: Into<nostro2::relay_events::NostrClientEvent> + Clone + Send + Sync,
    {
        let msg: nostro2::relay_events::NostrClientEvent = msg.into();
        futures_util::future::join_all(self.relays.values().map(|relay| relay.send(msg.clone())))
            .await;
        Ok(msg)
    }
    pub async fn recv(&self) -> Option<nostro2::relay_events::NostrRelayEvent> {
        futures_util::future::select_ok(self.relays.values().map(|relay| {
            Box::pin(async move {
                let next_msg = relay.recv().await;
                if let Some(nostro2::relay_events::NostrRelayEvent::NewNote(.., ref msg)) = next_msg
                {
                    if let Some(id) = msg.id.as_ref() {
                        if self.seen.add(&id).await {
                            return Ok::<
                                Option<nostro2::relay_events::NostrRelayEvent>,
                                Box<dyn std::error::Error>,
                            >(next_msg);
                        } else {
                            return Ok(Some(nostro2::relay_events::NostrRelayEvent::Ping));
                        }
                    }
                }
                Ok(next_msg)
            })
        }))
        .await
        .map(|(msg, _)| msg)
        .ok()?
    }
}
