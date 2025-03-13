use futures_util::FutureExt;

#[derive(Default, Clone)]
pub struct NostrPool {
    relays: std::sync::Arc<
        tokio::sync::Mutex<std::collections::HashMap<String, crate::relay::NostrRelay>>,
    >,
}
impl NostrPool {
    pub async fn new(relays: &[&str]) -> Self {
        let pool = Self::default();
        futures_util::future::join_all(relays.iter().map(|url| {
            let pool_clone = pool.relays.clone();
            async move {
                let relay = crate::relay::NostrRelay::new(url).await?;
                pool_clone.lock().await.insert((*url).to_string(), relay);
                Ok::<(), std::io::Error>(())
            }
        }))
        .await;
        pool
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
        futures_util::future::join_all(
            self.relays
                .lock()
                .await
                .values_mut()
                .map(|relay| relay.send(msg.clone())),
        )
        .await;
        Ok(())
    }
    pub async fn recv(&self) -> Option<nostro2::relay_events::NostrRelayEvent> {
        futures_util::future::select_ok(
            self.relays
                .lock()
                .await
                .values_mut()
                .map(|relay| Box::pin(relay.recv().map(Ok::<_, Box<dyn std::error::Error>>))),
        )
        .await
        .map(|(msg, _)| msg)
        .ok()?
    }
}
