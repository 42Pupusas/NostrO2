#[cfg(feature = "bourne")]
use json_bourne::{
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

/// Shared parser for NIP-01 relay-event wire frames.
///
/// Both the zero-copy [`NostrRelayEventView`](crate::NostrRelayEventView) and
/// the owning [`NostrRelayEvent`] parse the same `["TAG", …]` array shapes —
/// they differ only in allocation strategy and whether the tag is stored.
/// This trait factors out the common dispatch logic so there is a single
/// implementation to test and maintain. `bourne`-only: it drives directly
/// off `Lexer`'s checkpoint/restore API, which `serde` has no equivalent
/// for; the `serde` backend implements `NostrRelayEvent`/`NostrClientEvent`
/// directly against `serde_json::Value` instead.
#[cfg(feature = "bourne")]
pub trait RelayFrameParser<'input>: Sized {
    type Str: FromJson<'input>;
    type Note: FromJson<'input>;

    fn new_note(tag: RelayEventTag, sub_id: Self::Str, note: Self::Note) -> Self;
    fn sent_ok(tag: RelayEventTag, event_id: Self::Str, success: bool, message: Self::Str) -> Self;
    fn eose(tag: RelayEventTag, sub_id: Self::Str) -> Self;
    fn closed(tag: RelayEventTag, sub_id: Self::Str) -> Self;
    fn notice(tag: RelayEventTag, message: Self::Str) -> Self;
    fn auth(tag: RelayEventTag, challenge: Self::Str) -> Self;

    fn from_lex(lex: &mut Lexer<'input>) -> Result<Self, BourneError> {
        let tag = lex.parse_frame_tag()?;
        match tag {
            RelayEventTag::Event => {
                let sub_id = Self::Str::from_lex(lex)?;
                lex.expect_more()?;
                let note = Self::Note::from_lex(lex)?;
                lex.expect_end()?;
                Ok(Self::new_note(tag, sub_id, note))
            }
            RelayEventTag::Ok => {
                let event_id = Self::Str::from_lex(lex)?;
                lex.expect_more()?;
                let success = bool::from_lex(lex)?;
                lex.expect_more()?;
                let message = Self::Str::from_lex(lex)?;
                lex.expect_end()?;
                Ok(Self::sent_ok(tag, event_id, success, message))
            }
            RelayEventTag::Eose
            | RelayEventTag::Closed
            | RelayEventTag::Notice
            | RelayEventTag::Auth => {
                let val = Self::Str::from_lex(lex)?;
                lex.expect_end()?;
                Ok(match tag {
                    RelayEventTag::Eose => Self::eose(tag, val),
                    RelayEventTag::Closed => Self::closed(tag, val),
                    RelayEventTag::Notice => Self::notice(tag, val),
                    RelayEventTag::Auth => Self::auth(tag, val),
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

impl RelayEventTag {
    /// `"EVENT"`, `"OK"`, … — the bare (unquoted) wire form.
    #[cfg_attr(feature = "bourne", allow(dead_code))]
    pub(crate) const fn as_wire(self) -> &'static str {
        match self {
            Self::Event => "EVENT",
            Self::Ok => "OK",
            Self::Eose => "EOSE",
            Self::Notice => "NOTICE",
            Self::Close => "CLOSE",
            Self::Auth => "AUTH",
            Self::Req => "REQ",
            Self::Closed => "CLOSED",
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

#[cfg(feature = "bourne")]
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
}

#[cfg(feature = "bourne")]
impl<'input> FromJson<'input> for RelayEventTag {
    fn from_lex(lex: &mut Lexer<'input>) -> Result<Self, BourneError> {
        let s = lex.parse_str_value()?;
        Self::from_str_wire(s)
            .ok_or_else(|| BourneError::new(BourneErrorKind::UnknownField, lex.position()))
    }
}

#[cfg(feature = "bourne")]
impl ToJson for RelayEventTag {
    fn write_json<W: JsonWrite + ?Sized>(&self, w: &mut W) -> Result<(), W::Error> {
        w.write_str_raw(self.as_quoted())
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for RelayEventTag {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_wire())
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for RelayEventTag {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(RelayEventTagVisitor)
    }
}

/// Recognises a tag from the borrowed input, without allocating.
///
/// The set is closed and every member is a short literal, so the bytes
/// only need to be compared, never owned. Deserializing through `String`
/// costs an allocation per frame to build a value that is dropped as soon
/// as it has been matched.
#[cfg(feature = "serde")]
struct RelayEventTagVisitor;

#[cfg(feature = "serde")]
impl serde::de::Visitor<'_> for RelayEventTagVisitor {
    type Value = RelayEventTag;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a nostr relay frame tag")
    }

    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
        RelayEventTag::from_str_wire(v)
            .ok_or_else(|| E::custom(format_args!("unknown relay tag: {v}")))
    }
}

// ── Wire-frame parsing helpers ───────────────────────────────────

/// Extension trait that adds nostr wire-frame parsing sugar to `Lexer`.
#[cfg(feature = "bourne")]
pub trait WireFrameExt {
    /// Consumes `[`, the tag string, and the following comma.
    /// Returns `TypeMismatch` for an empty array, `UnknownField` for an
    /// unrecognized tag, or `MissingField` if no further elements follow.
    fn parse_frame_tag(&mut self) -> Result<RelayEventTag, BourneError>;
    /// Asserts the array has at least one more element (comma, not `]`).
    fn expect_more(&mut self) -> Result<(), BourneError>;
    /// Asserts the array has been fully consumed (closing `]`).
    fn expect_end(&mut self) -> Result<(), BourneError>;
}

#[cfg(feature = "bourne")]
impl WireFrameExt for Lexer<'_> {
    fn parse_frame_tag(&mut self) -> Result<RelayEventTag, BourneError> {
        if self.array_start()? {
            return Err(BourneError::new(
                BourneErrorKind::TypeMismatch,
                self.position(),
            ));
        }
        let tag = RelayEventTag::from_str_wire(self.parse_str_value()?)
            .ok_or_else(|| BourneError::new(BourneErrorKind::UnknownField, self.position()))?;
        self.expect_more()?;
        Ok(tag)
    }

    fn expect_more(&mut self) -> Result<(), BourneError> {
        if self.array_continue(b']')? {
            Err(BourneError::new(
                BourneErrorKind::MissingField,
                self.position(),
            ))
        } else {
            Ok(())
        }
    }

    fn expect_end(&mut self) -> Result<(), BourneError> {
        if self.array_continue(b']')? {
            Ok(())
        } else {
            Err(BourneError::new(
                BourneErrorKind::TrailingData,
                self.position(),
            ))
        }
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

#[cfg(feature = "bourne")]
impl RelayFrameParser<'_> for NostrRelayEvent {
    type Str = String;
    type Note = crate::note::NostrNote;

    fn new_note(tag: RelayEventTag, sub_id: String, note: Self::Note) -> Self {
        Self::NewNote(tag, sub_id, note)
    }
    fn sent_ok(tag: RelayEventTag, event_id: String, success: bool, message: String) -> Self {
        Self::SentOk(tag, event_id, success, message)
    }
    fn eose(tag: RelayEventTag, sub_id: String) -> Self {
        Self::EndOfSubscription(tag, sub_id)
    }
    fn closed(tag: RelayEventTag, sub_id: String) -> Self {
        Self::ClosedSubscription(tag, sub_id)
    }
    fn notice(tag: RelayEventTag, message: String) -> Self {
        Self::Notice(tag, message)
    }
    fn auth(tag: RelayEventTag, challenge: String) -> Self {
        Self::Auth(tag, challenge)
    }
}

#[cfg(feature = "bourne")]
impl<'input> FromJson<'input> for NostrRelayEvent {
    fn from_lex(lex: &mut Lexer<'input>) -> Result<Self, BourneError> {
        <Self as RelayFrameParser>::from_lex(lex)
    }
}

#[cfg(feature = "bourne")]
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

#[cfg(feature = "bourne")]
impl std::str::FromStr for NostrRelayEvent {
    type Err = json_bourne::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        json_bourne::parse_str(value)
    }
}

#[cfg(feature = "serde")]
impl std::str::FromStr for NostrRelayEvent {
    type Err = serde_json::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(value)
    }
}

#[cfg(feature = "bourne")]
impl TryFrom<&[u8]> for NostrRelayEvent {
    type Error = json_bourne::Error;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        json_bourne::parse(value)
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for NostrRelayEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq as _;
        match self {
            Self::NewNote(tag, sub_id, note) => {
                let mut seq = serializer.serialize_seq(Some(3))?;
                seq.serialize_element(tag)?;
                seq.serialize_element(sub_id)?;
                seq.serialize_element(note)?;
                seq.end()
            }
            Self::SentOk(tag, event_id, success, message) => {
                let mut seq = serializer.serialize_seq(Some(4))?;
                seq.serialize_element(tag)?;
                seq.serialize_element(event_id)?;
                seq.serialize_element(success)?;
                seq.serialize_element(message)?;
                seq.end()
            }
            Self::EndOfSubscription(tag, sub_id)
            | Self::ClosedSubscription(tag, sub_id)
            | Self::Notice(tag, sub_id)
            | Self::Auth(tag, sub_id) => {
                let mut seq = serializer.serialize_seq(Some(2))?;
                seq.serialize_element(tag)?;
                seq.serialize_element(sub_id)?;
                seq.end()
            }
        }
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for NostrRelayEvent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(NostrRelayEventVisitor)
    }
}

/// Reads a relay frame straight off the sequence, in one pass.
///
/// Each element is deserialized directly into the field it belongs to, so
/// the note is built once. Collecting into `Vec<serde_json::Value>` first
/// and re-reading it with `from_value` builds every note twice: once as a
/// generic tree of maps and boxed strings, then again as the struct, with
/// the tree dropped immediately after. Measured over 500 frames that cost
/// 11 allocations per message against 1 for the equivalent hand-written
/// parser.
#[cfg(feature = "serde")]
struct NostrRelayEventVisitor;

#[cfg(feature = "serde")]
impl NostrRelayEventVisitor {
    /// Reads the next element, naming the field when it is absent.
    fn field<'de, A, T>(seq: &mut A, field: &'static str) -> Result<T, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
        T: serde::Deserialize<'de>,
    {
        use serde::de::Error as _;
        seq.next_element()?
            .ok_or_else(|| A::Error::custom(format_args!("missing field: {field}")))
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::de::Visitor<'de> for NostrRelayEventVisitor {
    type Value = NostrRelayEvent;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a nostr relay frame")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> Result<Self::Value, A::Error> {
        use serde::de::Error as _;

        let tag: RelayEventTag = seq
            .next_element()?
            .ok_or_else(|| A::Error::custom("empty relay frame"))?;

        let event = match tag {
            RelayEventTag::Event => {
                let sub_id = Self::field(&mut seq, "sub_id")?;
                let note = Self::field(&mut seq, "note")?;
                NostrRelayEvent::NewNote(tag, sub_id, note)
            }
            RelayEventTag::Ok => {
                let event_id = Self::field(&mut seq, "event_id")?;
                let success = Self::field(&mut seq, "success")?;
                let message = Self::field(&mut seq, "message")?;
                NostrRelayEvent::SentOk(tag, event_id, success, message)
            }
            RelayEventTag::Eose
            | RelayEventTag::Closed
            | RelayEventTag::Notice
            | RelayEventTag::Auth => {
                let val = Self::field(&mut seq, "value")?;
                match tag {
                    RelayEventTag::Eose => NostrRelayEvent::EndOfSubscription(tag, val),
                    RelayEventTag::Closed => NostrRelayEvent::ClosedSubscription(tag, val),
                    RelayEventTag::Notice => NostrRelayEvent::Notice(tag, val),
                    RelayEventTag::Auth => NostrRelayEvent::Auth(tag, val),
                    _ => unreachable!(),
                }
            }
            RelayEventTag::Close | RelayEventTag::Req => {
                return Err(A::Error::custom(format_args!(
                    "not a relay-to-client tag: {}",
                    tag.as_wire()
                )));
            }
        };

        if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
            return Err(A::Error::custom("trailing data in relay frame"));
        }
        Ok(event)
    }
}

