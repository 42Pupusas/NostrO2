//! Tag types and collections for Nostr notes
//!
//! This module provides types for working with Nostr tags as specified in NIP-01.
//! Tags are used to add metadata, references, and relationships to notes.
//!
//! # Examples
//!
//! ```rust
//! use nostro2::NostrTags;
//!
//! let tags = NostrTags::new()
//!     .with_pubkey("abc123...", None)
//!     .with_event("event123...")
//!     .with_tag("t", "nostr");
//! ```

/// Tag type identifiers for Nostr protocol
#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, PartialEq, Eq, Hash)]
pub enum NostrTag {
    #[serde(rename = "p")]
    Pubkey,
    #[serde(rename = "e")]
    Event,
    #[serde(rename = "d")]
    Parameterized,
    Custom(std::borrow::Cow<'static, str>),
    Relay,
}
impl std::str::FromStr for NostrTag {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "p" => Ok(Self::Pubkey),
            "e" => Ok(Self::Event),
            "d" => Ok(Self::Parameterized),
            _ => Ok(Self::Custom(std::borrow::Cow::Owned(s.to_owned()))),
        }
    }
}
impl AsRef<str> for NostrTag {
    fn as_ref(&self) -> &str {
        match self {
            Self::Pubkey => "p",
            Self::Event => "e",
            Self::Parameterized => "d",
            Self::Custom(tag) => tag.as_ref(),
            Self::Relay => "r",
        }
    }
}

/// Collection of tags attached to a Nostr note
///
/// Tags are represented as a vector of string vectors, where each inner vector
/// represents a single tag with the tag type as the first element.
///
/// # Examples
///
/// ```rust
/// use nostro2::NostrTags;
///
/// // Create tags using the builder pattern
/// let tags = NostrTags::new()
///     .with_pubkey("abc123...", None)
///     .with_event("event123...")
///     .with_tag("t", "nostr");
///
/// // Or convert from Vec<Vec<String>>
/// let raw = vec![
///     vec!["p".to_string(), "abc123...".to_string()],
///     vec!["e".to_string(), "event123...".to_string()],
/// ];
/// let tags: NostrTags = raw.into();
/// ```
#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, PartialEq, Eq, Hash, Default)]
pub struct NostrTags(pub Vec<Vec<String>>);

impl AsRef<[Vec<String>]> for NostrTags {
    fn as_ref(&self) -> &[Vec<String>] {
        &self.0
    }
}

impl AsMut<[Vec<String>]> for NostrTags {
    fn as_mut(&mut self) -> &mut [Vec<String>] {
        &mut self.0
    }
}

