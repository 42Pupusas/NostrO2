//! NIP-01 filter matching against `NostrNote`.

use nostro2::{NostrNote, NostrSubscription};

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
}
