use serde::{Deserialize, Serialize};
#[cfg(not(target_arch = "wasm32"))]
use tokio_tungstenite::tungstenite::Utf8Bytes;
use crate::notes::NostrNote;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum RelayEventTag {
    EVENT,
    OK,
    EOSE,
    NOTICE,
    CLOSE,
    CLOSED,
    REQ,
}
// FROM RELAY TO CLIENT 
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(untagged)]
pub enum RelayEvent {
    NewNote((RelayEventTag, String, NostrNote)),
    SentOk((RelayEventTag, String, bool, String)),
    EndOfSubscription((RelayEventTag, String)),
    ClosedSubscription((RelayEventTag, String)),
    Notice((RelayEventTag, String)),
    Ping,
    Close(String),
}
impl TryFrom<String> for RelayEvent {
    type Error = serde_json::Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        serde_json::from_str(&value)
    }
}
impl TryFrom<&String> for RelayEvent {
    type Error = serde_json::Error;
    fn try_from(value: &String) -> Result<Self, Self::Error> {
        serde_json::from_str(value)
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl TryFrom<&Utf8Bytes> for RelayEvent {
    type Error = serde_json::Error;
    fn try_from(value: &Utf8Bytes) -> Result<Self, Self::Error> {
        serde_json::from_str(value)
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl TryFrom<Utf8Bytes> for RelayEvent {
    type Error = serde_json::Error;
    fn try_from(value: Utf8Bytes) -> Result<Self, Self::Error> {
        serde_json::from_str(value.as_str())
    }
}
impl TryFrom<&str> for RelayEvent {
    type Error = serde_json::Error;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        serde_json::from_str(value)
    }
}

// FROM CLIENT TO RELAY
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SubscribeEvent(pub RelayEventTag, pub String, pub super::NostrSubscription);
impl Into<String> for SubscribeEvent {
    fn into(self) -> String {
        serde_json::to_string(&self).unwrap()
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl Into<Utf8Bytes> for SubscribeEvent {
    fn into(self) -> Utf8Bytes {
        serde_json::to_string(&self).unwrap().into()
    }
}
impl Into<crate::relays::WebSocketMessage> for SubscribeEvent {
    fn into(self) -> crate::relays::WebSocketMessage {
        crate::relays::WebSocketMessage::Text(self.into())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SendNoteEvent(pub RelayEventTag, pub NostrNote);
impl Into<String> for SendNoteEvent {
    fn into(self) -> String {
        serde_json::to_string(&self).unwrap()
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl Into<Utf8Bytes> for SendNoteEvent {
    fn into(self) -> Utf8Bytes {
        serde_json::to_string(&self).unwrap().into()
    }
}
impl Into<crate::relays::WebSocketMessage> for SendNoteEvent {
    fn into(self) -> crate::relays::WebSocketMessage {
        crate::relays::WebSocketMessage::Text(self.into())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CloseEvent(pub RelayEventTag, pub String);
impl From<String> for CloseEvent {
    fn from(value: String) -> Self {
        CloseEvent(RelayEventTag::CLOSE, value)
    }
}
impl Into<String> for CloseEvent {
    fn into(self) -> String {
        serde_json::to_string(&self).unwrap()
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl Into<Utf8Bytes> for CloseEvent {
    fn into(self) -> Utf8Bytes {
        serde_json::to_string(&self).unwrap().into()
    }
}
impl Into<crate::relays::WebSocketMessage> for CloseEvent {
    fn into(self) -> crate::relays::WebSocketMessage {
        crate::relays::WebSocketMessage::Text(self.into())
    }
}
