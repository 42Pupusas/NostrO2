//! Subscription filter for querying Nostr events.
//!
//! Filters allow clients to request specific events from relays. All
//! fields are optional and combined with AND logic. Internals use
//! [`HashSet`] for O(1) matching; the wire format uses JSON arrays
//! (order-insensitive on deserialization).

use std::collections::{BTreeMap, HashSet};

use bourne::{
    Error as BourneError, ErrorKind as BourneErrorKind, FromJson, JsonWrite, Lexer, ToJson,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NostrSubscription {
    pub authors: Option<HashSet<String>>,
    pub ids: Option<HashSet<String>>,
    pub kinds: Option<HashSet<u32>>,
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub limit: Option<u32>,
    /// `#p`/`#e`/etc. tag filters. Key is the tag name *without* the
    /// leading `#` (e.g. `"p"`, `"e"`). Backed by `BTreeMap` so
    /// serialization order is deterministic.
    pub tags: Option<BTreeMap<String, HashSet<String>>>,
}

impl NostrSubscription {
    fn parse_field(&mut self, key: &str, lex: &mut Lexer<'_>) -> Result<(), BourneError> {
        match key {
            "authors" => self.authors = Some(Vec::from_lex(lex)?.into_iter().collect()),
            "ids" => self.ids = Some(Vec::from_lex(lex)?.into_iter().collect()),
            "kinds" => self.kinds = Some(Vec::from_lex(lex)?.into_iter().collect()),
            "since" => self.since = Option::<u64>::from_lex(lex)?,
            "until" => self.until = Option::<u64>::from_lex(lex)?,
            "limit" => {
                self.limit = Some(u32::try_from(lex.parse_i64_value()?).map_err(|_| {
                    BourneError::new(BourneErrorKind::NumberOutOfRange, lex.position())
                })?);
            }
            _ if key.starts_with('#') => {
                let values: HashSet<String> = Vec::from_lex(lex)?.into_iter().collect();
                self.tags
                    .get_or_insert_with(BTreeMap::new)
                    .insert(key[1..].to_string(), values);
            }
            _ => lex.skip_value()?,
        }
        Ok(())
    }

    /// Serialize a `HashSet<T>` as a sorted JSON array.
    fn write_set<W: JsonWrite + ?Sized, T: ToJson + Ord>(
        w: &mut W,
        set: &HashSet<T>,
    ) -> Result<(), W::Error> {
        let mut sorted: Vec<&T> = set.iter().collect();
        sorted.sort();
        w.write_byte(b'[')?;
        for (i, val) in sorted.iter().enumerate() {
            if i > 0 { w.write_byte(b',')?; }
            val.write_json(w)?;
        }
        w.write_byte(b']')
    }
}

impl<'input> FromJson<'input> for NostrSubscription {
    fn from_lex(lex: &mut Lexer<'input>) -> Result<Self, BourneError> {
        lex.object_start()?;
        let mut sub = Self::default();
        let mut maybe_key = lex.object_first_key()?;
        while let Some(key) = maybe_key {
            sub.parse_field(key, lex)?;
            maybe_key = lex.object_next_key()?;
        }
        Ok(sub)
    }
}

impl ToJson for NostrSubscription {
    fn write_json<W: JsonWrite + ?Sized>(&self, w: &mut W) -> Result<(), W::Error> {
        w.write_byte(b'{')?;
        let mut first = true;

        macro_rules! write_field {
            ($name:expr, set: $val:expr) => {
                if let Some(v) = $val {
                    if !first { w.write_byte(b',')?; }
                    first = false;
                    w.write_byte(b'"')?;
                    w.write_str_raw($name)?;
                    w.write_str_raw("\":")?;
                    Self::write_set(w, v)?;
                }
            };
            ($name:expr, short: $val:expr) => {
                if let Some(v) = $val {
                    if !first { w.write_byte(b',')?; }
                    first = false;
                    w.write_byte(b'"')?;
                    w.write_str_raw($name)?;
                    w.write_str_raw("\":")?;
                    v.write_json(w)?;
                }
            };
        }

        write_field!("authors", set: &self.authors);
        write_field!("ids",     set: &self.ids);
        write_field!("kinds",   set: &self.kinds);
        write_field!("since",  short: &self.since);
        write_field!("until",  short: &self.until);
        write_field!("limit",  short: &self.limit);

        if let Some(tags) = &self.tags {
            for (letter, values) in tags {
                if !first { w.write_byte(b',')?; }
                first = false;
                w.write_byte(b'"')?;
                w.write_byte(b'#')?;
                w.write_str_raw(letter)?;
                w.write_str_raw("\":")?;
                Self::write_set(w, values)?;
            }
        }

        w.write_byte(b'}')
    }
}

impl TryFrom<&[u8]> for NostrSubscription {
    type Error = bourne::Error;
    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        bourne::parse(value)
    }
}

