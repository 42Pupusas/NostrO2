#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PoolStatus {
    #[default]
    Disconnected,
    Connecting,
    Connected,
}

#[derive(Debug, Default)]
pub struct RelayPool {
    pub status: tokio::sync::RwLock<PoolStatus>,
    seen_notes: tokio::sync::RwLock<std::collections::HashSet<String>>,
    relays: tokio::sync::RwLock<std::collections::HashMap<String, crate::relay::NostrRelay>>,
}
impl PartialEq for RelayPool {
    fn eq(&self, other: &Self) -> bool {
        self.relays
            .try_read()
            .map(|r| {
                r.keys().all(|k| {
                    other
                        .relays
                        .try_read()
                        .map(|o| o.contains_key(k))
                        .unwrap_or(true)
                })
            })
            .unwrap_or(true)
            && self
                .seen_notes
                .try_read()
                .map(|s| {
                    other
                        .seen_notes
                        .try_read()
                        .map_or(true, |other_seen| s.eq(&other_seen))
                })
                .unwrap_or(true)
    }
}
impl From<&[String]> for RelayPool {
    fn from(urls: &[String]) -> Self {
        let mut relays = std::collections::HashMap::new();
        for url in urls {
            if let Ok(new_relay) = crate::relay::NostrRelay::new(url.as_str()) {
                relays.insert(url.clone(), new_relay);
            }
        }
        Self {
            status: tokio::sync::RwLock::new(PoolStatus::Disconnected),
            seen_notes: tokio::sync::RwLock::new(std::collections::HashSet::new()),
            relays: tokio::sync::RwLock::new(relays),
        }
    }
}
impl From<&[&str]> for RelayPool {
    fn from(urls: &[&str]) -> Self {
        let mut relays = std::collections::HashMap::new();
        for url in urls {
            if let Ok(new_relay) = crate::relay::NostrRelay::new(url) {
                relays.insert((*url).to_string(), new_relay);
            }
        }
        Self {
            status: tokio::sync::RwLock::new(PoolStatus::Disconnected),
            seen_notes: tokio::sync::RwLock::new(std::collections::HashSet::new()),
            relays: tokio::sync::RwLock::new(relays),
        }
    }
}
impl RelayPool {
    #[allow(clippy::future_not_send)]
    pub async fn is_empty(&self) -> bool {
        self.relays.read().await.is_empty()
    }
    /// Remove a relay from the pool
    ///
    /// This function will remove the relay from the pool. If the relay is already removed, it will
    /// not attempt to remove the relay.
    ///
    /// # Errors
    ///
    /// This function will return an error if the relay could not be removed. This could be due to
    /// the lock being poisoned.
    #[allow(clippy::future_not_send)]
    pub async fn remove(&self, url: &'static str) -> Result<(), web_sys::wasm_bindgen::JsValue> {
        let Some(relay) = self
            .relays
            .write()
            .await
            // .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?
            .remove(url)
        else {
            return Err(web_sys::wasm_bindgen::JsValue::from_str("Relay not found"));
        };
        drop(relay);
        Ok(())
    }
    /// Close all relays in the pool
    ///
    /// # Errors
    ///
    /// This function will return an error if the relay could not be closed. This could be due to
    /// the relay being closed or an error occurred while closing the relay.
    #[allow(clippy::future_not_send)]
    pub async fn close_all(&self) -> Result<(), web_sys::wasm_bindgen::JsValue> {
        self.relays
            .write()
            .await
            .values()
            .try_for_each(super::relay::NostrRelay::close)
    }
    /// Close a relay in the pool
    ///
    /// This function will close the relay in the pool. If the relay is already closed, it will not
    /// attempt to close the relay.
    ///
    /// # Errors
    ///
    /// This function will return an error if the relay could not be closed. This could be due to
    /// the lock being poisoned or an error occurred while closing the relay.
    #[allow(clippy::future_not_send)]
    pub async fn close(&self, url: &'static str) -> Result<(), web_sys::wasm_bindgen::JsValue> {
        self.relays
            .read()
            .await
            .get(url)
            .map(super::relay::NostrRelay::close)
            .ok_or_else(|| web_sys::wasm_bindgen::JsValue::from_str("Relay not found"))?
    }
    /// Connect to all relays in the pool
    ///
    /// This function will connect to all relays in the pool. If the relay is already open, it will
    /// not attempt to reconnect to the relay.
    ///
    /// # Errors
    ///
    /// This function will return an error if the relay could not be connected to. This could been
    /// due to the relay being closed or an error occurred while connecting to the relay.
    #[allow(clippy::future_not_send)]
    pub async fn connect(&self) -> Result<(), web_sys::wasm_bindgen::JsValue> {
        let mut status = self.status.write().await;
        *status = PoolStatus::Connecting;
        futures_util::future::join_all(
            self.relays
                .read()
                .await
                // .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?
                .values()
                .map(|relay| async move {
                    relay.is_open().await;
                }),
        )
        .await;
        if self.is_empty().await {
            *status = PoolStatus::Disconnected;
        } else {
            *status = PoolStatus::Connected;
            drop(status);
        }
        Ok(())
    }

    /// Send an event to all open relays in the pool
    ///
    /// This function will send the event to all open relays in the pool. If the relay is not open,
    /// the event will not be sent to that relay.
    ///
    /// # Errors
    ///
    /// This function will return an error if the event could not be sent to the relay. This could
    /// be due to the relay being closed or an error occurred while sending the event.
    #[allow(clippy::future_not_send)]
    pub async fn send<T>(&self, event: T) -> Result<(), web_sys::wasm_bindgen::JsValue>
    where
        T: Into<nostro2::relay_events::NostrClientEvent> + Clone + Send + Sync,
    {
        futures_util::future::join_all(
            self.relays
                .read()
                .await
                // .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?
                .values()
                .filter(|relay| relay.state() == nostro2::relay_events::RelayStatus::OPEN)
                .map(|relay| {
                    let event = event.clone();
                    async move {
                        relay.send(event)?;
                        Ok::<(), web_sys::wasm_bindgen::JsValue>(())
                    }
                }),
        )
        .await;
        Ok(())
    }
    /// Receive the next ready message from the pool of relays
    ///
    /// This function will return the next message from the pool of relays that is ready to be
    /// read. If no message is ready, it will wait until a message is ready. If the next message
    /// is a duplicate of a message that has already been seen, it will be skipped and the next
    /// message will be returned.
    ///
    /// # Errors
    ///
    /// This function will return an error if the message could not be received from the relay.
    /// This could be due to the relay being closed or an error occurred while receiving the
    /// message.
    #[allow(clippy::future_not_send)]
    pub async fn read(
        &self,
    ) -> Result<nostro2::relay_events::NostrRelayEvent, web_sys::wasm_bindgen::JsValue> {
        futures_util::future::select_ok(
            self.relays
                .write()
                .await
                // .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?
                .values_mut()
                .map(|relay| {
                    Box::pin(async move {
                        let msg = relay.reader.recv().await.map_err(|_| {
                            web_sys::wasm_bindgen::JsValue::from_str("Failed to receive message")
                        })?;
                        if let nostro2::relay_events::NostrRelayEvent::NewNote(.., ref note) = msg {
                            let id = note.id.as_ref().ok_or_else(|| {
                                web_sys::wasm_bindgen::JsValue::from_str("Note has no id")
                            })?;
                            if !self.seen_notes.write().await.insert(id.to_string()) {
                                return Ok(nostro2::relay_events::NostrRelayEvent::Ping);
                            }
                        }
                        Ok(msg)
                    })
                }),
        )
        .await
        .map(|(msg, _)| msg)
    }
}