#[cfg(feature = "serde")]
impl TryFrom<&[u8]> for NostrRelayEvent {
    type Error = serde_json::Error;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        serde_json::from_slice(value)
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
        use std::sync::OnceLock;
        use std::sync::atomic::{AtomicU64, Ordering};

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
        Self::Subscribe(
            RelayEventTag::Req,
            Self::fresh_sub_id(),
            subscription.clone(),
        )
    }
}

#[cfg(feature = "bourne")]
impl<'input> FromJson<'input> for NostrClientEvent {
    fn from_lex(lex: &mut Lexer<'input>) -> Result<Self, BourneError> {
        let tag = lex.parse_frame_tag()?;
        match tag {
            RelayEventTag::Event | RelayEventTag::Auth => {
                let note = crate::note::NostrNote::from_lex(lex)?;
                lex.expect_end()?;
                Ok(match tag {
                    RelayEventTag::Event => Self::SendNoteEvent(tag, note),
                    _ => Self::AuthEvent(tag, note),
                })
            }
            RelayEventTag::Req => {
                let sub_id = String::from_lex(lex)?;
                lex.expect_more()?;
                let filter = super::subscriptions::NostrSubscription::from_lex(lex)?;
                lex.expect_end()?;
                Ok(Self::Subscribe(tag, sub_id, filter))
            }
            RelayEventTag::Close => {
                let sub_id = String::from_lex(lex)?;
                lex.expect_end()?;
                Ok(Self::CloseSubscriptionEvent(tag, sub_id))
            }
            _ => Err(BourneError::new(
                BourneErrorKind::UnknownField,
                lex.position(),
            )),
        }
    }
}

