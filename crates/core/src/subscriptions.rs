//! Subscription filter for querying Nostr events
//!
//! Filters allow clients to request specific events from relays based on various criteria.
//! All filter fields are optional and combined with AND logic.

use bourne::{
    Error as BourneError, ErrorKind as BourneErrorKind, FromJson, JsonWrite, Lexer, ToJson,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NostrSubscription {
    pub authors: Option<Vec<String>>,
    pub ids: Option<Vec<String>>,
    pub kinds: Option<Vec<u32>>,
    pub since: Option<u64>,
    pub until: Option<u64>,
    pub limit: Option<u32>,
    /// `#p`/`#e`/etc. tag filters. Backed by `BTreeMap` so serialization
    /// order is deterministic.
    pub tags: Option<std::collections::BTreeMap<String, Vec<String>>>,
}

impl NostrSubscription {
    #[allow(unknown_lints, crappy)]
    fn parse_field(&mut self, key: &str, lex: &mut Lexer<'_>) -> Result<(), BourneError> {
        match key {
            "authors" => self.authors = Option::<Vec<String>>::from_lex(lex)?,
            "ids" => self.ids = Option::<Vec<String>>::from_lex(lex)?,
            "kinds" => self.kinds = Option::<Vec<u32>>::from_lex(lex)?,
            "since" => self.since = Option::<u64>::from_lex(lex)?,
            "until" => self.until = Option::<u64>::from_lex(lex)?,
            "limit" => {
                self.limit = Some(u32::try_from(lex.parse_i64_value()?).map_err(|_| {
                    BourneError::new(BourneErrorKind::NumberOutOfRange, lex.position())
                })?);
            }
            _ if key.starts_with('#') => {
                let values = Vec::<String>::from_lex(lex)?;
                self.tags
                    .get_or_insert_with(std::collections::BTreeMap::new)
                    .insert(key.to_string(), values);
            }
            _ => lex.skip_value()?,
        }
        Ok(())
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

        macro_rules! field {
            ($name:expr, $val:expr) => {
                if let Some(v) = $val {
                    if !first {
                        w.write_byte(b',')?;
                    }
                    first = false;
                    w.write_byte(b'"')?;
                    w.write_str_raw($name)?;
                    w.write_str_raw("\":")?;
                    v.write_json(w)?;
                }
            };
        }

        field!("authors", &self.authors);
        field!("ids", &self.ids);
        field!("kinds", &self.kinds);
        field!("since", &self.since);
        field!("until", &self.until);
        field!("limit", &self.limit);

        if let Some(tags) = &self.tags {
            for (key, values) in tags {
                if !first {
                    w.write_byte(b',')?;
                }
                first = false;
                w.write_escaped_str(key)?;
                w.write_byte(b':')?;
                values.write_json(w)?;
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

impl NostrSubscription {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_tag(&mut self, tag: &str, value: &str) {
        self.tags
            .get_or_insert_with(std::collections::BTreeMap::new)
            .entry(tag.to_string())
            .or_default()
            .push(value.to_string());
    }

    #[must_use]
    pub fn authors(mut self, authors: Vec<String>) -> Self {
        self.authors = Some(authors);
        self
    }

    #[must_use]
    pub fn author(mut self, author: impl Into<String>) -> Self {
        self.authors
            .get_or_insert_with(Vec::new)
            .push(author.into());
        self
    }

    #[must_use]
    pub fn ids(mut self, ids: Vec<String>) -> Self {
        self.ids = Some(ids);
        self
    }

    #[must_use]
    pub fn id(mut self, id: impl Into<String>) -> Self {
        self.ids.get_or_insert_with(Vec::new).push(id.into());
        self
    }

    #[must_use]
    pub fn kinds(mut self, kinds: Vec<u32>) -> Self {
        self.kinds = Some(kinds);
        self
    }

    #[must_use]
    pub fn kind(mut self, kind: u32) -> Self {
        self.kinds.get_or_insert_with(Vec::new).push(kind);
        self
    }

    #[must_use]
    pub const fn limit(mut self, limit: u32) -> Self {
        self.limit = Some(limit);
        self
    }

    #[must_use]
    pub const fn since(mut self, since: u64) -> Self {
        self.since = Some(since);
        self
    }

    #[must_use]
    pub const fn until(mut self, until: u64) -> Self {
        self.until = Some(until);
        self
    }

    #[must_use]
    pub fn tag(mut self, tag: &str, value: &str) -> Self {
        self.add_tag(tag, value);
        self
    }

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

    #[must_use]
    pub fn compile(&self) -> CompiledSubscription {
        CompiledSubscription::from(self)
    }
}

#[derive(Debug, Clone, Default)]
pub struct CompiledSubscription {
    wildcard: bool,
    ids: Option<std::collections::HashSet<String>>,
    authors: Option<std::collections::HashSet<String>>,
    kinds: Option<std::collections::HashSet<u32>>,
    since: Option<u64>,
    until: Option<u64>,
    tags: Vec<(String, std::collections::HashSet<String>)>,
}

impl From<&NostrSubscription> for CompiledSubscription {
    fn from(sub: &NostrSubscription) -> Self {
        let tags = sub.tags.as_ref().map_or_else(Vec::new, |t| {
            t.iter()
                .filter_map(|(k, v)| {
                    k.strip_prefix('#')
                        .map(|letter| (letter.to_string(), v.iter().cloned().collect()))
                })
                .collect()
        });
        Self {
            wildcard: sub.is_wildcard(),
            ids: sub.ids.as_ref().map(|v| v.iter().cloned().collect()),
            authors: sub.authors.as_ref().map(|v| v.iter().cloned().collect()),
            kinds: sub.kinds.as_ref().map(|v| v.iter().copied().collect()),
            since: sub.since,
            until: sub.until,
            tags,
        }
    }
}

impl CompiledSubscription {
    #[must_use]
    pub fn matches(&self, note: &crate::NostrNote) -> bool {
        if self.wildcard {
            return true;
        }
        if let Some(ids) = &self.ids {
            let Some(id) = note.id.as_deref() else {
                return false;
            };
            if !ids.contains(id) {
                return false;
            }
        }
        if let Some(authors) = &self.authors {
            if !authors.contains(&note.pubkey) {
                return false;
            }
        }
        if let Some(kinds) = &self.kinds {
            if !kinds.contains(&note.kind) {
                return false;
            }
        }
        if self.since.is_some() || self.until.is_some() {
            let Ok(ts) = u64::try_from(note.created_at) else {
                return false;
            };
            if let Some(since) = self.since {
                if ts < since {
                    return false;
                }
            }
            if let Some(until) = self.until {
                if ts > until {
                    return false;
                }
            }
        }
        for (letter, allowed) in &self.tags {
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
                allowed.contains(value.as_str())
            });
            if !any {
                return false;
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
        let json = bourne::to_string(&filter).unwrap();
        assert!(json.contains("\"kinds\":[4]"));
        assert!(json.contains("\"#p\":[\"value1\"]"));
        assert!(json.contains("\"#q\":[\"value2\"]"));
    }

    #[test]
    fn test_filter_tags_add() {
        let mut filter = NostrSubscription::default();
        filter.add_tag("#p", "value1");
        filter.add_tag("#q", "value2");
        filter.add_tag("#p", "value3");
        let json = bourne::to_string(&filter).unwrap();
        assert!(json.contains("\"#p\":[\"value1\",\"value3\"]"));
        assert!(json.contains("\"#q\":[\"value2\"]"));
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
        f.add_tag("#t", "go");
        assert!(f.matches(&n));
        let mut f2 = NostrSubscription::new();
        f2.add_tag("#p", "bob");
        f2.add_tag("#t", "go");
        assert!(!f2.matches(&n), "missing #t=go must fail");
    }

    #[test]
    fn compiled_matcher_agrees_with_linear() {
        let mut n_alice = note("alice", 1, 100);
        n_alice.tags.add_pubkey_tag("bob", None);
        n_alice.tags.add_custom_tag("t", "rust");
        let mut n_bob = note("bob", 2, 200);
        n_bob.tags.add_custom_tag("t", "go");
        let n_neg = note("alice", 1, -1);

        let mut filters = vec![
            NostrSubscription::default(),
            NostrSubscription::new().author("alice"),
            NostrSubscription::new().kind(1).since(50).until(150),
            NostrSubscription::new().since(0),
            NostrSubscription::new().id("a".repeat(64)),
            NostrSubscription::new().tag("#p", "bob"),
            NostrSubscription::new().tag("#p", "carol"),
            NostrSubscription::new().tag("#t", "rust"),
        ];
        let mut f_and = NostrSubscription::new();
        f_and.add_tag("#p", "bob");
        f_and.add_tag("#t", "rust");
        filters.push(f_and);
        let mut f_bogus = NostrSubscription::new();
        f_bogus.add_tag("nothash", "x");
        filters.push(f_bogus);

        let notes = [&n_alice, &n_bob, &n_neg];
        for f in &filters {
            let c = f.compile();
            for n in notes {
                assert_eq!(
                    f.matches(n),
                    c.matches(n),
                    "compiled/linear disagree on filter {f:?} note {n:?}",
                );
            }
        }
    }

    #[test]
    fn rejects_negative_limit() {
        let json = r#"{"limit":-1}"#;
        assert!(bourne::parse_str::<NostrSubscription>(json).is_err());
    }

    #[test]
    fn skips_unknown_fields_in_filter() {
        let json = r#"{"kinds":[1],"unknown_field":true}"#;
        let sub: NostrSubscription = bourne::parse_str(json).unwrap();
        assert_eq!(sub.kinds, Some(vec![1]));
    }

    #[test]
    fn parses_all_fields_from_json() {
        let json = r##"{"authors":["alice"],"ids":["deadbeef"],"kinds":[1,4],"since":100,"until":200,"limit":10,"#p":["bob"]}"##;
        let sub: NostrSubscription = bourne::parse_str(json).unwrap();
        assert_eq!(sub.authors, Some(vec!["alice".to_string()]));
        assert_eq!(sub.ids, Some(vec!["deadbeef".to_string()]));
        assert_eq!(sub.kinds, Some(vec![1, 4]));
        assert_eq!(sub.since, Some(100));
        assert_eq!(sub.until, Some(200));
        assert_eq!(sub.limit, Some(10));
        let tags = sub.tags.unwrap();
        assert_eq!(tags["#p"], vec!["bob".to_string()]);
    }

    #[test]
    fn round_trip_through_bourne() {
        let filter = NostrSubscription::new()
            .kind(1)
            .author("alice")
            .limit(10)
            .since(100)
            .until(200)
            .tag("#p", "bob");
        let json = bourne::to_string(&filter).unwrap();
        let back: NostrSubscription = bourne::parse_str(&json).unwrap();
        assert_eq!(filter, back);
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
            )
                .prop_map(
                    |(authors, ids, kinds, since, until, limit, tag_pairs)| {
                        let mut sub = NostrSubscription {
                            authors,
                            ids,
                            kinds,
                            since,
                            until,
                            limit,
                            tags: None,
                        };
                        for (k, v) in tag_pairs {
                            sub.add_tag(&format!("#{k}"), &v);
                        }
                        sub
                    },
                )
        }

        fn arb_note() -> impl Strategy<Value = crate::NostrNote> {
            (
                "[a-zA-Z0-9]{1,32}",
                any::<u32>(),
                any::<i64>(),
                proptest::option::of("[0-9a-f]{64}"),
                proptest::collection::vec(("[a-zA-Z0-9]{1,4}", "[a-zA-Z0-9]{1,16}"), 0..5),
            )
                .prop_map(|(pubkey, kind, created_at, id, tag_pairs)| {
                    let mut note = crate::NostrNote {
                        pubkey,
                        created_at,
                        kind,
                        id,
                        ..Default::default()
                    };
                    for (name, value) in tag_pairs {
                        note.tags.add_custom_tag(&name, &value);
                    }
                    note
                })
        }

        proptest! {
            #[test]
            fn round_trip(sub in arb_subscription()) {
                let json = bourne::to_string(&sub).unwrap();
                let back: NostrSubscription = bourne::parse_str(&json).unwrap();
                prop_assert_eq!(&sub, &back);
            }

            #[test]
            fn compiled_agrees_with_linear(sub in arb_subscription(), note in arb_note()) {
                let compiled = sub.compile();
                prop_assert_eq!(
                    sub.matches(&note),
                    compiled.matches(&note),
                    "compiled and linear matchers must agree"
                );
            }
        }
    }
}
