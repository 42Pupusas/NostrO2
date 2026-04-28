//! NIP-01 filter matching against `NostrNote`.

use nostro2::{NostrNote, NostrNoteView, NostrSubscription};

use crate::verify_pool::MatchView;

/// NIP-40 expiration tag scan.
///
/// Returns the unix timestamp from the first well-formed
/// `["expiration", "<seconds>"]` tag, or `None` if absent or malformed.
/// The spec allows multiple expiration tags but doesn't define merge
/// semantics — first-wins matches what most relays in the wild do.
///
/// # Examples
/// ```
/// # use nostro2::NostrNote;
/// # use ring_relay_nostr::expiration_from_note;
/// let mut n = NostrNote::default();
/// n.tags.add_custom_tag("expiration", "1700000000");
/// assert_eq!(expiration_from_note(&n), Some(1_700_000_000));
/// ```
#[must_use]
pub fn expiration_from_note(note: &NostrNote) -> Option<i64> {
    for tag in note.tags.iter() {
        if tag.first().map(String::as_str) == Some("expiration")
            && let Some(v) = tag.get(1)
            && let Ok(ts) = v.parse::<i64>()
        {
            return Some(ts);
        }
    }
    None
}

/// View counterpart to [`expiration_from_note`].
#[must_use]
pub fn expiration_from_view(note: &NostrNoteView<'_>) -> Option<i64> {
    for tag in note.tags.iter() {
        if tag.first().map(AsRef::as_ref) == Some("expiration")
            && let Some(v) = tag.get(1).map(AsRef::as_ref)
            && let Ok(ts) = v.parse::<i64>()
        {
            return Some(ts);
        }
    }
    None
}

/// NIP-09 deletion target reference. A kind-5 event MAY reference targets
/// either by event id (`e` tag) or by address (`a` tag). Addresses cover
/// replaceable (NIP-16) and parameterized (NIP-33) kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeletionRef {
    /// Hex-decoded event id (32 bytes).
    EventId([u8; 32]),
    /// Address tuple `(kind, pubkey, d_tag)`. `d_tag` is `""` for plain
    /// replaceable; non-empty for parameterized.
    Address {
        kind: u32,
        pubkey: [u8; 32],
        d_tag: Box<str>,
    },
}

/// Extract NIP-09 deletion targets from a kind-5 event view.
///
/// Walks `e` and `a` tags, decoding `e` values as 64-char hex and parsing
/// `a` values as `"kind:pubkey:d_tag"`. Malformed entries are silently
/// skipped — the spec doesn't define error semantics, and a single bad
/// tag shouldn't void the whole deletion. The caller is responsible for
/// the same-pubkey ownership check; this is just a parser.
#[must_use]
pub fn deletion_refs_from_view(note: &NostrNoteView<'_>) -> Vec<DeletionRef> {
    let mut out = Vec::new();
    for tag in note.tags.iter() {
        let Some(name) = tag.first().map(AsRef::as_ref) else {
            continue;
        };
        let Some(value) = tag.get(1).map(AsRef::as_ref) else {
            continue;
        };
        match name {
            "e" => {
                if let Some(id) = decode_hex32_str(value) {
                    out.push(DeletionRef::EventId(id));
                }
            }
            "a" => {
                if let Some(addr) = parse_address(value) {
                    out.push(addr);
                }
            }
            _ => {}
        }
    }
    out
}