#[cfg(feature = "bourne")]
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

#[cfg(feature = "serde")]
impl serde::Serialize for NostrClientEvent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq as _;
        match self {
            Self::SendNoteEvent(tag, note) | Self::AuthEvent(tag, note) => {
                let mut seq = serializer.serialize_seq(Some(2))?;
                seq.serialize_element(tag)?;
                seq.serialize_element(note)?;
                seq.end()
            }
            Self::Subscribe(tag, sub_id, filter) => {
                let mut seq = serializer.serialize_seq(Some(3))?;
                seq.serialize_element(tag)?;
                seq.serialize_element(sub_id)?;
                seq.serialize_element(filter)?;
                seq.end()
            }
            Self::CloseSubscriptionEvent(tag, sub_id) => {
                let mut seq = serializer.serialize_seq(Some(2))?;
                seq.serialize_element(tag)?;
                seq.serialize_element(sub_id)?;
                seq.end()
            }
        }
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for NostrClientEvent {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let elems = Vec::<serde_json::Value>::deserialize(deserializer)?;
        let mut iter = elems.into_iter();
        let tag_value = iter
            .next()
            .ok_or_else(|| D::Error::custom("empty client frame"))?;
        let tag: RelayEventTag = serde_json::from_value(tag_value).map_err(D::Error::custom)?;
        let from_val =
            |v: Option<serde_json::Value>, field: &str| -> Result<serde_json::Value, D::Error> {
                v.ok_or_else(|| D::Error::custom(format!("missing field: {field}")))
            };
        let mut rest_iter = iter;
        let event = match tag {
            RelayEventTag::Event | RelayEventTag::Auth => {
                let note: crate::note::NostrNote =
                    serde_json::from_value(from_val(rest_iter.next(), "note")?)
                        .map_err(D::Error::custom)?;
                match tag {
                    RelayEventTag::Event => Self::SendNoteEvent(tag, note),
                    _ => Self::AuthEvent(tag, note),
                }
            }
            RelayEventTag::Req => {
                let sub_id: String =
                    serde_json::from_value(from_val(rest_iter.next(), "sub_id")?)
                        .map_err(D::Error::custom)?;
                let filter: super::subscriptions::NostrSubscription =
                    serde_json::from_value(from_val(rest_iter.next(), "filter")?)
                        .map_err(D::Error::custom)?;
                Self::Subscribe(tag, sub_id, filter)
            }
            RelayEventTag::Close => {
                let sub_id: String =
                    serde_json::from_value(from_val(rest_iter.next(), "sub_id")?)
                        .map_err(D::Error::custom)?;
                Self::CloseSubscriptionEvent(tag, sub_id)
            }
            RelayEventTag::Ok
            | RelayEventTag::Eose
            | RelayEventTag::Closed
            | RelayEventTag::Notice => {
                return Err(D::Error::custom(format!(
                    "not a client-to-relay tag: {}",
                    tag.as_wire()
                )));
            }
        };
        if rest_iter.next().is_some() {
            return Err(D::Error::custom("trailing data in client frame"));
        }
        Ok(event)
    }
}

