use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::str::FromStr;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum NostrTag {
    Pubkey,
    Event,
    Custom(&'static str),
}
impl Into<String> for NostrTag {
    fn into(self) -> String {
        match self {
            NostrTag::Pubkey => "p".to_string(),
            NostrTag::Event => "e".to_string(),
            NostrTag::Custom(tag_type) => tag_type.to_string(),
        }
    }
}
impl FromStr for NostrTag {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "p" => Ok(NostrTag::Pubkey),
            "e" => Ok(NostrTag::Event),
            _ => Ok(NostrTag::Custom(Box::leak(s.to_string().into_boxed_str()))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TagList {
    pub tag_type: NostrTag,
    pub tags: Vec<String>,
}
impl Serialize for TagList {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut new_vec = Vec::new();
        new_vec.push(self.tag_type.clone().into());
        new_vec.extend(self.tags.iter().cloned());
        new_vec.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for TagList {
    fn deserialize<D>(deserializer: D) -> Result<TagList, D::Error>
    where
        D: Deserializer<'de>,
    {
        let mut vec: Vec<String> = Vec::deserialize(deserializer)?;
        if vec.is_empty() {
            return Err(serde::de::Error::custom("Empty tag list"));
        }
        let tag_type_str = vec.remove(0);
        let tag_type: NostrTag = tag_type_str
            .parse()
            .map_err(|_| serde::de::Error::custom("Invalid tag type"))?;
        Ok(TagList {
            tag_type,
            tags: vec,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NoteTags(pub Vec<TagList>);
impl Default for NoteTags {
    fn default() -> Self {
        NoteTags(Vec::new())
    }
}
impl Serialize for NoteTags {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for NoteTags {
    fn deserialize<D>(deserializer: D) -> Result<NoteTags, D::Error>
    where
        D: Deserializer<'de>,
    {
        let tags: Vec<TagList> = Vec::deserialize(deserializer)?;
        Ok(NoteTags(tags))
    }
}

impl NoteTags {
    pub fn find_first_tagged_pubkey(&self) -> Option<String> {
        self.0
            .iter()
            .find(|tag_list| tag_list.tag_type == NostrTag::Pubkey)
            .and_then(|tag_list| tag_list.tags.first().cloned())
    }
    pub fn find_first_tagged_event(&self) -> Option<String> {
        self.0
            .iter()
            .find(|tag_list| tag_list.tag_type == NostrTag::Event)
            .and_then(|tag_list| tag_list.tags.first().cloned())
    }
    pub fn find_pubkey_tags(&self) -> Vec<String> {
        self.0
            .iter()
            .filter(|tag_list| tag_list.tag_type == NostrTag::Pubkey)
            .flat_map(|tag_list| tag_list.tags.iter().cloned())
            .collect()
    }
    pub fn find_event_tags(&self) -> Vec<String> {
        self.0
            .iter()
            .filter(|tag_list| tag_list.tag_type == NostrTag::Event)
            .flat_map(|tag_list| tag_list.tags.iter().cloned())
            .collect()
    }
    pub fn find_custom_tags(&self, custom_tag: NostrTag) -> Vec<String> {
        self.0
            .iter()
            .filter(|tag_list| tag_list.tag_type == custom_tag)
            .flat_map(|tag_list| tag_list.tags.iter().cloned())
            .collect()
    }
    pub fn add_tag(&mut self, tag_type: NostrTag, tag: &str) {
        if let Some(index) = self.0.iter().position(|inner| inner.tag_type == tag_type) {
            self.0[index].tags.push(tag.to_string());
        } else {
            let new_inner = TagList {
                tag_type,
                tags: vec![tag.to_string()],
            };
            self.0.push(new_inner);
        }
    }
    pub fn add_pubkey_tag(&mut self, pubkey: &str) {
        if let Some(index) = self
            .0
            .iter()
            .position(|inner| inner.tag_type == NostrTag::Pubkey)
        {
            self.0[index].tags.push(pubkey.to_string());
        } else {
            let new_inner = TagList {
                tag_type: NostrTag::Pubkey,
                tags: vec![pubkey.to_string()],
            };
            self.0.push(new_inner);
        }
    }
    pub fn add_event_tag(&mut self, event_id: &str) {
        if let Some(index) = self
            .0
            .iter()
            .position(|inner| inner.tag_type == NostrTag::Event)
        {
            self.0[index].tags.push(event_id.to_string());
        } else {
            let new_inner = TagList {
                tag_type: NostrTag::Event,
                tags: vec![event_id.to_string()],
            };
            self.0.push(new_inner);
        }
    }
}
