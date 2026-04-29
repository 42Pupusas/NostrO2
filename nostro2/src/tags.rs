//! Tag types and collections for Nostr notes
//!
//! Tags as specified in NIP-01 are a JSON array of arrays of strings — the
//! first cell of each row names the tag (`p`, `e`, `d`, …) and the remaining
//! cells are name-specific. The protocol does *not* fix the number of cells
//! per row, which is why this module stores rows as raw `&[String]` slices
//! rather than a typed enum: every new NIP adds new shapes, and a "Custom"
//! fallback would dominate the type anyway.
//!
//! ## Storage
//!
//! `NostrTags` flattens every cell of every row into a single `Vec<String>`
//! and tracks per-row start offsets in a second `Vec<u32>`. Two allocations
//! total, regardless of tag count. Compare against the obvious
//! `Vec<Vec<String>>` shape, which allocates the outer vec, plus an inner
//! vec per row. The wire format is unchanged — custom `Serialize` and
//! `Deserialize` impls keep the JSON byte-for-byte identical to the legacy
//! shape, so this is a drop-in storage swap, not a protocol change.
//!
//! ## API
//!
//! Walk rows with [`NostrTags::iter`] (yields `&[String]` per row) or
//! [`NostrTags::row`]. There is no `Deref<Target = Vec<Vec<String>>>` and no
//! indexing operator — callers that previously did `tags[0][0]` should use
//! `tags.row(0).and_then(|r| r.first())` or just `tags.iter()`.
//!
//! ## Examples
//!
//! ```rust
//! use nostro2::NostrTags;
//!
//! let mut tags = NostrTags::new();
//! tags.add_pubkey_tag("abc123", None);
//! tags.add_event_tag("event123");
//! tags.add_custom_tag("t", "nostr");
//! assert_eq!(tags.len(), 3);
//! ```

use serde::de::{self, DeserializeSeed, Deserializer, SeqAccess, Visitor};
use serde::ser::{Serialize, SerializeSeq, Serializer};
use std::fmt;

/// Collection of tags attached to a Nostr note.
///
/// Stores cells flat with row offsets — see the module docs for the layout
/// and rationale.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct NostrTags {
    /// Every cell of every row, flattened in row-major order.
    cells: Vec<String>,
    /// `offsets[i]` is the start index in `cells` for row `i`;
    /// `offsets.last()` is always `cells.len()` (closing sentinel).
    /// Empty `NostrTags` carries `offsets = [0]` so the invariant
    /// `rows == offsets.len() - 1` holds without a special case.
    offsets: Vec<u32>,
}

impl NostrTags {
    /// Create a new empty tags collection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of tag rows.
    #[must_use]
    #[inline]
    pub const fn len(&self) -> usize {
        // `offsets` always has `rows + 1` entries (sentinel).
        // Default::default() leaves `offsets` empty; treat that as 0 rows.
        self.offsets.len().saturating_sub(1)
    }

    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Borrow row `i` as a slice of cells, or `None` if out of range.
    #[must_use]
    #[inline]
    pub fn row(&self, i: usize) -> Option<&[String]> {
        let start = *self.offsets.get(i)? as usize;
        let end = *self.offsets.get(i + 1)? as usize;
        Some(&self.cells[start..end])
    }

    /// Iterate rows as cell slices.
    pub fn iter(&self) -> impl Iterator<Item = &[String]> {
        self.offsets
            .windows(2)
            .map(|w| &self.cells[w[0] as usize..w[1] as usize])
    }

    /// Append an arbitrary tag row. Use this when an existing
    /// `add_*_tag` helper doesn't fit (e.g. NIP-17 `relay` lists with N
    /// values, NIP-13 `nonce` rows). The first cell is the tag name.
    pub fn add_row<I: IntoIterator<Item = String>>(&mut self, row: I) {
        self.push_row(row);
    }

    /// Append a row built from an iterator of owned strings.
    fn push_row<I: IntoIterator<Item = String>>(&mut self, row: I) {
        // Lazy-init the sentinel on first push so `Default::default()` stays
        // a zero-allocation cheap call.
        if self.offsets.is_empty() {
            self.offsets.push(0);
        }
        self.cells.extend(row);
        let len = u32::try_from(self.cells.len()).expect("tag cell count overflow (u32)");
        self.offsets.push(len);
    }

    /// Add an `r` tag with a relay URL.
    pub fn add_relay_tag(&mut self, url: &str) {
        self.push_row(["r".to_owned(), url.to_owned()]);
    }

