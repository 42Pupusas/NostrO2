use crate::tags::NostrTags;
use bourne::{JsonWrite, ToJson};

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
    #[must_use]
    pub fn new() -> Self {
        Self {
            created_at: Self::now(),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn builder() -> NostrNoteBuilder {
        NostrNoteBuilder { note: Self::new() }
    }

    #[must_use]
    pub fn text_note(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            kind: 1,
            ..Self::new()
        }
    }

    #[must_use]
    pub fn metadata(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            kind: 0,
            ..Self::new()
        }
    }

    #[must_use]
    pub fn with_kind(kind: u32) -> Self {
        Self {
            kind,
            ..Self::new()
        }
    }

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

    #[must_use]
    pub const fn with_timestamp(mut self, timestamp: i64) -> Self {
        self.created_at = timestamp;
        self
    }

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

    #[cfg(any(feature = "k256", feature = "secp256k1"))]
    #[inline]
    fn pubkey_bytes(&self) -> Option<[u8; 32]> {
        let mut out = [0_u8; 32];
        hex::decode_to_slice(self.pubkey.as_bytes(), &mut out).ok()?;
        Some(out)
    }

    #[inline]
    fn compute_id_bytes(&self) -> [u8; 32] {
        use sha2::Digest as _;

        let mut hasher = sha2::Sha256::new();
        let mut sink = Sha256Sink(&mut hasher);

        let _: Result<(), core::convert::Infallible> = (|| {
            sink.write_byte(b'[')?;
            sink.write_int_i64(0)?;
            sink.write_byte(b',')?;
            sink.write_escaped_str(&self.pubkey)?;
            sink.write_byte(b',')?;
            sink.write_int_i64(self.created_at)?;
            sink.write_byte(b',')?;
            sink.write_int_u64(u64::from(self.kind))?;
            sink.write_byte(b',')?;
            self.tags.write_json(&mut sink)?;
            sink.write_byte(b',')?;
            sink.write_escaped_str(&self.content)?;
            sink.write_byte(b']')
        })();

        hasher.finalize().into()
    }

    /// # Errors
    ///
    /// Will return `Err` if serialization fails
    pub fn serialize_id(&mut self) -> Result<(), crate::errors::NostrErrors> {
        let hash = self.compute_id_bytes();
        self.id = Some(hex::encode(hash));
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
        self.sig = Some(hex::encode(sig));
        Ok(())
    }

    pub fn serialize_id_raw(&mut self) -> [u8; 32] {
        let hash = self.compute_id_bytes();
        self.id = Some(hex::encode(hash));
        hash
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

    #[cfg(any(feature = "k256", feature = "secp256k1"))]
    #[inline]
    fn verify_content(&self) -> bool {
        let Some(stored_id) = self.id_bytes() else {
            return false;
        };
        stored_id == self.compute_id_bytes()
    }

    #[cfg(any(feature = "k256", feature = "secp256k1"))]
    #[must_use]
    #[inline]
    pub fn verify(&self) -> bool {
        self.verify_content() && self.verify_signature().is_ok_and(|t| t)
    }

    /// # Errors
    ///
    /// Returns [`crate::errors::NostrErrors::JsonError`] if serialization fails.
    pub fn serialize(&self) -> Result<String, crate::errors::NostrErrors> {
        Ok(bourne::to_string(self)?)
    }
}

/// Zero-alloc adapter: feeds bourne `JsonWrite` output directly into SHA-256.
#[allow(clippy::redundant_pub_crate)]
pub(crate) struct Sha256Sink<'a>(pub(crate) &'a mut sha2::Sha256);

impl JsonWrite for Sha256Sink<'_> {
    type Error = core::convert::Infallible;

    #[inline]
    fn write_byte(&mut self, b: u8) -> Result<(), Self::Error> {
        use sha2::Digest as _;
        self.0.update([b]);
        Ok(())
    }

    #[inline]
    fn write_str_raw(&mut self, s: &str) -> Result<(), Self::Error> {
        use sha2::Digest as _;
        self.0.update(s.as_bytes());
        Ok(())
    }

    #[inline]
    fn write_float_f64(&mut self, f: f64) -> Result<(), Self::Error> {
        use sha2::Digest as _;
        use std::io::Write as _;
        let mut buf = [0_u8; 24];
        let n = write!(&mut buf[..], "{f}").map_or(0, |()| {
            buf.iter().position(|&b| b == 0).unwrap_or(buf.len())
        });
        self.0.update(&buf[..n]);
        Ok(())
    }
}

impl core::str::FromStr for NostrNote {
    type Err = crate::errors::NostrErrors;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(bourne::parse_str(s)?)
    }
}

#[cfg(target_arch = "wasm32")]
impl TryFrom<NostrNote> for js_sys::wasm_bindgen::JsValue {
    type Error = js_sys::wasm_bindgen::JsValue;
    fn try_from(note: NostrNote) -> Result<Self, Self::Error> {
        let json = bourne::to_string(&note).map_err(|e| {
            Self::Error::from(js_sys::Error::new(&format!("serialize NostrNote: {e}")))
        })?;
        js_sys::JSON::parse(&json).map_err(|_| {
            Self::Error::from(js_sys::Error::new("parse NostrNote JSON in JS engine"))
        })
    }
}

#[derive(Debug)]
pub struct NostrNoteBuilder {
    note: NostrNote,
}

impl NostrNoteBuilder {
    #[must_use]
    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.note.content = content.into();
        self
    }

    #[must_use]
    pub const fn kind(mut self, kind: u32) -> Self {
        self.note.kind = kind;
        self
    }

    #[must_use]
    pub const fn timestamp(mut self, timestamp: i64) -> Self {
        self.note.created_at = timestamp;
        self
    }

    #[must_use]
    pub fn tag_pubkey(mut self, pubkey: &str) -> Self {
        self.note.tags.add_pubkey_tag(pubkey, None);
        self
    }

    #[must_use]
    pub fn tag_pubkey_with_relay(mut self, pubkey: &str, relay: &str) -> Self {
        self.note.tags.add_pubkey_tag(pubkey, Some(relay));
        self
    }

    #[must_use]
    pub fn tag_event(mut self, event_id: &str) -> Self {
        self.note.tags.add_event_tag(event_id);
        self
    }

    #[must_use]
    pub fn tag_parameter(mut self, parameter: &str) -> Self {
        self.note.tags.add_parameter_tag(parameter);
        self
    }

    #[must_use]
    pub fn tag(mut self, tag_type: &str, value: &str) -> Self {
        self.note.tags.add_custom_tag(tag_type, value);
        self
    }

    #[must_use]
    pub fn tag_relay(mut self, url: &str) -> Self {
        self.note.tags.add_relay_tag(url);
        self
    }

    #[must_use]
    pub fn build(self) -> NostrNote {
        self.note
    }
}
