use std::fmt::{Display, Formatter};
use super::utils::get_unix_timestamp;
use secp256k1::{schnorr::Signature, Message, XOnlyPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
}

impl Note {
    pub fn new(pubkey: &str, kind: u32, content: &str) -> Self {
        Note {
            pubkey: pubkey.to_string(),
            created_at: get_unix_timestamp(),
            kind,
            tags: Vec::new(),
            content: content.to_string(),
        }
    }

    pub fn add_tag(&mut self, tag_type: &str, tag: &str) {
        if tag_type == "p" || tag_type == "e" {
        } else {
            if let Some(index) = self
                .tags
                .iter()
                .position(|inner| inner.get(0) == Some(&tag_type.to_string()))
            {
                // Tag type exists, push the tag to the corresponding inner array.
                self.tags[index].push(tag.to_string());
            } else {
                // Tag type doesn't exist, create a new inner array and push it to the outer array.
                let mut new_inner = vec![tag_type.to_string()];
                new_inner.push(tag.to_string());
                self.tags.push(new_inner);
            }
        }
    }

    pub fn add_pubkey_tag(&mut self, pubkey: &str) {
        let tag_type = "p";
        if let Some(index) = self
            .tags
            .iter()
            .position(|inner| inner.get(0) == Some(&tag_type.to_string()))
        {
            // Tag type exists, push the tag to the corresponding inner array.
            self.tags[index].push(pubkey.to_string());
        } else {
            // Tag type doesn't exist, create a new inner array and push it to the outer array.
            let mut new_inner = vec![tag_type.to_string()];
            new_inner.push(pubkey.to_string());
            self.tags.push(new_inner);
        }
    }

    pub fn add_event_tag(&mut self, event_id: &str) {
        let tag_type = "e";
        if let Some(index) = self
            .tags
            .iter()
            .position(|inner| inner.get(0) == Some(&tag_type.to_string()))
        {
            // Tag type exists, push the tag to the corresponding inner array.
            self.tags[index].push(event_id.to_string());
        } else {
            // Tag type doesn't exist, create a new inner array and push it to the outer array.
            let mut new_inner = vec![tag_type.to_string()];
            new_inner.push(event_id.to_string());
            self.tags.push(new_inner);
        }
    }

    pub fn serialize_for_nostr(&self) -> String {
        // Directly use the custom Serialize implementation
        let serialized_data = (
            0,
            &*self.pubkey,
            self.created_at,
            self.kind,
            &self.tags,
            &*self.content,
        );

        let json_str = serde_json::to_string(&serialized_data).unwrap();

        json_str
    }
}

impl Display for Note {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(
            f,
            "{}",
            serde_json::to_string_pretty(self).expect("Failed to serialize Note.")
        )
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
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
    pub fn new(note: Note, id: String, sig: String) -> Self {
        SignedNote {
            id,
            pubkey: note.pubkey.to_string(),
            created_at: note.created_at,
            kind: note.kind,
            tags: note
                .tags
                .iter()
                .map(|inner| inner.iter().map(|x| x.to_string()).collect())
                .collect(),
            content: note.content.to_string(),
            sig,
        }
    }

    pub fn get_id(&self) -> &str {
        &*self.id
    }

    pub fn get_pubkey(&self) -> &str {
        &*self.pubkey
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

    pub fn get_content(&self) -> &str {
        &*self.content
    }

    pub fn get_sig(&self) -> &str {
        &*self.sig
    }

    fn verify_signature(&self) -> bool {
        let signature_of_signed_note = Signature::from_slice(
            &hex::decode(&*self.sig).expect("Failed to decode signed_note signature."),
        )
        .expect("Failed to instantiate Signature from byte array.");
        let message_of_signed_note =
            Message::from_slice(&hex::decode(&*self.id).expect("Failed to decode signed_note id."))
                .expect("Failed to instantiate Message from byte array.");
        let public_key_of_signed_note = XOnlyPublicKey::from_slice(
            &hex::decode(&*self.pubkey).expect("Failed to decode signed_note public"),
        )
        .expect("Failed to instantiate XOnlyPublicKey from byte array.");

        match signature_of_signed_note.verify(&message_of_signed_note, &public_key_of_signed_note) {
            Ok(()) => return true,
            _ => return false,
        };
    }

    fn verify_content(&self) -> bool {
        //let new_note = Note { signed_note.get_pubkey().to_string(), signed_note.get_kind(), signed_note.get_content() };
        let copied_note = Note {
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
            _ => return false,
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
