//! NIP-01 client → relay message parsing and relay → client message encoding.
//!
//! Wire format is an untagged JSON array. Parsing accepts the small set of
//! verbs a NIP-01 relay must handle: `EVENT`, `REQ`, `CLOSE`. Anything else
//! comes back as [`ClientMessage::Unknown`] so the dispatcher can NOTICE it.

use nostro2::{NostrNote, NostrNoteView, NostrSubscription};
use serde::Deserialize;
use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde_json::value::RawValue;
use std::fmt;

/// A parsed client → relay message.
#[derive(Debug, Clone)]
pub enum ClientMessage {
    Event(NostrNote),
    Req {
        sub_id: String,
        filters: Vec<NostrSubscription>,
    },
    Close {
        sub_id: String,
    },
    /// Well-formed JSON array but unknown verb — worth a NOTICE.
    Unknown(String),
}

/// Parsed client → relay message with borrowed payloads.
///
/// The `EVENT` variant carries both a [`NostrNoteView`] and the original
/// raw JSON substring of the note object. The view is used for verify +
/// filter matching; the raw JSON is spliced directly into outbound
/// `["EVENT", sub_id, <note>]` fan-out frames, skipping the reserialize
/// pass entirely. Both borrow from the input frame buffer, which must
/// outlive every read.
///
/// The `Req` sub_id and unknown-verb strings borrow too — those are short
/// strings already present in the wire frame, no reason to allocate.
/// Subscription filters are parsed to owned [`NostrSubscription`]s for now
/// because they outlive the parse call (the shard dispatcher stores them).
/// Revisit when we have a subscription view type.
#[derive(Debug)]
pub enum ClientMessageView<'a> {
    Event {
        note: NostrNoteView<'a>,
        raw: &'a RawValue,
    },
    Req {
        sub_id: &'a str,
        filters: Vec<NostrSubscription>,
    },
    Close {
        sub_id: &'a str,
    },
    Unknown(&'a str),
}

/// Reason a message could not be parsed.
#[derive(Debug)]
pub enum ParseError {
    NotJson,
    NotArray,
    Empty,
    BadTag,
    BadPayload(&'static str),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson => write!(f, "message is not valid JSON"),
            Self::NotArray => write!(f, "message is not a JSON array"),
            Self::Empty => write!(f, "empty message array"),
            Self::BadTag => write!(f, "first element is not a string tag"),
            Self::BadPayload(s) => write!(f, "malformed payload: {s}"),
        }
    }
}

/// Parse a single client → relay frame.
///
/// # Errors
/// Returns [`ParseError`] for malformed JSON or known verbs with bad payloads.
/// Unknown verbs are returned as [`ClientMessage::Unknown`], not an error.
pub fn parse(text: &str) -> Result<ClientMessage, ParseError> {
    serde_json::from_str::<ClientMessage>(text).map_err(ParseError::from_serde)
}

/// Parse a client → relay frame into a borrowed view.
///
/// The returned [`ClientMessageView`] references slices of `text`; the
/// caller must ensure `text` outlives every read of the view. In the
/// shard dispatcher this is automatic because everything downstream of
/// `on_text` runs synchronously inside the reader callback.
///
/// # Errors
/// Same error taxonomy as [`parse`].
pub fn parse_view(text: &str) -> Result<ClientMessageView<'_>, ParseError> {
    serde_json::from_str::<ClientMessageView<'_>>(text).map_err(ParseError::from_serde)
}

impl ParseError {
    fn from_serde(err: serde_json::Error) -> Self {
        // The visitor surfaces specific failure kinds via custom error messages;
        // generic JSON errors fall back to NotJson / NotArray based on category.
        use serde_json::error::Category;
        match err.classify() {
            Category::Eof | Category::Syntax => Self::NotJson,
            Category::Io => Self::NotJson,
            Category::Data => {
                let msg = err.to_string();
                if msg.starts_with("NotArray") {
                    Self::NotArray
                } else if msg.starts_with("Empty") {
                    Self::Empty
                } else if msg.starts_with("BadTag") {
                    Self::BadTag
                } else if let Some(rest) = msg.strip_prefix("BadPayload:") {
                    // Map the embedded reason back to a &'static str; the set
                    // below mirrors every literal the visitor emits.
                    let reason: &'static str = match rest.trim() {
                        "EVENT missing note" => "EVENT missing note",
                        "EVENT note" => "EVENT note",
                        "REQ missing sub id" => "REQ missing sub id",
                        "REQ filter" => "REQ filter",
                        "CLOSE missing sub id" => "CLOSE missing sub id",
                        _ => "unknown",
                    };
                    Self::BadPayload(reason)
                } else {
                    Self::NotArray
                }
            }
        }
    }
}

