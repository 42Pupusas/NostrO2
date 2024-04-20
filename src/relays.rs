use super::notes::SignedNote;
use super::utils::new_keys;
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use serde::{Deserialize, Serialize};
use serde_json::{from_str, json, Value};

#[cfg(not(target_arch = "wasm32"))]
use tokio::net::TcpStream;

#[cfg(not(target_arch = "wasm32"))]
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

use std::sync::Arc;
use tokio::sync::Mutex;

#[cfg(target_arch = "wasm32")]
use tokio_tungstenite_wasm::{connect, Message, WebSocketStream as WasmWebSocketStream};

use url::Url;

#[derive(Debug, Deserialize, PartialEq, Clone)]
pub enum RelayEvents {
    EVENT(String, String, SignedNote),
    EOSE(String, String),
    OK(String, String, bool, String),
    NOTICE(String, String),
}

#[derive(Debug)]
pub enum RelayErrors {
    ConnectionError,
    ParseError,
    SubscriptionError(String),
    SendError(String),
    ReadError(String),
}

impl RelayEvents {
    pub fn from_str(s: &str) -> Result<Self, RelayErrors> {
        if let Ok((event, id, signed_note)) = from_str::<(String, String, SignedNote)>(s) {
            Ok(RelayEvents::EVENT(event, id, signed_note))
        } else if let Ok((event, notice)) = from_str::<(String, String)>(s) {
            Ok(RelayEvents::EOSE(event, notice))
        } else if let Ok((event, id, success, notice)) =
            from_str::<(String, String, bool, String)>(s)
        {
            Ok(RelayEvents::OK(event, id, success, notice))
        } else if let Ok((event, notice)) = from_str::<(String, String)>(s) {
            Ok(RelayEvents::NOTICE(event, notice))
        } else {
            Err(RelayErrors::ParseError)
        }
    }
}

pub struct NostrRelay {
    _url: String,
    #[cfg(not(target_arch = "wasm32"))]
    websocket_writer: Arc<Mutex<SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>>>,
    #[cfg(not(target_arch = "wasm32"))]
    websocket_reader: Arc<Mutex<SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>>>,
    #[cfg(target_arch = "wasm32")]
    websocket_writer: Arc<Mutex<SplitSink<WasmWebSocketStream, Message>>>,
    #[cfg(target_arch = "wasm32")]
    websocket_reader: Arc<Mutex<SplitStream<WasmWebSocketStream>>>,
}

impl NostrRelay {
    pub async fn new(relay_string: &str) -> Result<Self, RelayErrors> {
        let relay_url = Url::parse(relay_string).map_err(|_| RelayErrors::ConnectionError)?;

        #[cfg(not(target_arch = "wasm32"))]
        let (websocket, _response) = connect_async(relay_url)
            .await
            .map_err(|_| RelayErrors::ConnectionError)?;

        #[cfg(target_arch = "wasm32")]
        let websocket = connect(relay_url)
            .await
            .map_err(|_| RelayErrors::ConnectionError)?;

        let (websocket_writer, websocket_reader) = websocket.split();

        let websocket_writer = Arc::new(Mutex::new(websocket_writer));
        let websocket_reader = Arc::new(Mutex::new(websocket_reader));

        Ok(NostrRelay {
            _url: relay_string.to_string(),
            websocket_writer,
            websocket_reader,
        })
    }

    pub async fn subscribe(&self, filter: Value) -> Result<String, RelayErrors> {
        let subscription = NostrSubscription::new(filter);
        self.websocket_writer
            .lock()
            .await
            .send(subscription.nostr_message())
            .await
            .map_err(|_| RelayErrors::SubscriptionError("Could not subscribe".into()))?;

        Ok(subscription.id())
    }

    pub async fn unsubscribe(&self, id: String) -> Result<(), RelayErrors> {
        let subscription = json!(["CLOSE", id]).to_string();
        self.websocket_writer
            .lock()
            .await
            .send(Message::Text(subscription))
            .await
            .map_err(|_| RelayErrors::SubscriptionError("Could not unsubscribe".into()))?;

        Ok(())
    }

    pub async fn send_note(&self, note: SignedNote) -> Result<(), RelayErrors> {
        let note = json!(["EVENT", note]);

        #[cfg(not(target_arch = "wasm32"))]
        let message = Message::Text(note.to_string());

        #[cfg(target_arch = "wasm32")]
        let message = Message::Text(note.to_string());

        self.websocket_writer
            .lock()
            .await
            .send(message)
            .await
            .map_err(|_| RelayErrors::SendError("Could not send note".into()))?;

        Ok(())
    }

    pub async fn read_relay_events(&self) -> Result<RelayEvents, RelayErrors> {
        if let Some(message) = self.websocket_reader.lock().await.next().await {
            match message {
                Ok(message) => {
                    let message = message.to_string();
                    RelayEvents::from_str(&message)
                }
                Err(_) => Err(RelayErrors::ReadError("Could not read message".into())),
            }
        } else {
            Err(RelayErrors::ReadError("Could not read message".into()))
        }
    }

    pub async fn close(&self) {
        let _ = self.websocket_writer.lock().await.close().await;
    }
}

#[derive(Serialize, Deserialize)]
pub struct NostrSubscription {
    id: String,
    filters: Value,
}

impl NostrSubscription {
    pub fn new(filter: Value) -> Self {
        NostrSubscription {
            id: hex::encode(&new_keys()[..]),
            filters: filter,
        }
    }

    pub fn nostr_message(&self) -> Message {
        let subscription = json!(["REQ", self.id, self.filters]).to_string();
        Message::Text(subscription)
    }

    pub fn id(&self) -> String {
        self.id.clone()
    }
}
