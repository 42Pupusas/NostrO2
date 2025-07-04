use futures_util::{SinkExt, StreamExt};
#[derive(Clone)]
pub struct NostrRelay {
    stream: std::sync::Arc<
        tokio::sync::Mutex<
            futures_util::stream::SplitStream<
                tokio_tungstenite::WebSocketStream<
                    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                >,
            >,
        >,
    >,
    sink: std::sync::Arc<
        tokio::sync::Mutex<
            futures_util::stream::SplitSink<
                tokio_tungstenite::WebSocketStream<
                    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                >,
                tokio_tungstenite::tungstenite::Message,
            >,
        >,
    >,
}
impl NostrRelay {
    /// Creates a new relay connection to the given URL.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails.
    pub async fn new(url: &str) -> Result<Self, crate::errors::NostrRelayError> {
        let (websocket, _response) = tokio_tungstenite::connect_async(url).await?;
        let (sink, stream) = futures_util::StreamExt::split(websocket);
        Ok(Self {
            stream: std::sync::Arc::new(stream.into()),
            sink: std::sync::Arc::new(sink.into()),
        })
    }
    /// Sends a message to the relay.
    /// Message must implement `Into<NostrClientEvent>`.
    ///
    /// # Errors
    ///
    /// Returns an error if the message fails to send.
    pub async fn send<T>(&self, msg: T) -> Result<(), crate::errors::NostrRelayError>
    where
        T: Into<nostro2::NostrClientEvent> + Send + Sync,
    {
        let msg: nostro2::NostrClientEvent = msg.into();
        let msg_str = serde_json::to_string(&msg).map_err(crate::errors::NostrRelayError::Serde)?;
        self.sink.lock().await.send(msg_str.into()).await?;
        Ok(())
    }
    /// Receives a message from the relay.
    ///
    /// # Errors
    ///
    /// Returns an error if the message fails to receive, due to the stream being closed.
    /// Should never failed to parse the message, as it is guaranteed to be a valid
    /// `NostrRelayEvent`.
    pub async fn recv(&self) -> Option<nostro2::NostrRelayEvent> {
        Some(
            self.stream
                .lock()
                .await
                .next()
                .await?
                .ok()?
                .to_text()
                .ok()?
                .parse()
                .unwrap_or(nostro2::NostrRelayEvent::Ping),
        )
    }
}