    /// Add a custom tag with a single value.
    pub fn add_custom_tag(&mut self, tag_type: &str, value: &str) {
        self.push_row([tag_type.to_owned(), value.to_owned()]);
    }

    /// Add a `p` tag (pubkey reference) with an optional relay hint.
    pub fn add_pubkey_tag(&mut self, pubkey: &str, relay: Option<&str>) {
        if let Some(relay) = relay {
            self.push_row(["p".to_owned(), pubkey.to_owned(), relay.to_owned()]);
        } else {
            self.push_row(["p".to_owned(), pubkey.to_owned()]);
        }
    }

    /// Add an `e` tag (event reference).
    pub fn add_event_tag(&mut self, event_id: &str) {
        self.push_row(["e".to_owned(), event_id.to_owned()]);
    }

    /// Add a `d` tag (parameterized replaceable identifier).
    pub fn add_parameter_tag(&mut self, parameter: &str) {
        self.push_row(["d".to_owned(), parameter.to_owned()]);
    }

    /// First `p`-tag value, owned.
    #[must_use]
    #[inline]
    pub fn first_tagged_pubkey(&self) -> Option<String> {
        self.first_tagged_pubkey_ref().map(String::from)
    }

    /// First `p`-tag value, borrowed.
    #[must_use]
    #[inline]
    pub fn first_tagged_pubkey_ref(&self) -> Option<&str> {
        self.iter()
            .find(|row| row.first().is_some_and(|t| t == "p"))
            .and_then(|row| row.get(1).map(String::as_str))
    }

    /// First `e`-tag value, owned.
    #[must_use]
    #[inline]
    pub fn first_tagged_event(&self) -> Option<String> {
        self.first_tagged_event_ref().map(String::from)
    }

    /// First `e`-tag value, borrowed.
    #[must_use]
    #[inline]
    pub fn first_tagged_event_ref(&self) -> Option<&str> {
        self.iter()
            .find(|row| row.first().is_some_and(|t| t == "e"))
            .and_then(|row| row.get(1).map(String::as_str))
    }

    /// First `d`-tag value, owned.
    #[must_use]
    #[inline]
    pub fn first_parameter(&self) -> Option<String> {
        self.first_parameter_ref().map(String::from)
    }

    /// First `d`-tag value, borrowed.
    #[must_use]
    #[inline]
    pub fn first_parameter_ref(&self) -> Option<&str> {
        self.iter()
            .find(|row| row.first().is_some_and(|t| t == "d"))
            .and_then(|row| row.get(1).map(String::as_str))
    }

    /// Collect every value cell from every row whose first cell equals
    /// `tag_type`. Owned strings.
    #[must_use]
    #[inline]
    pub fn find_tags(&self, tag_type: &str) -> Vec<String> {
        self.find_tags_ref(tag_type)
            .into_iter()
            .map(String::from)
            .collect()
    }

    /// Borrowed equivalent of [`Self::find_tags`].
    #[must_use]
    #[inline]
    pub fn find_tags_ref(&self, tag_type: &str) -> Vec<&str> {
        self.iter()
            .filter(|row| row.first().is_some_and(|t| t == tag_type))
            .flat_map(|row| row.iter().skip(1).map(String::as_str))
            .collect()
    }
}

// Wire format: `[[String]]`. Custom impls preserve the on-the-wire shape
// while keeping the flat-cells storage internally. Mirrors the borrowed
// `view::TagsView` deserializer; the row seed pushes cells straight into
// the buffer to avoid the per-row `Vec` a `Deserialize`-based row type
// would force.

impl Serialize for NostrTags {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(self.len()))?;
        for row in self.iter() {
            seq.serialize_element(row)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for NostrTags {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct TagsVisitor;

        impl<'de> Visitor<'de> for TagsVisitor {
            type Value = NostrTags;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a Nostr tag array (array of string arrays)")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let cap = seq.size_hint().unwrap_or(4);
                let mut cells: Vec<String> = Vec::with_capacity(cap * 2);
                let mut offsets: Vec<u32> = Vec::with_capacity(cap + 1);
                offsets.push(0);

                while seq
                    .next_element_seed(RowSeed { cells: &mut cells })?
                    .is_some()
                {
                    let len = u32::try_from(cells.len())
                        .map_err(|_| de::Error::custom("tag cell count overflow"))?;
                    offsets.push(len);
                }

                Ok(NostrTags { cells, offsets })
            }
        }

        d.deserialize_seq(TagsVisitor)
    }
}

