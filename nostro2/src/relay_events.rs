#[derive(Debug, Copy, serde::Serialize, serde::Deserialize, Clone, PartialEq, Eq, Hash)]
#[serde(rename_all = "UPPERCASE")]
pub enum RelayEventTag {
    Event,
    Ok,
    Eose,
    Notice,
    Close,
    Auth,
    Req,
    Closed,
}
// FROM RELAY TO CLIENT
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize, Hash)]
#[serde(untagged)]
pub enum NostrRelayEvent {
    NewNote(RelayEventTag, String, crate::note::NostrNote),
    SentOk(RelayEventTag, String, bool, String),
    EndOfSubscription(RelayEventTag, String),
    ClosedSubscription(RelayEventTag, String),
    Notice(RelayEventTag, String),
    Ping,
    Close(String),
    Auth(RelayEventTag, String),
}
impl std::str::FromStr for NostrRelayEvent {
    type Err = serde_json::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(value)
    }
}
impl TryFrom<&[u8]> for NostrRelayEvent {
    type Error = serde_json::Error;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        serde_json::from_slice(value)
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, PartialEq, Eq)]
#[serde(untagged)]
pub enum NostrClientEvent {
    SendNoteEvent(RelayEventTag, super::note::NostrNote),
    Subscribe(
        RelayEventTag,
        String,
        super::subscriptions::NostrSubscription,
    ),
    CloseSubscriptionEvent(RelayEventTag, String),
    AuthEvent(RelayEventTag, crate::note::NostrNote),
    Pong,
}
impl NostrClientEvent {
    #[must_use]
    pub fn close_subscription(sub_id: &str) -> Self {
        Self::CloseSubscriptionEvent(RelayEventTag::Close, sub_id.to_string())
    }
    #[must_use]
    pub const fn auth_event(note: super::note::NostrNote) -> Self {
        Self::AuthEvent(RelayEventTag::Auth, note)
    }
}
impl From<super::note::NostrNote> for NostrClientEvent {
    fn from(note: super::note::NostrNote) -> Self {
        Self::SendNoteEvent(RelayEventTag::Event, note)
    }
}
impl From<&super::note::NostrNote> for NostrClientEvent {
    fn from(note: &super::note::NostrNote) -> Self {
        Self::SendNoteEvent(RelayEventTag::Event, note.clone())
    }
}
impl From<super::subscriptions::NostrSubscription> for NostrClientEvent {
    fn from(subscription: super::subscriptions::NostrSubscription) -> Self {
        use secp256k1::rand::Rng;
        Self::Subscribe(
            RelayEventTag::Req,
            secp256k1::rand::thread_rng().gen::<u64>().to_string(),
            subscription,
        )
    }
}
impl From<&super::subscriptions::NostrSubscription> for NostrClientEvent {
    fn from(subscription: &super::subscriptions::NostrSubscription) -> Self {
        use secp256k1::rand::Rng;
        Self::Subscribe(
            RelayEventTag::Req,
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
