use futures_util::FutureExt;

#[derive(Default, Clone)]
pub struct NostrPool {
    _urls: std::collections::HashSet<String>,
    relays: std::collections::HashMap<String, crate::relay::NostrRelay>,
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
            _urls: relays.iter().map(std::string::ToString::to_string).collect(),
            relays: new_relays,
        }
    }
    /// Sends a message to all relays in the pool.
    ///
    /// # Errors
    ///
    /// Returns an error if the message fails to send.
    pub async fn send<T>(&self, msg: &T) -> Result<(), std::io::Error>
    where
        T: Into<nostro2::relay_events::NostrClientEvent> + Clone + Send + Sync,
    {
        futures_util::future::join_all(self.relays.values().map(|relay| relay.send(msg.clone())))
            .await;
        Ok(())
    }
    pub async fn recv(&self) -> Option<nostro2::relay_events::NostrRelayEvent> {
        futures_util::future::select_ok(
            self.relays
                .values()
                .map(|relay| Box::pin(relay.recv().map(Ok::<_, Box<dyn std::error::Error>>))),
        )
        .await
        .map(|(msg, _)| msg)
        .ok()?
    }
}
