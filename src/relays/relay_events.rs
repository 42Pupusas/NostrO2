use serde::{Deserialize, Serialize};
use crate::notes::SignedNote;


#[derive(Debug, Serialize, Deserialize, Clone)]
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
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NoteEvent(pub RelayEventTag, pub String, pub SignedNote);
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OkEvent(pub RelayEventTag, pub String, pub bool, pub String);
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct EndOfSubscriptionEvent(pub RelayEventTag, pub String);
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SubscriptionClosedEvent(pub RelayEventTag, pub String);
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct NoticeEvent(pub RelayEventTag, pub String);

// FROM CLIENT TO RELAY
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SubscribeEvent(pub RelayEventTag, pub String, pub super::NostrSubscription);
impl Into<String> for SubscribeEvent {
    fn into(self) -> String {
        serde_json::to_string(&self).unwrap()
    }
}
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SendNoteEvent(pub RelayEventTag, pub SignedNote);
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

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(untagged)]
pub enum RelayEvent {
    NewNote(NoteEvent),
    SentOk(OkEvent),
    EndOfSubscription(EndOfSubscriptionEvent),
    ClosedSubscription(SubscriptionClosedEvent),
    Notice(NoticeEvent),
}
impl TryFrom<String> for RelayEvent {
    type Error = serde_json::Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        serde_json::from_str(&value)
    }
}
