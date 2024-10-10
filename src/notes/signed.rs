use secp256k1::{schnorr::Signature, Message, XOnlyPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::{Display, Formatter};

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash)]
pub struct SignedNote {
    // id is a crypto representation of the the kind, tags, pukey and content
    id: String,
    pubkey: String,
    created_at: u64,
    kind: u32,
    tags: Vec<Vec<String>>,
    content: String,
    // is a schnorr signed string of the ID
    sig: String,
}

impl SignedNote {
    pub fn new(note: super::Note, id: String, sig: String) -> Self {
        SignedNote {
            id,
            pubkey: note.pubkey.to_string(),
            created_at: note.created_at,
            kind: note.kind,
            tags: note.tags,
            content: note.content.to_string(),
            sig,
        }
    }
    pub fn get_id(&self) -> String {
        self.id.clone()
    }
    pub fn get_note_id(&self) -> String {
        let hrp = bech32::Hrp::parse("note").expect("valid hrp");
        let note_data = self.id.as_bytes();
        let string =
            bech32::encode::<bech32::Bech32>(hrp, &note_data).expect("failed to encode string");
        string
    }
    pub fn get_pubkey(&self) -> String {
        self.pubkey.clone()
    }
    pub fn get_created_at(&self) -> u64 {
        self.created_at
    }
    pub fn get_kind(&self) -> u32 {
        self.kind
    }
    pub fn get_tags(&self) -> Vec<Vec<String>> {
        self.tags.clone()
    }
    pub fn get_tags_by_id(&self, key: &str) -> Option<Vec<String>> {
        let mut tags = Vec::new();
        if let Some(index) = self
            .tags
            .iter()
            .position(|inner| inner.get(0) == Some(&key.to_string()))
        {
            for tag in &self.tags[index][1..] {
                tags.push(tag.to_string());
            }
            return Some(tags);
        }
        None
    }
    pub fn get_content(&self) -> String {
        self.content.clone()
    }
    pub fn get_sig(&self) -> String {
        self.sig.clone()
    }
    fn verify_signature(&self) -> bool {
        let signature_of_signed_note = Signature::from_slice(
            &hex::decode(&*self.sig).expect("Failed to decode signed_note signature."),
        );
        let message_of_signed_note =
            Message::from_slice(&hex::decode(&*self.id).expect("Failed to decode signed_note id."));
        let public_key_of_signed_note = XOnlyPublicKey::from_slice(
            &hex::decode(&*self.pubkey).expect("Failed to decode signed_note public"),
        );

        if let (
            Ok(signature_of_signed_note),
            Ok(message_of_signed_note),
            Ok(public_key_of_signed_note),
        ) = (
            signature_of_signed_note,
            message_of_signed_note,
            public_key_of_signed_note,
        ) {
            if signature_of_signed_note
                .verify(&message_of_signed_note, &public_key_of_signed_note)
                .is_ok()
            {
                return true;
            } else {
                return false;
            }
        } else {
            return false;
        }
    }
    fn verify_content(&self) -> bool {
        //let new_note = Note { signed_note.get_pubkey().to_string(), signed_note.get_kind(), signed_note.get_content() };
        let copied_note = super::Note {
            pubkey: self.pubkey.to_string(),
            created_at: self.created_at,
            kind: self.kind,
            tags: self.tags.clone(),
            content: self.content.to_string(),
        };
        // if we serialize and has the note content, kind and tags, we can compare the id
        // with the id that was signed
        let serialized_note = copied_note.serialize_for_nostr();

        let mut hasher = Sha256::new();
        hasher.update(serialized_note);

        // Hex Encod the hash
        let hash_result = hasher.finalize();
        let new_id = hex::encode(hash_result);

        match &new_id == &*self.id {
            true => return true,
            _ => {
                println!("{} != {}", &new_id, &self.id);
                return false;
            }
        }
    }
    pub fn verify(&self) -> bool {
        if self.verify_signature() && self.verify_content() {
            return true;
        }
        false
    }
}

impl Display for SignedNote {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_string_pretty(self).expect("Failed to serialize SignedNote.")
        )
    }
}
impl TryFrom<String> for SignedNote {
    type Error = serde_json::Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        serde_json::from_str(&value)
    }
}
impl Into<String> for SignedNote {
    fn into(self) -> String {
        serde_json::to_string(&self).unwrap()
    }
}
impl TryFrom<&str> for SignedNote {
    type Error = serde_json::Error;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        serde_json::from_str(&value)
    }
}
impl TryFrom<serde_json::Value> for SignedNote {
    type Error = serde_json::Error;
    fn try_from(value: serde_json::Value) -> Result<Self, Self::Error> {
        serde_json::from_value(value)
    }
}
impl TryFrom<&serde_json::Value> for SignedNote {
    type Error = serde_json::Error;
    fn try_from(value: &serde_json::Value) -> Result<Self, Self::Error> {
        serde_json::from_value(value.clone())
    }
}
impl Into<serde_json::Value> for SignedNote {
    fn into(self) -> serde_json::Value {
        serde_json::to_value(&self).unwrap()
    }
}
#[cfg(target_arch = "wasm32")]
impl TryFrom<wasm_bindgen::JsValue> for SignedNote {
    type Error = wasm_bindgen::JsError;
    fn try_from(value: wasm_bindgen::JsValue) -> Result<Self, Self::Error> {
        Ok(serde_wasm_bindgen::from_value(value)?)
    }
}
#[cfg(target_arch = "wasm32")]
impl Into<wasm_bindgen::JsValue> for SignedNote {
    fn into(self) -> wasm_bindgen::JsValue {
        serde_wasm_bindgen::to_value(&self).unwrap()
    }
}