use serde::Deserialize;

struct RowSeed<'buf> {
    cells: &'buf mut Vec<String>,
}

impl<'de> DeserializeSeed<'de> for RowSeed<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        struct RowVisitor<'buf> {
            cells: &'buf mut Vec<String>,
        }

        impl<'de> Visitor<'de> for RowVisitor<'_> {
            type Value = ();

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a tag row (array of strings)")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
                while let Some(cell) = seq.next_element::<String>()? {
                    self.cells.push(cell);
                }
                Ok(())
            }
        }

        d.deserialize_seq(RowVisitor { cells: self.cells })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_default_has_no_rows() {
        let tags = NostrTags::new();
        assert_eq!(tags.len(), 0);
        assert!(tags.is_empty());
        assert!(tags.row(0).is_none());
    }

    #[test]
    fn add_methods_populate_rows() {
        let mut tags = NostrTags::new();
        tags.add_pubkey_tag("abc123", None);
        tags.add_event_tag("event123");
        tags.add_custom_tag("t", "test");

        assert_eq!(tags.len(), 3);
        assert_eq!(
            tags.row(0),
            Some(["p".to_string(), "abc123".to_string()].as_slice())
        );
        assert_eq!(
            tags.row(1),
            Some(["e".to_string(), "event123".to_string()].as_slice())
        );
        assert_eq!(tags.first_tagged_pubkey_ref(), Some("abc123"));
        assert_eq!(tags.first_tagged_event_ref(), Some("event123"));
    }

    #[test]
    fn add_pubkey_with_relay_appends_third_cell() {
        let mut tags = NostrTags::new();
        tags.add_pubkey_tag("abc", Some("wss://relay"));
        let row = tags.row(0).unwrap();
        assert_eq!(row.len(), 3);
        assert_eq!(&row[2], "wss://relay");
    }

    #[test]
    fn add_relay_tag_uses_r_prefix() {
        let mut tags = NostrTags::new();
        tags.add_relay_tag("wss://relay.example.com");
        let row = tags.row(0).unwrap();
        assert_eq!(&row[0], "r");
        assert_eq!(&row[1], "wss://relay.example.com");
    }

    /// JSON wire format must match the legacy `Vec<Vec<String>>` shape so
    /// the storage swap is invisible to relays and to the canonical event-id
    /// hash (which serializes the tags inside the prehash). A regression here
    /// would fork the network silently.
    #[test]
    fn json_wire_format_matches_vec_vec_string() {
        let mut tags = NostrTags::new();
        tags.add_custom_tag("t", "nostr");
        tags.add_pubkey_tag(&"a".repeat(64), None);
        tags.add_event_tag(&"b".repeat(64));

        let from_tags = serde_json::to_string(&tags).unwrap();
        let raw: Vec<Vec<String>> = vec![
            vec!["t".to_string(), "nostr".to_string()],
            vec!["p".to_string(), "a".repeat(64)],
            vec!["e".to_string(), "b".repeat(64)],
        ];
        let from_vec = serde_json::to_string(&raw).unwrap();
        assert_eq!(from_tags, from_vec);
    }

    #[test]
    fn deserialize_from_legacy_shape() {
        let json = r#"[["t","nostr"],["p","abc","wss://relay"],["e","ev"]]"#;
        let tags: NostrTags = serde_json::from_str(json).unwrap();
        assert_eq!(tags.len(), 3);
        assert_eq!(tags.row(1).unwrap().len(), 3);
        assert_eq!(tags.first_tagged_pubkey_ref(), Some("abc"));
    }

    #[test]
    fn iter_walks_every_row_in_order() {
        let mut tags = NostrTags::new();
        tags.add_custom_tag("a", "1");
        tags.add_custom_tag("b", "2");
        tags.add_custom_tag("c", "3");
        let firsts: Vec<&str> = tags
            .iter()
            .filter_map(|row| row.first().map(String::as_str))
            .collect();
        assert_eq!(firsts, vec!["a", "b", "c"]);
    }

    #[test]
    fn find_tags_collects_values() {
        let mut tags = NostrTags::new();
        tags.add_custom_tag("t", "rust");
        tags.add_custom_tag("t", "nostr");
        tags.add_custom_tag("z", "ignored");
        let found = tags.find_tags("t");
        assert_eq!(found, vec!["rust".to_string(), "nostr".to_string()]);
    }
}
