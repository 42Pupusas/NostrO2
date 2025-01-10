use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::{Notify, RwLock};

use super::{
    tcp::{NostrWebsocketWriter, WebSocketMessage},
    NostrWebsocketReader, RelayEvent, Url,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebsocketStatus {
    Connecting,
    Open,
    Closed(String),
}

#[derive(Clone)]
pub struct RelayStatus {
    state: Arc<RwLock<WebsocketStatus>>,
    notify: Arc<Notify>,
}
impl RelayStatus {
    fn new() -> Self {
        RelayStatus {
            state: Arc::new(RwLock::new(WebsocketStatus::Connecting)),
            notify: Arc::new(Notify::new()),
        }
    }
    async fn connected(&self) {
        let mut state = self.state.write().await;
        *state = WebsocketStatus::Open;
        self.notify.notify_waiters(); // Notify all waiting tasks
    }
    async fn disconnected(&self, reason: String) {
        let mut state = self.state.write().await;
        *state = WebsocketStatus::Closed(reason);
        self.notify.notify_waiters(); // Notify all waiting tasks
    }
    pub async fn state(&self) -> WebsocketStatus {
        self.state.read().await.clone()
    }
    pub async fn wait_for_open(&self) -> anyhow::Result<()> {
        let mut state = self.state.read().await;
        if let WebsocketStatus::Closed(reason) = &*state {
            return Err(anyhow::anyhow!("Disconnected: {}", reason));
        }
        while *state != WebsocketStatus::Open {
            drop(state); // Drop the read lock before waiting
            self.notify.notified().await; // Wait for a notification
            state = self.state.read().await; // Re-acquire the read lock
        }
        Ok(())
    }
}

#[derive(Clone)]
struct NostrWriter(Arc<RwLock<NostrWebsocketWriter>>);
impl NostrWriter {
    pub fn new() -> Self {
        NostrWriter(Arc::new(RwLock::new(None)))
    }
    async fn send<T>(&self, message: T) -> anyhow::Result<()>
    where
        T: Into<WebSocketMessage>,
    {
        let mut writer = self.0.write().await;
        let writer = writer.as_mut().ok_or(anyhow::anyhow!("No writer"))?;
        writer.send(message.into()).await?;
        Ok(())
    }
    async fn close(&self) {
        let mut writer = self.0.write().await;
        if let Some(writer) = writer.as_mut() {
            let _ = writer.close().await;
        }
    }
}

#[derive(Clone)]
pub struct NostrReader(Arc<RwLock<NostrWebsocketReader>>);
impl NostrReader {
    pub fn new() -> Self {
        NostrReader(Arc::new(RwLock::new(None)))
    }
    pub async fn read(&self) -> Option<RelayEvent> {
        let mut reader = self.0.write().await;
        let message = reader.as_mut()?.next().await?.ok()?;
        match message {
            WebSocketMessage::Text(text) => RelayEvent::try_from(text.as_str()).ok(),
            WebSocketMessage::Close(e) => RelayEvent::Close(e.unwrap().to_string()).into(),
            _ => RelayEvent::Ping.into(),
        }
    }
}

#[derive(Clone)]
pub struct NostrRelay {
    pub url: String,
    writer: NostrWriter,
    reader: NostrReader,
    state: RelayStatus,
}
impl NostrRelay {
    pub async fn state(&self) -> WebsocketStatus {
        self.state.state().await.clone()
    }
    pub fn new(relay_string: &str) -> anyhow::Result<Self> {
        Url::new(&relay_string)?;
        let relay = NostrRelay {
            url: relay_string.to_string(),
            reader: NostrReader::new(),
            writer: NostrWriter::new(),
            state: RelayStatus::new(),
        };
        let relay_clone = relay.clone();
        crate::relays::spawn_thread(async move {
            if let Err(e) = relay_clone.connect().await {
                relay_clone.state.disconnected(e.to_string()).await;
            }
        });
        Ok(relay)
    }
    pub async fn connect(&self) -> anyhow::Result<()> {
        let relay_url = Url::new(&self.url)?;
        #[cfg(not(target_arch = "wasm32"))]
        let (websocket, _response) = tokio_tungstenite::connect_async(relay_url.url).await?;
        #[cfg(target_arch = "wasm32")]
        let websocket = tokio_tungstenite_wasm::connect(relay_url.url).await?;
        let (websocket_writer, websocket_reader) = websocket.split();
        let mut writer = self.writer.0.write().await;
        let mut reader = self.reader.0.write().await;
        *writer = Some(websocket_writer);
        *reader = Some(websocket_reader);
        self.state.connected().await;
        Ok(())
    }
    pub async fn send_to_relay<T>(&self, note: T) -> anyhow::Result<T>
    where
        T: Into<WebSocketMessage> + Clone,
    {
        self.state.wait_for_open().await?;
        self.writer.send(note.clone()).await?;
        Ok(note)
    }
    pub async fn next_relay_event(&self) -> Option<RelayEvent> {
        self.state.wait_for_open().await.ok()?;
        self.reader.read().await
    }
    pub async fn close(self) {
        self.writer.close().await;
        drop(self);
    }
}

#[cfg(test)]
mod tests {
    fn _debug(s: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            println!("{}", s);
        }
        #[cfg(target_arch = "wasm32")]
        {
            wasm_bindgen_test::console_log!("{}", s);
        }
    }
    fn _error(s: &str) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            eprintln!("{}", s);
        }
        #[cfg(target_arch = "wasm32")]
        {
            wasm_bindgen_test::console_error!("{}", s);
        }
    }

    use crate::{
        notes::NostrNote,
        relays::{NostrSubscription, SubscribeEvent},
    };

    //#[tokio::test]
    //#[tracing_test::traced_test]
    //#[wasm_bindgen_test::wasm_bindgen_test]
    async fn _single_stress() -> Result<(), anyhow::Error> {
        use super::*;
        let relay = NostrRelay::new("wss://relay.arrakis.lat")?;
        let filter: SubscribeEvent = NostrSubscription {
            kinds: Some(vec![1]),
            limit: Some(1000),
            ..Default::default()
        }
        .into();
        let id = relay.send_to_relay(filter).await?.1;
        tracing::info!("Subscribed");

        let mut finished = String::new();
        while let Some(event) = relay.next_relay_event().await {
            match event {
                RelayEvent::EndOfSubscription((_, id)) => {
                    _debug(&format!("End of subscription: {}", id));
                    finished = id;
                    break;
                }
                _ => (),
            }
        }
        assert_eq!(id, finished);
        Ok(())
    }
    #[tokio::test]
    #[tracing_test::traced_test]
    //#[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_single_relay() -> Result<(), anyhow::Error> {
        use super::*;
        let relay = NostrRelay::new("wss://relay.arrakis.lat")?;
        let filter: SubscribeEvent = NostrSubscription {
            kinds: Some(vec![1]),
            limit: Some(3),
            ..Default::default()
        }
        .into();
        let id = relay.send_to_relay(filter).await?.1;

        let mut finished = String::new();
        while let Some(event) = relay.reader.read().await {
            match event {
                RelayEvent::EndOfSubscription((_, id)) => {
                    _debug(&format!("End of subscription: {}", id));
                    finished = id;
                    break;
                }
                _ => (),
            }
        }
        assert_eq!(id, finished);
        Ok(())
    }
    #[tokio::test]
    // #[wasm_bindgen_test::wasm_bindgen_test]
    async fn _test_relay_send_note() -> Result<(), anyhow::Error> {
        use super::*;
        let relay = NostrRelay::new("wss://relay.illuminodes.com")?;
        _debug(relay.url.as_str());
        let user_keys = crate::keypair::NostrKeypair::generate(false);
        let mut note = NostrNote {
            pubkey: user_keys.public_key(),
            content: "Hello, world!".to_string(),
            ..Default::default()
        };
        user_keys.sign_nostr_event(&mut note);
        relay.send_to_relay(note).await?;
        let mut sent = false;
        while let Some(event) = relay.reader.read().await {
            match RelayEvent::try_from(event) {
                Ok(RelayEvent::SentOk((_, _, did_sent, _))) => {
                    _debug(&format!("Sent Ok: {}", did_sent));
                    sent = did_sent;
                    break;
                }
                Err(e) => {
                    _error(&format!("{:?}", e));
                    // break;
                }
                _ => (),
            }
        }
        assert!(sent);
        Ok(())
    }
}