impl<'de> Deserialize<'de> for ClientMessage {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_seq(ClientMessageVisitor)
    }
}

/// Visits a NIP-01 client frame in array form. Reads the verb tag first, then
/// dispatches to the verb-specific shape. Avoids the `serde_json::Value`
/// round-trip the older `parse` implementation used.
///
/// Error messages are prefixed (`NotArray`, `BadTag`, `BadPayload:<reason>`)
/// so `ParseError::from_serde` can recover the typed variant from the string
/// serde_json hands back. Not beautiful, but keeps the public error type
/// unchanged.
struct ClientMessageVisitor;

impl<'de> Visitor<'de> for ClientMessageVisitor {
    type Value = ClientMessage;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a NIP-01 client frame")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        // Try to read the first element as a string; anything else (missing,
        // non-string) is reported as BadTag to match the old parser's
        // behavior. The old parser treated `[]` and `[123]` identically
        // because it used `Value::as_str` + `ok_or(BadTag)`.
        let tag: String = match seq.next_element::<String>() {
            Ok(Some(t)) => t,
            Ok(None) | Err(_) => return Err(de::Error::custom("BadTag")),
        };

        match tag.as_str() {
            "EVENT" => {
                let note: NostrNote = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::custom("BadPayload: EVENT missing note"))?;
                // Ensure no trailing elements (tolerant: ignore extras).
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(ClientMessage::Event(note))
            }
            "REQ" => {
                let sub_id: String = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::custom("BadPayload: REQ missing sub id"))?;
                let mut filters = Vec::new();
                while let Some(filter) = seq.next_element::<NostrSubscription>()? {
                    filters.push(filter);
                }
                Ok(ClientMessage::Req { sub_id, filters })
            }
            "CLOSE" => {
                let sub_id: String = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::custom("BadPayload: CLOSE missing sub id"))?;
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(ClientMessage::Close { sub_id })
            }
            _ => {
                // Unknown verb: drain remaining elements so the full frame is
                // consumed, then return Unknown.
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(ClientMessage::Unknown(tag))
            }
        }
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for ClientMessageView<'a> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_seq(ClientMessageViewVisitor(std::marker::PhantomData))
    }
}

/// Borrowed counterpart to [`ClientMessageVisitor`]. Same error taxonomy so
/// `ParseError::from_serde` can recover the same variants.
struct ClientMessageViewVisitor<'a>(std::marker::PhantomData<&'a ()>);

impl<'de: 'a, 'a> Visitor<'de> for ClientMessageViewVisitor<'a> {
    type Value = ClientMessageView<'a>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a NIP-01 client frame")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let tag: &'a str = match seq.next_element::<&'a str>() {
            Ok(Some(t)) => t,
            Ok(None) | Err(_) => return Err(de::Error::custom("BadTag")),
        };

        match tag {
            "EVENT" => {
                // Capture the raw JSON slice of the note first — that's
                // the contiguous byte range we'll splice into fan-out
                // frames. Then parse a view *from that same slice*, so
                // verify + filter work against the structured form.
                let raw: &'a RawValue = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::custom("BadPayload: EVENT missing note"))?;
                let note: NostrNoteView<'a> = serde_json::from_str(raw.get())
                    .map_err(|_| de::Error::custom("BadPayload: EVENT note"))?;
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(ClientMessageView::Event { note, raw })
            }
            "REQ" => {
                let sub_id: &'a str = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::custom("BadPayload: REQ missing sub id"))?;
                let mut filters = Vec::new();
                while let Some(filter) = seq.next_element::<NostrSubscription>()? {
                    filters.push(filter);
                }
                Ok(ClientMessageView::Req { sub_id, filters })
            }
            "CLOSE" => {
                let sub_id: &'a str = seq
                    .next_element()?
                    .ok_or_else(|| de::Error::custom("BadPayload: CLOSE missing sub id"))?;
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(ClientMessageView::Close { sub_id })
            }
            other => {
                while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
                Ok(ClientMessageView::Unknown(other))
            }
        }
    }
}

/// Encode `["OK", event_id, accepted, message]` per NIP-20 / NIP-01.
///
/// `message` convention: prefix with `"duplicate: "`, `"invalid: "`,
/// `"blocked: "`, `"rate-limited: "`, or `"error: "`.
#[must_use]
pub fn ok(event_id: &str, accepted: bool, message: &str) -> String {
    serde_json::to_string(&("OK", event_id, accepted, message))
        .expect("OK serialization cannot fail")
}

