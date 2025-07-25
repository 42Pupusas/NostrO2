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
        let (websocket, _response) = tokio_tungstenite::connect_async_with_config(
            url,
            Some(
                tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                    .max_write_buffer_size(5 << 20) // 5 MiB
                    .max_frame_size(Some(256 << 10)) // 64 KiB
                    .max_message_size(Some(5 << 20)) // 2 MiB
                    .read_buffer_size(8 << 10) // 8 KiB
                    .write_buffer_size(8 << 10), // 8 KiB
            ),
            false,
        )
        .await?;

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
    /// Feeds a message to the relay, without flushing.
    /// Use for batching messages to be sent later.
    /// Message must implement `Into<NostrClientEvent>`.
    ///
    /// # Errors
    ///
    /// Returns an error if the message fails to send.
    pub async fn send_all<St, T>(&self, stream: St) -> Result<(), crate::errors::NostrRelayError>
    where
        T: Into<nostro2::NostrClientEvent> + Send + Sync + std::fmt::Debug,
        St: futures_util::Stream<Item = T> + Unpin + Sized,
    {
        let mut stream = stream.map(|msg: T| {
            let msg: nostro2::NostrClientEvent = msg.into();
            serde_json::to_string(&msg)
                .map(std::convert::Into::into)
                .map_err(|_| tokio_tungstenite::tungstenite::Error::Utf8)
        });
        self.sink.lock().await.send_all(&mut stream).await?;
        Ok(())
    }
    /// Flushes the sink, sending all buffered messages to the relay.
    /// # Errors
    /// Returns an error if the sink fails to flush.
    pub async fn flush(&self) -> Result<(), crate::errors::NostrRelayError> {
        self.sink.lock().await.flush().await?;
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
