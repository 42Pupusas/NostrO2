use rand::{thread_rng, Rng};
use std::{time::{SystemTime, UNIX_EPOCH}, sync::{Arc, Mutex}};

use secp256k1::SecretKey;
use serde::{Deserialize, Serialize};
use serde_json::{json, to_value, Value};

use futures_util::{StreamExt, SinkExt, stream::SplitSink};
use tokio::{task::spawn_blocking, sync::mpsc::{UnboundedReceiver, unbounded_channel}};
use tokio_tungstenite::{tungstenite::{protocol::{Message as WsMessage, CloseFrame, frame::coding::CloseCode}, Error as TungsteniteError}, connect_async, WebSocketStream};

use super::secrets::RELAY_URL;

pub struct NostrRelay {
    _url: Arc<str>,
    ws_write: Arc<Mutex<SplitSink<WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, WsMessage>>>,
    notes_receiver: tokio::sync::Mutex<UnboundedReceiver<Result<WsMessage, TungsteniteError>>>,
}

impl NostrRelay {
    pub async fn new() -> Self {
        let url = RELAY_URL;
        let url_object = url::Url::parse(url).unwrap();

        if let Ok((ws_stream, _)) = connect_async(url_object).await {
            let (ws_write, mut ws_read) = ws_stream.split();

            let (tx, rx) = unbounded_channel();
            tokio::spawn(async move {
                while let Some(note) = ws_read.next().await {
                    match tx.send(note) {
                        Ok(_) => (),
                        Err(e) => {
                            println!("Error sending note to channel: {:?}", e);
                        }
                    }
                }
            });

            NostrRelay {
                _url: Arc::from(url),
                ws_write: Arc::new(Mutex::new(ws_write)),
                notes_receiver: tokio::sync::Mutex::new(rx),
            } } else {
                panic!("Failed to connect to Nostr Relay");
            }
    }

    pub async fn subscribe(&self, filter: Value) -> Result<(), Box<dyn std::error::Error>> {
        let nostr_subscription = NostrSubscription::new(filter);
        let ws_stream = Arc::clone(&self.ws_write);
        spawn_blocking(move || {
            let mut write = ws_stream.lock().unwrap();
            match tokio::runtime::Handle::current().block_on(write.send(nostr_subscription)) {
                Ok(_) => (),
                Err(e) => {
                    println!("Error subscribing: {:?}", e);
                }
            }
        });

        Ok(())
    }

    pub async fn send_note(&self, note: SignedNote) {
        let ws_stream = Arc::clone(&self.ws_write);
        spawn_blocking(move || {
            let mut write = ws_stream.lock().unwrap();
            match tokio::runtime::Handle::current().block_on(write.send(note.prepare_ws_message())) {
                Ok(_) => {
                    ()
                },
                Err(e) => {
                    println!("Error sending note to relay: {:?}", e);
                }
            }
        });
    }

    pub async fn read_notes(&self) -> Option<Result<WsMessage, TungsteniteError>> {
        let mut lock = self.notes_receiver.lock().await;
        lock.recv().await
    }

   pub async fn close(&self) -> Result<(), Box<dyn std::error::Error>> {
        let ws_write = Arc::clone(&self.ws_write);
        let close_msg = WsMessage::Close(Some(CloseFrame {
            code: CloseCode::Normal,
            reason: "Bye bye".into(),
        }));
        tokio::task::spawn_blocking(move || {
            let mut write_guard = ws_write.lock().unwrap();
            match tokio::runtime::Handle::current().block_on(write_guard.send(close_msg)) {
                Ok(_) => (),
                Err(e) => {
                    println!("Error closing the connection: {:?}", e);
                }
            }
        });
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
pub struct NostrSubscription {
    id: String,
    filters: Value,
}

impl NostrSubscription {
    pub fn new(filter: Value) -> WsMessage {
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

#[derive(Serialize, Deserialize, Debug)]
pub struct Note {
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
}

impl Note {
    pub fn new(pubkey: String, tags: Vec<Vec<String>>, kind: u32, content: String) -> Self {
        Note {
            pubkey,
            created_at: get_unix_timestamp(),
            kind,
            tags,
            content,
        }
    }
    pub fn serialize_for_nostr(&self) -> String {
        let value = to_value(self).unwrap();

        let json_str = json!([
            0,
            value["pubkey"],
            value["created_at"],
            value["kind"],
            value["tags"],
            value["content"]
        ]);
        json_str.to_string()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SignedNote {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

impl SignedNote {
    pub fn prepare_ws_message(&self) -> WsMessage {
        let event_string = json!(["EVENT", self]).to_string();
        let event_ws_message = WsMessage::Text(event_string);
        event_ws_message
    }
}

pub fn new_keys() -> SecretKey {
    let mut rng = thread_rng();

    // Generate a random 256-bit integer as the private key
    let private_key: [u8; 32] = rng.gen();

    // Convert the private key to a secp256k1 SecretKey object
    let secret_key = SecretKey::from_slice(&private_key).unwrap();

    // Return the private key in hexadecimal format
    secret_key
}

pub fn get_unix_timestamp() -> u64 {
    // Get the current time as a SystemTime object
    let current_time = SystemTime::now();

    // Get the duration between the current time and the Unix epoch
    let duration_since_epoch = current_time.duration_since(UNIX_EPOCH).unwrap();

    // Get the number of seconds since the Unix epoch as a u64 value
    let unix_timestamp = duration_since_epoch.as_secs();

    unix_timestamp
}
