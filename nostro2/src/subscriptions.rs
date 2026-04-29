/// Subscription filter for querying Nostr events
///
/// Filters allow clients to request specific events from relays based on various criteria.
/// All filter fields are optional and combined with AND logic.
///
/// # Examples
///
/// ```rust
/// use nostro2::NostrSubscription;
///
/// // Get recent text notes from specific authors
/// let filter = NostrSubscription::new()
///     .kinds(vec![1])
///     .authors(vec!["pubkey1...".to_string(), "pubkey2...".to_string()])
///     .limit(20)
///     .since(1234567890);
///
/// // Filter by tags
/// let filter = NostrSubscription::new()
///     .kind(1)
///     .tag("#p", "pubkey...")
///     .tag("#t", "nostr");
/// ```
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct NostrSubscription {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<u32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// `#p`/`#e`/etc. tag filters. Backed by `BTreeMap` (not `HashMap`) so
    /// the JSON serialization order is deterministic across runs — required
    /// for the `nostro2-cache` filter dedup key and useful for snapshot tests.
    #[serde(flatten)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<std::collections::BTreeMap<String, Vec<String>>>,
}
impl TryFrom<serde_json::Value> for NostrSubscription {
    type Error = serde_json::Error;
    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        serde_json::from_value(value)
    }
}
impl TryFrom<&[u8]> for NostrSubscription {
    type Error = serde_json::Error;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        serde_json::from_slice(value)
    }
}
impl std::str::FromStr for NostrSubscription {
    type Err = serde_json::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(value)
    }
}
impl NostrSubscription {
    /// Create a new empty subscription filter
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a tag filter. Repeated calls with the same tag append to its
    /// value list (`#p` → multi-author OR semantics, per NIP-01).
    pub fn add_tag(&mut self, tag: &str, value: &str) {
        self.tags
            .get_or_insert_with(std::collections::BTreeMap::new)
            .entry(tag.to_string())
            .or_default()
            .push(value.to_string());
    }

    /// Set authors filter (replaces existing)
    #[must_use]
    pub fn authors(mut self, authors: Vec<String>) -> Self {
        self.authors = Some(authors);
        self
    }

    /// Add a single author to the filter
    #[must_use]
    pub fn author(mut self, author: impl Into<String>) -> Self {
        self.authors
            .get_or_insert_with(Vec::new)
            .push(author.into());
        self
    }

    /// Set event IDs filter (replaces existing)
    #[must_use]
    pub fn ids(mut self, ids: Vec<String>) -> Self {
        self.ids = Some(ids);
        self
    }

