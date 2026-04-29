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
/// Generate a fresh subscription id, unique within this process for the
/// lifetime of the program.
///
/// Format: `"{start_ns}-{counter}"` where `start_ns` is the process-start
/// nanosecond timestamp (read once, lazily) and `counter` is a monotonically
/// increasing `AtomicU64`. The prefix keeps ids roughly chronological across
/// process restarts; the counter guarantees uniqueness within a process —
/// no clock-precision collisions, no platform branching at the call site.
fn fresh_sub_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;

    static START_NS: OnceLock<u64> = OnceLock::new();
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let start_ns = *START_NS.get_or_init(|| {
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|d| u64::try_from(d.as_nanos()).ok())
                .unwrap_or(0)
        }
        #[cfg(target_arch = "wasm32")]
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            // Date::now() is ms; pad to ns for a uniformly-shaped prefix.
            // Precision doesn't matter — the prefix is only for ordering
            // across runs, the counter handles in-process uniqueness.
            (js_sys::Date::now() * 1_000_000.0) as u64
        }
    });

    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{start_ns}-{n}")
}

impl From<super::subscriptions::NostrSubscription> for NostrClientEvent {
    fn from(subscription: super::subscriptions::NostrSubscription) -> Self {
        Self::Subscribe(RelayEventTag::Req, fresh_sub_id(), subscription)
    }
}
impl From<&super::subscriptions::NostrSubscription> for NostrClientEvent {
    fn from(subscription: &super::subscriptions::NostrSubscription) -> Self {
        Self::Subscribe(RelayEventTag::Req, fresh_sub_id(), subscription.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscriptions::NostrSubscription;

    /// Two consecutive `From` conversions must produce distinct `sub_id`s.
    /// The previous implementation read a clock; on wasm (millisecond
    /// precision) and tightly on native (nanosecond precision) it could
    /// hand back duplicates.
    #[test]
    fn fresh_sub_id_does_not_collide() {
        let sub = NostrSubscription::new().kind(1);
        let mut ids = std::collections::HashSet::new();
        for _ in 0..1024 {
            let NostrClientEvent::Subscribe(_, id, _) = NostrClientEvent::from(&sub) else {
                panic!("expected Subscribe variant");
            };
            assert!(ids.insert(id), "duplicate sub_id");
        }
    }

    #[test]
    fn fresh_sub_id_has_start_prefix_format() {
        let sub = NostrSubscription::new();
        let NostrClientEvent::Subscribe(_, a, _) = NostrClientEvent::from(&sub) else {
            unreachable!()
        };
        let NostrClientEvent::Subscribe(_, b, _) = NostrClientEvent::from(&sub) else {
            unreachable!()
        };
        // Same process → same prefix, different counter suffix.
        let (pa, na) = a.split_once('-').expect("start_ns-counter format");
        let (pb, nb) = b.split_once('-').expect("start_ns-counter format");
        assert_eq!(pa, pb, "process-start prefix should be stable");
        assert_ne!(na, nb, "counter must advance");
    }
}
