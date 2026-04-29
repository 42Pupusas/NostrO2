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
}
