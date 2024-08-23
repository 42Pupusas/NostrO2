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
    ConnectionError(Box<dyn std::error::Error + Send>),
    ParseError,
    SubscriptionError(String),
    SendError(String),
    ReadError(String),
}

impl std::fmt::Display for RelayErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            RelayErrors::ConnectionError(_) => write!(f, "Could not connect to relay"),
            RelayErrors::ParseError => write!(f, "Could not parse message"),
            RelayErrors::SubscriptionError(s) => write!(f, "Could not subscribe: {}", s),
            RelayErrors::SendError(s) => write!(f, "Could not send note: {}", s),
            RelayErrors::ReadError(s) => write!(f, "Could not read message: {}", s),
        }
    }
}

impl std::error::Error for RelayErrors {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
    fn description(&self) -> &str {
        match self {
            RelayErrors::ConnectionError(_) => "Could not connect to relay",
            RelayErrors::ParseError => "Could not parse message",
            RelayErrors::SubscriptionError(_) => "Could not subscribe",
            RelayErrors::SendError(_) => "Could not send note",
            RelayErrors::ReadError(_) => "Could not read message",
        }
    }
    fn cause(&self) -> Option<&dyn std::error::Error> {
        None
    }
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
        let relay_url =
            Url::parse(relay_string).map_err(|e| RelayErrors::ConnectionError(Box::new(e)))?;

        #[cfg(not(target_arch = "wasm32"))]
        let (websocket, _response) = connect_async(relay_url)
            .await
            .map_err(|e| RelayErrors::ConnectionError(Box::new(e)))?;

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

    pub async fn subscribe(&self, filter: &NostrSubscription) -> Result<String, RelayErrors> {
        self.websocket_writer
            .lock()
            .await
            .send(filter.nostr_message())
            .await
            .map_err(|_| RelayErrors::SubscriptionError("Could not subscribe".into()))?;

        Ok(filter.id())
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

    pub async fn subscribe_until_eose(
        &self,
        filter: &NostrSubscription,
    ) -> Result<Vec<RelayEvents>, RelayErrors> {
        let id = self.subscribe(filter).await?;
        let mut events = Vec::new();

        loop {
            let event = self.read_relay_events().await?;
            events.push(event.clone());

            match event {
                RelayEvents::EOSE(_, _) => {
                    self.unsubscribe(id).await?;
                    break;
                }
                _ => (),
            }
        }
        Ok(events)
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct NostrFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    authors: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kinds: Option<Vec<u32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    since: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    until: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,
    #[serde(skip)]
    tags: Option<Vec<(String, Vec<String>)>>,
}

impl Default for NostrFilter {
    fn default() -> Self {
        NostrFilter {
            authors: None,
            ids: None,
            kinds: None,
            since: None,
            until: None,
            limit: None,
            tags: None,
        }
    }
}

impl NostrFilter {
    fn json(&self) -> Value {
        let mut base_json = json!(self);
        let base_map = base_json.as_object_mut().unwrap();
        if let Some(tags) = &self.tags {
            tags.iter().for_each(|(key, value)| {
                base_map.insert(format!("#{}", key), json!(value));
            });
        }
        json!(base_map)
    }
    pub fn subscribe(&self) -> NostrSubscription {
        NostrSubscription::new(self.clone())
    }
    pub fn new_author(&mut self, author: &str) {
        if let Some(authors) = &mut self.authors {
            authors.push(author.to_string());
        } else {
            self.authors = Some(vec![author.to_string()]);
        }
    }
    pub fn new_authors(&mut self, authors: Vec<String>) {
        if let Some(old_authors) = &mut self.authors {
            old_authors.extend(authors);
        } else {
            self.authors = Some(authors);
        }
    }
    pub fn new_id(&mut self, id: &str) {
        if let Some(ids) = &mut self.ids {
            ids.push(id.to_string());
        } else {
            self.ids = Some(vec![id.to_string()]);
        }
    }
    pub fn new_ids(&mut self, ids: Vec<String>) {
        if let Some(old_ids) = &mut self.ids {
            old_ids.extend(ids);
        } else {
            self.ids = Some(ids);
        }
    }
    pub fn new_kind(&mut self, kind: u32) {
        if let Some(kinds) = &mut self.kinds {
            kinds.push(kind);
        } else {
            self.kinds = Some(vec![kind]);
        }
    }
    pub fn new_kinds(&mut self, kinds: Vec<u32>) {
        if let Some(old_kinds) = &mut self.kinds {
            old_kinds.extend(kinds);
        } else {
            self.kinds = Some(kinds);
        }
    }
    pub fn new_since(&mut self, since: u64) {
        self.since = Some(since);
    }
    pub fn new_until(&mut self, until: u64) {
        self.until = Some(until);
    }
    pub fn new_limit(&mut self, limit: u32) {
        self.limit = Some(limit);
    }
    pub fn new_tag(&mut self, key: &str, value: Vec<String>) {
        if let Some(tags) = &mut self.tags {
            tags.push((key.to_string(), value));
        } else {
            self.tags = Some(vec![(key.to_string(), value)]);
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct NostrSubscription {
    id: String,
    filters: NostrFilter,
}

impl NostrSubscription {
    pub fn new(filter: NostrFilter) -> Self {
        NostrSubscription {
            id: hex::encode(&new_keys()[..]),
            filters: filter,
        }
    }

    pub fn nostr_message(&self) -> Message {
        let subscription = json!(["REQ", self.id, self.filters.json()]).to_string();
        Message::Text(subscription)
    }

    pub fn id(&self) -> String {
        self.id.clone()
    }
}