impl std::str::FromStr for NostrSubscription {
    type Err = bourne::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        bourne::parse_str(value)
    }
}

// ── Builder / query API ────────────────────────────────────────

impl NostrSubscription {
    #[must_use]
    pub fn new() -> Self { Self::default() }

    pub fn add_tag(&mut self, tag: &str, value: &str) {
        let letter = tag.strip_prefix('#').unwrap_or(tag);
        self.tags
            .get_or_insert_with(BTreeMap::new)
            .entry(letter.to_string())
            .or_default()
            .insert(value.to_string());
    }

    #[must_use] pub fn authors(mut self, authors: HashSet<String>) -> Self { self.authors = Some(authors); self }
    #[must_use] pub fn author(mut self, author: impl Into<String>) -> Self { self.authors.get_or_insert_with(HashSet::new).insert(author.into()); self }
    #[must_use] pub fn ids(mut self, ids: HashSet<String>) -> Self { self.ids = Some(ids); self }
    #[must_use] pub fn id(mut self, id: impl Into<String>) -> Self { self.ids.get_or_insert_with(HashSet::new).insert(id.into()); self }
    #[must_use] pub fn kinds(mut self, kinds: HashSet<u32>) -> Self { self.kinds = Some(kinds); self }
    #[must_use] pub fn kind(mut self, kind: u32) -> Self { self.kinds.get_or_insert_with(HashSet::new).insert(kind); self }
    #[must_use] pub const fn limit(mut self, limit: u32) -> Self { self.limit = Some(limit); self }
    #[must_use] pub const fn since(mut self, since: u64) -> Self { self.since = Some(since); self }
    #[must_use] pub const fn until(mut self, until: u64) -> Self { self.until = Some(until); self }
    #[must_use] pub fn tag(mut self, tag: &str, value: &str) -> Self { self.add_tag(tag, value); self }

    #[must_use] #[inline]
    pub const fn is_wildcard(&self) -> bool {
        self.ids.is_none() && self.authors.is_none() && self.kinds.is_none()
            && self.since.is_none() && self.until.is_none() && self.tags.is_none()
    }

