//! NIP-11 relay information document.
//!
//! Served on `GET /` when the request carries `Accept: application/nostr+json`.
//! All fields default to empty / absent so operators can fill only what applies.

use serde::{Deserialize, Serialize};

/// Declarative per-operation limits the relay advertises to clients.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Limitation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_message_length: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_subscriptions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_filters: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_subid_length: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_event_tags: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_content_length: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_pow_difficulty: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment_required: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at_lower_limit: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at_upper_limit: Option<i64>,
}

/// NIP-11 relay information document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RelayInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 32-byte hex pubkey of the relay operator.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pubkey: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<String>,
    /// NIPs this relay implements (e.g. `1` for NIP-01, `11` for NIP-11).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub supported_nips: Vec<u32>,
    /// Software URL (e.g. `https://github.com/42Pupusas/NostrO2`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub software: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limitation: Option<Limitation>,
    /// Optional Lightning URL for paid features (NIP-11 extension).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payments_url: Option<String>,
    /// Optional list of tags the relay makes available (e.g. `#t` values).
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub posting_policy: Option<String>,
}

impl RelayInfo {
    /// Build a minimal info document that just advertises NIP-01 and NIP-11
    /// support — a reasonable default for this relay implementation.
    #[must_use]
    pub fn minimal() -> Self {
        Self {
            software: Some("https://github.com/42Pupusas/NostrO2".into()),
            version: Some(env!("CARGO_PKG_VERSION").into()),
            // 1: NIP-01 core. 9: deletion via kind-5 (storage mode).
            // 11: this info doc. 13: optional proof-of-work via
            // `min_pow_difficulty`. 40: expiration tag is honored on
            // ingest and skipped on REQ replay.
            supported_nips: vec![1, 9, 11, 13, 40],
            ..Self::default()
        }
    }

    /// Replace this info's `limitation` block with values reflecting what the
    /// relay actually enforces. Pass `None` for a field to leave it
    /// unadvertised (relay has no limit on that axis).
    #[must_use]
    pub fn with_limits(mut self, limits: Limitation) -> Self {
        self.limitation = Some(limits);
        self
    }
}

/// Render `info` as a complete HTTP/1.1 200 response (status line + headers + body).
#[must_use]
pub fn http_response(info: &RelayInfo) -> Vec<u8> {
    let body = serde_json::to_vec(info).expect("serialize RelayInfo");
    let header = format!(
        "HTTP/1.1 200 OK\r\n\
         Content-Type: application/nostr+json\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let mut out = header.into_bytes();
    out.extend_from_slice(&body);
    out
}

/// Build a 404 HTTP response for non-NIP-11 requests we don't handle.
#[must_use]
pub fn not_found() -> Vec<u8> {
    let body = b"404 Not Found\r\n";
    let header = format!(
        "HTTP/1.1 404 Not Found\r\n\
         Content-Type: text/plain\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let mut out = header.into_bytes();
    out.extend_from_slice(body);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_serialises_compactly() {
        let info = RelayInfo::minimal();
        let json = serde_json::to_value(&info).unwrap();
        assert_eq!(json["supported_nips"], serde_json::json!([1, 9, 11, 13, 40]));
        assert!(json.get("name").is_none());
    }

    #[test]
    fn http_response_includes_headers() {
        let resp = http_response(&RelayInfo::minimal());
        let as_str = String::from_utf8(resp).unwrap();
        assert!(as_str.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(as_str.contains("Content-Type: application/nostr+json"));
        assert!(as_str.contains("\r\n\r\n{"));
    }
}