    /// Add a single event ID to the filter
    #[must_use]
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.ids.get_or_insert_with(Vec::new).push(id.into());
        self
    }

    /// Set kinds filter (replaces existing)
    #[must_use]
    pub fn kinds(mut self, kinds: Vec<u32>) -> Self {
        self.kinds = Some(kinds);
        self
    }

    /// Add a single kind to the filter
    #[must_use]
    pub fn kind(mut self, kind: u32) -> Self {
        self.kinds.get_or_insert_with(Vec::new).push(kind);
        self
    }

    /// Set the limit
    #[must_use]
    pub const fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Set the since timestamp
    #[must_use]
    pub const fn since(mut self, since: u64) -> Self {
        self.since = Some(since);
        self
    }

    /// Set the until timestamp
    #[must_use]
    pub const fn until(mut self, until: u64) -> Self {
        self.until = Some(until);
        self
    }

    /// Add a tag filter (chainable)
    #[must_use]
    pub fn tag(mut self, tag: &str, value: &str) -> Self {
        self.add_tag(tag, value);
        self
    }

    /// Test whether a note matches this filter under NIP-01 semantics.
    ///
    /// Filter semantics, lifted from NIP-01:
    /// - For each scalar list (`ids`, `authors`, `kinds`), the note's value
    ///   must be in the list (OR within the list).
    /// - For tag filters (`#e`, `#p`, `#t`, …), the note must carry at least
    ///   one tag row whose first cell matches the letter after `#` and
    ///   whose second cell is in the filter list.
    /// - `since` / `until` are inclusive timestamp bounds.
    /// - `limit` is a result-set cap, not a per-note predicate, so this
    ///   method ignores it. Apply `limit` at the consumer level after
    ///   collecting matches.
    /// - Fields that are `None` are wildcards (always match).
    ///
    /// Returns `true` iff every present field is satisfied. Used by relays
    /// and caches to filter incoming events; previously every consumer
    /// reimplemented this, which is why it's now in the library.
    /// Returns `true` if every filter field is `None` — i.e. the wire-format
    /// filter is `{}` and matches every event. Useful for callers who want
    /// to skip iteration entirely (e.g. fan out to every cached event)
    /// instead of running [`matches`](Self::matches) per note.
    #[must_use]
    #[inline]
    pub const fn is_wildcard(&self) -> bool {
        self.ids.is_none()
            && self.authors.is_none()
            && self.kinds.is_none()
            && self.since.is_none()
            && self.until.is_none()
            && self.tags.is_none()
    }

    #[must_use]
    pub fn matches(&self, note: &crate::NostrNote) -> bool {
        // Wildcard fast path: a `{}` filter accepts every event. Saves the
        // six `is_some` branches on the hot loop in relays/caches that iterate
        // over a stream with a default subscription.
        if self.is_wildcard() {
            return true;
        }
        if let Some(ids) = &self.ids {
            let Some(id) = note.id.as_deref() else {
                return false;
            };
            if !ids.iter().any(|s| s == id) {
                return false;
            }
        }
        if let Some(authors) = &self.authors {
            if !authors.iter().any(|a| a == &note.pubkey) {
                return false;
            }
        }
        if let Some(kinds) = &self.kinds {
            if !kinds.contains(&note.kind) {
                return false;
            }
        }
        // `since` / `until` use `u64` in the wire format but `created_at` is
        // `i64`. A note with negative `created_at` cannot satisfy a `since`
        // bound; clamp via try_from.
        if let Some(since) = self.since {
            let Ok(ts) = u64::try_from(note.created_at) else {
                return false;
            };
            if ts < since {
                return false;
            }
        }
        if let Some(until) = self.until {
            let Ok(ts) = u64::try_from(note.created_at) else {
                return false;
            };
            if ts > until {
                return false;
            }
        }
        if let Some(tags) = &self.tags {
            for (key, values) in tags {
                // Per NIP-01, tag filter keys are `#x` where `x` is the tag
                // letter. Skip anything that doesn't match that shape — it's
                // either a typo on the wire or a non-tag field (we use
                // `#[serde(flatten)]`, so unknown keys land here).
                let Some(letter) = key.strip_prefix('#') else {
                    continue;
                };
                let any = note.tags.iter().any(|row| {
                    let Some(name) = row.first() else {
                        return false;
                    };
                    if name != letter {
                        return false;
                    }
                    let Some(value) = row.get(1) else {
                        return false;
                    };
                    values.iter().any(|v| v == value)
                });
                if !any {
                    return false;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_tags() {
        let mut tags = std::collections::BTreeMap::new();
        tags.insert("#p".to_string(), vec!["value1".to_string()]);
        tags.insert("#q".to_string(), vec!["value2".to_string()]);
        let filter = NostrSubscription {
            kinds: Some(vec![4]),
            tags: Some(tags),
            ..Default::default()
        };
        let filter_value = serde_json::to_value(&filter).unwrap();
        assert_eq!(
            filter_value,
            serde_json::json!({
                "kinds": [4],
                "#p": ["value1"],
                "#q": ["value2"]
            })
        );
    }
    #[test]
    fn test_filter_tags_add() {
        let mut filter = NostrSubscription::default();
        filter.add_tag("#p", "value1");
        filter.add_tag("#q", "value2");
        filter.add_tag("#p", "value3");
        let filter_value = serde_json::to_value(&filter).unwrap();
        assert_eq!(
            filter_value,
            serde_json::json!({
                "#p": ["value1", "value3"],
                "#q": ["value2"]
            })
        );
    }

    #[test]
    fn test_subscription_builder() {
        let filter = NostrSubscription::new()
            .kind(1)
            .author("abc123")
            .limit(10)
            .since(1_234_567_890);

        assert_eq!(filter.kinds, Some(vec![1]));
        assert_eq!(filter.authors, Some(vec!["abc123".to_string()]));
        assert_eq!(filter.limit, Some(10));
        assert_eq!(filter.since, Some(1_234_567_890));
    }

    #[test]
    fn test_subscription_builder_multiple_kinds() {
        let filter = NostrSubscription::new().kind(1).kind(4).kinds(vec![0, 3]);

        assert_eq!(filter.kinds, Some(vec![0, 3]));
    }

    fn note(pubkey: &str, kind: u32, ts: i64) -> crate::NostrNote {
        crate::NostrNote {
            id: Some("a".repeat(64)),
            pubkey: pubkey.to_string(),
            created_at: ts,
            kind,
            content: String::new(),
            sig: Some("b".repeat(128)),
            ..Default::default()
        }
    }

    #[test]
    fn matches_default_filter_accepts_anything() {
        assert!(NostrSubscription::default().matches(&note("a", 1, 100)));
    }

    #[test]
    fn matches_author_kind_and_time() {
        let f = NostrSubscription::new()
            .author("alice")
            .kind(1)
            .since(50)
            .until(150);
        assert!(f.matches(&note("alice", 1, 100)));
        assert!(!f.matches(&note("bob", 1, 100)), "wrong author");
        assert!(!f.matches(&note("alice", 2, 100)), "wrong kind");
        assert!(!f.matches(&note("alice", 1, 49)), "before since");
        assert!(!f.matches(&note("alice", 1, 151)), "after until");
        assert!(f.matches(&note("alice", 1, 50)), "since is inclusive");
        assert!(f.matches(&note("alice", 1, 150)), "until is inclusive");
    }

    #[test]
    fn matches_negative_created_at_fails_since() {
        // i64 → u64 try_from fails for negative values; spec doesn't
        // contemplate this, but we treat it as "out of bound."
        let f = NostrSubscription::new().since(0);
        assert!(!f.matches(&note("a", 1, -1)));
    }

    #[test]
    fn matches_ids_requires_present_id() {
        let mut n = note("a", 1, 100);
        n.id = Some("dead".repeat(16));
        let f = NostrSubscription::new().id(n.id.clone().unwrap());
        assert!(f.matches(&n));
        n.id = None;
        assert!(!f.matches(&n), "missing id field cannot match an ids filter");
    }

    #[test]
    fn matches_p_tag_filter() {
        let mut n = note("alice", 1, 100);
        n.tags.add_pubkey_tag("bob", None);
        n.tags.add_custom_tag("t", "rust");
        let f = NostrSubscription::new().tag("#p", "bob");
        assert!(f.matches(&n));
        let f = NostrSubscription::new().tag("#p", "carol");
        assert!(!f.matches(&n));
        let f = NostrSubscription::new().tag("#t", "rust");
        assert!(f.matches(&n));
    }

    #[test]
    fn matches_multiple_tag_filters_are_anded() {
        let mut n = note("a", 1, 100);
        n.tags.add_pubkey_tag("bob", None);
        n.tags.add_custom_tag("t", "rust");
        let mut f = NostrSubscription::new();
        f.add_tag("#p", "bob");
        f.add_tag("#t", "rust");
        assert!(f.matches(&n));
        f.add_tag("#t", "go"); // OR within #t — bob still has rust, still matches
        assert!(f.matches(&n));
        let mut f2 = NostrSubscription::new();
        f2.add_tag("#p", "bob");
        f2.add_tag("#t", "go");
        assert!(!f2.matches(&n), "missing #t=go must fail");
    }
}
