#[derive(Debug, Default)]
pub struct RelayPool {
    seen_notes: std::sync::RwLock<std::collections::HashSet<String>>,
    relays: std::sync::RwLock<std::collections::HashMap<String, crate::relay::NostrRelay>>,
}
impl From<&[String]> for RelayPool {
    fn from(urls: &[String]) -> Self {
        let pool = Self::default();
        for url in urls {
            let _ = pool.get(url.as_str());
        }
        pool
    }
}
impl From<&[&str]> for RelayPool {
    fn from(urls: &[&str]) -> Self {
        let pool = Self::default();
        for url in urls {
            let _ = pool.get(url);
        }
        pool
    }
}
impl RelayPool {
    /// Get a relay from the pool
    ///
    /// This function will return a relay from the pool. If the relay is not in the pool, it will
    /// create a new relay and add it to the pool. If the relay is already in the pool, it will
    /// return the relay.
    ///
    /// # Errors
    ///
    /// This function will return an error if the relay could not be created. This could be due to
    /// the relay being closed or an error occurred while creating the relay.
    pub fn get(
        &self,
        url: &str,
    ) -> Result<crate::relay::NostrRelay, web_sys::wasm_bindgen::JsValue> {
        let mut relays = self
            .relays
            .write()
            .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?;
        if let Some(relay) = relays.get(url) {
            return Ok(relay.clone());
        }
        let relay = crate::relay::NostrRelay::new(url)?;
        relays.insert(url.to_string(), relay.clone());
        drop(relays);
        Ok(relay)
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
    pub fn remove(&self, url: &'static str) -> Result<(), web_sys::wasm_bindgen::JsValue> {
        let Some(relay) = self
            .relays
            .write()
            .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?
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
    pub fn close_all(&self) -> Result<(), web_sys::wasm_bindgen::JsValue> {
        for relay in self
            .relays
            .read()
            .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?
            .values()
        {
            relay.close()?;
        }
        Ok(())
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
    pub fn close(&self, url: &'static str) -> Result<(), web_sys::wasm_bindgen::JsValue> {
        if let Some(relay) = self
            .relays
            .read()
            .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?
            .get(url)
        {
            relay.close()?;
        }
        Ok(())
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
        let tasks = self
            .relays
            .read()
            .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?
            .values()
            .map(|relay| {
                let relay = relay.clone();
                async move {
                    relay.is_open().await;
                }
            })
            .collect::<Vec<_>>();
        let _ = futures_util::future::join_all(tasks).await;
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
    pub fn send(
        &self,
        event: &nostro2::relay_events::NostrClientEvent,
    ) -> Result<(), web_sys::wasm_bindgen::JsValue> {
        for relay in self
            .relays
            .read()
            .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?
            .values()
            .filter(|relay| relay.state() == nostro2::relay_events::RelayStatus::OPEN)
        {
            relay.send(event)?;
        }
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
        let tasks = self
            .relays
            .read()
            .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?
            .values()
            .map(|relay| {
                let mut reader = relay.reader.resubscribe();
                Box::pin(async move {
                    let msg = reader.recv().await.map_err(|_| {
                        web_sys::wasm_bindgen::JsValue::from_str("Failed to receive message")
                    })?;
                    if let nostro2::relay_events::NostrRelayEvent::NewNote(.., ref note) = msg {
                        let id = note.id.as_ref().ok_or_else(|| {
                            web_sys::wasm_bindgen::JsValue::from_str("Note has no id")
                        })?;
                        if !self
                            .seen_notes
                            .write()
                            .map_err(|e| {
                                web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str())
                            })?
                            .insert(id.to_string())
                        {
                            return Err(web_sys::wasm_bindgen::JsValue::from_str(
                                "Note already seen",
                            ));
                        }
                    }
                    Ok(msg)
                })
            })
            .collect::<Vec<_>>();

        futures_util::future::select_ok(tasks)
            .await
            .map(|(msg, _)| msg)
    }
}
