#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, PartialEq, Eq, Hash)]
pub enum NostrTag {
    #[serde(rename = "p")]
    Pubkey,
    #[serde(rename = "e")]
    Event,
    #[serde(rename = "d")]
    Parameterized,
    Custom(std::borrow::Cow<'static, str>),
    Relay,
}
impl std::str::FromStr for NostrTag {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "p" => Ok(Self::Pubkey),
            "e" => Ok(Self::Event),
            "d" => Ok(Self::Parameterized),
            _ => Ok(Self::Custom(std::borrow::Cow::Owned(s.to_owned()))),
        }
    }
}
impl AsRef<str> for NostrTag {
    fn as_ref(&self) -> &str {
        match self {
            Self::Pubkey => "p",
            Self::Event => "e",
            Self::Parameterized => "d",
            Self::Custom(tag) => tag.as_ref(),
            Self::Relay => "r",
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize, Clone, PartialEq, Eq, Hash, Default)]
pub struct NostrTags(pub Vec<Vec<String>>);
impl AsRef<[Vec<String>]> for NostrTags {
    fn as_ref(&self) -> &[Vec<String>] {
        &self.0
    }
}
impl AsMut<[Vec<String>]> for NostrTags {
    fn as_mut(&mut self) -> &mut [Vec<String>] {
        &mut self.0
    }
}
impl NostrTags {
    pub fn add_custom_tag(&mut self, tag_type: &str, tag: &str) {
        let tags = vec![tag_type.to_owned(), tag.to_owned()];
        self.0.push(tags);
    }
    pub fn add_pubkey_tag(&mut self, pubkey: &str, relay: Option<&str>) {
        let mut tags = vec!["p".to_owned(), pubkey.to_owned()];
        if let Some(relay) = relay {
            tags.push(relay.to_owned());
        }
        self.0.push(tags);
    }
    pub fn add_event_tag(&mut self, event_id: &str) {
        let tags = vec!["e".to_owned(), event_id.to_owned()];
        self.0.push(tags);
    }
    pub fn add_parameter_tag(&mut self, parameter: &str) {
        let tags = vec!["d".to_owned(), parameter.to_owned()];
        self.0.push(tags);
    }
    #[must_use]
    pub fn first_tagged_pubkey(&self) -> Option<String> {
        self.0
            .iter()
            .find(|tag_list| tag_list.first().is_some_and(|tag| tag == "p"))
            .and_then(|tag_list| tag_list.get(1).cloned())
    }
    #[must_use]
    pub fn first_tagged_event(&self) -> Option<String> {
        self.0
            .iter()
            .find(|tag_list| tag_list.first().is_some_and(|tag| tag == "e"))
            .and_then(|tag_list| tag_list.get(1).cloned())
    }
    #[must_use]
    pub fn first_parameter(&self) -> Option<String> {
        self.0
            .iter()
            .find(|tag_list| tag_list.first().is_some_and(|tag| tag == "d"))
            .and_then(|tag_list| tag_list.get(1).cloned())
    }
    #[must_use]
    pub fn find_tags(&self, tag_type: &str) -> Vec<String> {
        self.0
            .iter()
            .filter(|tag_list| tag_list.first().is_some_and(|tag| tag == tag_type))
            .flat_map(|tag_list| tag_list.iter().cloned())
            .skip(1)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    //use super::*;
    //use serde_json::json;

    // #[test]
    // fn test_serialize_tag_list() {
    //     let tag_list = TagList {
    //         tag_type: NostrTag::Pubkey,
    //         tags: vec!["tag1".to_string(), "tag2".to_string()],
    //     };
    //     let serialized = serde_json::to_string(&tag_list).unwrap();
    //     assert_eq!(serialized, r#"["p","tag1","tag2"]"#,);
    // }

    // #[test]
    // fn test_deserialize_tag_list() {
    //     let data = r#"["p","tag1","tag2"]"#;
    //     let deserialized: TagList = serde_json::from_str(data).unwrap();
    //     assert_eq!(
    //         deserialized,
    //         TagList {
    //             tag_type: NostrTag::Pubkey,
    //             tags: vec!["tag1".to_string(), "tag2".to_string()],
    //         }
    //     );
    // }
}
