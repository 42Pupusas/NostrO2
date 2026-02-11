use crate::tags::NostrTags;

/// A Nostr note (event) as defined by NIP-01
///
/// Notes are the fundamental data structure in the Nostr protocol. They represent
/// all types of events including text notes, metadata, direct messages, and more.
///
/// # Structure
///
/// - `pubkey`: Author's public key (32-byte hex string)
/// - `created_at`: Unix timestamp in seconds
/// - `kind`: Event type (see [NIP-01](https://github.com/nostr-protocol/nips/blob/master/01.md))
/// - `tags`: Array of tags for metadata and references
/// - `content`: Event content (format depends on kind)
/// - `id`: Event ID (SHA256 hash of serialized event)
/// - `sig`: Schnorr signature over the event ID
///
/// # Examples
///
/// ```rust
/// use nostro2::NostrNote;
///
/// // Simple text note
/// let note = NostrNote::text_note("Hello, Nostr!");
///
/// // Builder pattern
/// let note = NostrNote::builder()
///     .content("Hello!")
///     .kind(1)
///     .tag_pubkey("abc123...")
///     .build();
/// ```
#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, PartialEq, Eq, Hash)]
pub struct NostrNote {
    pub pubkey: String,
    pub created_at: i64,
    pub kind: u32,
    pub tags: NostrTags,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
}
impl Default for NostrNote {
    fn default() -> Self {
        Self {
            pubkey: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                .try_into()
                .unwrap_or(0),
            #[cfg(target_arch = "wasm32")]
            #[allow(clippy::cast_possible_truncation)]
            created_at: (js_sys::Date::now() / 1000.0) as i64,
            kind: 1,
            tags: NostrTags::default(),
            content: String::new(),
            id: None,
            sig: None,
        }
    }
}
impl NostrNote {
    /// Create a builder for constructing a `NostrNote`
    ///
    /// # Example
    ///
    /// ```
    /// use nostro2::NostrNote;
    ///
    /// let note = NostrNote::builder()
    ///     .content("Hello, Nostr!")
    ///     .kind(1)
    ///     .build();
    /// ```
    #[must_use]
    pub fn builder() -> NostrNoteBuilder {
        NostrNoteBuilder::default()
    }