fn decode_hex32_str(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        let hi = hex_nibble(s.as_bytes()[i * 2])?;
        let lo = hex_nibble(s.as_bytes()[i * 2 + 1])?;
        *byte = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_address(s: &str) -> Option<DeletionRef> {
    // Format: "<kind>:<64-hex pubkey>:<d_tag>"
    let mut parts = s.splitn(3, ':');
    let kind: u32 = parts.next()?.parse().ok()?;
    let pubkey = decode_hex32_str(parts.next()?)?;
    let d_tag = parts.next()?.to_owned().into_boxed_str();
    Some(DeletionRef::Address {
        kind,
        pubkey,
        d_tag,
    })
}

/// Check whether `note` matches `filter` per NIP-01 semantics.
///
/// All supplied fields are ANDed. Within a list field (authors, ids, kinds),
/// any match counts. Tag filters (`#p`, `#e`, ...) require at least one tag on
/// the note to match any value in the filter list; multiple tag filters are ANDed.
#[must_use]
pub fn matches(note: &NostrNote, filter: &NostrSubscription) -> bool {
    if let Some(ids) = &filter.ids {
        let Some(id) = note.id.as_deref() else {
            return false;
        };
        if !ids.iter().any(|i| id.starts_with(i) || i == id) {
            return false;
        }
    }

    if let Some(authors) = &filter.authors
        && !authors
            .iter()
            .any(|a| note.pubkey.starts_with(a) || a == &note.pubkey)
    {
        return false;
    }

    if let Some(kinds) = &filter.kinds
        && !kinds.contains(&note.kind)
    {
        return false;
    }

    if let Some(since) = filter.since
        && (note.created_at as i128) < (since as i128)
    {
        return false;
    }

    if let Some(until) = filter.until
        && (note.created_at as i128) > (until as i128)
    {
        return false;
    }

    if let Some(tag_filters) = &filter.tags {
        for (key, values) in tag_filters {
            // Tag keys in filters are like "#p", "#e", "#t" — strip the '#'.
            let Some(tag_name) = key.strip_prefix('#') else {
                // Non-tag key slipped in — ignore per spec (unknown fields).
                continue;
            };
            let mut matched = false;
            for tag in note.tags.iter() {
                if tag.first().map(String::as_str) == Some(tag_name)
                    && let Some(val) = tag.get(1)
                    && values.iter().any(|v| v == val)
                {
                    matched = true;
                    break;
                }
            }
            if !matched {
                return false;
            }
        }
    }

    true
}

/// Borrowed-input counterpart to [`matches`]. Same NIP-01 semantics — all
/// supplied filter fields are ANDed, list fields are ORed internally, tag
/// filters require a per-key match on the note. Works off the zero-copy
/// [`NostrNoteView`] so the relay hot path never needs an owned
/// [`NostrNote`].
#[must_use]
pub fn matches_view(note: &NostrNoteView<'_>, filter: &NostrSubscription) -> bool {
    if let Some(ids) = &filter.ids {
        let Some(id) = note.id.as_deref() else {
            return false;
        };
        if !ids.iter().any(|i| id.starts_with(i.as_str()) || i == id) {
            return false;
        }
    }

    let pubkey = note.pubkey.as_ref();
    if let Some(authors) = &filter.authors
        && !authors
            .iter()
            .any(|a| pubkey.starts_with(a.as_str()) || a == pubkey)
    {
        return false;
    }

    if let Some(kinds) = &filter.kinds
        && !kinds.contains(&note.kind)
    {
        return false;
    }

    if let Some(since) = filter.since
        && (note.created_at as i128) < (since as i128)
    {
        return false;
    }

    if let Some(until) = filter.until
        && (note.created_at as i128) > (until as i128)
    {
        return false;
    }

    if let Some(tag_filters) = &filter.tags {
        for (key, values) in tag_filters {
            let Some(tag_name) = key.strip_prefix('#') else {
                continue;
            };
            let mut matched = false;
            for tag in note.tags.iter() {
                if tag.first().map(AsRef::as_ref) == Some(tag_name)
                    && let Some(val) = tag.get(1).map(AsRef::as_ref)
                    && values.iter().any(|v| v == val)
                {
                    matched = true;
                    break;
                }
            }
            if !matched {
                return false;
            }
        }
    }

    true
}

/// Owned-input counterpart to [`matches_view`]. Matches the same NIP-01
/// semantics against the verify worker's pre-built [`MatchView`], so the
/// shard's fan-out path doesn't have to re-parse the event JSON.
#[must_use]
pub fn matches_match_view(view: &MatchView, filter: &NostrSubscription) -> bool {
    if let Some(ids) = &filter.ids {
        let id = view.id.as_ref();
        if !ids.iter().any(|i| id.starts_with(i.as_str()) || i == id) {
            return false;
        }
    }

    let pubkey = view.pubkey.as_ref();
    if let Some(authors) = &filter.authors
        && !authors
            .iter()
            .any(|a| pubkey.starts_with(a.as_str()) || a == pubkey)
    {
        return false;
    }

    if let Some(kinds) = &filter.kinds
        && !kinds.contains(&view.kind)
    {
        return false;
    }

    if let Some(since) = filter.since
        && (view.created_at as i128) < (since as i128)
    {
        return false;
    }

    if let Some(until) = filter.until
        && (view.created_at as i128) > (until as i128)
    {
        return false;
    }

    if let Some(tag_filters) = &filter.tags {
        for (key, values) in tag_filters {
            let Some(tag_name) = key.strip_prefix('#') else {
                continue;
            };
            let mut matched = false;
            for tag in view.iter_tags() {
                if tag.first().map(|s| s.as_ref()) == Some(tag_name)
                    && let Some(val) = tag.get(1).map(|s| s.as_ref())
                    && values.iter().any(|v| v.as_str() == val)
                {
                    matched = true;
                    break;
                }
            }
            if !matched {
                return false;
            }
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostro2::NostrNote;

    fn note_kind(kind: u32) -> NostrNote {
        NostrNote {
            pubkey: "a".repeat(64),
            created_at: 1000,
            kind,
            id: Some("b".repeat(64)),
            sig: Some("c".repeat(128)),
            ..Default::default()
        }
    }

    #[test]
    fn empty_filter_matches_all() {
        let note = note_kind(1);
        let filter = NostrSubscription::default();
        assert!(matches(&note, &filter));
    }

    #[test]
    fn expiration_present_returns_ts() {
        let mut note = note_kind(1);
        note.tags.add_custom_tag("expiration", "1700000000");
        assert_eq!(expiration_from_note(&note), Some(1_700_000_000));
    }

    #[test]
    fn expiration_absent_returns_none() {
        let note = note_kind(1);
        assert!(expiration_from_note(&note).is_none());
    }

    #[test]
    fn expiration_malformed_value_returns_none() {
        let mut note = note_kind(1);
        note.tags.add_custom_tag("expiration", "not-a-number");
        assert!(expiration_from_note(&note).is_none());
    }

    #[test]
    fn expiration_first_wins() {
        let mut note = note_kind(1);
        note.tags.add_custom_tag("expiration", "1700000000");
        note.tags.add_custom_tag("expiration", "1800000000");
        assert_eq!(expiration_from_note(&note), Some(1_700_000_000));
    }

    #[test]
    fn expiration_view_matches_owned() {
        let mut note = note_kind(1);
        note.tags.add_custom_tag("expiration", "1700000000");
        let json = serde_json::to_string(&note).unwrap();
        let view: NostrNoteView<'_> = serde_json::from_str(&json).unwrap();
        assert_eq!(expiration_from_note(&note), expiration_from_view(&view));
    }

    #[test]
    fn kind_filter() {
        let note = note_kind(1);
        assert!(matches(&note, &NostrSubscription::new().kind(1)));
        assert!(!matches(&note, &NostrSubscription::new().kind(7)));
    }

    #[test]
    fn author_filter() {
        let note = note_kind(1);
        let pub_match = note.pubkey.clone();
        assert!(matches(
            &note,
            &NostrSubscription::new().authors(vec![pub_match])
        ));
        assert!(!matches(
            &note,
            &NostrSubscription::new().authors(vec!["z".repeat(64)])
        ));
    }

    #[test]
    fn since_until() {
        let note = note_kind(1);
        assert!(matches(&note, &NostrSubscription::new().since(999)));
        assert!(!matches(&note, &NostrSubscription::new().since(1001)));
        assert!(matches(&note, &NostrSubscription::new().until(1001)));
        assert!(!matches(&note, &NostrSubscription::new().until(999)));
    }

    #[test]
    fn tag_filter() {
        let mut note = note_kind(1);
        note.tags.add_custom_tag("t", "nostr");
        let filter = NostrSubscription::new().tag("#t", "nostr");
        assert!(matches(&note, &filter));
        let filter = NostrSubscription::new().tag("#t", "bitcoin");
        assert!(!matches(&note, &filter));
    }

    #[test]
    fn tag_filter_multiple_keys_anded() {
        let mut note = note_kind(1);
        note.tags.add_custom_tag("t", "nostr");
        note.tags.add_pubkey_tag(&"d".repeat(64), None);
        let filter = NostrSubscription::new()
            .tag("#t", "nostr")
            .tag("#p", &"d".repeat(64));
        assert!(matches(&note, &filter));
        let filter = NostrSubscription::new()
            .tag("#t", "nostr")
            .tag("#p", &"e".repeat(64));
        assert!(!matches(&note, &filter));
    }

    #[test]
    fn id_prefix_match() {
        let note = note_kind(1);
        let prefix = note.id.as_ref().unwrap()[..16].to_string();
        assert!(matches(&note, &NostrSubscription::new().ids(vec![prefix])));
    }

    /// Helper: serialize the owned note to JSON and reparse as a view, so
    /// the view tests run against a freshly-parsed `NostrNoteView` backed
    /// by the JSON string.
    fn note_and_view<'a>(note: &NostrNote, buf: &'a mut String) -> nostro2::NostrNoteView<'a> {
        *buf = serde_json::to_string(note).unwrap();
        serde_json::from_str(buf).unwrap()
    }

    #[test]
    fn view_matches_parity_with_owned() {
        // Spot-check every branch of the matcher against its owned twin.
        let mut note = note_kind(1);
        note.tags.add_custom_tag("t", "nostr");
        note.tags.add_pubkey_tag(&"d".repeat(64), None);

        let mut buf = String::new();
        let view = note_and_view(&note, &mut buf);

        for filter in [
            NostrSubscription::default(),
            NostrSubscription::new().kind(1),
            NostrSubscription::new().kind(7),
            NostrSubscription::new().authors(vec![note.pubkey.clone()]),
            NostrSubscription::new().authors(vec!["z".repeat(64)]),
            NostrSubscription::new().since(999),
            NostrSubscription::new().since(1001),
            NostrSubscription::new().until(1001),
            NostrSubscription::new().until(999),
            NostrSubscription::new().tag("#t", "nostr"),
            NostrSubscription::new().tag("#t", "bitcoin"),
            NostrSubscription::new()
                .tag("#t", "nostr")
                .tag("#p", &"d".repeat(64)),
            NostrSubscription::new().ids(vec![note.id.as_ref().unwrap()[..16].to_string()]),
        ] {
            assert_eq!(
                matches(&note, &filter),
                matches_view(&view, &filter),
                "view/owned disagree on {filter:?}"
            );
        }
    }
}
