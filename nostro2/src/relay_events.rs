#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RelayStatus {
    #[default]
    CONNECTING = 0,
    OPEN = 1,
    CLOSING = 2,
    CLOSED = 3,
}
impl From<u16> for RelayStatus {
    fn from(value: u16) -> Self {
        match value {
            1 => Self::OPEN,
            2 => Self::CLOSING,
            3 => Self::CLOSED,
            _ => Self::CONNECTING,
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, PartialEq, Eq)]
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
#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum NostrRelayEvent {
    NewNote(RelayEventTag, String, crate::note::NostrNote),
    SentOk(RelayEventTag, String, bool, String),
    EndOfSubscription(RelayEventTag, String),
    ClosedSubscription(RelayEventTag, String),
    Notice(RelayEventTag, String),
    Ping,
    Close(String),
}
impl TryFrom<&[u8]> for NostrRelayEvent {
    type Error = serde_json::Error;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        serde_json::from_slice(value)
    }
}
impl std::str::FromStr for NostrRelayEvent {
    type Err = serde_json::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(value)
    }
}
impl std::fmt::Display for NostrRelayEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_string(self).expect("Failed to serialize RelayEvent")
        )
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone)]
#[serde(untagged)]
pub enum NostrClientEvent {
    SendNoteEvent(RelayEventTag, super::note::NostrNote),
    Subscribe(
        RelayEventTag,
        String,
        super::subscriptions::NostrSubscription,
    ),
    CloseSubscriptionEvent(RelayEventTag, String),
}
impl NostrClientEvent {
    #[must_use]
    pub fn close_subscription(sub_id: &str) -> Self {
        Self::CloseSubscriptionEvent(RelayEventTag::REQ, sub_id.to_string())
    }
}
impl From<super::note::NostrNote> for NostrClientEvent {
    fn from(note: super::note::NostrNote) -> Self {
        Self::SendNoteEvent(RelayEventTag::EVENT, note)
    }
}
impl From<&super::note::NostrNote> for NostrClientEvent {
    fn from(note: &super::note::NostrNote) -> Self {
        Self::SendNoteEvent(RelayEventTag::EVENT, note.clone())
    }
}
impl From<super::subscriptions::NostrSubscription> for NostrClientEvent {
    fn from(subscription: super::subscriptions::NostrSubscription) -> Self {
        use secp256k1::rand::Rng;
        Self::Subscribe(
            RelayEventTag::REQ,
            secp256k1::rand::thread_rng().gen::<u64>().to_string(),
            subscription,
        )
    }
}
impl From<&super::subscriptions::NostrSubscription> for NostrClientEvent {
    fn from(subscription: &super::subscriptions::NostrSubscription) -> Self {
        use secp256k1::rand::Rng;
        Self::Subscribe(
            RelayEventTag::REQ,
            secp256k1::rand::thread_rng().gen::<u64>().to_string(),
            subscription.clone(),
        )
    }
}
impl std::str::FromStr for NostrClientEvent {
    type Err = serde_json::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(value)
    }
}
impl TryFrom<&[u8]> for NostrClientEvent {
    type Error = serde_json::Error;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        serde_json::from_slice(value)
    }
}
impl std::fmt::Display for NostrClientEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_string(self).expect("Failed to serialize ClientEvent")
        )
    }
}