    /// Create a text note (kind 1) with the given content
    ///
    /// # Example
    ///
    /// ```
    /// use nostro2::NostrNote;
    ///
    /// let note = NostrNote::text_note("Hello, Nostr!");
    /// assert_eq!(note.kind, 1);
    /// ```
    #[must_use]
    pub fn text_note(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            kind: 1,
            ..Default::default()
        }
    }

    /// Create a metadata note (kind 0) with the given content
    ///
    /// # Example
    ///
    /// ```
    /// use nostro2::NostrNote;
    ///
    /// let metadata = r#"{"name":"Alice","about":"Nostr user"}"#;
    /// let note = NostrNote::metadata(metadata);
    /// assert_eq!(note.kind, 0);
    /// ```
    #[must_use]
    pub fn metadata(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            kind: 0,
            ..Default::default()
        }
    }

    /// Create a note with the specified kind
    ///
    /// # Example
    ///
    /// ```
    /// use nostro2::NostrNote;
    ///
    /// let note = NostrNote::with_kind(4); // Encrypted DM
    /// assert_eq!(note.kind, 4);
    /// ```
    #[must_use]
    pub fn with_kind(kind: u32) -> Self {
        Self {
            kind,
            ..Default::default()
        }
    }

    /// Get the current timestamp in the appropriate format for the platform
    ///
    /// Returns Unix timestamp (seconds since epoch)
    #[must_use]
    pub fn now() -> i64 {
        #[cfg(not(target_arch = "wasm32"))]
        {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|d| i64::try_from(d.as_secs()).ok())
                .unwrap_or(0)
        }
        #[cfg(target_arch = "wasm32")]
        #[allow(clippy::cast_possible_truncation)]
        {
            (js_sys::Date::now() / 1000.0) as i64
        }
    }

    /// Set the timestamp and return self for chaining
    ///
    /// # Example
    ///
    /// ```
    /// use nostro2::NostrNote;
    ///
    /// let note = NostrNote::text_note("Hello")
    ///     .with_timestamp(1234567890);
    /// assert_eq!(note.created_at, 1234567890);
    /// ```
    #[must_use]
    pub const fn with_timestamp(mut self, timestamp: i64) -> Self {
        self.created_at = timestamp;
        self
    }

    /// Set the content and return self for chaining
    #[must_use]
    pub fn with_content(mut self, content: impl Into<String>) -> Self {
        self.content = content.into();
        self
    }

    #[must_use]
    #[inline]
    pub fn id_bytes(&self) -> Option<[u8; 32]> {
        let mut id_bytes = [0_u8; 32];
        let id = hex::decode(self.id.as_ref()?).ok()?;
        if id.len() != 32 {
            return None;
        }
        id_bytes.copy_from_slice(&id);
        Some(id_bytes)
    }
    /// Returns the signature as a byte array
    #[inline]
    fn sig_bytes(&self) -> Option<[u8; 64]> {
        let mut sig_bytes = [0_u8; 64];
        let sig = hex::decode(self.sig.as_ref()?).ok()?;
        if sig.len() != 64 {
            return None;
        }
        sig_bytes.copy_from_slice(&sig);
        Some(sig_bytes)
    }
    /// Returns the public key as a byte array
    #[inline]
    fn pubkey_bytes(&self) -> [u8; 32] {
        let mut pubkey_bytes = [0_u8; 32];
        let pubkey = hex::decode(&self.pubkey).unwrap_or_default();
        if pubkey.len() != 32 {
            return pubkey_bytes;
        }
        pubkey_bytes.copy_from_slice(&pubkey);
        pubkey_bytes
    }

    /// # Errors
    ///
    /// Will return `Err` if `serde` cannot serialize the data
    pub fn serialize_id(&mut self) -> Result<(), crate::errors::NostrErrors> {
        use sha2::Digest as _;

        let serialized_data = (
            0,
            &*self.pubkey,
            self.created_at,
            self.kind,
            &self.tags,
            &*self.content,
        );
        let json_str = serde_json::to_string(&serialized_data)?;
        let mut hasher = sha2::Sha256::new();
        hasher.update(json_str.as_bytes());
        self.id = Some(hex::encode(hasher.finalize()));
        Ok(())
    }
    /// Used to verify the signature of the note
    ///
    /// Verifies the signature of the note using the k256 library (pure Rust)
    fn verify_signature(&self) -> Result<bool, crate::errors::NostrErrors> {
        use k256::schnorr::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey};
        let id = self
            .id_bytes()
            .ok_or(crate::errors::NostrErrors::MissingId)?;
        let sig = self
            .sig_bytes()
            .ok_or(crate::errors::NostrErrors::MissingSignature)?;
        let verifying_key = VerifyingKey::from_bytes(&self.pubkey_bytes())
            .map_err(|_| crate::errors::NostrErrors::InvalidPublicKey)?;
        let signature = Signature::try_from(sig.as_slice())
            .map_err(|_| crate::errors::NostrErrors::InvalidSignature)?;
        Ok(verifying_key.verify_prehash(&id, &signature).is_ok())
    }
    /// Used to verify the content of the note
    ///
    /// Rebuilds the note and rehashes the content to verify the id
    #[inline]
    fn verify_content(&self) -> bool {
        use sha2::Digest as _;

        let serialized_data = (
            0,
            &*self.pubkey,
            self.created_at,
            self.kind,
            &self.tags,
            &*self.content,
        );
        let Ok(json_str) = serde_json::to_string(&serialized_data) else {
            return false;
        };
        let mut hasher = sha2::Sha256::new();
        hasher.update(json_str.as_bytes());
        let computed_id = hex::encode(hasher.finalize());
        self.id.as_ref() == Some(&computed_id)
    }
    /// Verify the note's signature and content
    ///
    /// Returns true if both the signature and content hash are valid
    #[must_use]
    #[inline]
    pub fn verify(&self) -> bool {
        self.verify_signature().is_ok_and(|t| t) && self.verify_content()
    }
    /// Creates a JSON encoded string from the `NostrNote` struct
    ///
    /// # Errors
    ///
    /// Will return `Err` if `serde` cannot serialize the data,
    /// but because of data types should never realistically fail.
    pub fn serialize(&self) -> Result<String, crate::errors::NostrErrors> {
        Ok(serde_json::to_string(self)?)
    }
}
impl core::str::FromStr for NostrNote {
    type Err = crate::errors::NostrErrors;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(serde_json::from_str(s)?)
    }
}
impl TryFrom<serde_json::Value> for NostrNote {
    type Error = crate::errors::NostrErrors;
    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        Ok(serde_json::from_value(value)?)
    }
}
impl TryFrom<&serde_json::Value> for NostrNote {
    type Error = crate::errors::NostrErrors;
    fn try_from(value: &serde_json::Value) -> Result<Self, Self::Error> {
        Ok(serde_json::from_value(value.clone())?)
    }
}
impl From<NostrNote> for serde_json::Value {
    fn from(note: NostrNote) -> Self {
        serde_json::to_value(note).expect("Failed to serialize NostrNote.")
    }
}
/// Builder for constructing `NostrNote` instances
///
/// # Example
///
/// ```
/// use nostro2::NostrNote;
///
/// let note = NostrNote::builder()
///     .content("Hello, Nostr!")
///     .kind(1)
///     .tag_pubkey("abc123...")
///     .build();
/// ```
#[derive(Debug, Default)]
pub struct NostrNoteBuilder {
    note: NostrNote,
}

