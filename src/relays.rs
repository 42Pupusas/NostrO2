use crate::errors::NostroError;

use super::notes::SignedNote;
use super::utils::new_keys;
use async_channel::Receiver;
use futures_util::{
    stream::{SplitSink, SplitStream},
    SinkExt, StreamExt,
};
use serde::{Deserialize, Serialize};
use serde_json::{from_str, json, Value};

use url::Url;

#[cfg(not(target_arch = "wasm32"))]
use tokio::{net::TcpStream, spawn as new_thread};
#[cfg(not(target_arch = "wasm32"))]
use tokio_tungstenite::{
    connect_async, tungstenite::Message as WebSocketMessage, MaybeTlsStream, WebSocketStream,
};

#[cfg(target_arch = "wasm32")]
use tokio_tungstenite_wasm::{connect, Message as WebSocketMessage, WebSocketStream};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_futures::spawn_local as new_thread;

#[cfg(not(target_arch = "wasm32"))]
type NostrWebSocketStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
#[cfg(target_arch = "wasm32")]
type NostrWebSocketStream = WebSocketStream;

#[derive(Debug, Deserialize, PartialEq, Clone)]
pub enum RelayEvents {
    EVENT(String, String, SignedNote),
    EOSE(String, String),
    OK(String, String, bool, String),
    NOTICE(String, String),
}

impl RelayEvents {
    pub fn from_str(s: &str) -> Result<Self, NostroError> {
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
            Err(NostroError::ParseError)
        }
    }
}

#[derive(Debug, Clone)]
pub struct NostrRelay {
    url: String,
    reader_rx: async_channel::Receiver<RelayEvents>,
    writer_tx: async_channel::Sender<WebSocketMessage>,
}