#[cfg(feature = "bourne")]
impl std::str::FromStr for NostrClientEvent {
    type Err = json_bourne::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        json_bourne::parse_str(value)
    }
}

#[cfg(feature = "serde")]
impl std::str::FromStr for NostrClientEvent {
    type Err = serde_json::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(value)
    }
}

#[cfg(feature = "bourne")]
impl TryFrom<&[u8]> for NostrClientEvent {
    type Error = json_bourne::Error;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        json_bourne::parse(value)
    }
}

#[cfg(feature = "serde")]
impl TryFrom<&[u8]> for NostrClientEvent {
    type Error = serde_json::Error;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        serde_json::from_slice(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::note::NostrNote;
    use crate::subscriptions::NostrSubscription;

    /// Counts allocations made by the calling thread while a gate is open.
    ///
    /// The tally is thread-local, not global: `cargo test` runs tests
    /// concurrently, and a process-wide counter picks up whatever the
    /// other threads happen to be doing. That noise is larger than the
    /// difference being measured here.
    struct CountingAllocator;

    thread_local! {
        static COUNTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        static ALLOCS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    }

    unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
            // `try_with` because a thread's locals are gone during its own
            // teardown, and allocation continues past that point.
            let _ = COUNTING.try_with(|counting| {
                if counting.get() {
                    let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
                }
            });
            unsafe { std::alloc::System.alloc(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: std::alloc::Layout) {
            unsafe { std::alloc::System.dealloc(ptr, layout) };
        }
    }

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    impl CountingAllocator {
        /// Returns how many allocations `body` made on this thread.
        #[cfg_attr(not(feature = "serde"), allow(dead_code))]
        fn count(body: impl FnOnce()) -> u64 {
            let before = ALLOCS.with(std::cell::Cell::get);
            COUNTING.with(|c| c.set(true));
            body();
            COUNTING.with(|c| c.set(false));
            ALLOCS.with(std::cell::Cell::get) - before
        }
    }

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

    #[cfg(feature = "bourne")]
    fn to_json_string<T: json_bourne::ToJson + ?Sized>(v: &T) -> String {
        json_bourne::to_string(v).unwrap()
    }
    #[cfg(feature = "serde")]
    fn to_json_string<T: serde::Serialize + ?Sized>(v: &T) -> String {
        serde_json::to_string(v).unwrap()
    }

    #[cfg(feature = "bourne")]
    fn from_json_str<T: for<'a> json_bourne::FromJson<'a>>(s: &str) -> Result<T, String> {
        json_bourne::parse_str(s).map_err(|e| e.to_string())
    }
    #[cfg(feature = "serde")]
    fn from_json_str<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, String> {
        serde_json::from_str(s).map_err(|e| e.to_string())
    }

    #[cfg(feature = "bourne")]
    fn round_trip<T: json_bourne::ToJson + for<'a> json_bourne::FromJson<'a> + std::fmt::Debug + PartialEq>(
        val: &T,
    ) {
        let json = to_json_string(val);
        let back: T = from_json_str(&json).unwrap();
        assert_eq!(val, &back);
    }
    #[cfg(feature = "serde")]
    fn round_trip<T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + PartialEq>(
        val: &T,
    ) {
        let json = to_json_string(val);
        let back: T = from_json_str(&json).unwrap();
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
        let json = to_json_string(&event);
        let parsed: NostrRelayEvent = json.parse().unwrap();
        assert_eq!(event, parsed);
    }

    #[test]
    fn client_event_from_str_round_trip() {
        let note = sample_note();
        let event = NostrClientEvent::SendNoteEvent(RelayEventTag::Event, note);
        let json = to_json_string(&event);
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
            let quoted = to_json_string(&tag);
            assert_eq!(quoted, format!("\"{expected_wire}\""));
            let parsed = RelayEventTag::from_str_wire(expected_wire).unwrap();
            assert_eq!(tag, parsed);
        }
        assert!(RelayEventTag::from_str_wire("UNKNOWN").is_none());
    }

