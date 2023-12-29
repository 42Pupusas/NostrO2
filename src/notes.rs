use super::utils::get_unix_timestamp;
use secp256k1::{schnorr::Signature, Message, XOnlyPublicKey};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

#[derive(Debug)]
pub struct Note {
    pub pubkey: Arc<str>,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<Arc<str>>>,
    pub content: Arc<str>,
}

impl Serialize for Note {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let serializable_tags: Vec<Vec<&str>> = self
            .tags
            .iter()
            .map(|inner_vec| inner_vec.iter().map(|arc| &**arc).collect())
            .collect();

        let serialized_data = (
            &*self.pubkey,
            self.created_at,
            self.kind,
            &serializable_tags,
            &*self.content,
        );
        serialized_data.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Note {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let (pubkey, created_at, kind, tags, content) =
            <(String, u64, u32, Vec<Vec<String>>, String)>::deserialize(deserializer)?;

        let deserialized_tags: Vec<Vec<Arc<str>>> = tags
            .into_iter()
            .map(|inner_vec| inner_vec.into_iter().map(|s| Arc::from(s)).collect())
            .collect();

        Ok(Note {
            pubkey: Arc::from(pubkey),
            created_at,
            kind,
            tags: deserialized_tags,
            content: Arc::from(content),
        })
    }
}

impl Note {
    pub fn new(pubkey: String, kind: u32, content: &str) -> Self {
        Note {
            pubkey: Arc::from(pubkey),
            created_at: get_unix_timestamp(),
            kind,
            tags: Vec::new(),
            content: Arc::from(content),
        }
    }

    pub fn tag_note(&mut self, tag_type: &str, tag: &str) {
        if tag_type == "p" || tag_type == "e" {
        } else {
            let tag_type = Arc::from(tag_type);
            let tag = Arc::from(tag);
            if let Some(index) = self
                .tags
                .iter()
                .position(|inner| inner.get(0) == Some(&tag_type))
            {
                // Tag type exists, push the tag to the corresponding inner array.
                self.tags[index].push(tag);
            } else {
                // Tag type doesn't exist, create a new inner array and push it to the outer array.
                let mut new_inner = vec![tag_type];
                new_inner.push(tag);
                self.tags.push(new_inner);
            }
        }
    }

    pub fn serialize_for_nostr(&self) -> String {
        // Directly use the custom Serialize implementation
        let serialized_data = (
            0,
            &*self.pubkey,
            self.created_at,
            self.kind,
            &self
                .tags
                .iter()
                .map(|v| v.iter().map(AsRef::as_ref).collect::<Vec<&str>>())
                .collect::<Vec<Vec<&str>>>(),
            &*self.content,
        );

        let json_str = serde_json::to_string(&serialized_data).unwrap();

        json_str
    }
}

#[derive(Debug)]
pub struct SignedNote {
    // id is a crypto representation of the the kind, tags, pukey and content
    id: Arc<str>,
    pubkey: Arc<str>,
    created_at: u64,
    kind: u32,
    tags: Vec<Vec<Arc<str>>>,
    content: Arc<str>,

    // is a schnorr signed string of the ID
    sig: Arc<str>,
}

use serde::ser::SerializeStruct;

impl Serialize for SignedNote {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let serializable_tags: Vec<Vec<&str>> = self
            .tags
            .iter()
            .map(|inner_vec| inner_vec.iter().map(|arc| &**arc).collect())
            .collect();

        let mut s = serializer.serialize_struct("SignedNote", 7)?;
        s.serialize_field("id", &*self.id)?;
        s.serialize_field("pubkey", &*self.pubkey)?;
        s.serialize_field("created_at", &self.created_at)?;
        s.serialize_field("kind", &self.kind)?;
        s.serialize_field("tags", &serializable_tags)?;
        s.serialize_field("content", &*self.content)?;
        s.serialize_field("sig", &*self.sig)?;
        s.end()
    }
}

impl<'de> Deserialize<'de> for SignedNote {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // We expect a map here
        let data = std::collections::HashMap::<String, Value>::deserialize(deserializer)?;

        // Extract values from the map, convert to the appropriate types,
        // and handle missing values using .get() and .ok_or().

        let id = Arc::from(
            data.get("id")
                .ok_or(de::Error::missing_field("id"))?
                .as_str()
                .unwrap()
                .to_string(),
        );
        let pubkey = Arc::from(
            data.get("pubkey")
                .ok_or(de::Error::missing_field("pubkey"))?
                .as_str()
                .unwrap()
                .to_string(),
        );
        let created_at = data
            .get("created_at")
            .ok_or(de::Error::missing_field("created_at"))?
            .as_u64()
            .unwrap();
        let kind = data
            .get("kind")
            .ok_or(de::Error::missing_field("kind"))?
            .as_u64()
            .unwrap() as u32;
        let content = Arc::from(
            data.get("content")
                .ok_or(de::Error::missing_field("content"))?
                .as_str()
                .unwrap()
                .to_string(),
        );
        let sig = Arc::from(
            data.get("sig")
                .ok_or(de::Error::missing_field("sig"))?
                .as_str()
                .unwrap()
                .to_string(),
        );

        // For tags

        static EMPTY_VEC: Vec<Value> = Vec::new();
        let tags_data = data
            .get("tags")
            .ok_or(de::Error::missing_field("tags"))?
            .as_array()
            .unwrap_or(&EMPTY_VEC);
        let mut tags = Vec::with_capacity(tags_data.len());
        for tag_set in tags_data {
            let mut inner_tags = Vec::new();
            if let Some(tag_array) = tag_set.as_array() {
                for tag in tag_array {
                    inner_tags.push(Arc::from(tag.as_str().unwrap().to_string()));
                }
            }
            tags.push(inner_tags);
        }

        Ok(SignedNote {
            id,
            pubkey,
            created_at,
            kind,
            tags,
            content,
            sig,
        })
    }
}

impl SignedNote {
    pub fn new(
        id: String,
        pubkey: String,
        tags: Vec<Vec<Arc<str>>>,
        kind: u32,
        content: &str,
        sig: String,
    ) -> Self {
        SignedNote {
            id: Arc::from(id),
            pubkey: Arc::from(pubkey),
            created_at: get_unix_timestamp(),
            kind,
            tags,
            content: Arc::from(content),
            sig: Arc::from(sig),
        }
    }

    pub fn prepare_ws_message(&self) -> WsMessage {
        let event_string = json!(["EVENT", self]).to_string();
        let event_ws_message = WsMessage::Text(event_string);
        event_ws_message
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

    pub fn get_tags(&self) -> &Vec<Vec<Arc<str>>> {
        &self.tags
    }

    pub fn get_tags_by_id(&self, key: &str) -> Vec<String> {
        let mut tags = Vec::new();
        for tag_set in &self.tags {
            if &*tag_set[0] == key {
                for tag in &tag_set[1..] {
                    tags.push(tag.to_string());
                }
            }
        }
        tags
    }

    pub fn get_content(&self) -> &str {
        &*self.content
    }

    pub fn get_sig(&self) -> &str {
        &*self.sig
    }

    pub fn verify_signature(&self) -> bool {
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

    pub fn verify_content(&self) -> bool {
        //let new_note = Note { signed_note.get_pubkey().to_string(), signed_note.get_kind(), signed_note.get_content() };
        let copied_note = Note {
            pubkey: self.pubkey.clone(),
            created_at: self.created_at,
            kind: self.kind,
            tags: self.tags.clone(),
            content: self.content.clone(),
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
}
