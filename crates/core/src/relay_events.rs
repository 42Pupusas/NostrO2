use bourne::{
    Error as BourneError, ErrorKind as BourneErrorKind, FromJson, JsonWrite, Lexer, ToJson,
};

/// NIP-01 relay message tags. Wire form is uppercase (`"EVENT"`, `"OK"`, …).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
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

impl RelayEventTag {
    const fn as_quoted(self) -> &'static str {
        match self {
            Self::Event => "\"EVENT\"",
            Self::Ok => "\"OK\"",
            Self::Eose => "\"EOSE\"",
            Self::Notice => "\"NOTICE\"",
            Self::Close => "\"CLOSE\"",
            Self::Auth => "\"AUTH\"",
            Self::Req => "\"REQ\"",
            Self::Closed => "\"CLOSED\"",
        }
    }

    pub(crate) fn from_str_wire(s: &str) -> Option<Self> {
        Some(match s {
            "EVENT" => Self::Event,
            "OK" => Self::Ok,
            "EOSE" => Self::Eose,
            "NOTICE" => Self::Notice,
            "CLOSE" => Self::Close,
            "AUTH" => Self::Auth,
            "REQ" => Self::Req,
            "CLOSED" => Self::Closed,
            _ => return None,
        })
    }
}

impl<'input> FromJson<'input> for RelayEventTag {
    fn from_lex(lex: &mut Lexer<'input>) -> Result<Self, BourneError> {
        let s = lex.parse_str_value()?;
        Self::from_str_wire(s)
            .ok_or_else(|| BourneError::new(BourneErrorKind::UnknownField, lex.position()))
    }
}

impl ToJson for RelayEventTag {
    fn write_json<W: JsonWrite + ?Sized>(&self, w: &mut W) -> Result<(), W::Error> {
        w.write_str_raw(self.as_quoted())
    }
}

// ── FROM RELAY TO CLIENT ──────────────────────────────────────────
//
// Nostr wire frames are JSON arrays: `["EVENT", "sub_id", {note}]`,
// `["OK", "event_id", true, "msg"]`, etc. Each variant maps 1:1 to
// a NIP-01 / NIP-42 frame shape. Discrimination is by the first
// array element (the tag string) plus the element count.

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NostrRelayEvent {
    NewNote(RelayEventTag, String, crate::note::NostrNote),
    SentOk(RelayEventTag, String, bool, String),
    EndOfSubscription(RelayEventTag, String),
    ClosedSubscription(RelayEventTag, String),
    Notice(RelayEventTag, String),
    Auth(RelayEventTag, String),
}

impl<'input> FromJson<'input> for NostrRelayEvent {
    fn from_lex(lex: &mut Lexer<'input>) -> Result<Self, BourneError> {
        if lex.array_start()? {
            return Err(BourneError::new(
                BourneErrorKind::TypeMismatch,
                lex.position(),
            ));
        }
        let tag_str = lex.parse_str_value()?;
        let tag = RelayEventTag::from_str_wire(tag_str)
            .ok_or_else(|| BourneError::new(BourneErrorKind::UnknownField, lex.position()))?;
        // expect comma (more elements) or end
        if lex.array_continue(b']')? {
            return Err(BourneError::new(
                BourneErrorKind::MissingField,
                lex.position(),
            ));
        }
        match tag {
            RelayEventTag::Event => {
                let sub_id = String::from_lex(lex)?;
                if lex.array_continue(b']')? {
                    return Err(BourneError::new(BourneErrorKind::MissingField, lex.position()));
                }
                let note = crate::note::NostrNote::from_lex(lex)?;
                if !lex.array_continue(b']')? {
                    return Err(BourneError::new(BourneErrorKind::TrailingData, lex.position()));
                }
                Ok(Self::NewNote(RelayEventTag::Event, sub_id, note))
            }
            RelayEventTag::Ok => {
                let event_id = String::from_lex(lex)?;
                if lex.array_continue(b']')? {
                    return Err(BourneError::new(BourneErrorKind::MissingField, lex.position()));
                }
                let success = bool::from_lex(lex)?;
                if lex.array_continue(b']')? {
                    return Err(BourneError::new(BourneErrorKind::MissingField, lex.position()));
                }
                let message = String::from_lex(lex)?;
                if !lex.array_continue(b']')? {
                    return Err(BourneError::new(BourneErrorKind::TrailingData, lex.position()));
                }
                Ok(Self::SentOk(RelayEventTag::Ok, event_id, success, message))
            }
            RelayEventTag::Eose
            | RelayEventTag::Closed
            | RelayEventTag::Notice
            | RelayEventTag::Auth => {
                let val = String::from_lex(lex)?;
                if !lex.array_continue(b']')? {
                    return Err(BourneError::new(BourneErrorKind::TrailingData, lex.position()));
                }
                Ok(match tag {
                    RelayEventTag::Eose => Self::EndOfSubscription(tag, val),
                    RelayEventTag::Closed => Self::ClosedSubscription(tag, val),
                    RelayEventTag::Notice => Self::Notice(tag, val),
                    RelayEventTag::Auth => Self::Auth(tag, val),
                    _ => unreachable!(),
                })
            }
            _ => Err(BourneError::new(
                BourneErrorKind::UnknownField,
                lex.position(),
            )),
        }
    }
}

