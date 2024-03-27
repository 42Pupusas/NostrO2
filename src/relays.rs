use super::notes::SignedNote;
use super::utils::new_keys;
use serde::{Deserialize, Serialize};
use serde_json::{from_str, json, Value};
use std::net::TcpStream;
use tungstenite::{connect, stream::MaybeTlsStream, Message, WebSocket};
use url::Url;


#[derive(Debug, Deserialize)]
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

pub struct NostrRelay<'a> {
    _url: &'a str,
    websocket: WebSocket<MaybeTlsStream<TcpStream>>,
}

impl<'a> NostrRelay<'a> {
    pub fn new(relay_string: &'a str) -> Result<Self, RelayErrors> {
        let relay_url = Url::parse(relay_string).map_err(|_| RelayErrors::ConnectionError)?;
        let (socket, _response) = connect(relay_url).map_err(|_| RelayErrors::ConnectionError)?;
        Ok(NostrRelay {
            _url: relay_string,
            websocket: socket,
        })
    }

    pub fn subscribe(&mut self, filter: Value) -> Result<String, RelayErrors> {
        let subscription = NostrSubscription::new(filter);
        self.websocket
            .send(subscription.nostr_message())
            .map_err(|_| RelayErrors::SubscriptionError("Could not send subscription".into()))?;
        Ok(subscription.id())
    }

    pub fn send_note(&mut self, note: SignedNote) -> Result<(), RelayErrors> {
        let note = json!(["EVENT", note]);
        self.websocket
            .send(Message::Text(note.to_string()))
            .map_err(|_| RelayErrors::SendError("Could not send note".into()))?;
        Ok(())
    }

    pub fn read_relay_events(&mut self) -> Result<RelayEvents, RelayErrors> {
        let message = self
            .websocket
            .read()
            .map_err(|_| RelayErrors::ReadError("Could not read message".into()))?;
        match message {
            Message::Text(msg) => {
                let event = RelayEvents::from_str(&msg)?;
                Ok(event)
            }
            _ => Err(RelayErrors::ParseError),
        }
    }

    pub fn close(&mut self) {
        let _ = self.websocket.close(None);
    }
}

#[derive(Serialize, Deserialize)]
struct NostrSubscription {
    id: String,
    filters: Value,
}

impl NostrSubscription {
    fn new(filter: Value) -> Self {
        NostrSubscription {
            id: hex::encode(&new_keys()[..]),
            filters: filter,
        }
    }

    fn nostr_message(&self) -> Message {
        let subscription = json!(["REQ", self.id, self.filters]).to_string();
        Message::Text(subscription)
    }

    fn id(&self) -> String {
        self.id.clone()
    }
}
