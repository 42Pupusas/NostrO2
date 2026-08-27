//! The subscriptions one connection must carry.
//!
//! A subscription lives on the relay, not on the client. When a connection
//! drops, the relay forgets every filter the client had registered, so a
//! reconnect produces a healthy socket with nothing flowing over it. The
//! service sees no error: it is connected, and simply never receives another
//! event.
//!
//! [`Session`] is the driver's memory of what it asked for, so it can ask
//! again on the next connection.

/// The set of subscriptions to restore after a reconnect.
///
/// Frames are remembered by subscription id, so a resubscription replaces
/// the earlier filter for that id instead of accumulating duplicates. A
/// `CLOSE` forgets its subscription, because a service that closed a
/// subscription must not have it silently reopened by a reconnect.
#[derive(Debug, Default)]
pub struct Session {
    open: Vec<(String, String)>,
}

impl Session {
    /// An empty session.
    #[must_use]
    pub const fn new() -> Self {
        Self { open: Vec::new() }
    }

    /// Records one outbound frame, keeping only what a reconnect must repeat.
    ///
    /// Events and other one-shot messages are not remembered: replaying a
    /// published note on every reconnect would duplicate it on the relay.
    pub fn observe(&mut self, frame: &str) {
        match SubscriptionFrame::classify(frame) {
            SubscriptionFrame::Open(id) => {
                self.open.retain(|(known, _)| known != &id);
                self.open.push((id, frame.to_owned()));
            }
            SubscriptionFrame::Close(id) => {
                self.open.retain(|(known, _)| known != &id);
            }
            SubscriptionFrame::Other => {}
        }
    }

    /// The frames to replay on a fresh connection, in their original order.
    pub fn replay(&self) -> impl Iterator<Item = &str> {
        self.open.iter().map(|(_, frame)| frame.as_str())
    }

    /// How many subscriptions are open.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.open.len()
    }

    /// Whether any subscription is open.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.open.is_empty()
    }
}

/// What one outbound frame means to a [`Session`].
enum SubscriptionFrame {
    Open(String),
    Close(String),
    Other,
}

impl SubscriptionFrame {
    /// Classifies a client frame by its tag and subscription id.
    ///
    /// The frame is read positionally rather than parsed, because the driver
    /// already holds it as serialized JSON and a failed parse must not lose a
    /// subscription.
    fn classify(frame: &str) -> Self {
        let trimmed = frame.trim_start();
        let Some(rest) = trimmed.strip_prefix('[') else {
            return Self::Other;
        };
        let rest = rest.trim_start();
        for (tag, make) in [
            ("\"REQ\"", Self::Open as fn(String) -> Self),
            ("\"CLOSE\"", Self::Close as fn(String) -> Self),
        ] {
            if let Some(after) = rest.strip_prefix(tag) {
                return Self::id_of(after).map_or(Self::Other, make);
            }
        }
        Self::Other
    }

    /// Reads the quoted subscription id that follows the tag.
    fn id_of(after_tag: &str) -> Option<String> {
        let rest = after_tag.trim_start().strip_prefix(',')?.trim_start();
        let rest = rest.strip_prefix('"')?;
        let end = rest.find('"')?;
        Some(rest[..end].to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_session_has_nothing_to_replay() {
        let session = Session::new();
        assert!(session.is_empty());
        assert_eq!(session.replay().count(), 0);
    }

    #[test]
    fn a_request_is_remembered() {
        let mut session = Session::new();
        session.observe(r#"["REQ","sub-a",{"kinds":[1]}]"#);
        assert_eq!(session.len(), 1);
        assert_eq!(
            session.replay().collect::<Vec<_>>(),
            vec![r#"["REQ","sub-a",{"kinds":[1]}]"#]
        );
    }

    // A service that resubscribes with the same id replaces its filter. A
    // session that appended instead would grow without bound across a long
    // run and replay stale filters on every reconnect.
    #[test]
    fn a_repeated_id_replaces_its_filter() {
        let mut session = Session::new();
        session.observe(r#"["REQ","sub-a",{"kinds":[1]}]"#);
        session.observe(r#"["REQ","sub-a",{"kinds":[7]}]"#);
        assert_eq!(session.len(), 1);
        assert_eq!(
            session.replay().collect::<Vec<_>>(),
            vec![r#"["REQ","sub-a",{"kinds":[7]}]"#]
        );
    }

    // A closed subscription must stay closed. Reopening it on a reconnect
    // would resurrect a stream the service deliberately ended.
    #[test]
    fn a_close_forgets_its_subscription() {
        let mut session = Session::new();
        session.observe(r#"["REQ","sub-a",{"kinds":[1]}]"#);
        session.observe(r#"["CLOSE","sub-a"]"#);
        assert!(session.is_empty());
    }

    #[test]
    fn a_close_leaves_the_other_subscriptions_alone() {
        let mut session = Session::new();
        session.observe(r#"["REQ","sub-a",{"kinds":[1]}]"#);
        session.observe(r#"["REQ","sub-b",{"kinds":[2]}]"#);
        session.observe(r#"["CLOSE","sub-a"]"#);
        assert_eq!(
            session.replay().collect::<Vec<_>>(),
            vec![r#"["REQ","sub-b",{"kinds":[2]}]"#]
        );
    }

    // Replaying a published note on every reconnect would publish it again.
    #[test]
    fn an_event_is_never_replayed() {
        let mut session = Session::new();
        session.observe(r#"["EVENT",{"id":"aa","kind":1}]"#);
        assert!(session.is_empty());
    }

    #[test]
    fn an_auth_frame_is_never_replayed() {
        let mut session = Session::new();
        session.observe(r#"["AUTH",{"id":"aa","kind":22242}]"#);
        assert!(session.is_empty());
    }

    #[test]
    fn subscriptions_replay_in_the_order_they_were_opened() {
        let mut session = Session::new();
        session.observe(r#"["REQ","a",{}]"#);
        session.observe(r#"["REQ","b",{}]"#);
        session.observe(r#"["REQ","c",{}]"#);
        assert_eq!(
            session.replay().collect::<Vec<_>>(),
            vec![r#"["REQ","a",{}]"#, r#"["REQ","b",{}]"#, r#"["REQ","c",{}]"#]
        );
    }

    #[test]
    fn a_malformed_frame_is_ignored_rather_than_panicking() {
        let mut session = Session::new();
        for frame in ["", "[", "[\"REQ\"", "[\"REQ\",", "[\"REQ\",\"unterminated", "null"] {
            session.observe(frame);
        }
        assert!(session.is_empty());
    }

    #[test]
    fn whitespace_around_the_frame_does_not_hide_a_subscription() {
        let mut session = Session::new();
        session.observe("  [ \"REQ\" , \"sub-a\" , {} ]");
        assert_eq!(session.len(), 1);
    }
}
