use crate::event::NostrEvent;
use crate::tags::NostrTags;
use bourne::ToJson;
use nostro2_traits::hex::Hexable;

bourne::json! {
    #[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
    pub struct NostrNote {
        pub pubkey: String,
        pub created_at: i64,
        pub kind: u32,
        #[bourne(default)]
        pub tags: NostrTags,
        pub content: String,
        #[bourne(skip_if_none)]
        pub id: Option<String>,
        #[bourne(skip_if_none)]
        pub sig: Option<String>,
    }
}

impl NostrNote {
    /// # Errors
    ///
    /// Will return `Err` if serialization fails.
    pub fn serialize_id(&mut self) -> Result<(), crate::errors::NostrErrors> {
        let hash = self.compute_id_bytes();
        self.id = Some(Hexable::to_hex(&hash));
        Ok(())
    }

    /// # Errors
    ///
    /// Returns [`crate::errors::NostrErrors::Signer`] if signing fails.
    pub fn sign_with<S: nostro2_traits::NostrSigner + ?Sized>(
        &mut self,
        signer: &S,
    ) -> Result<(), crate::errors::NostrErrors> {
        self.pubkey = signer.public_key();
        let id = self.serialize_id_raw();
        let sig = signer.sign_prehash(&id)?;
        self.sig = Some(Hexable::to_hex(&sig));
        Ok(())
    }

    pub fn serialize_id_raw(&mut self) -> [u8; 32] {
        let hash = self.compute_id_bytes();
        self.id = Some(Hexable::to_hex(&hash));
        hash
    }

    /// # Errors
    ///
    /// Returns [`crate::errors::NostrErrors::JsonError`] if serialization fails.
    pub fn serialize(&self) -> Result<String, crate::errors::NostrErrors> {
        Ok(bourne::to_string(self)?)
    }
}

impl NostrEvent for NostrNote {
    fn pubkey_str(&self) -> std::borrow::Cow<'_, str> { std::borrow::Cow::Borrowed(&self.pubkey) }
    fn created_at(&self) -> i64 { self.created_at }
    fn kind(&self) -> u32 { self.kind }
    fn content_str(&self) -> std::borrow::Cow<'_, str> { std::borrow::Cow::Borrowed(&self.content) }
    fn id_hex(&self) -> Option<std::borrow::Cow<'_, str>> { self.id.as_deref().map(std::borrow::Cow::Borrowed) }
    fn sig_hex(&self) -> Option<std::borrow::Cow<'_, str>> { self.sig.as_deref().map(std::borrow::Cow::Borrowed) }
    fn write_tags<W: bourne::JsonWrite + ?Sized>(&self, sink: &mut W) -> Result<(), W::Error> { self.tags.write_json(sink) }
}

impl core::str::FromStr for NostrNote {
    type Err = crate::errors::NostrErrors;
    fn from_str(s: &str) -> Result<Self, Self::Err> { Ok(bourne::parse_str(s)?) }
}

// ── Builder ───────────────────────────────────────────────────────

#[derive(Debug)]
pub struct NostrNoteBuilder {
    note: NostrNote,
}

impl Default for NostrNoteBuilder {
    fn default() -> Self { Self::new() }
}

impl NostrNoteBuilder {
    /// Current Unix timestamp in seconds.
    fn now() -> i64 {
        #[cfg(not(target_arch = "wasm32"))]
        { std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().and_then(|d| i64::try_from(d.as_secs()).ok()).unwrap_or(0) }
        #[cfg(target_arch = "wasm32")]
        #[allow(clippy::cast_possible_truncation)]
        { (js_sys::Date::now() / 1000.0) as i64 }
    }

    #[must_use]
    pub fn new() -> Self {
        Self { note: NostrNote { created_at: Self::now(), ..Default::default() } }
    }

    /// Start building a text note (kind 1).
    #[must_use]
    pub fn text_note(content: impl Into<String>) -> Self {
        Self { note: NostrNote { content: content.into(), kind: 1, created_at: Self::now(), ..Default::default() } }
    }

    /// Start building a metadata note (kind 0).
    #[must_use]
    pub fn metadata(content: impl Into<String>) -> Self {
        Self { note: NostrNote { content: content.into(), kind: 0, created_at: Self::now(), ..Default::default() } }
    }

    #[must_use] pub fn content(mut self, content: impl Into<String>) -> Self { self.note.content = content.into(); self }
    #[must_use] pub const fn kind(mut self, kind: u32) -> Self { self.note.kind = kind; self }
    #[must_use] pub const fn timestamp(mut self, timestamp: i64) -> Self { self.note.created_at = timestamp; self }
    #[must_use] pub fn tag_pubkey(mut self, pubkey: &str) -> Self { self.note.tags.add_pubkey_tag(pubkey, None); self }
    #[must_use] pub fn tag_pubkey_with_relay(mut self, pubkey: &str, relay: &str) -> Self { self.note.tags.add_pubkey_tag(pubkey, Some(relay)); self }
    #[must_use] pub fn tag_event(mut self, event_id: &str) -> Self { self.note.tags.add_event_tag(event_id); self }
    #[must_use] pub fn tag_parameter(mut self, parameter: &str) -> Self { self.note.tags.add_parameter_tag(parameter); self }
    #[must_use] pub fn tag(mut self, tag_type: &str, value: &str) -> Self { self.note.tags.add_custom_tag(tag_type, value); self }
    #[must_use] pub fn tag_relay(mut self, url: &str) -> Self { self.note.tags.add_relay_tag(url); self }
    #[must_use] pub fn build(self) -> NostrNote { self.note }
}
