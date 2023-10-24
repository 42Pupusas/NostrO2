use futures_util::{stream::SplitSink, SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{from_str, json, Value};
use std::sync::Arc;
use tokio::sync::{
    mpsc::{unbounded_channel, UnboundedReceiver},
    Mutex,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        protocol::{frame::coding::CloseCode, CloseFrame, Message as WsMessage},
        Error as TungsteniteError,
    },
    WebSocketStream,
};

use super::notes::SignedNote;
use super::utils::new_keys;

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

pub struct NostrRelay {
    _url: Arc<str>,
    ws_write: Arc<
        tokio::sync::Mutex<
            SplitSink<
                WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
                WsMessage,
            >,
        >,
    >,
    notes_receiver: tokio::sync::Mutex<UnboundedReceiver<Result<WsMessage, TungsteniteError>>>,
}

impl NostrRelay {
    pub async fn new(relay_url: &str) -> Result<Self, RelayErrors> {
        let url = relay_url;
        let url_object = url::Url::parse(url).unwrap();

        if let Ok((ws_stream, _)) = connect_async(url_object).await {
            let (ws_write, mut ws_read) = ws_stream.split();

            let (tx, rx) = unbounded_channel();

            tokio::spawn(async move {
                while let Some(note) = ws_read.next().await {
                    match &note {
                        Ok(tokio_tungstenite::tungstenite::protocol::Message::Text(_)) => {
                            match tx.send(note) {
                                Ok(_) => (),
                                Err(_) => {}
                            }
                        }
                        _ => continue,
                    }
                }
            });

            Ok(NostrRelay {
                _url: Arc::from(url),
                ws_write: Arc::new(Mutex::new(ws_write)),
                notes_receiver: tokio::sync::Mutex::new(rx),
            })
        } else {
            Err(RelayErrors::ConnectionError)
        }
    }

    pub async fn subscribe(&self, filter: Value) -> Result<(), RelayErrors> {
        let nostr_subscription = NostrSubscription::new(filter);
        let mut ws_stream = self.ws_write.lock().await;
        match ws_stream.send(nostr_subscription).await {
            Ok(_) => Ok(()),
            Err(e) => {
                println!("Error subscribing to relay: {}", e);
                Err(RelayErrors::SubscriptionError(e.to_string()))
            }
        }
    }

    pub async fn send_note(&self, note: SignedNote) -> Result<(), RelayErrors> {
        let mut ws_stream = self.ws_write.lock().await;
        match ws_stream.send(note.prepare_ws_message()).await {
            Ok(_) => Ok(()),
            Err(e) => {
                println!("Error sending note to relay: {}", e);
                Err(RelayErrors::SendError(e.to_string()))
            }
        }
    }

    pub async fn read_from_relay(&self) -> Option<Result<RelayEvents, RelayErrors>> {
        let mut lock = self.notes_receiver.lock().await;
        match lock.recv().await {
            Some(Ok(WsMessage::Text(text))) => {
                let event = RelayEvents::from_str(&text).unwrap();
                Some(Ok(event))
            }
            Some(Err(e)) => Some(Err(RelayErrors::ReadError(e.to_string()))),
            // Handle other message types like Close, Ping, Pong or continue to ignore them
            _ => None,
        }
    }

    pub async fn close(&self) -> Result<(), RelayErrors> {
        let mut ws_write = self.ws_write.lock().await;
        let close_msg = WsMessage::Close(Some(CloseFrame {
            code: CloseCode::Normal,
            reason: "Bye bye".into(),
        }));
        match ws_write.send(close_msg).await {
            Ok(_) => Ok(()),
            Err(_e) => Err(RelayErrors::ConnectionError),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct NostrSubscription {
    id: String,
    filters: Value,
}

impl NostrSubscription {
    fn new(filter: Value) -> WsMessage {
        let id = hex::encode(&new_keys()[..]);
        let nostr_subscription = NostrSubscription {
            id,
            filters: filter,
        };
        let nostr_subscription_string = WsMessage::Text(
            json!(["REQ", nostr_subscription.id, nostr_subscription.filters]).to_string(),
        );
        nostr_subscription_string
    }
}
