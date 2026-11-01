//! ACP content blocks — the baseline `text`/`resource_link` variants every
//! agent MUST support in prompts, plus `image`/`resource` which CR advertises
//! via `PromptCapabilities`. Mirrors
//! `agent-client-protocol-schema/src/v1/content.rs` field-for-field (the
//! `type` discriminator + camelCase field names), without the upstream
//! crate's schemars/derive_more machinery.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text(TextContent),
    Image(ImageContent),
    ResourceLink(ResourceLink),
    Resource(EmbeddedResource),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Annotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<f64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    /// Base64-encoded image bytes.
    pub data: String,
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLink {
    pub name: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedResource {
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_block_serializes_with_type_discriminator() {
        let block = ContentBlock::Text(TextContent {
            text: "hello".to_string(),
            annotations: None,
        });
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "hello");
    }

    #[test]
    fn resource_link_block_round_trips() {
        let block = ContentBlock::ResourceLink(ResourceLink {
            name: "README.md".to_string(),
            uri: "file:///README.md".to_string(),
            title: None,
            description: None,
            mime_type: Some("text/markdown".to_string()),
        });
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "resource_link");
        let parsed: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, block);
    }

    #[test]
    fn unknown_content_type_fails_to_parse() {
        let json = serde_json::json!({"type": "video", "uri": "file:///x.mp4"});
        let parsed: Result<ContentBlock, _> = serde_json::from_value(json);
        assert!(parsed.is_err());
    }
}