impl ToJson for NostrRelayEvent {
    fn write_json<W: JsonWrite + ?Sized>(&self, w: &mut W) -> Result<(), W::Error> {
        w.write_byte(b'[')?;
        match self {
            Self::NewNote(tag, sub_id, note) => {
                tag.write_json(w)?;
                w.write_byte(b',')?;
                w.write_escaped_str(sub_id)?;
                w.write_byte(b',')?;
                note.write_json(w)?;
            }
            Self::SentOk(tag, event_id, success, message) => {
                tag.write_json(w)?;
                w.write_byte(b',')?;
                w.write_escaped_str(event_id)?;
                w.write_byte(b',')?;
                success.write_json(w)?;
                w.write_byte(b',')?;
                w.write_escaped_str(message)?;
            }
            Self::EndOfSubscription(tag, sub_id)
            | Self::ClosedSubscription(tag, sub_id)
            | Self::Notice(tag, sub_id)
            | Self::Auth(tag, sub_id) => {
                tag.write_json(w)?;
                w.write_byte(b',')?;
                w.write_escaped_str(sub_id)?;
            }
        }
        w.write_byte(b']')
    }
}

impl std::str::FromStr for NostrRelayEvent {
    type Err = bourne::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        bourne::parse_str(value)
    }
}

impl TryFrom<&[u8]> for NostrRelayEvent {
    type Error = bourne::Error;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        bourne::parse(value)
    }
}

// ── FROM CLIENT TO RELAY ──────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NostrClientEvent {
    SendNoteEvent(RelayEventTag, super::note::NostrNote),
    Subscribe(
        RelayEventTag,
        String,
        super::subscriptions::NostrSubscription,
    ),
    CloseSubscriptionEvent(RelayEventTag, String),
    AuthEvent(RelayEventTag, crate::note::NostrNote),
}

impl NostrClientEvent {
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
                (js_sys::Date::now() * 1_000_000.0) as u64
            }
        });

        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("{start_ns}-{n}")
    }

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
        Self::Subscribe(RelayEventTag::Req, Self::fresh_sub_id(), subscription)
    }
}

impl From<&super::subscriptions::NostrSubscription> for NostrClientEvent {
    fn from(subscription: &super::subscriptions::NostrSubscription) -> Self {
        Self::Subscribe(RelayEventTag::Req, Self::fresh_sub_id(), subscription.clone())
    }
}

