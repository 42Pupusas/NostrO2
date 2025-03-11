use web_sys::wasm_bindgen::JsCast;

#[derive(Debug)]
pub struct RelayState {
    ws: std::sync::Arc< web_sys::WebSocket>,
    // TODO
    // implement reconnection logic
    // url: &'static str,
    notified: tokio::sync::Notify,
}
impl RelayState {
    /// Create a new relay state
    ///
    /// # Errors
    ///
    /// This will error out if the websocket could not be created. This could be due to the relay
    /// being closed or an error occurred while creating the relay.
    pub fn new(
        url: &str,
    ) -> Result<std::rc::Rc<std::sync::RwLock<Self>>, web_sys::wasm_bindgen::JsValue> {
        let ws = web_sys::WebSocket::new(url)?;
        Ok(std::rc::Rc::new(std::sync::RwLock::new(Self {
            ws: ws.into(),
            // url,
            notified: tokio::sync::Notify::new(),
        })))
    }
    pub fn status(&self) -> nostro2::relay_events::RelayStatus {
        self.ws.ready_state().into()
    }
}
unsafe impl Send for RelayState {}

#[derive(Debug, Clone)]
pub struct NostrRelay {
    state: std::rc::Rc<std::sync::RwLock<RelayState>>,
    pub reader: std::rc::Rc<
        tokio::sync::RwLock<
            tokio::sync::broadcast::Receiver<nostro2::relay_events::NostrRelayEvent>,
        >,
    >,
}
impl NostrRelay {
    #[must_use]
    pub fn state(&self) -> nostro2::relay_events::RelayStatus {
        self.state
            .read()
            .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))
            .map(|state| state.status())
            .unwrap_or(nostro2::relay_events::RelayStatus::CLOSED)
    }
    pub async fn is_open(&self) -> bool {
        if let Ok(state) = self.state.read() {
            state.notified.notified().await;
            return state.status() == nostro2::relay_events::RelayStatus::OPEN;
        }
        false
    }
    /// Add an open handler to the relay
    ///
    /// # Errors
    ///
    /// This will error out if the lock is poisoned.
    pub fn on_open(
        &self,
        closure: impl FnMut() + 'static,
    ) -> Result<(), web_sys::wasm_bindgen::JsValue> {
        let Ok(ws) = self.state.read() else {
            return Err(web_sys::wasm_bindgen::JsValue::from_str(
                "Failed to read ws",
            ));
        };
        ws.ws.set_onopen(Some(
            web_sys::wasm_bindgen::closure::Closure::wrap(Box::new(closure) as Box<dyn FnMut()>)
                .into_js_value()
                .unchecked_ref(),
        ));
        Ok(())
    }
    /// Add a message handler to the relay
    ///
    /// # Errors
    ///
    /// This will error out if the lock is poisoned.
    pub fn on_message(
        &self,
        closure: impl FnMut(web_sys::MessageEvent) + 'static,
    ) -> Result<(), web_sys::wasm_bindgen::JsValue> {
        let Ok(ws) = self.state.read() else {
            return Err(web_sys::wasm_bindgen::JsValue::from_str(
                "Failed to read ws",
            ));
        };
        ws.ws.set_onmessage(Some(
            web_sys::wasm_bindgen::closure::Closure::wrap(Box::new(closure) as Box<dyn FnMut(_)>)
                .into_js_value()
                .unchecked_ref(),
        ));
        Ok(())
    }
    /// Add a close handler to the relay
    ///
    /// # Errors
    ///
    /// This will error out if the lock is poisoned.
    pub fn on_close(
        &self,
        closure: impl FnMut() + 'static,
    ) -> Result<(), web_sys::wasm_bindgen::JsValue> {
        let state_clone = self.state.clone();
        let Ok(ws) = state_clone.read() else {
            return Err(web_sys::wasm_bindgen::JsValue::from_str(
                "Failed to read ws",
            ));
        };
        ws.ws.set_onclose(Some(
            web_sys::wasm_bindgen::closure::Closure::wrap(Box::new(closure) as Box<dyn FnMut()>)
                .into_js_value()
                .unchecked_ref(),
        ));
        Ok(())
    }
    /// Add an error handler to the relay
    ///
    /// # Errors
    ///
    /// This will error out if the lock is poisoned.
    pub fn on_error(
        &self,
        closure: impl FnMut() + 'static,
    ) -> Result<(), web_sys::wasm_bindgen::JsValue> {
        let state_clone = self.state.clone();
        let Ok(ws) = state_clone.read() else {
            return Err(web_sys::wasm_bindgen::JsValue::from_str(
                "Failed to read ws",
            ));
        };
        ws.ws.set_onerror(Some(
            web_sys::wasm_bindgen::closure::Closure::wrap(Box::new(closure) as Box<dyn FnMut()>)
                .into_js_value()
                .unchecked_ref(),
        ));
        Ok(())
    }
    /// Create a new relay
    ///
    /// # Errors
    ///
    /// This will error out if the relay could not be created. This could be due to the relay being
    /// closed or an error occurred while creating the relay.
    pub fn new(url: &str) -> Result<Self, web_sys::wasm_bindgen::JsValue> {
        let state = RelayState::new(url)?;
        let (sender, reader) = tokio::sync::broadcast::channel(100);
        let reader = std::rc::Rc::new(tokio::sync::RwLock::new(reader));
        let new_self = Self {
            state: state.clone(),
            reader,
        };

        let state_clone = state.clone();
        let on_open = move || {
            if let Ok(state) = state_clone.read() {
                state.notified.notify_waiters();
            }
        };

        let state_clone = state.clone();
        let on_message = move |event: web_sys::MessageEvent| {
            let Some(Ok(event)) = event.data().as_string().map(|s| s.parse()) else {
                return;
            };
            let _ = sender
                .send(event)
                .map_err(|_| state_clone.read().map(|ws| ws.ws.close_with_code(1000)));
        };

        let on_close = move || {
            if let Ok(state) = state.read() {
                state.notified.notify_waiters();
            }
        };
        let on_error = move || {};

        new_self.on_open(on_open)?;
        new_self.on_message(on_message)?;
        new_self.on_close(on_close)?;
        new_self.on_error(on_error)?;

        Ok(new_self)
    }
    /// Send an event to the relay
    ///
    /// # Errors
    ///
    /// This will error out if the lock is poisoned.
    pub fn send(
        &self,
        event: &nostro2::relay_events::NostrClientEvent,
    ) -> Result<(), web_sys::wasm_bindgen::JsValue> {
        let ws = self
            .state
            .read()
            .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?
            .ws
            .clone();
        ws.send_with_str(event.to_string().as_str())
    }
    /// Close the relay
    ///
    /// # Errors
    ///
    /// This will error out if the lock is poisoned.
    pub fn close(&self) -> Result<(), web_sys::wasm_bindgen::JsValue> {
        self.state
            .read()
            .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?
            .ws
            .close()
    }
}