impl NostrNoteBuilder {
    /// Set the content of the note
    #[must_use]
    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.note.content = content.into();
        self
    }

    /// Set the kind of the note
    #[must_use]
    pub const fn kind(mut self, kind: u32) -> Self {
        self.note.kind = kind;
        self
    }

    /// Set the timestamp of the note
    #[must_use]
    pub const fn timestamp(mut self, timestamp: i64) -> Self {
        self.note.created_at = timestamp;
        self
    }

    /// Add a pubkey tag (p-tag)
    #[must_use]
    pub fn tag_pubkey(mut self, pubkey: &str) -> Self {
        self.note.tags.add_pubkey_tag(pubkey, None);
        self
    }

    /// Add a pubkey tag with a relay hint
    #[must_use]
    pub fn tag_pubkey_with_relay(mut self, pubkey: &str, relay: &str) -> Self {
        self.note.tags.add_pubkey_tag(pubkey, Some(relay));
        self
    }

    /// Add an event tag (e-tag)
    #[must_use]
    pub fn tag_event(mut self, event_id: &str) -> Self {
        self.note.tags.add_event_tag(event_id);
        self
    }

    /// Add a parameter tag (d-tag)
    #[must_use]
    pub fn tag_parameter(mut self, parameter: &str) -> Self {
        self.note.tags.add_parameter_tag(parameter);
        self
    }

    /// Add a custom tag
    #[must_use]
    pub fn tag(mut self, tag_type: &str, value: &str) -> Self {
        self.note.tags.add_custom_tag(tag_type, value);
        self
    }

    /// Add a relay tag (r-tag)
    #[must_use]
    pub fn tag_relay(mut self, url: &str) -> Self {
        self.note.tags.add_relay_tag(url);
        self
    }

    /// Build the `NostrNote`
    #[must_use]
    pub fn build(self) -> NostrNote {
        self.note
    }
}

#[cfg(target_arch = "wasm32")]
impl From<NostrNote> for js_sys::wasm_bindgen::JsValue {
    fn from(note: NostrNote) -> Self {
        let json = serde_json::to_string(&note).expect("Failed to serialize NostrNote.");
        js_sys::JSON::parse(&json).expect("Failed to parse NostrNote.")
    }
}
