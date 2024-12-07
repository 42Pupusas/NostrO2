use serde::{Deserialize, Serialize};
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
pub struct NoteEvent(pub RelayEventTag, pub String, pub NostrNote);
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct OkEvent(pub RelayEventTag, pub String, pub bool, pub String);
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct EndOfSubscriptionEvent(pub RelayEventTag, pub String);
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SubscriptionClosedEvent(pub RelayEventTag, pub String);
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct NoticeEvent(pub RelayEventTag, pub String);

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(untagged)]
pub enum RelayEvent {
    NewNote(NoteEvent),
    SentOk(OkEvent),
    EndOfSubscription(EndOfSubscriptionEvent),
    ClosedSubscription(SubscriptionClosedEvent),
    Notice(NoticeEvent),
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

// FROM CLIENT TO RELAY
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SubscribeEvent(pub RelayEventTag, pub String, pub super::NostrSubscription);
impl Into<String> for SubscribeEvent {
    fn into(self) -> String {
        serde_json::to_string(&self).unwrap()
    }
}
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SendNoteEvent(pub RelayEventTag, pub NostrNote);
impl Into<String> for SendNoteEvent {
    fn into(self) -> String {
        serde_json::to_string(&self).unwrap()
    }
}
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CloseEvent(pub RelayEventTag, pub String);
impl Into<String> for CloseEvent {
    fn into(self) -> String {
        serde_json::to_string(&self).unwrap()
    }
}