#[derive(Debug, Default)]
pub struct RelayPool {
    seen_notes: std::sync::RwLock<std::collections::HashSet<String>>,
    relays: std::sync::RwLock<std::collections::HashMap<String, NostrRelay>>,
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
    pub fn get(&self, url: &str) -> Result<NostrRelay, web_sys::wasm_bindgen::JsValue> {
        let mut relays = self
            .relays
            .write()
            .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?;
        if let Some(relay) = relays.get(url) {
            return Ok(relay.clone());
        }
        let relay = NostrRelay::new(url)?;
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
    pub async fn read(
        &self,
    ) -> Result<nostro2::relay_events::NostrRelayEvent, web_sys::wasm_bindgen::JsValue> {
        // let relays = self
        //     .relays
        //     .read()
        //     .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?
        //     .to_owned();
        let tasks = self
            .relays
            .read()
            .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?
            .values()
            .map(|relay| {
                let reader = relay.reader.clone();
                Box::pin(async move {
                    let msg = reader.write().await.recv().await.map_err(|_| {
                        web_sys::wasm_bindgen::JsValue::from_str("Failed to receive message")
                    })?;
                    if let nostro2::relay_events::NostrRelayEvent::NewNote(.., ref note) = msg {
                        let id = note.id.as_ref().ok_or_else(|| {
                            web_sys::wasm_bindgen::JsValue::from_str("Note has no id")
                        })?;
                        let mut seen_notes = self.seen_notes.write().map_err(|e| {
                            web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str())
                        })?;
                        if seen_notes.contains(id) {
                            return Err(web_sys::wasm_bindgen::JsValue::from_str(
                                "Note already seen",
                            ));
                        }
                        seen_notes.insert(id.to_string());
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

#[cfg(test)]
mod tests {

    use web_sys::wasm_bindgen::JsCast;

    use super::*;
    wasm_bindgen_test::wasm_bindgen_test_configure!(run_in_browser);

    // #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_wasm_connection() {
        let relay = NostrRelay::new("wss://relay.illuminodes.com").unwrap();
        relay.is_open().await;
        assert_eq!(relay.state(), nostro2::relay_events::RelayStatus::OPEN);
        let filter = nostro2::filters::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(10),
            ..Default::default()
        };
        relay.send(&filter.into()).expect("Failed to send filter");
        let mut reader = relay.reader.write().await;

        while let Ok(msg) = reader.recv().await {
            let nostro2::relay_events::NostrRelayEvent::EndOfSubscription(_, _) = msg else {
                continue;
            };

            break;
        }
        relay.close().expect("Failed to close relay");
        assert_eq!(relay.state(), nostro2::relay_events::RelayStatus::CLOSING);
    }
    // #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_two_connections() {
        let relay = NostrRelay::new("wss://relay.illuminodes.com").unwrap();
        let relay_2 = NostrRelay::new("wss://relay.arrakis.lat").unwrap();
        let relay_4 = NostrRelay::new("wss://a.nos.lol").unwrap();
        wasm_bindgen_test::console_log!("Created relays");
        let count = std::rc::Rc::new(std::sync::RwLock::new(0));
        let count_clone = count.clone();
        wasm_bindgen_futures::spawn_local(async move {
            relay.is_open().await;
            assert_eq!(relay.state(), nostro2::relay_events::RelayStatus::OPEN);
            if let Ok(mut count) = count_clone.write() {
                *count += 1;
            }
        });
        let count_clone = count.clone();
        wasm_bindgen_futures::spawn_local(async move {
            relay_2.is_open().await;
            assert_eq!(relay_2.state(), nostro2::relay_events::RelayStatus::OPEN);
            wasm_bindgen_test::console_log!("Relay 2 is open");
            if let Ok(mut count) = count_clone.write() {
                *count += 1;
            }
        });
        let count_clone = count.clone();
        wasm_bindgen_futures::spawn_local(async move {
            relay_4.is_open().await;
            assert_eq!(relay_4.state(), nostro2::relay_events::RelayStatus::OPEN);
            wasm_bindgen_test::console_log!("Relay 4 is open");
            if let Ok(mut count) = count_clone.write() {
                *count += 1;
            }
        });
        web_sys::window()
            .unwrap()
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                web_sys::wasm_bindgen::closure::Closure::new(Box::new(move || {
                    if let Ok(count) = count.read() {
                        assert_eq!(*count, 3);
                    }
                })
                    as Box<dyn FnMut()>)
                .into_js_value()
                .unchecked_ref(),
                500,
            )
            .unwrap();
    }
    // #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_relay_pool() {
        let pool: RelayPool = [
            "wss://relay.illuminodes.com",
            "wss://relay.arrakis.lat",
            "wss://frens.nostr1.com",
            "wss://bitcoiner.social",
            "wss://bouncer.minibolt.info",
            "wss://freespeech.casa",
            "wss://junxingwang.org",
            "wss://nostr.0x7e.xyz",
        ]
        .as_slice()
        .into();

        pool.connect().await.unwrap();
        let filter = nostro2::filters::NostrSubscription {
            kinds: vec![1].into(),
            limit: Some(10),
            ..Default::default()
        };
        pool.send(&(filter.into())).expect("Failed to send filter");
        loop {
            let Ok(msg) = pool.read().await else {
                wasm_bindgen_test::console_log!("Failed to read from pool");
                continue;
            };
            let nostro2::relay_events::NostrRelayEvent::EndOfSubscription(_, _) = msg else {
                break;
            };
            wasm_bindgen_test::console_log!("Received {:?}", msg);
        }
    }
    #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _stress_test_relay_pool() {
        let pool: RelayPool = [
            "wss://relay.illuminodes.com",
            "wss://relay.arrakis.lat",
            "wss://frens.nostr1.com",
            "wss://bitcoiner.social",
            "wss://bouncer.minibolt.info",
            "wss://freespeech.casa",
            "wss://junxingwang.org",
            "wss://nostr.0x7e.xyz",
        ]
        .as_slice()
        .into();

        pool.connect().await.unwrap();
        let filter = nostro2::filters::NostrSubscription {
            kinds: vec![1].into(),
            ..Default::default()
        };
        pool.send(&filter.into()).expect("Failed to send filter");
        let mut count = 0;
        loop {
            let Ok(msg) = pool.read().await else {
                wasm_bindgen_test::console_log!("Failed to read from pool");
                continue;
            };
            if let nostro2::relay_events::NostrRelayEvent::NewNote(..) = msg {
                count += 1;
            };
            if count > 100 {
                break;
            }
            // wasm_bindgen_test::console_log!("Received {:?}", msg);
        }
        assert!(count > 100);
    }
}
