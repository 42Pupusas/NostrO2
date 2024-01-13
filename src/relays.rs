use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{from_str, json, Value};
use std::sync::Arc;
use tokio::sync::{
    mpsc,
    Mutex,
};
use async_utility::thread;


use tokio_tungstenite_wasm::{
    connect, CloseCode, CloseFrame, Error as TungsteniteError, Message as WsMessage,
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
    url: Arc<str>,
    ws_stream: Arc<Mutex<WebSocketStream>>,
    sender: mpsc::Sender<WsMessage>,
    receiver: mpsc::Receiver<Result<WsMessage, TungsteniteError>>,
}

impl std::fmt::Display for NostrRelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.url)
    }
}

impl NostrRelay {
    pub async fn new(relay_url: &str) -> Result<Self, RelayErrors> {
        let url_object = url::Url::parse(relay_url).unwrap();

        if let Ok(ws_stream) = connect(url_object).await {
            let (sender, outgoing) = mpsc::channel(32);
            let (incoming_sender, receiver) = mpsc::channel(32);

            let ws_stream = Arc::new(Mutex::new(ws_stream));

            let ws_stream_clone_out = Arc::clone(&ws_stream);
            let ws_stream_clone_in = Arc::clone(&ws_stream);

            thread::spawn(async move {
                let mut outgoing = outgoing;
                while let Some(msg) = outgoing.recv().await {
                    let mut lock = ws_stream_clone_out.lock().await;
                    lock.send(msg).await.unwrap();
                }
            });

            thread::spawn(async move {
                let mut lock = ws_stream_clone_in.lock().await;
                while let Some(Ok(msg)) = lock.next().await {
                    incoming_sender.send(Ok(msg)).await.unwrap();
                }
            });

            Ok(NostrRelay {
                url: Arc::from(relay_url),
                ws_stream,
                sender,
                receiver,
            })
        } else {
            Err(RelayErrors::ConnectionError)
        }
    }

    pub async fn subscribe(&self, filter: Value) -> Result<(), RelayErrors> {
        let nostr_subscription = NostrSubscription::new(filter);
        self.sender.send(nostr_subscription).await.unwrap();
        Ok(())
    }

    pub async fn send_note(&self, note: SignedNote) -> Result<(), RelayErrors> {
        self.sender.send(note.prepare_ws_message()).await.unwrap();
        Ok(())
    }

    pub async fn read_from_relay(&mut self) -> Option<Result<RelayEvents, RelayErrors>> {
        match self.receiver.recv().await {
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
        let mut ws_stream = self.ws_stream.lock().await;
        let close_msg = WsMessage::Close(Some(CloseFrame {
            code: CloseCode::Normal,
            reason: "Bye bye".into(),
        }));
        match ws_stream.send(close_msg).await {
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
