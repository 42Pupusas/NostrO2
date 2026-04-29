use futures_util::{SinkExt, StreamExt};
use std::time::Duration;

/// Configuration for automatic reconnection with exponential backoff
///
/// When a relay connection drops, it will automatically attempt to reconnect
/// using an exponential backoff strategy.
#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    /// Maximum number of reconnection attempts (0 = infinite)
    pub max_retries: u32,
    /// Initial delay before first reconnection attempt
    pub initial_delay: Duration,
    /// Maximum delay between reconnection attempts
    pub max_delay: Duration,
    /// Multiplier for exponential backoff (e.g., 2.0 doubles the delay each time)
    pub backoff_multiplier: f64,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            max_retries: 0, // Infinite retries by default
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            backoff_multiplier: 2.0,
        }
    }
}

impl ReconnectConfig {
    /// Create a config with no automatic reconnection
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            max_retries: 0,
            initial_delay: Duration::from_secs(0),
            max_delay: Duration::from_secs(0),
            backoff_multiplier: 0.0,
        }
    }

    /// Check if reconnection is enabled
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.max_delay.as_secs() > 0
    }

    /// Calculate the next delay using exponential backoff
    #[must_use]
    pub fn next_delay(&self, attempt: u32) -> Duration {
        if !self.is_enabled() {
            return Duration::from_secs(0);
        }

        let delay_secs =
            self.initial_delay.as_secs_f64() * self.backoff_multiplier.powf(f64::from(attempt));
        Duration::from_secs_f64(delay_secs.min(self.max_delay.as_secs_f64()))
    }
}

#[derive(Clone)]
pub struct NostrRelay {
    /// Channel for receiving raw messages from the reader task
    receiver: std::sync::Arc<
        tokio::sync::RwLock<
            tokio::sync::mpsc::UnboundedReceiver<tokio_tungstenite::tungstenite::Utf8Bytes>,
        >,
    >,
    /// Channel for sending messages to the writer task
    sender: tokio::sync::mpsc::UnboundedSender<tokio_tungstenite::tungstenite::Utf8Bytes>,
    /// URL of the relay for reconnection
    #[allow(dead_code)]
    url: std::sync::Arc<String>,
    /// Reconnection configuration
    #[allow(dead_code)]
    reconnect_config: std::sync::Arc<ReconnectConfig>,
}
impl NostrRelay {
    /// Creates a new relay connection with default reconnection settings.
    ///
    /// By default, the relay will automatically reconnect with exponential backoff
    /// if the connection drops.
    ///
    /// # Errors
    ///
    /// Returns an error if the initial connection fails.
    pub async fn new(url: &str) -> Result<Self, crate::errors::NostrRelayError> {
        Self::with_reconnect(url, ReconnectConfig::default()).await
    }

    /// Creates a new relay connection with custom reconnection configuration.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use nostro2_relay::{NostrRelay, ReconnectConfig};
    /// use std::time::Duration;
    ///
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// // Custom reconnection with max 10 retries
    /// let config = ReconnectConfig {
    ///     max_retries: 10,
    ///     initial_delay: Duration::from_secs(1),
    ///     max_delay: Duration::from_secs(30),
    ///     backoff_multiplier: 2.0,
    /// };
    /// let relay = NostrRelay::with_reconnect("wss://relay.example.com", config).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the initial connection fails.
    pub async fn with_reconnect(
        url: &str,
        reconnect_config: ReconnectConfig,
    ) -> Result<Self, crate::errors::NostrRelayError> {
        // Create persistent channels for communication
        let (incoming_tx, incoming_rx) =
            tokio::sync::mpsc::unbounded_channel::<tokio_tungstenite::tungstenite::Utf8Bytes>();
        let (outgoing_tx, outgoing_rx) =
            tokio::sync::mpsc::unbounded_channel::<tokio_tungstenite::tungstenite::Utf8Bytes>();

        let url = url.to_string();
        let url_arc = std::sync::Arc::new(url.clone());
        let reconnect_config_arc = std::sync::Arc::new(reconnect_config.clone());

        // Try initial connection
        let initial_connection = Self::connect(&url).await?;
        let (sink, stream) = futures_util::StreamExt::split(initial_connection);

        // Spawn connection manager task
        tokio::spawn(Self::connection_manager(
            url,
            reconnect_config,
            incoming_tx,
            outgoing_rx,
            sink,
            stream,
        ));

        Ok(Self {
            receiver: std::sync::Arc::new(tokio::sync::RwLock::new(incoming_rx)),
            sender: outgoing_tx,
            url: url_arc,
            reconnect_config: reconnect_config_arc,
        })
    }

