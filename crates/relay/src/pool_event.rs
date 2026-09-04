//! What a pool reports to its reader.
//!
//! A single relay's events already say what happened, because the reader
//! knows which relay it is holding. A pool merges many relays into one
//! stream, so the same events become ambiguous: "disconnected" is only
//! actionable when it names the relay that dropped, and a note is only
//! attributable when it names the relay that served it.
//!
//! [`PoolEvent`] is therefore a relay event plus its origin, for every
//! variant without exception.
//!
//! The origin is an [`Arc`], not a [`RelayUrl`]: a relay's address never
//! changes, and every message it serves names the same one. Cloning the
//! URL itself would copy three heap `String`s per message on the pool's
//! hot path, which measured ~8x the cost of bumping a refcount.
//!
//! [`Arc`]: std::sync::Arc
//! [`RelayUrl`]: crate::url::RelayUrl

/// One event from one relay in a pool.
///
/// Every variant names its relay, so [`Self::url`] is total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PoolEvent {
    /// A relay connected, and its subscriptions were restored.
    Connected(std::sync::Arc<crate::url::RelayUrl>),
    /// A relay's connection ended, with the reason when there is one.
    Disconnected(std::sync::Arc<crate::url::RelayUrl>, Option<String>),
    /// A relay gave up: its retry budget is spent, and it will send no more.
    Exhausted(std::sync::Arc<crate::url::RelayUrl>),
    /// A relay delivered a protocol message, and which relay that was.
    ///
    /// The URL distinguishes otherwise identical notes arriving from
    /// different relays, which is what makes per-relay accounting,
    /// trust decisions, and "who served this first" possible.
    Message(
        std::sync::Arc<crate::url::RelayUrl>,
        Box<nostro2::NostrRelayEvent>,
    ),
}

impl PoolEvent {
    /// Pairs a driver event with the relay it came from.
    ///
    /// The forwarder holds one `Arc` per relay for its whole life, so this
    /// bumps a refcount rather than copying the address.
    pub(crate) fn from_driver(
        url: &std::sync::Arc<crate::url::RelayUrl>,
        event: crate::driver::DriverEvent,
    ) -> Self {
        match event {
            crate::driver::DriverEvent::Connected => Self::Connected(url.clone()),
            crate::driver::DriverEvent::Disconnected(reason) => {
                Self::Disconnected(url.clone(), reason)
            }
            crate::driver::DriverEvent::Exhausted => Self::Exhausted(url.clone()),
            crate::driver::DriverEvent::Message(event) => Self::Message(url.clone(), event),
        }
    }

    /// The relay this event came from.
    ///
    /// Cheap to clone: the address is shared, not copied.
    #[must_use]
    pub const fn url(&self) -> &std::sync::Arc<crate::url::RelayUrl> {
        match self {
            Self::Connected(url)
            | Self::Disconnected(url, _)
            | Self::Exhausted(url)
            | Self::Message(url, _) => url,
        }
    }

    /// The protocol message, when this event carries one.
    #[must_use]
    pub fn message(self) -> Option<nostro2::NostrRelayEvent> {
        match self {
            Self::Message(_, event) => Some(*event),
            _ => None,
        }
    }

    /// The protocol message together with the relay that served it.
    ///
    /// Use this over [`Self::message`] when the same note may arrive from
    /// several relays and the reader has to tell them apart.
    #[must_use]
    pub fn into_message(
        self,
    ) -> Option<(
        std::sync::Arc<crate::url::RelayUrl>,
        nostro2::NostrRelayEvent,
    )> {
        match self {
            Self::Message(url, event) => Some((url, *event)),
            _ => None,
        }
    }

    /// The protocol message without consuming the event.
    #[must_use]
    pub fn as_message(&self) -> Option<&nostro2::NostrRelayEvent> {
        match self {
            Self::Message(_, event) => Some(event),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url() -> std::sync::Arc<crate::url::RelayUrl> {
        std::sync::Arc::new(crate::url::RelayUrl::parse("wss://relay.example.com").unwrap())
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
        assert_eq!(event.url().as_ref(), url().as_ref());
    }

    // Attribution must not copy the address per message: the forwarder
    // holds one Arc per relay, and every event shares it.
    #[test]
    fn attribution_shares_one_url_rather_than_copying_it() {
        let url = url();
        let before = std::sync::Arc::strong_count(&url);
        let event = PoolEvent::from_driver(
            &url,
            crate::driver::DriverEvent::Message(Box::new(note_event())),
        );

        assert_eq!(std::sync::Arc::strong_count(&url), before + 1);
        assert!(std::sync::Arc::ptr_eq(event.url(), &url));
    }

    #[test]
    fn a_disconnect_keeps_its_reason_and_its_relay() {
        let event = PoolEvent::from_driver(
            &url(),
            crate::driver::DriverEvent::Disconnected(Some("peer went away".to_string())),
        );
        match event {
            PoolEvent::Disconnected(from, reason) => {
                assert_eq!(from.as_ref(), url().as_ref());
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
    fn every_variant_names_its_relay() {
        for event in [
            crate::driver::DriverEvent::Connected,
            crate::driver::DriverEvent::Disconnected(None),
            crate::driver::DriverEvent::Exhausted,
            crate::driver::DriverEvent::Message(Box::new(note_event())),
        ] {
            assert_eq!(
                PoolEvent::from_driver(&url(), event).url().as_ref(),
                url().as_ref()
            );
        }
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
        assert!(event.as_message().is_none());
        assert!(event.clone().message().is_none());
        assert!(event.into_message().is_none());
    }

    // The reason this type exists: a merged stream must attribute a note,
    // not just a disconnect. A message that cannot name its relay makes
    // per-relay accounting impossible.
    #[test]
    fn a_message_names_the_relay_that_served_it() {
        let event = PoolEvent::from_driver(
            &url(),
            crate::driver::DriverEvent::Message(Box::new(note_event())),
        );
        assert_eq!(event.url().as_ref(), url().as_ref());
        let (from, message) = event.into_message().expect("a message");
        assert_eq!(from.as_ref(), url().as_ref());
        assert_eq!(message, note_event());
    }

    // The same note from two relays is two attributable events, so a reader
    // can tell which relay served it first.
    #[test]
    fn the_same_note_from_two_relays_is_told_apart_by_its_url() {
        let other =
            std::sync::Arc::new(crate::url::RelayUrl::parse("wss://other.example.com").unwrap());
        let one = PoolEvent::from_driver(
            &url(),
            crate::driver::DriverEvent::Message(Box::new(note_event())),
        );
        let two = PoolEvent::from_driver(
            &other,
            crate::driver::DriverEvent::Message(Box::new(note_event())),
        );

        assert_ne!(one, two);
        assert_eq!(one.as_message(), two.as_message());
        assert_ne!(one.url(), two.url());
    }

}
