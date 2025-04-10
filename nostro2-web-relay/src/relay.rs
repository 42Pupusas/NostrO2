use web_sys::wasm_bindgen::JsCast;

#[derive(Debug)]
pub struct NostrRelay {
    _url: String,
    state: tokio::sync::RwLock<tokio::sync::watch::Receiver<nostro2::relay_events::RelayStatus>>,
    reader: tokio::sync::RwLock<
        tokio::sync::mpsc::UnboundedReceiver<nostro2::relay_events::NostrRelayEvent>,
    >,
    writer: tokio::sync::RwLock<
        tokio::sync::mpsc::UnboundedSender<nostro2::relay_events::NostrClientEvent>,
    >,
    closer: tokio::sync::RwLock<tokio::sync::mpsc::Sender<()>>,
}
impl NostrRelay {
    #[must_use]
    pub fn new(url: &str) -> Self {
        let (state_tx, state_rx) =
            tokio::sync::watch::channel(nostro2::relay_events::RelayStatus::CONNECTING);
        let (reader_tx, reader_rx) = tokio::sync::mpsc::unbounded_channel();
        let (writer_tx, mut writer_rx) =
            tokio::sync::mpsc::unbounded_channel::<nostro2::relay_events::NostrClientEvent>();
        let (closer_tx, mut closer_rx) = tokio::sync::mpsc::channel(1);
        let new_url = url.to_string();
        wasm_bindgen_futures::spawn_local(async move {
            let Ok(ws) = web_sys::WebSocket::new(&new_url) else {
                let _ = state_tx.send(nostro2::relay_events::RelayStatus::CLOSED);
                return;
            };
            let state_clone = state_tx.clone();
            ws.set_onopen(Some(
                web_sys::wasm_bindgen::closure::Closure::once_into_js(move || {
                    let _ = state_clone.send(nostro2::relay_events::RelayStatus::OPEN);
                })
                .unchecked_ref(),
            ));
            let state_clone = state_tx.clone();
            ws.set_onmessage(Some(
                web_sys::wasm_bindgen::closure::Closure::wrap(Box::new(
                    move |event: web_sys::MessageEvent| {
                        let Some(Ok(event)) = event.data().as_string().map(|s| s.parse()) else {
                            return;
                        };
                        reader_tx.send(event).is_err().then(|| {
                            let _ = state_clone.send(nostro2::relay_events::RelayStatus::CLOSING);
                        });
                    },
                )
                    as Box<dyn FnMut(_)>)
                .into_js_value()
                .unchecked_ref(),
            ));
            let state_clone = state_tx.clone();
            ws.set_onclose(Some(
                web_sys::wasm_bindgen::closure::Closure::once_into_js(move || {
                    let _ = state_clone.send(nostro2::relay_events::RelayStatus::CLOSED);
                })
                .unchecked_ref(),
            ));
            let ws_clone = ws.clone();
            let state_clone = state_tx.clone();
            ws.set_onerror(Some(
                web_sys::wasm_bindgen::closure::Closure::once_into_js(move || {
                    let _ = state_clone.send(nostro2::relay_events::RelayStatus::CLOSING);
                    let _ = ws_clone.close();
                })
                .unchecked_ref(),
            ));
            loop {
                tokio::select! {
                    Some(msg) = writer_rx.recv() => {
                        if let Err(_err) = ws.send_with_str(msg.to_string().as_str()) {
                            let _ = state_tx.send(nostro2::relay_events::RelayStatus::CLOSING);
                            break;
                        }
                    }
                    _ = closer_rx.recv() => {
                        let _ = ws.close();
                        let _ = state_tx
                            .send(nostro2::relay_events::RelayStatus::CLOSED);
                        break;
                    }
                }
            }
        });

        Self {
            _url: url.to_string(),
            state: state_rx.into(),
            reader: reader_rx.into(),
            writer: writer_tx.into(),
            closer: closer_tx.into(),
        }
    }

    pub async fn relay_state(&self) -> nostro2::relay_events::RelayStatus {
        let status_watch: tokio::sync::watch::Receiver<nostro2::relay_events::RelayStatus> =
            self.state.read().await.clone();
        let status = status_watch.borrow();
        *status
    }

    pub async fn is_open(&self) -> bool {
        let _ = self
            .state
            .write()
            .await
            .wait_for(|status| {
                status == &nostro2::relay_events::RelayStatus::OPEN
                    || status == &nostro2::relay_events::RelayStatus::CLOSED
            })
            .await
            .is_ok();
        if self.relay_state().await == nostro2::relay_events::RelayStatus::OPEN {
            true
        } else {
            self.disconnect().await;
            false
        }
    }

    /// Send an event to the relay
    ///
    /// # Errors
    ///
    /// If the event cannot be sent to the relay, an error is returned.
    pub async fn send<T>(
        &self,
        event: T,
    ) -> Result<nostro2::relay_events::NostrClientEvent, Box<dyn std::error::Error>>
    where
        T: Into<nostro2::relay_events::NostrClientEvent> + Send + 'static + Sync,
    {
        if !self.is_open().await {
            self.disconnect().await;
            return Err("Relay is not open".into());
        };
        // Send the event to the relay
        let msg: nostro2::relay_events::NostrClientEvent = event.into();
        self.writer.write().await.send(msg.clone())?;
        Ok(msg)
    }

    pub async fn read(&self) -> Option<nostro2::relay_events::NostrRelayEvent> {
        // Return the reader
        self.reader.write().await.recv().await // Return the event
    }

    pub async fn disconnect(&self) {
        let _ = self.closer.write().await.send(()).await;
        let _ = self
            .state
            .write()
            .await
            .wait_for(|status| status == &nostro2::relay_events::RelayStatus::CLOSED)
            .await;
    }
}