impl NostrRelay {
    pub fn url(&self) -> String {
        self.url.clone()
    }
    pub async fn new(relay_string: &str) -> Result<Self, NostroError> {
        let relay_url =
            Url::parse(relay_string).map_err(|e| NostroError::ConnectionError(Box::new(e)))?;

        #[cfg(not(target_arch = "wasm32"))]
        let (websocket, _response) = connect_async(relay_url)
            .await
            .map_err(|e| NostroError::ConnectionError(Box::new(e)))?;

        #[cfg(target_arch = "wasm32")]
        let websocket = connect(relay_url)
            .await
            .map_err(|e| NostroError::ConnectionError(Box::new(e)))?;

        let (websocket_writer, websocket_reader) = websocket.split();
        let (writer_tx, writer_rx) = async_channel::unbounded::<WebSocketMessage>();
        let (reader_tx, reader_rx) = async_channel::unbounded::<RelayEvents>();
        let new_relay = NostrRelay {
            url: relay_string.to_string(),
            reader_rx,
            writer_tx,
        };

        new_thread(
            new_relay
                .clone()
                .websocket_reader_handler(reader_tx, websocket_reader),
        );
        new_thread(
            new_relay
                .clone()
                .websocket_writer_handler(writer_rx, websocket_writer),
        );

        Ok(new_relay)
    }
    async fn websocket_writer_handler(
        self,
        writer_rx: Receiver<WebSocketMessage>,
        mut ws_writer: SplitSink<NostrWebSocketStream, WebSocketMessage>,
    ) {
        while let Ok(message) = writer_rx.recv().await {
            if let Err(_e) = ws_writer.send(message).await {}
        }
        let _ = self.close().await;
    }
    async fn websocket_reader_handler(
        self,
        reader_tx: async_channel::Sender<RelayEvents>,
        mut ws_reader: SplitStream<NostrWebSocketStream>,
    ) {
        while let Some(Ok(message)) = ws_reader.next().await {
            let message = message.to_string();
            match RelayEvents::from_str(&message) {
                Ok(event) => {
                    let _ = reader_tx.send(event).await;
                }
                Err(e) => {
                    println!("{}", e);
                    println!("{}", message);
                }
            }
        }
        let _ = self.close().await;
    }
    pub async fn subscribe(&self, filter: &NostrSubscription) -> Result<String, NostroError> {
        self.writer_tx
            .send(filter.nostr_message())
            .await
            .map_err(|e| NostroError::ConnectionError(Box::new(e)))?;
        Ok(filter.id())
    }
    pub async fn unsubscribe(&self, id: String) -> Result<(), NostroError> {
        let subscription = json!(["CLOSE", id]).to_string();
        let message = WebSocketMessage::Text(subscription);
        self.writer_tx
            .send(message)
            .await
            .map_err(|e| NostroError::ConnectionError(Box::new(e)))?;
        Ok(())
    }
    pub async fn send_note(&self, note: SignedNote) -> Result<(), NostroError> {
        let note = json!(["EVENT", note]);
        let message = WebSocketMessage::Text(note.to_string());
        self.writer_tx
            .send(message)
            .await
            .map_err(|e| NostroError::ConnectionError(Box::new(e)))?;
        Ok(())
    }
    pub fn relay_event_reader(&self) -> Receiver<RelayEvents> {
        self.reader_rx.clone()
    }
    pub async fn close(&self) {
        let _ = self.writer_tx.send(WebSocketMessage::Close(None)).await;
    }
    pub async fn subscribe_until_eose(
        &self,
        filter: &NostrSubscription,
    ) -> Result<Vec<RelayEvents>, NostroError> {
        let id = self.subscribe(filter).await?;
        let mut events = Vec::new();

        while let Ok(event) = self.relay_event_reader().recv().await {
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
    pub fn new_author(mut self, author: &str) -> Self {
        if let Some(authors) = &mut self.authors {
            authors.push(author.to_string());
        } else {
            self.authors = Some(vec![author.to_string()]);
        }
        self
    }
    pub fn new_authors(mut self, authors: Vec<String>) -> Self {
        if let Some(old_authors) = &mut self.authors {
            old_authors.extend(authors);
        } else {
            self.authors = Some(authors);
        }
        self
    }
    pub fn new_id(mut self, id: &str) -> Self {
        if let Some(ids) = &mut self.ids {
            ids.push(id.to_string());
        } else {
            self.ids = Some(vec![id.to_string()]);
        }
        self
    }
    pub fn new_ids(mut self, ids: Vec<String>) -> Self {
        if let Some(old_ids) = &mut self.ids {
            old_ids.extend(ids);
        } else {
            self.ids = Some(ids);
        }
        self
    }
    pub fn new_kind(mut self, kind: u32) -> Self {
        if let Some(kinds) = &mut self.kinds {
            kinds.push(kind);
        } else {
            self.kinds = Some(vec![kind]);
        }
        self
    }
    pub fn new_kinds(mut self, kinds: Vec<u32>) -> Self {
        if let Some(old_kinds) = &mut self.kinds {
            old_kinds.extend(kinds);
        } else {
            self.kinds = Some(kinds);
        }
        self
    }
    pub fn new_since(mut self, since: u64) -> Self {
        self.since = Some(since);
        self
    }
    pub fn new_until(mut self, until: u64) -> Self {
        self.until = Some(until);
        self
    }
    pub fn new_limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }
    pub fn new_tag(mut self, key: &str, value: Vec<String>) -> Self {
        if let Some(tags) = &mut self.tags {
            tags.push((key.to_string(), value));
        } else {
            self.tags = Some(vec![(key.to_string(), value)]);
        }
        self
    }
}

#[derive(Serialize, Deserialize, Clone)]
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

    pub fn nostr_message(&self) -> WebSocketMessage {
        let subscription = json!(["REQ", self.id, self.filters.json()]).to_string();
        WebSocketMessage::Text(subscription)
    }

    pub fn id(&self) -> String {
        self.id.clone()
    }
}
