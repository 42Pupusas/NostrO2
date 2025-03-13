use futures_util::{SinkExt, StreamExt};

pub struct NostrRelay {
    stream: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    sink: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::tungstenite::Message,
    >,
}
impl NostrRelay {
    /// Creates a new relay connection to the given URL.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails.
    pub async fn new(url: &str) -> Result<Self, std::io::Error> {
        let (websocket, _response) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::HostUnreachable, e))?;
        let (sink, stream) = websocket.split();
        Ok(Self { stream, sink })
    }
    /// Sends a message to the relay.
    /// Message must implement `Into<NostrClientEvent>`.
    ///
    /// # Errors
    ///
    /// Returns an error if the message fails to send.
    pub async fn send<T>(&mut self, msg: T) -> Result<(), std::io::Error>
    where
        T: Into<nostro2::relay_events::NostrClientEvent> + Send + Sync,
    {
        let msg: nostro2::relay_events::NostrClientEvent = msg.into();
        self.sink
            .send(msg.to_string().into())
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::HostUnreachable, e))?;
        Ok(())
    }
    /// Receives a message from the relay.
    ///
    /// # Errors
    ///
    /// Returns an error if the message fails to receive, due to the stream being closed.
    /// Should never failed to parse the message, as it is guaranteed to be a valid
    /// `NostrRelayEvent`.
    pub async fn recv(&mut self) -> Option<nostro2::relay_events::NostrRelayEvent> {
        self.stream.next().await?.ok()?.to_text().ok()?.parse().ok()
    }
}
