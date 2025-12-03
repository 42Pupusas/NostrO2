use futures_util::{SinkExt, StreamExt};

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
                    .max_frame_size(Some(256 << 10)) // 256 KiB
                    .max_message_size(Some(5 << 20)) // 5 MiB
                    .read_buffer_size(4 << 20) // 128 KiB (increased from 8 KiB)
                    .write_buffer_size(4 << 20), // 128 KiB (increased from 8 KiB)
            ),
            false,
        )
        .await?;

        let (mut sink, mut stream) = futures_util::StreamExt::split(websocket);

        // Create channels for communication
        let (incoming_tx, incoming_rx) =
            tokio::sync::mpsc::unbounded_channel::<tokio_tungstenite::tungstenite::Utf8Bytes>();
        let (outgoing_tx, mut outgoing_rx) =
            tokio::sync::mpsc::unbounded_channel::<tokio_tungstenite::tungstenite::Utf8Bytes>();

        // Spawn reader task - continuously pumps messages from WebSocket to channel
        tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                        if incoming_tx.send(text).is_err() {
                            // Receiver dropped, exit task
                            break;
                        }
                    }
                    Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                        // Connection closed
                        break;
                    }
                    Err(_) => {
                        // Error reading from stream
                        break;
                    }
                    _ => {
                        // Ignore other message types (binary, ping, pong)
                    }
                }
            }
        });

        // Spawn writer task - continuously sends messages from channel to WebSocket
        tokio::spawn(async move {
            while let Some(msg) = outgoing_rx.recv().await {
                if sink
                    .send(tokio_tungstenite::tungstenite::Message::Text(msg))
                    .await
                    .is_err()
                {
                    // Error writing to sink, exit task
                    break;
                }
            }
            // Flush remaining messages before closing
            let _ = sink.flush().await;
        });

        Ok(Self {
            receiver: std::sync::Arc::new(tokio::sync::RwLock::new(incoming_rx)),
            sender: outgoing_tx,
        })
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
    /// Returns None if the stream is closed or the message fails to parse.
    pub async fn recv(&self) -> Option<nostro2::NostrRelayEvent> {
        let msg_text = self.receiver.write().await.recv().await?;
        // Parse raw string to NostrRelayEvent
        msg_text
            .parse()
            .ok()
            .or(Some(nostro2::NostrRelayEvent::Ping))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_relay() {
        let time = std::time::Instant::now();
        println!("Connecting to relay...");
        let relay = NostrRelay::new("wss://relay.damus.io").await.unwrap();
        let subscription = nostro2::NostrSubscription {
            kinds: vec![1].into(),
            limit: 5000.into(),
            ..Default::default()
        };
        relay.send(subscription).unwrap();
        println!("Connected in {:?}", time.elapsed());
        while let Some(msg) = relay.recv().await {
            if let nostro2::NostrRelayEvent::EndOfSubscription(..) = msg {
                break;
            }
        }
        println!("Done in {:?}", time.elapsed());
    }
}
