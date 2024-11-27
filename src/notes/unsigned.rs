use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};

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
            created_at: chrono::Utc::now().timestamp() as u64,
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
impl TryFrom<String> for Note {
    type Error = serde_json::Error;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        serde_json::from_str(&value)
    }
}
impl Into<String> for Note {
    fn into(self) -> String {
        serde_json::to_string(&self).unwrap()
    }
}
