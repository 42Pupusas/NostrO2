#[derive(Debug, Clone, Default)]
struct SeenNotes(std::sync::Arc<tokio::sync::Mutex<std::collections::HashSet<Option<String>>>>);
impl SeenNotes {
    pub async fn add(&self, id: Option<String>) -> bool {
        let mut seen = self.0.lock().await;
        seen.insert(id)
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
    #[must_use]
    pub fn new(relays: &[&str]) -> Self {
        let (stream_tx, stream) =
            tokio::sync::mpsc::unbounded_channel::<nostro2::NostrRelayEvent>();
        let (sink, sink_rx) = tokio::sync::broadcast::channel(100);
        let seen = SeenNotes(std::sync::Arc::new(tokio::sync::Mutex::new(
            std::collections::HashSet::new(),
        )));
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
                                if let Err(e) = relay.send(msg).await {
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
                                if let Err(e) = stream_send.send(msg.clone()) {
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
