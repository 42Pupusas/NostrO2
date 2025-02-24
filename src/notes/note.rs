use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::{Display, Formatter};

use super::NoteTags;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash)]
pub struct NostrNote {
    pub pubkey: String,
    pub created_at: i64,
    pub kind: u32,
    pub tags: NoteTags,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
}
impl Default for NostrNote {
    fn default() -> Self {
        NostrNote {
            pubkey: "".to_string(),
            created_at: chrono::Utc::now().timestamp(),
            kind: 1,
            tags: NoteTags::default(),
            content: "".to_string(),
            id: None,
            sig: None,
        }
    }
}
impl NostrNote {
    pub fn get_note_hrp(&self) -> Option<String> {
        let hrp = bech32::Hrp::parse("note").ok()?;
        let note_data = self.id.as_ref()?;
        let string = bech32::encode::<bech32::Bech32>(hrp, &note_data.as_bytes()).ok()?;
        Some(string)
    }
    pub fn id_bytes(&self) -> Option<[u8; 32]> {
        let mut id_bytes = [0u8; 32];
        let id = Self::hex_decode(&self.id.as_ref()?);
        id_bytes.copy_from_slice(&id);
        Some(id_bytes)
    }
    fn sig_bytes(&self) -> Option<[u8; 64]> {
        let mut sig_bytes = [0u8; 64];
        let sig = Self::hex_decode(&self.sig.as_ref()?);
        sig_bytes.copy_from_slice(&sig);
        Some(sig_bytes)
    }
    fn pubkey_bytes(&self) -> Option<[u8; 32]> {
        let mut pubkey_bytes = [0u8; 32];
        let pubkey = Self::hex_decode(&self.pubkey);
        pubkey_bytes.copy_from_slice(&pubkey);
        Some(pubkey_bytes)
    }
    pub fn serialize_id(&mut self) -> anyhow::Result<()> {
        let serialized_data = (
            0,
            &*self.pubkey,
            self.created_at,
            self.kind,
            &self.tags,
            &*self.content,
        );
        let json_str = serde_json::to_string(&serialized_data)?;
        let mut hasher = Sha256::new();
        hasher.update(json_str.as_bytes());
        self.id = Some(Self::hex_encode(hasher.finalize().to_vec()));
        Ok(())
    }
    fn verify_signature(&self) -> anyhow::Result<()> {
        use secp256k1::{schnorr, Secp256k1, XOnlyPublicKey};
        let secp = Secp256k1::verification_only();
        let id = self
            .id_bytes()
            .ok_or(anyhow::anyhow!("Failed to get id bytes."))?;
        let sig = self
            .sig_bytes()
            .ok_or(anyhow::anyhow!("Failed to get sig bytes."))?;
        let public_key = XOnlyPublicKey::from_slice(
            &self
                .pubkey_bytes()
                .ok_or(anyhow::anyhow!("Failed to get pubkey bytes."))?,
        )?;
        let signature = schnorr::Signature::from_byte_array(sig);
        Ok(secp.verify_schnorr(&signature, &id, &public_key)?)
    }
    fn verify_content(&self) -> bool {
        let mut copied_note = Self {
            pubkey: self.pubkey.to_string(),
            created_at: self.created_at,
            kind: self.kind,
            tags: self.tags.clone(),
            content: self.content.to_string(),
            ..Default::default()
        };
        if copied_note.serialize_id().is_err() {
            return false;
        }
        self.id == copied_note.id
    }
    pub fn verify(&self) -> bool {
        if self.verify_signature().is_ok() && self.verify_content() {
            return true;
        }
        false
    }
    fn hex_decode(hex_string: &str) -> Vec<u8> {
        hex_string
            .as_bytes()
            .chunks(2)
            .filter_map(|b| u8::from_str_radix(std::str::from_utf8(b).ok()?, 16).ok())
            .collect()
    }
    fn hex_encode(bytes: Vec<u8>) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
}
impl Into<crate::relays::WebSocketMessage> for NostrNote {
    fn into(self) -> crate::relays::WebSocketMessage {
        let note: String =
            crate::relays::SendNoteEvent(crate::relays::RelayEventTag::EVENT, self).into();
        crate::relays::WebSocketMessage::Text(note.into())
    }
}
impl Display for NostrNote {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_string_pretty(self).expect("Failed to serialize NostrNote.")
        )
    }
}
impl TryFrom<&String> for NostrNote {
    type Error = serde_json::Error;
    fn try_from(value: &String) -> Result<Self, Self::Error> {
        serde_json::from_str(value)
    }
}
impl Into<String> for NostrNote {
    fn into(self) -> String {
        serde_json::to_string(&self).unwrap()
    }
}
impl TryFrom<String> for NostrNote {
    type Error = serde_json::Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        serde_json::from_str(&value)
    }
}
impl Into<String> for &NostrNote {
    fn into(self) -> String {
        serde_json::to_string(self).unwrap()
    }
}
impl TryFrom<&str> for NostrNote {
    type Error = serde_json::Error;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        serde_json::from_str(&value)
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
impl Into<serde_json::Value> for NostrNote {
    fn into(self) -> serde_json::Value {
        serde_json::to_value(&self).unwrap()
    }
}
#[cfg(target_arch = "wasm32")]
impl TryFrom<web_sys::wasm_bindgen::JsValue> for NostrNote {
    type Error = web_sys::wasm_bindgen::JsError;
    fn try_from(value: web_sys::wasm_bindgen::JsValue) -> Result<Self, Self::Error> {
        Ok(serde_wasm_bindgen::from_value(value)?)
    }
}
#[cfg(target_arch = "wasm32")]
impl Into<web_sys::wasm_bindgen::JsValue> for NostrNote {
    fn into(self) -> web_sys::wasm_bindgen::JsValue {
        serde_wasm_bindgen::to_value(&self).unwrap()
    }
}
#[cfg(target_arch = "wasm32")]
impl TryFrom<&web_sys::wasm_bindgen::JsValue> for NostrNote {
    type Error = web_sys::wasm_bindgen::JsError;
    fn try_from(value: &web_sys::wasm_bindgen::JsValue) -> Result<Self, Self::Error> {
        Ok(serde_wasm_bindgen::from_value(value.clone())?)
    }
}
#[cfg(target_arch = "wasm32")]
impl Into<web_sys::wasm_bindgen::JsValue> for &NostrNote {
    fn into(self) -> web_sys::wasm_bindgen::JsValue {
        serde_wasm_bindgen::to_value(self).unwrap()
    }
}
