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
    /// Cheap, side-effect-free default. **Does not stamp `created_at`** —
    /// callers that want "now" use [`NostrNote::new`], the explicit
    /// constructors ([`text_note`](Self::text_note),
    /// [`metadata`](Self::metadata), [`with_kind`](Self::with_kind)), or the
    /// [`builder`](Self::builder), all of which call [`now()`](Self::now)
    /// for them.
    ///
    /// Why: `..Default::default()` is the common Rust shorthand for "the
    /// rest of the fields are uninteresting." Making it expensive (a
    /// syscall, or a JS bridge call on wasm) and racy was a foot-gun.
    fn default() -> Self {
        Self {
            pubkey: String::new(),
            created_at: 0,
            kind: 0,
            tags: NostrTags::default(),
            content: String::new(),
            id: None,
            sig: None,
        }
    }
}
impl NostrNote {
    /// Create an empty note with `created_at` stamped to the current time.
    ///
    /// Use this when you want "now" but no other field defaults — typical
    /// pattern is `NostrNote { kind: 13, content: ct, ..NostrNote::new() }`
    /// instead of `..Default::default()`, which used to stamp time as a
    /// hidden side effect and no longer does.
    #[must_use]
    pub fn new() -> Self {
        Self {
            created_at: Self::now(),
            ..Self::default()
        }
    }