/// Encode `["EOSE", sub_id]`.
#[must_use]
pub fn eose(sub_id: &str) -> String {
    serde_json::to_string(&("EOSE", sub_id)).expect("EOSE serialization cannot fail")
}

/// Encode `["CLOSED", sub_id, message]`.
#[must_use]
pub fn closed(sub_id: &str, message: &str) -> String {
    serde_json::to_string(&("CLOSED", sub_id, message))
        .expect("CLOSED serialization cannot fail")
}

/// Encode `["NOTICE", message]`.
#[must_use]
pub fn notice(message: &str) -> String {
    serde_json::to_string(&("NOTICE", message)).expect("NOTICE serialization cannot fail")
}

/// Serialize a note to its JSON object form for reuse across a fan-out pass.
///
/// Pair with [`event_from_serialized`]: serialize once per event, then build
/// each subscriber's frame cheaply by splicing the JSON. That's ~N times
/// faster than re-serializing the whole note per matching subscriber.
#[must_use]
pub fn serialize_note(note: &NostrNote) -> String {
    serde_json::to_string(note).expect("note serialization cannot fail")
}

/// View-based counterpart to [`serialize_note`]. Emits the note as the same
/// canonical JSON object the owned path would, walking the flat tag index
/// directly — no intermediate owned `NostrNote`.
#[must_use]
pub fn serialize_note_view(note: &NostrNoteView<'_>) -> String {
    use serde::ser::{SerializeMap, SerializeSeq};

    struct TagsSer<'b, 'a>(&'b nostro2::TagsView<'a>);
    impl serde::Serialize for TagsSer<'_, '_> {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            let mut seq = s.serialize_seq(Some(self.0.len()))?;
            for row in self.0.iter() {
                seq.serialize_element(row)?;
            }
            seq.end()
        }
    }

    struct NoteSer<'b, 'a>(&'b NostrNoteView<'a>);
    impl serde::Serialize for NoteSer<'_, '_> {
        fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            // Field order and `skip_serializing_if = Option::is_none` mirror
            // the owned `NostrNote` derive, so downstream clients see the
            // same shape on the wire.
            let n = &self.0;
            let len = 5 + usize::from(n.id.is_some()) + usize::from(n.sig.is_some());
            let mut map = s.serialize_map(Some(len))?;
            map.serialize_entry("pubkey", n.pubkey.as_ref())?;
            map.serialize_entry("created_at", &n.created_at)?;
            map.serialize_entry("kind", &n.kind)?;
            map.serialize_entry("tags", &TagsSer(&n.tags))?;
            map.serialize_entry("content", n.content.as_ref())?;
            if let Some(id) = n.id.as_deref() {
                map.serialize_entry("id", id)?;
            }
            if let Some(sig) = n.sig.as_deref() {
                map.serialize_entry("sig", sig)?;
            }
            map.end()
        }
    }

    serde_json::to_string(&NoteSer(note)).expect("note serialization cannot fail")
}

