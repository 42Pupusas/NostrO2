//! NIP-01 client → relay message parsing and relay → client message encoding.
//!
//! Wire format is an untagged JSON array. Parsing accepts the small set of
//! verbs a NIP-01 relay must handle: `EVENT`, `REQ`, `CLOSE`. Anything else
//! comes back as [`ClientMessage::Unknown`] so the dispatcher can NOTICE it.

use nostro2::{NostrNote, NostrSubscription};
use serde_json::Value;

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
    let value: Value = serde_json::from_str(text).map_err(|_| ParseError::NotJson)?;
    let arr = value.as_array().ok_or(ParseError::NotArray)?;
    let tag = arr
        .first()
        .and_then(Value::as_str)
        .ok_or(ParseError::BadTag)?;

    match tag {
        "EVENT" => {
            let payload = arr.get(1).ok_or(ParseError::BadPayload("EVENT missing note"))?;
            let note: NostrNote = serde_json::from_value(payload.clone())
                .map_err(|_| ParseError::BadPayload("EVENT note"))?;
            Ok(ClientMessage::Event(note))
        }
        "REQ" => {
            let sub_id = arr
                .get(1)
                .and_then(Value::as_str)
                .ok_or(ParseError::BadPayload("REQ missing sub id"))?
                .to_string();
            let mut filters = Vec::with_capacity(arr.len().saturating_sub(2));
            for raw in arr.iter().skip(2) {
                let filter: NostrSubscription = serde_json::from_value(raw.clone())
                    .map_err(|_| ParseError::BadPayload("REQ filter"))?;
                filters.push(filter);
            }
            Ok(ClientMessage::Req { sub_id, filters })
        }
        "CLOSE" => {
            let sub_id = arr
                .get(1)
                .and_then(Value::as_str)
                .ok_or(ParseError::BadPayload("CLOSE missing sub id"))?
                .to_string();
            Ok(ClientMessage::Close { sub_id })
        }
        other => Ok(ClientMessage::Unknown(other.to_string())),
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

/// Encode `["EVENT", sub_id, note]` for fan-out.
#[must_use]
pub fn event(sub_id: &str, note: &NostrNote) -> String {
    serde_json::to_string(&("EVENT", sub_id, note)).expect("EVENT serialization cannot fail")
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
