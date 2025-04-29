// #[derive(Debug)]
// pub enum NostrRelayError {
//     Standard(Box<dyn std::error::Error>),
//     Tungstenite(tokio_tungstenite::tungstenite::Error),
// }
// impl std::fmt::Display for NostrRelayError {
//     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
//         write!(f, "NostrRelayError: {:?}", self.to_string())
//     }
// }
// impl std::error::Error for NostrRelayError {}
// impl From<tokio_tungstenite::tungstenite::Error> for NostrRelayError {
//     fn from(err: tokio_tungstenite::tungstenite::Error) -> Self {
//         NostrRelayError::Tungstenite(err)
//     }
// }
// impl From<Box<dyn std::error::Error>> for NostrRelayError {
//     fn from(err: Box<dyn std::error::Error>) -> Self {
//         NostrRelayError::Standard(err)
//     }
// }
//
use futures_util::{SinkExt, StreamExt};
//
// #[derive(Clone)]
// pub struct NostrRelay {
//     stream: std::sync::Arc<
//         tokio::sync::mpsc::UnboundedReceiver<nostro2::relay_events::NostrRelayEvent>,
//     >,
//     sink: tokio::sync::mpsc::UnboundedSender<nostro2::relay_events::NostrClientEvent>,
// }
// impl NostrRelay {
//     /// Creates a new relay connection to the given URL.
//     ///
//     /// # Errors
//     ///
//     /// Returns an error if the connection fails.
//     pub async fn new(url: &str) -> Result<Self, NostrRelayError> {
//         let (stream_tx, stream) = tokio::sync::mpsc::unbounded_channel();
//         let (sink, mut sink_rx) =
//             tokio::sync::mpsc::unbounded_channel::<nostro2::relay_events::NostrClientEvent>();
//         let (ws_stream, _) = tokio_tungstenite::connect_async(url).await?;
//         let (mut ws_sink, mut ws_stream) = ws_stream.split();
//         tokio::spawn(async move {
//             while let Some(msg) = ws_stream.next().await {
//                 let Ok(msg) = msg else {
//                     continue;
//                 };
//                 if let Ok(Ok(event)) = msg
//                     .to_text()
//                     .map(|s| s.parse::<nostro2::relay_events::NostrRelayEvent>())
//                 {
//                     stream_tx.send(event).unwrap();
//                 }
//             }
//         });
//         tokio::spawn(async move {
//             while let Some(msg) = sink_rx.recv().await {
//                 if let Err(e) = ws_sink.send(msg.to_string().into()).await {
//                     eprintln!("Failed to send message: {}", e);
//                 }
//             }
//         });
//         Ok(Self {
//             stream: stream.into(),
//             sink,
//         })
//     }
// }

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
        Some(
            self.stream
                .write()
                .await
                .next()
                .await?
                .ok()?
                .to_text()
                .ok()?
                .parse()
                .unwrap_or(nostro2::relay_events::NostrRelayEvent::Ping),
        )
    }
}