/// Encode `["EVENT", sub_id, <note_json>]` where `note_json` is the already-
/// serialized JSON object form of a `NostrNote` (typically from
/// [`serialize_note`]).
#[must_use]
pub fn event_from_serialized(sub_id: &str, note_json: &str) -> String {
    let sub_id_json = serde_json::to_string(sub_id).expect("sub_id serialization cannot fail");
    // `["EVENT",` + sub_id JSON + `,` + note JSON + `]`
    let mut out = String::with_capacity(10 + sub_id_json.len() + note_json.len());
    out.push_str("[\"EVENT\",");
    out.push_str(&sub_id_json);
    out.push(',');
    out.push_str(note_json);
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_event() {
        // Use a syntactically well-formed note — parse only, no signature check here.
        let note_json = r#"{"pubkey":"a","created_at":1,"kind":1,"tags":[],"content":"hi","id":"b","sig":"c"}"#;
        let msg = format!(r#"["EVENT",{note_json}]"#);
        match parse(&msg).unwrap() {
            ClientMessage::Event(note) => assert_eq!(note.content, "hi"),
            other => panic!("expected Event, got {other:?}"),
        }
    }

    #[test]
    fn parse_req_single_filter() {
        let msg = r#"["REQ","sub1",{"kinds":[1],"limit":10}]"#;
        match parse(msg).unwrap() {
            ClientMessage::Req { sub_id, filters } => {
                assert_eq!(sub_id, "sub1");
                assert_eq!(filters.len(), 1);
                assert_eq!(filters[0].kinds, Some(vec![1]));
            }
            other => panic!("expected Req, got {other:?}"),
        }
    }

    #[test]
    fn parse_req_multiple_filters() {
        let msg = r#"["REQ","s",{"kinds":[1]},{"kinds":[7]}]"#;
        match parse(msg).unwrap() {
            ClientMessage::Req { filters, .. } => assert_eq!(filters.len(), 2),
            _ => panic!("expected Req"),
        }
    }

    #[test]
    fn parse_close() {
        match parse(r#"["CLOSE","sub1"]"#).unwrap() {
            ClientMessage::Close { sub_id } => assert_eq!(sub_id, "sub1"),
            _ => panic!("expected Close"),
        }
    }

    #[test]
    fn parse_unknown_verb() {
        match parse(r#"["AUTH","challenge"]"#).unwrap() {
            ClientMessage::Unknown(verb) => assert_eq!(verb, "AUTH"),
            _ => panic!("expected Unknown"),
        }
    }

    #[test]
    fn parse_errors() {
        assert!(matches!(parse("not-json"), Err(ParseError::NotJson)));
        assert!(matches!(parse("{}"), Err(ParseError::NotArray)));
        assert!(matches!(parse("[]"), Err(ParseError::BadTag)));
        assert!(matches!(parse(r#"[123]"#), Err(ParseError::BadTag)));
        assert!(matches!(
            parse(r#"["EVENT"]"#),
            Err(ParseError::BadPayload(_))
        ));
        assert!(matches!(
            parse(r#"["REQ"]"#),
            Err(ParseError::BadPayload(_))
        ));
    }

    #[test]
    fn encode_ok() {
        let s = ok("deadbeef", true, "");
        assert_eq!(s, r#"["OK","deadbeef",true,""]"#);
    }

    #[test]
    fn encode_eose() {
        assert_eq!(eose("s1"), r#"["EOSE","s1"]"#);
    }

    #[test]
    fn encode_closed() {
        assert_eq!(
            closed("s1", "rate-limited: fifo"),
            r#"["CLOSED","s1","rate-limited: fifo"]"#
        );
    }

    #[test]
    fn parse_view_event() {
        let note_json = r#"{"pubkey":"a","created_at":1,"kind":1,"tags":[["t","x"]],"content":"hi","id":"b","sig":"c"}"#;
        let msg = format!(r#"["EVENT",{note_json}]"#);
        match parse_view(&msg).unwrap() {
            ClientMessageView::Event { note, raw } => {
                assert_eq!(note.content.as_ref(), "hi");
                assert_eq!(note.id.as_deref(), Some("b"));
                assert_eq!(note.tags.len(), 1);
                // raw must preserve the original note JSON byte-for-byte.
                assert_eq!(raw.get(), note_json);
            }
            other => panic!("expected Event, got {other:?}"),
        }
    }

    #[test]
    fn parse_view_req_close_unknown() {
        match parse_view(r#"["REQ","s1",{"kinds":[1]}]"#).unwrap() {
            ClientMessageView::Req { sub_id, filters } => {
                assert_eq!(sub_id, "s1");
                assert_eq!(filters.len(), 1);
            }
            other => panic!("expected Req, got {other:?}"),
        }
        match parse_view(r#"["CLOSE","s2"]"#).unwrap() {
            ClientMessageView::Close { sub_id } => assert_eq!(sub_id, "s2"),
            _ => panic!("expected Close"),
        }
        match parse_view(r#"["AUTH","c"]"#).unwrap() {
            ClientMessageView::Unknown(v) => assert_eq!(v, "AUTH"),
            _ => panic!("expected Unknown"),
        }
    }

    #[test]
    fn serialize_note_view_matches_owned() {
        use nostro2::NostrNote;
        let mut note = NostrNote {
            pubkey: "a".repeat(64),
            created_at: 1_700_000_000,
            kind: 1,
            content: "hello \"world\"".into(),
            id: Some("b".repeat(64)),
            sig: Some("c".repeat(128)),
            ..Default::default()
        };
        note.tags.add_custom_tag("t", "nostr");
        note.tags.add_pubkey_tag(&"d".repeat(64), Some("wss://example"));

        let owned_json = serialize_note(&note);
        let view: nostro2::NostrNoteView<'_> = serde_json::from_str(&owned_json).unwrap();
        let view_json = serialize_note_view(&view);

        assert_eq!(owned_json, view_json);
    }
}
