#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum NostrTag {
    Pubkey,
    Event,
    Parameterized,
    Custom(&'static str),
}
impl std::str::FromStr for NostrTag {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "p" => Ok(Self::Pubkey),
            "e" => Ok(Self::Event),
            "d" => Ok(Self::Parameterized),
            _ => Ok(Self::Custom(Box::leak(s.to_owned().into_boxed_str()))),
        }
    }
}
impl std::fmt::Display for NostrTag {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Self::Pubkey => write!(f, "p"),
            Self::Event => write!(f, "e"),
            Self::Parameterized => write!(f, "d"),
            Self::Custom(s) => write!(f, "{s}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TagList {
    pub tag_type: NostrTag,
    pub tags: Vec<String>,
}
impl serde::Serialize for TagList {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut new_vec = Vec::new();
        new_vec.push(self.tag_type.to_string());
        new_vec.extend(self.tags.iter().cloned());
        new_vec.serialize(serializer)
    }
}
impl<'de> serde::Deserialize<'de> for TagList {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let mut vec: Vec<String> = Vec::deserialize(deserializer)?;
        if vec.is_empty() {
            return Err(serde::de::Error::custom("Empty tag list"));
        }
        let tag_type_str = vec.remove(0);
        let tag_type: NostrTag = tag_type_str
            .parse()
            .map_err(|()| serde::de::Error::custom("Invalid tag type"))?;
        Ok(Self {
            tag_type,
            tags: vec,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct NoteTags(pub Vec<TagList>);
impl serde::Serialize for NoteTags {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}
impl<'de> serde::Deserialize<'de> for NoteTags {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let tags: Vec<TagList> = Vec::deserialize(deserializer)?;
        Ok(Self(tags))
    }
}

impl NoteTags {
    #[must_use]
    pub fn find_first_tagged_pubkey(&self) -> Option<String> {
        self.0
            .iter()
            .find(|tag_list| tag_list.tag_type == NostrTag::Pubkey)
            .and_then(|tag_list| tag_list.tags.first().cloned())
    }
    #[must_use]
    pub fn find_first_tagged_event(&self) -> Option<String> {
        self.0
            .iter()
            .find(|tag_list| tag_list.tag_type == NostrTag::Event)
            .and_then(|tag_list| tag_list.tags.first().cloned())
    }
    #[must_use]
    pub fn find_first_parameter(&self) -> Option<String> {
        self.0
            .iter()
            .find(|tag_list| tag_list.tag_type == NostrTag::Parameterized)
            .and_then(|tag_list| tag_list.tags.first().cloned())
    }
    #[must_use]
    pub fn find_tags(&self, tag_type: &NostrTag) -> Vec<String> {
        self.0
            .iter()
            .filter(|tag_list| &tag_list.tag_type == tag_type)
            .flat_map(|tag_list| tag_list.tags.iter().cloned())
            .collect()
    }
    pub fn add_custom_tag(&mut self, tag_type: NostrTag, tag: &str) {
        if let Some(index) = self.0.iter().position(|inner| inner.tag_type == tag_type) {
            self.0[index].tags.push(tag.to_owned());
        } else {
            let new_inner = TagList {
                tag_type,
                tags: vec![tag.to_owned()],
            };
            self.0.push(new_inner);
        }
    }
    pub fn add_parameter_tag(&mut self, parameter: &str) {
        if let Some(index) = self
            .0
            .iter()
            .position(|inner| inner.tag_type == NostrTag::Parameterized)
        {
            self.0[index].tags.push(parameter.to_owned());
        } else {
            let new_inner = TagList {
                tag_type: NostrTag::Parameterized,
                tags: vec![parameter.to_owned()],
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
            self.0[index].tags.push(pubkey.to_owned());
        } else {
            let new_inner = TagList {
                tag_type: NostrTag::Pubkey,
                tags: vec![pubkey.to_owned()],
            };
            self.0.push(new_inner);
        }
    }
    pub fn add_event_tag(&mut self, event_id: &str) {
        if let Some(index) = self
            .0
            .iter_mut()
            .find(|inner| inner.tag_type == NostrTag::Event)
        {
            index.tags.push(event_id.to_owned());
        } else {
            let new_inner = TagList {
                tag_type: NostrTag::Event,
                tags: vec![event_id.to_owned()],
            };
            self.0.push(new_inner);
        }
    }
}
