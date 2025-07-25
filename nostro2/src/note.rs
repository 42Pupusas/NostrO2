use crate::tags::NostrTags;
use std::fmt::Write as _;

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
    pub fn id_bytes(&self) -> Option<[u8; 32]> {
        let mut id_bytes = [0_u8; 32];
        let id = Self::hex_decode(self.id.as_ref()?);
        if id.len() != 32 {
            return None;
        }
        id_bytes.copy_from_slice(&id);
        Some(id_bytes)
    }
    /// Returns the signature as a byte array
    fn sig_bytes(&self) -> Option<[u8; 64]> {
        let mut sig_bytes = [0_u8; 64];
        let sig = Self::hex_decode(self.sig.as_ref()?);
        if sig.len() != 64 {
            return None;
        }
        sig_bytes.copy_from_slice(&sig);
        Some(sig_bytes)
    }
    /// Returns the public key as a byte array
    fn pubkey_bytes(&self) -> [u8; 32] {
        let mut pubkey_bytes = [0_u8; 32];
        let pubkey = Self::hex_decode(&self.pubkey);
        if pubkey.len() != 32 {
            return pubkey_bytes;
        }
        pubkey_bytes.copy_from_slice(&pubkey);
        pubkey_bytes
    }

    /// # Errors
    ///
    /// Will return `Err` if `serde` cannot serialize the data
    pub fn serialize_id(&mut self) -> Result<(), Box<dyn std::error::Error>> {
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
        self.id = Some(
            hasher
                .finalize()
                .iter()
                .fold(String::new(), |mut acc, byte| {
                    write!(acc, "{byte:02x}").unwrap();
                    // acc.push_str(&format!("{byte:02x}"));
                    acc
                }),
        );
        Ok(())
    }
    /// Used to verify the signature of the note
    ///
    /// Verifies the signature of the note using the secp256k1 library
    fn verify_signature(&self) -> Result<bool, crate::errors::NostrErrors> {
        use secp256k1::{schnorr, Secp256k1, XOnlyPublicKey};
        let secp = Secp256k1::verification_only();
        let id = self.id_bytes().ok_or("Failed to get id bytes.")?;
        let sig = self.sig_bytes().ok_or("Failed to get signature bytes.")?;
        let public_key = XOnlyPublicKey::from_slice(&self.pubkey_bytes())?;
        let signature = schnorr::Signature::from_byte_array(sig);
        Ok(secp.verify_schnorr(&signature, &id, &public_key).is_ok())
    }
    /// Used to verify the content of the note
    ///
    /// Rebuilds the note and rehashes the content to verify the id
    fn verify_content(&self) -> bool {
        let mut copied_note = Self {
            content: self.content.to_string(),
            pubkey: self.pubkey.to_string(),
            created_at: self.created_at,
            kind: self.kind,
            tags: self.tags.clone(),
            ..Default::default()
        };
        if copied_note.serialize_id().is_err() {
            return false;
        }
        self.id == copied_note.id
    }
    #[must_use]
    pub fn verify(&self) -> bool {
        self.verify_signature().is_ok_and(|t| t) && self.verify_content()
    }
    /// Generic function to decode a hex string into a byte vector
    fn hex_decode(hex_string: &str) -> Vec<u8> {
        hex_string
            .as_bytes()
            .chunks(2)
            .filter_map(|b| u8::from_str_radix(core::str::from_utf8(b).ok()?, 16).ok())
            .collect()
    }
    /// Creates a JSON encoded string from the `NostrNote` struct
    ///
    /// # Errors
    ///
    /// Will return `Err` if `serde` cannot serialize the data,
    /// but because of data types should never realistically fail.
    pub fn serialize(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}
impl core::str::FromStr for NostrNote {
    type Err = serde_json::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}
impl TryFrom<serde_json::Value> for NostrNote {
    type Error = serde_json::Error;
    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        serde_json::from_value(value)
    }
}
impl TryFrom<&serde_json::Value> for NostrNote {
    type Error = serde_json::Error;
    fn try_from(value: &serde_json::Value) -> Result<Self, Self::Error> {
        serde_json::from_value(value.clone())
    }
}
impl From<NostrNote> for serde_json::Value {
    fn from(note: NostrNote) -> Self {
        serde_json::to_value(note).expect("Failed to serialize NostrNote.")
    }
}