    #[must_use]
    pub fn matches(&self, note: &crate::NostrNote) -> bool {
        if self.is_wildcard() { return true; }
        if let Some(ids) = &self.ids {
            let Some(id) = note.id.as_deref() else { return false; };
            if !ids.contains(id) { return false; }
        }
        if let Some(authors) = &self.authors {
            if !authors.contains(&note.pubkey) { return false; }
        }
        if let Some(kinds) = &self.kinds {
            if !kinds.contains(&note.kind) { return false; }
        }
        if let Some(since) = self.since {
            let Ok(ts) = u64::try_from(note.created_at) else { return false; };
            if ts < since { return false; }
        }
        if let Some(until) = self.until {
            let Ok(ts) = u64::try_from(note.created_at) else { return false; };
            if ts > until { return false; }
        }
        if let Some(tags) = &self.tags {
            for (letter, allowed) in tags {
                let any = note.tags.iter().any(|row| {
                    let Some(name) = row.first() else { return false; };
                    if name != letter { return false; }
                    let Some(value) = row.get(1) else { return false; };
                    allowed.contains(value.as_str())
                });
                if !any { return false; }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn tag_keys_omit_hash_prefix() {
        let mut sub = NostrSubscription::new();
        sub.add_tag("#p", "bob");
        let tags = sub.tags.as_ref().unwrap();
        assert!(tags.contains_key("p"));
        assert!(!tags.contains_key("#p"));
    }
    #[test] fn test_filter_tags() {
        let mut sub = NostrSubscription { kinds: Some([4].into()), ..Default::default() };
        sub.add_tag("#p", "value1");
        sub.add_tag("#q", "value2");
        let json = bourne::to_string(&sub).unwrap();
        assert!(json.contains("\"kinds\":[4]"));
        assert!(json.contains("\"#p\":[\"value1\"]"));
        assert!(json.contains("\"#q\":[\"value2\"]"));
    }
    #[test] fn test_filter_tags_add() {
        let mut filter = NostrSubscription::default();
        filter.add_tag("#p", "value1"); filter.add_tag("#q", "value2");
        filter.add_tag("#p", "value3");
        let json = bourne::to_string(&filter).unwrap();
        assert!(json.contains("\"#p\":[\"value1\",\"value3\"]"));
        assert!(json.contains("\"#q\":[\"value2\"]"));
    }
    #[test] fn test_subscription_builder() {
        let filter = NostrSubscription::new().kind(1).author("abc123").limit(10).since(1_234_567_890);
        assert_eq!(filter.kinds, Some([1].into()));
        assert_eq!(filter.authors, Some(["abc123".to_string()].into()));
        assert_eq!(filter.limit, Some(10));
        assert_eq!(filter.since, Some(1_234_567_890));
    }
    #[test] fn test_subscription_builder_multiple_kinds() {
        let filter = NostrSubscription::new().kind(1).kind(4).kinds([0, 3].into());
        assert_eq!(filter.kinds, Some([0, 3].into()));
    }
    #[test] fn kinds_set_ignores_order() {
        let filter = NostrSubscription::new().kind(1).kind(4);
        assert_eq!(filter.kinds, Some(HashSet::from([1, 4])));
    }
    fn note(pk: &str, k: u32, ts: i64) -> crate::NostrNote {
        crate::NostrNote { id: Some("a".repeat(64)), pubkey: pk.to_string(), created_at: ts, kind: k, content: String::new(), sig: Some("b".repeat(128)), ..Default::default() }
    }
    #[test] fn matches_default_filter_accepts_anything() {
        assert!(NostrSubscription::default().matches(&note("a", 1, 100)));
    }
    #[test] fn matches_author_kind_and_time() {
        let f = NostrSubscription::new().author("alice").kind(1).since(50).until(150);
        assert!(f.matches(&note("alice", 1, 100)));
        assert!(!f.matches(&note("bob", 1, 100)), "wrong author");
        assert!(!f.matches(&note("alice", 2, 100)), "wrong kind");
        assert!(!f.matches(&note("alice", 1, 49)), "before since");
        assert!(!f.matches(&note("alice", 1, 151)), "after until");
        assert!(f.matches(&note("alice", 1, 50)), "since is inclusive");
        assert!(f.matches(&note("alice", 1, 150)), "until is inclusive");
    }
    #[test] fn matches_negative_created_at_fails_since() {
        assert!(!NostrSubscription::new().since(0).matches(&note("a", 1, -1)));
    }
    #[test] fn matches_ids_requires_present_id() {
        let mut n = note("a", 1, 100);
        n.id = Some("dead".repeat(16));
        let f = NostrSubscription::new().id(n.id.clone().unwrap());
        assert!(f.matches(&n));
        n.id = None;
        assert!(!f.matches(&n), "missing id field cannot match an ids filter");
    }
    #[test] fn matches_p_tag_filter() {
        let mut n = note("alice", 1, 100);
        n.tags.add_pubkey_tag("bob", None);
        n.tags.add_custom_tag("t", "rust");
        assert!(NostrSubscription::new().tag("#p", "bob").matches(&n));
        assert!(!NostrSubscription::new().tag("#p", "carol").matches(&n));
        assert!(NostrSubscription::new().tag("#t", "rust").matches(&n));
    }
    #[test] fn matches_multiple_tag_filters_are_anded() {
        let mut n = note("a", 1, 100);
        n.tags.add_pubkey_tag("bob", None);
        n.tags.add_custom_tag("t", "rust");
        let mut f = NostrSubscription::new();
        f.add_tag("#p", "bob"); f.add_tag("#t", "rust");
        assert!(f.matches(&n));
        f.add_tag("#t", "go");
        assert!(f.matches(&n));
        assert!(!NostrSubscription::new().tag("#p","bob").tag("#t","go").matches(&n));
    }
    #[test] fn rejects_negative_limit() {
        assert!(bourne::parse_str::<NostrSubscription>(r#"{"limit":-1}"#).is_err());
    }
    #[test] fn skips_unknown_fields_in_filter() {
        let sub: NostrSubscription = bourne::parse_str(r#"{"kinds":[1],"unknown_field":true}"#).unwrap();
        assert_eq!(sub.kinds, Some([1].into()));
    }
    #[test] fn parses_all_fields_from_json() {
        let json = r##"{"authors":["alice"],"ids":["deadbeef"],"kinds":[1,4],"since":100,"until":200,"limit":10,"#p":["bob"]}"##;
        let sub: NostrSubscription = bourne::parse_str(json).unwrap();
        assert_eq!(sub.authors, Some(["alice".to_string()].into()));
        assert_eq!(sub.ids, Some(["deadbeef".to_string()].into()));
        assert_eq!(sub.kinds, Some([1, 4].into()));
        assert_eq!(sub.since, Some(100));
        assert_eq!(sub.until, Some(200));
        assert_eq!(sub.limit, Some(10));
        assert!(sub.tags.as_ref().unwrap()["p"].contains("bob"));
    }
    #[test] fn round_trip_through_bourne() {
        let filter = NostrSubscription::new().kind(1).author("alice").limit(10).since(100).until(200).tag("#p", "bob");
        let json = bourne::to_string(&filter).unwrap();
        let back: NostrSubscription = bourne::parse_str(&json).unwrap();
        assert_eq!(filter, back);
    }
    #[test] fn json_output_is_deterministic() {
        let filter = NostrSubscription::new().kind(1).kind(4).author("bob").author("alice");
        let json1 = bourne::to_string(&filter).unwrap();
        let json2 = bourne::to_string(&filter).unwrap();
        assert_eq!(json1, json2);
    }
    #[cfg(not(target_arch = "wasm32"))]
    mod proptests {
        use super::*;
        use proptest::prelude::*;

        fn arb_subscription() -> impl Strategy<Value = NostrSubscription> {
            (
                proptest::option::of(proptest::collection::vec("[a-zA-Z0-9]{1,32}", 1..5)),
                proptest::option::of(proptest::collection::vec("[a-zA-Z0-9]{1,32}", 1..5)),
                proptest::option::of(proptest::collection::vec(any::<u32>(), 1..5)),
                proptest::option::of(any::<u64>()),
                proptest::option::of(any::<u64>()),
                proptest::option::of(any::<u32>()),
                proptest::collection::vec(("[a-zA-Z0-9]{1,8}", "[a-zA-Z0-9]{1,32}"), 0..4),
            ).prop_map(|(authors, ids, kinds, since, until, limit, tag_pairs)| {
                let mut sub = NostrSubscription {
                    authors: authors.map(|v| v.into_iter().collect()),
                    ids: ids.map(|v| v.into_iter().collect()),
                    kinds: kinds.map(|v| v.into_iter().collect()),
                    since, until, limit, tags: None,
                };
                for (k, v) in tag_pairs { sub.add_tag(&format!("#{k}"), &v); }
                sub
            })
        }

        proptest! {
            #[test]
            fn round_trip(sub in arb_subscription()) {
                let json = bourne::to_string(&sub).unwrap();
                let back: NostrSubscription = bourne::parse_str(&json).unwrap();
                prop_assert_eq!(&sub, &back);
            }
        }
    }
}
