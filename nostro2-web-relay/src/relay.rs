use futures_util::FutureExt;
use web_sys::wasm_bindgen::JsCast;

#[derive(Debug, Clone)]
pub struct NostrRelay {
    notified: std::rc::Rc<tokio::sync::Notify>,
    state: std::rc::Rc<std::sync::RwLock<web_sys::WebSocket>>,
    pub reader:
        std::rc::Rc<tokio::sync::broadcast::Receiver<nostro2::relay_events::NostrRelayEvent>>,
}
impl NostrRelay {
    #[must_use]
    pub fn state(&self) -> nostro2::relay_events::RelayStatus {
        self.state
            .read()
            .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))
            .map(|state| state.ready_state().into())
            .unwrap_or(nostro2::relay_events::RelayStatus::CLOSED)
    }
    #[allow(clippy::future_not_send)]
    pub async fn is_open(&self) -> bool {
        let notifier = self.notified.clone();
        notifier
            .notified()
            .map(|()| {
                self.state
                    .read()
                    .map(|state| {
                        nostro2::relay_events::RelayStatus::from(state.ready_state())
                            == nostro2::relay_events::RelayStatus::OPEN
                    })
                    .unwrap_or(false)
            })
            .await
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
        ws.set_onopen(Some(
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
        ws.set_onmessage(Some(
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
        ws.set_onclose(Some(
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
        ws.set_onerror(Some(
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
        let state = std::rc::Rc::new(std::sync::RwLock::new(web_sys::WebSocket::new(url)?));
        let (sender, reader) = tokio::sync::broadcast::channel(100);
        let reader = std::rc::Rc::new(reader);
        let new_self = Self {
            state: state.clone(),
            reader,
            notified: tokio::sync::Notify::new().into(),
        };

        let state_clone = new_self.notified.clone();
        let on_open = move || {
            state_clone.notify_waiters();
        };

        let on_message = move |event: web_sys::MessageEvent| {
            let Some(Ok(event)) = event.data().as_string().map(|s| s.parse()) else {
                return;
            };
            let _ = sender
                .send(event)
                .map_err(|_| state.read().map(|ws| ws.close_with_code(1000)));
        };

        let notifier = new_self.notified.clone();
        let on_close = move || {
            notifier.notify_waiters();
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
            .map_err(|e| web_sys::wasm_bindgen::JsValue::from_str(e.to_string().as_str()))?;
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
            .close()
    }
}