    #[test]
    fn relay_event_rejects_empty_array() {
        assert!(from_json_str::<NostrRelayEvent>("[]").is_err());
    }

    #[test]
    fn relay_event_rejects_unknown_tag() {
        assert!(from_json_str::<NostrRelayEvent>(r#"["BOGUS","sub"]"#).is_err());
    }

    #[test]
    fn relay_event_rejects_tag_only() {
        assert!(from_json_str::<NostrRelayEvent>(r#"["EVENT"]"#).is_err());
    }

    #[test]
    fn relay_event_rejects_truncated_ok() {
        assert!(from_json_str::<NostrRelayEvent>(r#"["OK","eid"]"#).is_err());
        assert!(from_json_str::<NostrRelayEvent>(r#"["OK","eid",true]"#).is_err());
    }

    #[test]
    fn relay_event_rejects_trailing_data() {
        assert!(from_json_str::<NostrRelayEvent>(r#"["EOSE","sub","extra"]"#).is_err());
    }

    #[test]
    fn relay_event_rejects_not_array() {
        assert!(from_json_str::<NostrRelayEvent>(r#"{"tag":"EVENT"}"#).is_err());
    }

    /// A frame must be read in one pass, not staged through a generic
    /// tree.
    ///
    /// Collecting into `Vec<serde_json::Value>` and re-reading it with
    /// `from_value` parses every note twice and costs 11 allocations per
    /// frame instead of 1. It also type-checks and passes every other
    /// test here, so only a count catches it coming back.
    #[cfg(feature = "serde")]
    #[test]
    fn relay_event_parses_without_staging_through_a_value_tree() {
        let note = sample_note();
        let json = format!(r#"["EVENT","sub",{}]"#, to_json_string(&note));

        let bare = to_json_string(&note);
        let baseline = CountingAllocator::count(|| {
            drop(from_json_str::<crate::note::NostrNote>(&bare));
        });
        let framed = CountingAllocator::count(|| {
            drop(from_json_str::<NostrRelayEvent>(&json));
        });

        assert!(
            baseline > 0,
            "the counting allocator saw nothing, so it is not installed and this \
             test proves nothing"
        );

        assert_eq!(
            framed,
            baseline + 1,
            "framing a note should add exactly one allocation, the subscription id: \
             the frame cost {framed} against {baseline} for the bare note. Staging \
             through Vec<serde_json::Value> and re-reading with from_value parses \
             every note twice and lands around {} instead.",
            baseline * 2 + 6
        );
    }

    #[test]
    fn relay_event_rejects_event_missing_note() {
        assert!(from_json_str::<NostrRelayEvent>(r#"["EVENT","sub"]"#).is_err());
    }

    #[test]
    fn relay_event_rejects_event_trailing_data() {
        let note = sample_note();
        let note_json = to_json_string(&note);
        let json = format!(r#"["EVENT","sub",{note_json},"extra"]"#);
        assert!(from_json_str::<NostrRelayEvent>(&json).is_err());
    }

    #[test]
    fn relay_event_rejects_ok_trailing_data() {
        assert!(from_json_str::<NostrRelayEvent>(r#"["OK","eid",true,"msg","extra"]"#).is_err());
    }

    #[test]
    fn relay_event_rejects_ok_missing_bool() {
        assert!(from_json_str::<NostrRelayEvent>(r#"["OK","eid"]"#).is_err());
    }

    #[test]
    fn relay_event_rejects_ok_missing_message() {
        assert!(from_json_str::<NostrRelayEvent>(r#"["OK","eid",true]"#).is_err());
    }

    #[test]
    fn relay_event_rejects_client_only_tags() {
        assert!(from_json_str::<NostrRelayEvent>(r#"["REQ","sub",{}]"#).is_err());
        assert!(from_json_str::<NostrRelayEvent>(r#"["CLOSE","sub"]"#).is_err());
    }

    #[test]
    fn client_event_rejects_empty_array() {
        assert!(from_json_str::<NostrClientEvent>("[]").is_err());
    }

    #[test]
    fn client_event_rejects_unknown_tag() {
        assert!(from_json_str::<NostrClientEvent>(r#"["BOGUS","sub"]"#).is_err());
    }

    #[test]
    fn client_event_rejects_server_only_tags() {
        assert!(from_json_str::<NostrClientEvent>(r#"["EOSE","sub"]"#).is_err());
        assert!(from_json_str::<NostrClientEvent>(r#"["OK","eid",true,""]"#).is_err());
        assert!(from_json_str::<NostrClientEvent>(r#"["NOTICE","msg"]"#).is_err());
    }

    #[test]
    fn client_event_rejects_truncated_event() {
        assert!(from_json_str::<NostrClientEvent>(r#"["EVENT"]"#).is_err());
    }

    #[test]
    fn client_event_rejects_truncated_req() {
        assert!(from_json_str::<NostrClientEvent>(r#"["REQ"]"#).is_err());
        assert!(from_json_str::<NostrClientEvent>(r#"["REQ","sub"]"#).is_err());
    }

    #[test]
    fn client_event_rejects_truncated_close() {
        assert!(from_json_str::<NostrClientEvent>(r#"["CLOSE"]"#).is_err());
    }

    #[test]
    fn client_event_rejects_truncated_auth() {
        assert!(from_json_str::<NostrClientEvent>(r#"["AUTH"]"#).is_err());
    }

    #[test]
    fn client_event_rejects_event_trailing_data() {
        let note = sample_note();
        let note_json = to_json_string(&note);
        let json = format!(r#"["EVENT",{note_json},"extra"]"#);
        assert!(from_json_str::<NostrClientEvent>(&json).is_err());
    }

    #[test]
    fn client_event_rejects_auth_trailing_data() {
        let note = sample_note();
        let note_json = to_json_string(&note);
        let json = format!(r#"["AUTH",{note_json},"extra"]"#);
        assert!(from_json_str::<NostrClientEvent>(&json).is_err());
    }

    #[test]
    fn client_event_rejects_close_trailing_data() {
        assert!(from_json_str::<NostrClientEvent>(r#"["CLOSE","sub","extra"]"#).is_err());
    }

    #[test]
    fn client_event_rejects_req_trailing_data() {
        assert!(from_json_str::<NostrClientEvent>(r#"["REQ","sub",{},"extra"]"#).is_err());
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