    /// Create a builder for constructing a `NostrNote`. The builder stamps
    /// `created_at` to the current time; override with
    /// [`NostrNoteBuilder::timestamp`].
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
        NostrNoteBuilder { note: Self::new() }
    }

    /// Create a text note (kind 1) with the given content. `created_at` is
    /// stamped to the current time.
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
            ..Self::new()
        }
    }

    /// Create a metadata note (kind 0) with the given content. `created_at`
    /// is stamped to the current time.
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
            ..Self::new()
        }
    }

    /// Create a note with the specified kind. `created_at` is stamped to
    /// the current time.
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
            ..Self::new()
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
    #[cfg(any(feature = "k256", feature = "secp256k1"))]
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
    /// Decode the stored `pubkey` hex into raw bytes. Returns `None` if the
    /// field is not exactly 64 hex characters. Used by the curve-backend
    /// verifiers; gated to suppress the dead-code warning when neither curve
    /// feature is enabled (parse-only consumers like `nostro2-relay`).
    #[cfg(any(feature = "k256", feature = "secp256k1"))]
    #[inline]
    fn pubkey_bytes(&self) -> Option<[u8; 32]> {
        let mut out = [0_u8; 32];
        hex::decode_to_slice(self.pubkey.as_bytes(), &mut out).ok()?;
        Some(out)
    }

    /// Compute the SHA256 hash of the canonical event serialization directly,
    /// without allocating an intermediate JSON string.
    #[inline]
    fn compute_id_bytes(&self) -> Result<[u8; 32], crate::errors::NostrErrors> {
        use sha2::Digest as _;

        let serialized_data = (
            0,
            &*self.pubkey,
            self.created_at,
            self.kind,
            &self.tags,
            &*self.content,
        );
        let mut hasher = sha2::Sha256::new();
        serde_json::to_writer(Sha256Writer(&mut hasher), &serialized_data)?;
        Ok(hasher.finalize().into())
    }

    /// # Errors
    ///
    /// Will return `Err` if `serde` cannot serialize the data
    pub fn serialize_id(&mut self) -> Result<(), crate::errors::NostrErrors> {
        let hash = self.compute_id_bytes()?;
        self.id = Some(hex::encode(hash));
        Ok(())
    }

    /// Sign this note with the given signer, populating `pubkey`, `id`, and
    /// `sig` in place.
    ///
    /// # Errors
    /// Returns [`crate::errors::NostrErrors::SerdeError`] if id serialization
    /// fails, or [`crate::errors::NostrErrors::Signer`] wrapping the
    /// backend's [`nostro2_traits::SignerError`] if signing fails (hardware
    /// wallet rejection, NIP-46 transport error, etc.).
    pub fn sign_with<S: nostro2_traits::NostrSigner + ?Sized>(
        &mut self,
        signer: &S,
    ) -> Result<(), crate::errors::NostrErrors> {
        self.pubkey = signer.public_key();
        let id = self.serialize_id_raw()?;
        let sig = signer.sign_prehash(&id)?;
        self.sig = Some(hex::encode(sig));
        Ok(())
    }

    /// Compute and set the event ID, returning the raw 32-byte hash.
    ///
    /// This avoids a hex-decode round-trip when the caller needs the raw bytes
    /// immediately (e.g., for signing).
    ///
    /// # Errors
    ///
    /// Will return `Err` if `serde` cannot serialize the data
    pub fn serialize_id_raw(&mut self) -> Result<[u8; 32], crate::errors::NostrErrors> {
        let hash = self.compute_id_bytes()?;
        self.id = Some(hex::encode(hash));
        Ok(hash)
    }
    #[cfg(feature = "k256")]
    fn verify_signature(&self) -> Result<bool, crate::errors::NostrErrors> {
        use k256::schnorr::{signature::hazmat::PrehashVerifier, Signature, VerifyingKey};
        let id = self
            .id_bytes()
            .ok_or(crate::errors::NostrErrors::MissingId)?;
        let sig = self
            .sig_bytes()
            .ok_or(crate::errors::NostrErrors::MissingSignature)?;
        let pubkey = self
            .pubkey_bytes()
            .ok_or(crate::errors::NostrErrors::InvalidPublicKey)?;
        let verifying_key = VerifyingKey::from_bytes((&pubkey).into())
            .map_err(|_| crate::errors::NostrErrors::InvalidPublicKey)?;
        let signature = Signature::try_from(sig.as_slice())
            .map_err(|_| crate::errors::NostrErrors::InvalidSignature)?;
        Ok(verifying_key.verify_prehash(&id, &signature).is_ok())
    }

    #[cfg(feature = "secp256k1")]
    fn verify_signature(&self) -> Result<bool, crate::errors::NostrErrors> {
        use secp256k1::{schnorr::Signature, Message, XOnlyPublicKey, SECP256K1};
        let id = self
            .id_bytes()
            .ok_or(crate::errors::NostrErrors::MissingId)?;
        let sig_bytes = self
            .sig_bytes()
            .ok_or(crate::errors::NostrErrors::MissingSignature)?;
        let pubkey = self
            .pubkey_bytes()
            .ok_or(crate::errors::NostrErrors::InvalidPublicKey)?;
        let xonly = XOnlyPublicKey::from_slice(&pubkey)
            .map_err(|_| crate::errors::NostrErrors::InvalidPublicKey)?;
        let sig = Signature::from_slice(sig_bytes.as_slice())
            .map_err(|_| crate::errors::NostrErrors::InvalidSignature)?;
        let msg = Message::from_digest(id);
        Ok(SECP256K1.verify_schnorr(&sig, &msg, &xonly).is_ok())
    }
    /// Used to verify the content of the note
    ///
    /// Rebuilds the note and rehashes the content to verify the id.
    /// Compares raw bytes to avoid hex encoding overhead.
    /// Only used by `verify`, which is itself curve-feature-gated.
    #[cfg(any(feature = "k256", feature = "secp256k1"))]
    #[inline]
    fn verify_content(&self) -> bool {
        let Some(stored_id) = self.id_bytes() else {
            return false;
        };
        let Ok(computed_id) = self.compute_id_bytes() else {
            return false;
        };
        stored_id == computed_id
    }
    /// Verify the note's signature and content
    ///
    /// Returns true if both the signature and content hash are valid.
    /// Available only when a curve backend feature (`k256` or `secp256k1`)
    /// is enabled — parse-only consumers can build `nostro2` without one.
    #[cfg(any(feature = "k256", feature = "secp256k1"))]
    #[must_use]
    #[inline]
    pub fn verify(&self) -> bool {
        self.verify_content() && self.verify_signature().is_ok_and(|t| t)
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
/// Zero-alloc adapter: feeds `serde_json::to_writer` output directly into SHA-256.
/// Shared with `view::NostrNoteView::compute_id_bytes` so both id paths agree by
/// construction; a divergence here would silently fork the network.
#[allow(clippy::redundant_pub_crate)]
pub(crate) struct Sha256Writer<'a>(pub(crate) &'a mut sha2::Sha256);

impl std::io::Write for Sha256Writer<'_> {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest as _;
        self.0.update(buf);
        Ok(buf.len())
    }
    #[inline]
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
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
impl TryFrom<NostrNote> for serde_json::Value {
    type Error = crate::errors::NostrErrors;
    fn try_from(note: NostrNote) -> Result<Self, Self::Error> {
        Ok(serde_json::to_value(note)?)
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
#[derive(Debug)]
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
impl TryFrom<NostrNote> for js_sys::wasm_bindgen::JsValue {
    // `js_sys::Error` is the natural error type on the wasm boundary —
    // it lands as a JS `Error` in the host without needing a separate
    // wrapper enum. Callers convert via `?` in functions returning
    // `Result<_, JsValue>`.
    type Error = js_sys::wasm_bindgen::JsValue;
    fn try_from(note: NostrNote) -> Result<Self, Self::Error> {
        let json = serde_json::to_string(&note).map_err(|e| {
            Self::Error::from(js_sys::Error::new(&format!("serialize NostrNote: {e}")))
        })?;
        js_sys::JSON::parse(&json).map_err(|_| {
            Self::Error::from(js_sys::Error::new("parse NostrNote JSON in JS engine"))
        })
    }
}
