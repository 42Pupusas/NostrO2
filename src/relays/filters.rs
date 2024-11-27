use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use super::SubscribeEvent;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NostrSubscription {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub authors: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<u32>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(flatten)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<HashMap<String, Vec<String>>>,
}
impl Default for NostrSubscription {
    fn default() -> Self {
        NostrSubscription {
            authors: None,
            ids: None,
            kinds: None,
            since: None,
            until: None,
            limit: None,
            tags: None,
        }
    }
}
impl NostrSubscription {
    pub fn add_tag(&mut self, tag: &str, value: &str) {
        if let Some(tags) = &mut self.tags {
            if let Some(tag_values) = tags.get_mut(tag) {
                tag_values.push(value.to_string());
            } else {
                tags.insert(tag.to_string(), vec![value.to_string()]);
            }
        } else {
            let mut tags = HashMap::new();
            tags.insert(tag.to_string(), vec![value.to_string()]);
            self.tags = Some(tags);
        }
    }
    pub fn relay_subscription(&self) -> super::SubscribeEvent {
        let random_id = format!("{:x}", rand::random::<u64>());
        SubscribeEvent(
            super::RelayEventTag::REQ,
            random_id,
            self.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_tags() {
        let mut tags = HashMap::new();
        tags.insert("#p".to_string(), vec!["value1".to_string()]);
        tags.insert("#q".to_string(), vec!["value2".to_string()]);
        let filter = NostrSubscription {
            kinds: Some(vec![4]),
            tags: Some(tags),
            ..Default::default()
        };
        let filter_value = serde_json::to_value(&filter).unwrap();
        assert_eq!(
            filter_value,
            serde_json::json!({
                "kinds": [4],
                "#p": ["value1"],
                "#q": ["value2"]
            })
        );
    }
    #[test]
    fn test_filter_tags_add() {
        let mut filter = NostrSubscription::default();
        filter.add_tag("#p", "value1");
        filter.add_tag("#q", "value2");
        filter.add_tag("#p", "value3");
        let filter_value = serde_json::to_value(&filter).unwrap();
        assert_eq!(
            filter_value,
            serde_json::json!({
                "#p": ["value1", "value3"],
                "#q": ["value2"]
            })
        );
    }
}

