//! NIP-01 client → relay message parsing and relay → client message encoding.
//!
//! Wire format is an untagged JSON array. Parsing accepts the small set of
//! verbs a NIP-01 relay must handle: `EVENT`, `REQ`, `CLOSE`. Anything else
//! comes back as [`ClientMessage::Unknown`] so the dispatcher can NOTICE it.

use nostro2::{NostrNote, NostrSubscription};
use serde::Deserialize;
use serde::de::{self, Deserializer, SeqAccess, Visitor};
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
}