impl<'input> FromJson<'input> for NostrClientEvent {
    fn from_lex(lex: &mut Lexer<'input>) -> Result<Self, BourneError> {
        if lex.array_start()? {
            return Err(BourneError::new(
                BourneErrorKind::TypeMismatch,
                lex.position(),
            ));
        }
        let tag_str = lex.parse_str_value()?;
        let tag = RelayEventTag::from_str_wire(tag_str)
            .ok_or_else(|| BourneError::new(BourneErrorKind::UnknownField, lex.position()))?;
        match tag {
            RelayEventTag::Event | RelayEventTag::Auth => {
                if lex.array_continue(b']')? {
                    return Err(BourneError::new(BourneErrorKind::MissingField, lex.position()));
                }
                let note = crate::note::NostrNote::from_lex(lex)?;
                if !lex.array_continue(b']')? {
                    return Err(BourneError::new(BourneErrorKind::TrailingData, lex.position()));
                }
                Ok(match tag {
                    RelayEventTag::Event => Self::SendNoteEvent(tag, note),
                    _ => Self::AuthEvent(tag, note),
                })
            }
            RelayEventTag::Req => {
                if lex.array_continue(b']')? {
                    return Err(BourneError::new(BourneErrorKind::MissingField, lex.position()));
                }
                let sub_id = String::from_lex(lex)?;
                if lex.array_continue(b']')? {
                    return Err(BourneError::new(BourneErrorKind::MissingField, lex.position()));
                }
                let filter = super::subscriptions::NostrSubscription::from_lex(lex)?;
                if !lex.array_continue(b']')? {
                    return Err(BourneError::new(BourneErrorKind::TrailingData, lex.position()));
                }
                Ok(Self::Subscribe(RelayEventTag::Req, sub_id, filter))
            }
            RelayEventTag::Close => {
                if lex.array_continue(b']')? {
                    return Err(BourneError::new(BourneErrorKind::MissingField, lex.position()));
                }
                let sub_id = String::from_lex(lex)?;
                if !lex.array_continue(b']')? {
                    return Err(BourneError::new(BourneErrorKind::TrailingData, lex.position()));
                }
                Ok(Self::CloseSubscriptionEvent(RelayEventTag::Close, sub_id))
            }
            _ => Err(BourneError::new(
                BourneErrorKind::UnknownField,
                lex.position(),
            )),
        }
    }
}

impl ToJson for NostrClientEvent {
    fn write_json<W: JsonWrite + ?Sized>(&self, w: &mut W) -> Result<(), W::Error> {
        w.write_byte(b'[')?;
        match self {
            Self::SendNoteEvent(tag, note) | Self::AuthEvent(tag, note) => {
                tag.write_json(w)?;
                w.write_byte(b',')?;
                note.write_json(w)?;
            }
            Self::Subscribe(tag, sub_id, filter) => {
                tag.write_json(w)?;
                w.write_byte(b',')?;
                w.write_escaped_str(sub_id)?;
                w.write_byte(b',')?;
                filter.write_json(w)?;
            }
            Self::CloseSubscriptionEvent(tag, sub_id) => {
                tag.write_json(w)?;
                w.write_byte(b',')?;
                w.write_escaped_str(sub_id)?;
            }
        }
        w.write_byte(b']')
    }
}

impl std::str::FromStr for NostrClientEvent {
    type Err = bourne::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        bourne::parse_str(value)
    }
}

