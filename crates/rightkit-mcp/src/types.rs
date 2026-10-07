use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolAnnotations {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(
        default,
        rename = "readOnlyHint",
        skip_serializing_if = "Option::is_none"
    )]
    pub read_only: Option<bool>,
    #[serde(
        default,
        rename = "destructiveHint",
        skip_serializing_if = "Option::is_none"
    )]
    pub destructive: Option<bool>,
    #[serde(
        default,
        rename = "idempotentHint",
        skip_serializing_if = "Option::is_none"
    )]
    pub idempotent: Option<bool>,
    #[serde(
        default,
        rename = "openWorldHint",
        skip_serializing_if = "Option::is_none"
    )]
    pub open_world: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "empty_schema", rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(default)]
    pub annotations: ToolAnnotations,
}

fn empty_schema() -> Value {
    json!({"type": "object"})
}

impl ToolDescriptor {
    pub fn new(name: &str, description: &str, input_schema: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            annotations: ToolAnnotations::default(),
        }
    }
    pub fn read_only(mut self, yes: bool) -> Self {
        self.annotations.read_only = Some(yes);
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Text { text: String },
    Image { data: String, mime_type: String },
    Other(Value),
}

impl Content {
    pub fn to_value(&self) -> Value {
        match self {
            Content::Text { text } => json!({"type": "text", "text": text}),
            Content::Image { data, mime_type } => {
                json!({"type": "image", "data": data, "mimeType": mime_type})
            }
            Content::Other(v) => v.clone(),
        }
    }
    pub fn from_value(v: &Value) -> Content {
        match v.get("type").and_then(Value::as_str) {
            Some("text") => Content::Text {
                text: v.get("text").and_then(Value::as_str).unwrap_or("").into(),
            },
            Some("image") => Content::Image {
                data: v.get("data").and_then(Value::as_str).unwrap_or("").into(),
                mime_type: v
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .into(),
            },
            _ => Content::Other(v.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    pub content: Vec<Content>,
    pub structured: Option<Value>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            content: vec![Content::Text { text: s.into() }],
            structured: None,
            is_error: false,
        }
    }
    pub fn error(s: impl Into<String>) -> Self {
        Self {
            is_error: true,
            ..Self::text(s)
        }
    }
    pub fn json(v: Value) -> Self {
        Self {
            content: vec![Content::Text {
                text: v.to_string(),
            }],
            structured: Some(v),
            is_error: false,
        }
    }
    /// All text blocks joined with newlines.
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| match c {
                Content::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    pub fn to_value(&self) -> Value {
        let mut v = json!({
            "content": self.content.iter().map(Content::to_value).collect::<Vec<_>>(),
            "isError": self.is_error,
        });
        if let Some(s) = &self.structured {
            v["structuredContent"] = s.clone();
        }
        v
    }
    pub fn from_value(v: &Value) -> ToolResult {
        ToolResult {
            content: v
                .get("content")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(Content::from_value).collect())
                .unwrap_or_default(),
            structured: v.get("structuredContent").cloned(),
            is_error: v.get("isError").and_then(Value::as_bool).unwrap_or(false),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResourceDescriptor {
    pub uri: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, rename = "mimeType")]
    pub mime_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResourceContents {
    pub uri: String,
    #[serde(default, rename = "mimeType")]
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PromptArgument {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PromptDescriptor {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub arguments: Vec<PromptArgument>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptMessage {
    pub role: String,
    pub text: String,
}

impl PromptMessage {
    pub fn to_value(&self) -> Value {
        json!({"role": self.role, "content": {"type": "text", "text": self.text}})
    }
    pub fn from_value(v: &Value) -> PromptMessage {
        PromptMessage {
            role: v
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("user")
                .into(),
            text: v
                .pointer("/content/text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
        }
    }
}

pub const PROTOCOL_VERSION: &str = "2025-03-26";
pub const SUPPORTED_VERSIONS: &[&str] = &["2025-03-26", "2024-11-05"];
