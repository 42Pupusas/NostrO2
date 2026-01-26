use crate::tags::NostrTags;

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
    #[must_use]
    pub fn get_note_hrp(&self) -> Option<String> {
        let hrp = bech32::Hrp::parse("note").ok()?;
        let note_data = self.id.as_ref()?;
        let string = bech32::encode::<bech32::Bech32>(hrp, note_data.as_bytes()).ok()?;
        Some(string)
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
    /// Verifies the signature of the note using the secp256k1 library
    fn verify_signature(&self) -> Result<bool, crate::errors::NostrErrors> {
        use secp256k1::{schnorr, Secp256k1, XOnlyPublicKey};
        let secp = Secp256k1::verification_only();
        let id = self
            .id_bytes()
            .ok_or(crate::errors::NostrErrors::MissingId)?;
        let sig = self
            .sig_bytes()
            .ok_or(crate::errors::NostrErrors::MissingSignature)?;
        let public_key = XOnlyPublicKey::from_slice(&self.pubkey_bytes())?;
        let signature = schnorr::Signature::from_byte_array(sig);
        Ok(secp.verify_schnorr(&signature, &id, &public_key).is_ok())
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
#[cfg(target_arch = "wasm32")]
impl From<NostrNote> for js_sys::wasm_bindgen::JsValue {
    fn from(note: NostrNote) -> Self {
        let json = serde_json::to_string(&note).expect("Failed to serialize NostrNote.");
        js_sys::JSON::parse(&json).expect("Failed to parse NostrNote.")
    }
}