impl TryFrom<&[u8]> for NostrClientEvent {
    type Error = bourne::Error;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        bourne::parse(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::NostrNote;
    use crate::subscriptions::NostrSubscription;

    fn sample_note() -> NostrNote {
        NostrNote {
            pubkey: "a".repeat(64),
            created_at: 1_700_000_000,
            kind: 1,
            content: "hello relay".into(),
            id: Some("b".repeat(64)),
            sig: Some("c".repeat(128)),
            ..Default::default()
        }
    }

    fn round_trip<
        T: bourne::ToJson + for<'a> bourne::FromJson<'a> + std::fmt::Debug + PartialEq,
    >(
        val: &T,
    ) {
        let json = bourne::to_string(val).expect("serialize");
        let back: T = bourne::parse_str(&json).expect("parse back");
        assert_eq!(val, &back);
    }

    #[test]
    fn relay_event_round_trip() {
        let note = sample_note();
        round_trip(&NostrRelayEvent::NewNote(
            RelayEventTag::Event,
            "sub1".into(),
            note,
        ));
    }

    #[test]
    fn relay_ok_round_trip() {
        round_trip(&NostrRelayEvent::SentOk(
            RelayEventTag::Ok,
            "abc".repeat(21),
            true,
            "duplicate:".into(),
        ));
        round_trip(&NostrRelayEvent::SentOk(
            RelayEventTag::Ok,
            "def".repeat(21),
            false,
            "blocked: not in whitelist".into(),
        ));
    }

    #[test]
    fn relay_eose_round_trip() {
        round_trip(&NostrRelayEvent::EndOfSubscription(
            RelayEventTag::Eose,
            "sub42".into(),
        ));
    }

    #[test]
    fn relay_notice_round_trip() {
        round_trip(&NostrRelayEvent::Notice(
            RelayEventTag::Notice,
            "rate limited".into(),
        ));
    }

    #[test]
    fn relay_auth_round_trip() {
        round_trip(&NostrRelayEvent::Auth(
            RelayEventTag::Auth,
            "challenge-xyz".into(),
        ));
    }

    #[test]
    fn relay_closed_round_trip() {
        round_trip(&NostrRelayEvent::ClosedSubscription(
            RelayEventTag::Closed,
            "sub7".into(),
        ));
    }

    #[test]
    fn client_event_round_trip() {
        let note = sample_note();
        round_trip(&NostrClientEvent::SendNoteEvent(RelayEventTag::Event, note));
    }

    #[test]
    fn client_auth_round_trip() {
        let note = sample_note();
        round_trip(&NostrClientEvent::AuthEvent(RelayEventTag::Auth, note));
    }

    #[test]
    fn client_close_round_trip() {
        round_trip(&NostrClientEvent::CloseSubscriptionEvent(
            RelayEventTag::Close,
            "sub99".into(),
        ));
    }

    #[test]
    fn client_subscribe_round_trip() {
        let sub = NostrSubscription::new().kind(1).limit(10);
        round_trip(&NostrClientEvent::Subscribe(
            RelayEventTag::Req,
            "mysub".into(),
            sub,
        ));
    }

    #[test]
    fn relay_event_from_str_round_trip() {
        let note = sample_note();
        let event = NostrRelayEvent::NewNote(RelayEventTag::Event, "sub1".into(), note);
        let json = bourne::to_string(&event).unwrap();
        let parsed: NostrRelayEvent = json.parse().unwrap();
        assert_eq!(event, parsed);
    }

    #[test]
    fn client_event_from_str_round_trip() {
        let note = sample_note();
        let event = NostrClientEvent::SendNoteEvent(RelayEventTag::Event, note);
        let json = bourne::to_string(&event).unwrap();
        let parsed: NostrClientEvent = json.parse().unwrap();
        assert_eq!(event, parsed);
    }

    #[test]
    fn relay_event_tag_all_variants() {
        let tags = [
            (RelayEventTag::Event, "EVENT"),
            (RelayEventTag::Ok, "OK"),
            (RelayEventTag::Eose, "EOSE"),
            (RelayEventTag::Notice, "NOTICE"),
            (RelayEventTag::Close, "CLOSE"),
            (RelayEventTag::Auth, "AUTH"),
            (RelayEventTag::Req, "REQ"),
            (RelayEventTag::Closed, "CLOSED"),
        ];
        for (tag, expected_wire) in tags {
            let quoted = tag.as_quoted();
            assert_eq!(quoted, format!("\"{expected_wire}\""));
            let parsed = RelayEventTag::from_str_wire(expected_wire).unwrap();
            assert_eq!(tag, parsed);
        }
        assert!(RelayEventTag::from_str_wire("UNKNOWN").is_none());
    }

    #[test]
    fn relay_event_rejects_empty_array() {
        let result: Result<NostrRelayEvent, _> = bourne::parse_str("[]");
        assert!(result.is_err());
    }

    #[test]
    fn relay_event_rejects_unknown_tag() {
        let result: Result<NostrRelayEvent, _> = bourne::parse_str(r#"["BOGUS","sub"]"#);
        assert!(result.is_err());
    }

    #[test]
    fn relay_event_rejects_tag_only() {
        let result: Result<NostrRelayEvent, _> = bourne::parse_str(r#"["EVENT"]"#);
        assert!(result.is_err());
    }

    #[test]
    fn relay_event_rejects_truncated_ok() {
        let result: Result<NostrRelayEvent, _> = bourne::parse_str(r#"["OK","eid"]"#);
        assert!(result.is_err());
        let result: Result<NostrRelayEvent, _> = bourne::parse_str(r#"["OK","eid",true]"#);
        assert!(result.is_err());
    }

    #[test]
    fn relay_event_rejects_trailing_data() {
        let result: Result<NostrRelayEvent, _> = bourne::parse_str(r#"["EOSE","sub","extra"]"#);
        assert!(result.is_err());
    }

    #[test]
    fn relay_event_rejects_not_array() {
        let result: Result<NostrRelayEvent, _> = bourne::parse_str(r#"{"tag":"EVENT"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn relay_event_rejects_event_missing_note() {
        let result: Result<NostrRelayEvent, _> = bourne::parse_str(r#"["EVENT","sub"]"#);
        assert!(result.is_err());
    }

    #[test]
    fn relay_event_rejects_event_trailing_data() {
        let note = sample_note();
        let note_json = bourne::to_string(&note).unwrap();
        let json = format!(r#"["EVENT","sub",{note_json},"extra"]"#);
        let result: Result<NostrRelayEvent, _> = bourne::parse_str(&json);
        assert!(result.is_err());
    }

    #[test]
    fn relay_event_rejects_ok_trailing_data() {
        let result: Result<NostrRelayEvent, _> =
            bourne::parse_str(r#"["OK","eid",true,"msg","extra"]"#);
        assert!(result.is_err());
    }

    #[test]
    fn relay_event_rejects_ok_missing_bool() {
        let result: Result<NostrRelayEvent, _> = bourne::parse_str(r#"["OK","eid"]"#);
        assert!(result.is_err());
    }

    #[test]
    fn relay_event_rejects_ok_missing_message() {
        let result: Result<NostrRelayEvent, _> = bourne::parse_str(r#"["OK","eid",true]"#);
        assert!(result.is_err());
    }

    #[test]
    fn relay_event_rejects_client_only_tags() {
        let result: Result<NostrRelayEvent, _> = bourne::parse_str(r#"["REQ","sub",{}]"#);
        assert!(result.is_err());
        let result: Result<NostrRelayEvent, _> = bourne::parse_str(r#"["CLOSE","sub"]"#);
        assert!(result.is_err());
    }

    #[test]
    fn client_event_rejects_empty_array() {
        let result: Result<NostrClientEvent, _> = bourne::parse_str("[]");
        assert!(result.is_err());
    }

    #[test]
    fn client_event_rejects_unknown_tag() {
        let result: Result<NostrClientEvent, _> = bourne::parse_str(r#"["BOGUS","sub"]"#);
        assert!(result.is_err());
    }

    #[test]
    fn client_event_rejects_server_only_tags() {
        let result: Result<NostrClientEvent, _> = bourne::parse_str(r#"["EOSE","sub"]"#);
        assert!(result.is_err());
        let result: Result<NostrClientEvent, _> = bourne::parse_str(r#"["OK","eid",true,""]"#);
        assert!(result.is_err());
        let result: Result<NostrClientEvent, _> = bourne::parse_str(r#"["NOTICE","msg"]"#);
        assert!(result.is_err());
    }

    #[test]
    fn client_event_rejects_truncated_event() {
        let result: Result<NostrClientEvent, _> = bourne::parse_str(r#"["EVENT"]"#);
        assert!(result.is_err());
    }

    #[test]
    fn client_event_rejects_truncated_req() {
        let result: Result<NostrClientEvent, _> = bourne::parse_str(r#"["REQ"]"#);
        assert!(result.is_err());
        let result: Result<NostrClientEvent, _> = bourne::parse_str(r#"["REQ","sub"]"#);
        assert!(result.is_err());
    }

    #[test]
    fn client_event_rejects_truncated_close() {
        let result: Result<NostrClientEvent, _> = bourne::parse_str(r#"["CLOSE"]"#);
        assert!(result.is_err());
    }

    #[test]
    fn client_event_rejects_truncated_auth() {
        let result: Result<NostrClientEvent, _> = bourne::parse_str(r#"["AUTH"]"#);
        assert!(result.is_err());
    }

    #[test]
    fn client_event_rejects_event_trailing_data() {
        let note = sample_note();
        let note_json = bourne::to_string(&note).unwrap();
        let json = format!(r#"["EVENT",{note_json},"extra"]"#);
        let result: Result<NostrClientEvent, _> = bourne::parse_str(&json);
        assert!(result.is_err());
    }

    #[test]
    fn client_event_rejects_auth_trailing_data() {
        let note = sample_note();
        let note_json = bourne::to_string(&note).unwrap();
        let json = format!(r#"["AUTH",{note_json},"extra"]"#);
        let result: Result<NostrClientEvent, _> = bourne::parse_str(&json);
        assert!(result.is_err());
    }

    #[test]
    fn client_event_rejects_close_trailing_data() {
        let result: Result<NostrClientEvent, _> = bourne::parse_str(r#"["CLOSE","sub","extra"]"#);
        assert!(result.is_err());
    }

    #[test]
    fn client_event_rejects_req_trailing_data() {
        let result: Result<NostrClientEvent, _> = bourne::parse_str(r#"["REQ","sub",{},"extra"]"#);
        assert!(result.is_err());
    }

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
        let (pa, na) = a.split_once('-').expect("start_ns-counter format");
        let (pb, nb) = b.split_once('-').expect("start_ns-counter format");
        assert_eq!(pa, pb, "process-start prefix should be stable");
        assert_ne!(na, nb, "counter must advance");
    }
}
