use futures_util::{SinkExt, StreamExt};

#[derive(Clone)]
pub struct NostrRelay {
    stream: std::sync::Arc<
        tokio::sync::RwLock<
            futures_util::stream::SplitStream<
                tokio_tungstenite::WebSocketStream<
                    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
                >,
            >,
        >,
    >,
    sink: std::sync::Arc<
        tokio::sync::RwLock<
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
    pub async fn new(url: &str) -> Result<Self, std::io::Error> {
        let Ok((websocket, _response)) = tokio_tungstenite::connect_async(url).await else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "Failed to connect to relay",
            ));
        };
        let (sink, stream) = websocket.split();
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
    pub async fn send<T>(&self, msg: T) -> Result<(), std::io::Error>
    where
        T: Into<nostro2::relay_events::NostrClientEvent> + Send + Sync,
    {
        let msg: nostro2::relay_events::NostrClientEvent = msg.into();
        if self
            .sink
            .write()
            .await
            .send(msg.to_string().into())
            .await
            .is_err()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "Failed to send message to relay",
            ));
        }
        Ok(())
    }
    /// Receives a message from the relay.
    ///
    /// # Errors
    ///
    /// Returns an error if the message fails to receive, due to the stream being closed.
    /// Should never failed to parse the message, as it is guaranteed to be a valid
    /// `NostrRelayEvent`.
    pub async fn recv(&self) -> Option<nostro2::relay_events::NostrRelayEvent> {
        self.stream
            .write()
            .await
            .next()
            .await?
            .ok()?
            .to_text()
            .ok()?
            .parse()
            .ok()
    }
}
