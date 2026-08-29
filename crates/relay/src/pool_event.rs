//! What a pool reports to its reader.
//!
//! A single relay's events already say what happened, because the reader
//! knows which relay it is holding. A pool merges many relays into one
//! stream, so the same events become ambiguous: "disconnected" is only
//! actionable when it names the relay that dropped.
//!
//! [`PoolEvent`] is therefore a relay event plus its origin.

/// One event from one relay in a pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolEvent {
    /// A relay connected, and its subscriptions were restored.
    Connected(crate::url::RelayUrl),
    /// A relay's connection ended, with the reason when there is one.
    Disconnected(crate::url::RelayUrl, Option<String>),
    /// A relay gave up: its retry budget is spent, and it will send no more.
    Exhausted(crate::url::RelayUrl),
    /// A relay delivered a protocol message.
    Message(Box<nostro2::NostrRelayEvent>),
}

impl PoolEvent {
    /// Pairs a driver event with the relay it came from.
    pub(crate) fn from_driver(url: &crate::url::RelayUrl, event: crate::driver::DriverEvent) -> Self {
        match event {
            crate::driver::DriverEvent::Connected => Self::Connected(url.clone()),
            crate::driver::DriverEvent::Disconnected(reason) => {
                Self::Disconnected(url.clone(), reason)
            }
            crate::driver::DriverEvent::Exhausted => Self::Exhausted(url.clone()),
            crate::driver::DriverEvent::Message(event) => Self::Message(event),
        }
    }

    /// The relay this event came from.
    #[must_use]
    pub const fn url(&self) -> Option<&crate::url::RelayUrl> {
        match self {
            Self::Connected(url) | Self::Disconnected(url, _) | Self::Exhausted(url) => Some(url),
            Self::Message(_) => None,
        }
    }

    /// The protocol message, when this event carries one.
    #[must_use]
    pub fn message(self) -> Option<nostro2::NostrRelayEvent> {
        match self {
            Self::Message(event) => Some(*event),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url() -> crate::url::RelayUrl {
        crate::url::RelayUrl::parse("wss://relay.example.com").unwrap()
    }

    fn note_event() -> nostro2::NostrRelayEvent {
        nostro2::NostrRelayEvent::EndOfSubscription(
            nostro2::RelayEventTag::Eose,
            "sub".to_string(),
        )
    }

    // A merged stream is only actionable when a disconnect names its relay.
    #[test]
    fn a_lifecycle_event_carries_its_relay() {
        let event = PoolEvent::from_driver(&url(), crate::driver::DriverEvent::Connected);
        assert_eq!(event.url(), Some(&url()));
    }

    #[test]
    fn a_disconnect_keeps_its_reason_and_its_relay() {
        let event = PoolEvent::from_driver(
            &url(),
            crate::driver::DriverEvent::Disconnected(Some("peer went away".to_string())),
        );
        match event {
            PoolEvent::Disconnected(from, reason) => {
                assert_eq!(from, url());
                assert_eq!(reason.as_deref(), Some("peer went away"));
            }
            other => panic!("expected a disconnect, got {other:?}"),
        }
    }

    #[test]
    fn an_exhausted_relay_names_itself() {
        let event = PoolEvent::from_driver(&url(), crate::driver::DriverEvent::Exhausted);
        assert!(matches!(event, PoolEvent::Exhausted(from) if from == url()));
    }

    #[test]
    fn a_message_survives_the_conversion() {
        let event = PoolEvent::from_driver(
            &url(),
            crate::driver::DriverEvent::Message(Box::new(note_event())),
        );
        assert_eq!(event.message(), Some(note_event()));
    }

    #[test]
    fn a_lifecycle_event_carries_no_message() {
        let event = PoolEvent::from_driver(&url(), crate::driver::DriverEvent::Connected);
        assert!(event.message().is_none());
    }

    #[test]
    fn a_message_has_no_single_url_field() {
        let event = PoolEvent::from_driver(
            &url(),
            crate::driver::DriverEvent::Message(Box::new(note_event())),
        );
        assert!(event.url().is_none());
    }
}