    /// Establishes a WebSocket connection to the relay
    async fn connect(
        url: &str,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        crate::errors::NostrRelayError,
    > {
        let (websocket, _response) = tokio_tungstenite::connect_async_with_config(
            url,
            Some(
                tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                    .max_write_buffer_size(5 << 20) // 5 MiB
                    .max_frame_size(Some(256 << 10)) // 256 KiB
                    .max_message_size(Some(5 << 20)) // 5 MiB
                    .read_buffer_size(4 << 20) // 4 MiB
                    .write_buffer_size(4 << 20), // 4 MiB
            ),
            false,
        )
        .await?;
        Ok(websocket)
    }

    /// Manages the connection lifecycle with automatic reconnection
    #[allow(clippy::too_many_lines)]
    async fn connection_manager(
        url: String,
        config: ReconnectConfig,
        incoming_tx: tokio::sync::mpsc::UnboundedSender<tokio_tungstenite::tungstenite::Utf8Bytes>,
        mut outgoing_rx: tokio::sync::mpsc::UnboundedReceiver<
            tokio_tungstenite::tungstenite::Utf8Bytes,
        >,
        initial_sink: futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            tokio_tungstenite::tungstenite::Message,
        >,
        initial_stream: futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    ) {
        let mut attempt = 0;
        let mut current_sink = initial_sink;
        let mut current_stream = initial_stream;

        loop {
            // Run the connection until it fails
            let _result = Self::run_connection(
                &incoming_tx,
                &mut outgoing_rx,
                &mut current_sink,
                &mut current_stream,
            )
            .await;

            // Check if we should reconnect
            if !config.is_enabled() {
                // Reconnection disabled, exit
                break;
            }

            if config.max_retries > 0 && attempt >= config.max_retries {
                log::warn!(
                    "max reconnection attempts ({}) reached for {url}",
                    config.max_retries
                );
                break;
            }

            let delay = config.next_delay(attempt);
            if delay.as_secs() == 0 {
                break;
            }

            log::info!(
                "connection to {url} lost, reconnecting in {delay:?} (attempt {})",
                attempt + 1
            );
            tokio::time::sleep(delay).await;

            match Self::connect(&url).await {
                Ok(websocket) => {
                    log::info!("reconnected to {url}");
                    let (sink, stream) = futures_util::StreamExt::split(websocket);
                    current_sink = sink;
                    current_stream = stream;
                    attempt = 0;
                }
                Err(e) => {
                    log::warn!("failed to reconnect to {url}: {e}");
                    attempt += 1;
                }
            }
        }
    }

    /// Runs the connection, handling read/write operations
    async fn run_connection(
        incoming_tx: &tokio::sync::mpsc::UnboundedSender<tokio_tungstenite::tungstenite::Utf8Bytes>,
        outgoing_rx: &mut tokio::sync::mpsc::UnboundedReceiver<
            tokio_tungstenite::tungstenite::Utf8Bytes,
        >,
        sink: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            tokio_tungstenite::tungstenite::Message,
        >,
        stream: &mut futures_util::stream::SplitStream<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
        >,
    ) -> Result<(), ()> {
        loop {
            tokio::select! {
                // Handle incoming messages from WebSocket
                Some(msg) = stream.next() => {
                    match msg {
                        Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                            if incoming_tx.send(text).is_err() {
                                // Receiver dropped, exit
                                return Err(());
                            }
                        }
                        Ok(tokio_tungstenite::tungstenite::Message::Close(_)) | Err(_) => {
                            // Connection closed or error
                            return Err(());
                        }
                        _ => {
                            // Ignore other message types (binary, ping, pong)
                        }
                    }
                }
                // Handle outgoing messages to WebSocket
                Some(msg) = outgoing_rx.recv() => {
                    if sink
                        .send(tokio_tungstenite::tungstenite::Message::Text(msg))
                        .await
                        .is_err()
                    {
                        // Error writing to sink
                        return Err(());
                    }
                }
                else => {
                    // Both channels closed
                    let _ = sink.flush().await;
                    return Err(());
                }
            }
        }
    }
    /// Sends a message to the relay.
    /// Message must implement `Into<NostrClientEvent>`.
    ///
    /// # Errors
    ///
    /// Returns an error if the message fails to send.
    pub fn send<T>(&self, msg: T) -> Result<(), crate::errors::NostrRelayError>
    where
        T: Into<nostro2::NostrClientEvent> + Send + Sync,
    {
        let msg: nostro2::NostrClientEvent = msg.into();
        // Pre-serialize JSON before sending to writer task
        let msg_str = serde_json::to_string(&msg).map_err(crate::errors::NostrRelayError::Serde)?;
        self.sender
            .send(msg_str.into())
            .map_err(|_| crate::errors::NostrRelayError::SendError)?;
        Ok(())
    }
    /// Sends multiple messages to the relay.
    /// Messages are pre-serialized and sent through the writer task.
    /// Message must implement `Into<NostrClientEvent>`.
    ///
    /// # Errors
    ///
    /// Returns an error if any message fails to send.
    pub async fn send_all<St, T>(
        &self,
        mut stream: St,
    ) -> Result<(), crate::errors::NostrRelayError>
    where
        T: Into<nostro2::NostrClientEvent> + Send + Sync + std::fmt::Debug,
        St: futures_util::Stream<Item = T> + Unpin + Sized,
    {
        while let Some(msg) = stream.next().await {
            let msg: nostro2::NostrClientEvent = msg.into();
            let msg_str =
                serde_json::to_string(&msg).map_err(crate::errors::NostrRelayError::Serde)?;
            self.sender
                .send(msg_str.into())
                .map_err(|_| crate::errors::NostrRelayError::SendError)?;
        }
        Ok(())
    }