impl std::ops::Deref for NostrTags {
    type Target = Vec<Vec<String>>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for NostrTags {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl From<Vec<Vec<String>>> for NostrTags {
    fn from(tags: Vec<Vec<String>>) -> Self {
        Self(tags)
    }
}

impl From<NostrTags> for Vec<Vec<String>> {
    fn from(tags: NostrTags) -> Self {
        tags.0
    }
}

impl NostrTags {
    /// Create a new empty tags collection
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Chainable method to add a pubkey tag
    #[must_use]
    pub fn with_pubkey(mut self, pubkey: &str, relay: Option<&str>) -> Self {
        self.add_pubkey_tag(pubkey, relay);
        self
    }

    /// Chainable method to add an event tag
    #[must_use]
    pub fn with_event(mut self, event_id: &str) -> Self {
        self.add_event_tag(event_id);
        self
    }

    /// Chainable method to add a custom tag
    #[must_use]
    pub fn with_tag(mut self, tag_type: &str, value: &str) -> Self {
        self.add_custom_tag(tag_type, value);
        self
    }

    /// Add a relay tag
    pub fn add_relay_tag(&mut self, url: &str) {
        self.0.push(vec!["r".to_owned(), url.to_owned()]);
    }

    pub fn add_custom_tag(&mut self, tag_type: &str, tag: &str) {
        let tags = vec![tag_type.to_owned(), tag.to_owned()];
        self.0.push(tags);
    }
    pub fn add_pubkey_tag(&mut self, pubkey: &str, relay: Option<&str>) {
        let mut tags = vec!["p".to_owned(), pubkey.to_owned()];
        if let Some(relay) = relay {
            tags.push(relay.to_owned());
        }
        self.0.push(tags);
    }
    pub fn add_event_tag(&mut self, event_id: &str) {
        let tags = vec!["e".to_owned(), event_id.to_owned()];
        self.0.push(tags);
    }
    pub fn add_parameter_tag(&mut self, parameter: &str) {
        let tags = vec!["d".to_owned(), parameter.to_owned()];
        self.0.push(tags);
    }
    #[must_use]
    #[inline]
    pub fn first_tagged_pubkey(&self) -> Option<String> {
        self.first_tagged_pubkey_ref().map(String::from)
    }
    #[must_use]
    #[inline]
    pub fn first_tagged_pubkey_ref(&self) -> Option<&str> {
        self.0
            .iter()
            .find(|tag_list| tag_list.first().is_some_and(|tag| tag == "p"))
            .and_then(|tag_list| tag_list.get(1).map(String::as_str))
    }
    #[must_use]
    #[inline]
    pub fn first_tagged_event(&self) -> Option<String> {
        self.first_tagged_event_ref().map(String::from)
    }
    #[must_use]
    #[inline]
    pub fn first_tagged_event_ref(&self) -> Option<&str> {
        self.0
            .iter()
            .find(|tag_list| tag_list.first().is_some_and(|tag| tag == "e"))
            .and_then(|tag_list| tag_list.get(1).map(String::as_str))
    }
    #[must_use]
    #[inline]
    pub fn first_parameter(&self) -> Option<String> {
        self.first_parameter_ref().map(String::from)
    }
    #[must_use]
    #[inline]
    pub fn first_parameter_ref(&self) -> Option<&str> {
        self.0
            .iter()
            .find(|tag_list| tag_list.first().is_some_and(|tag| tag == "d"))
            .and_then(|tag_list| tag_list.get(1).map(String::as_str))
    }
    #[must_use]
    #[inline]
    pub fn find_tags(&self, tag_type: &str) -> Vec<String> {
        self.0
            .iter()
            .filter(|tag_list| tag_list.first().is_some_and(|tag| tag == tag_type))
            .flat_map(|tag_list| tag_list.iter().cloned())
            .skip(1)
            .collect()
    }
    #[must_use]
    #[inline]
    pub fn find_tags_ref(&self, tag_type: &str) -> Vec<&str> {
        self.0
            .iter()
            .filter(|tag_list| tag_list.first().is_some_and(|tag| tag == tag_type))
            .flat_map(|tag_list| tag_list.iter().skip(1).map(String::as_str))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tags_deref() {
        let mut tags = NostrTags::new();
        tags.push(vec!["p".to_string(), "abc123".to_string()]);

        // Should be able to use Vec methods via Deref
        assert_eq!(tags.len(), 1);
        assert!(!tags.is_empty());
        assert_eq!(tags[0][0], "p");
    }

    #[test]
    fn test_tags_from_vec() {
        let raw_tags = vec![
            vec!["p".to_string(), "abc123".to_string()],
            vec!["e".to_string(), "event123".to_string()],
        ];

        let tags: NostrTags = raw_tags.clone().into();
        assert_eq!(tags.len(), 2);
        assert_eq!(tags.as_ref(), raw_tags.as_slice());
    }

    #[test]
    fn test_tags_builder() {
        let tags = NostrTags::new()
            .with_pubkey("abc123", None)
            .with_event("event123")
            .with_tag("t", "test");

        assert_eq!(tags.len(), 3);
        assert_eq!(tags.first_tagged_pubkey_ref(), Some("abc123"));
        assert_eq!(tags.first_tagged_event_ref(), Some("event123"));
    }

    #[test]
    fn test_add_relay_tag() {
        let mut tags = NostrTags::new();
        tags.add_relay_tag("wss://relay.example.com");

        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0][0], "r");
        assert_eq!(tags[0][1], "wss://relay.example.com");
    }
}