    /// Receives a message from the relay.
    /// Pulls raw text from the reader task's channel and parses it.
    ///
    /// # Errors
    ///
    /// Returns `None` if the stream is closed or the frame fails to parse as a
    /// NIP-01 / NIP-42 message. Callers that need to distinguish "stream
    /// closed" from "garbage frame" should add a richer return type — the
    /// previous implementation collapsed parse failures into a fake `Ping`
    /// variant, which masked relay bugs.
    pub async fn recv(&self) -> Option<nostro2::NostrRelayEvent> {
        let msg_text = self.receiver.write().await.recv().await?;
        msg_text.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // Live-relay smoke test. Connects to a real wss endpoint and
    // streams an open subscription with no termination condition, so
    // it hangs `cargo test` indefinitely. Kept around for manual
    // verification (`cargo test -p nostro2-relay -- --ignored`); not
    // part of the default suite.
    #[tokio::test]
    #[ignore = "live relay; manual run only"]
    async fn test_relay() {
        let time = std::time::Instant::now();
        println!("Connecting to relay...");
        let relay = NostrRelay::new("wss://relay.illuminodes.com")
            .await
            .unwrap();
        let subscription = nostro2::NostrSubscription {
            kinds: vec![20001].into(),
            ..Default::default()
        };
        relay.send(subscription).unwrap();
        println!("Connected in {:?}", time.elapsed());
        while let Some(msg) = relay.recv().await {
            println!("{msg:?}",);
        }
        println!("Done in {:?}", time.elapsed());
    }
}
