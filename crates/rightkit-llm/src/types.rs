use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default)]
        is_error: bool,
    },
}

impl ContentBlock {
    pub fn text(t: impl Into<String>) -> Self {
        ContentBlock::Text { text: t.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: Role::User, content: vec![ContentBlock::text(text)] }
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: vec![ContentBlock::text(text)] }
    }
    pub fn tool_result(id: impl Into<String>, content: impl Into<String>, is_error: bool) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.into(),
                content: content.into(),
                is_error,
            }],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool input.
    pub input_schema: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheMode {
    #[default]
    Off,
    /// Place provider cache breakpoints on the stable prefix (tools, system, trailing turns).
    Auto,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub max_tokens: u32,
    pub temperature: Option<f32>,
    pub stop_sequences: Vec<String>,
    pub tools: Vec<ToolSpec>,
    /// Force the model to call this tool (structured output).
    pub force_tool: Option<String>,
    pub cache: CacheMode,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        Self {
            model: model.into(),
            system: None,
            messages,
            max_tokens: 1024,
            temperature: None,
            stop_sequences: Vec::new(),
            tools: Vec::new(),
            force_tool: None,
            cache: CacheMode::Off,
        }
    }
    /// One system prompt plus one user turn.
    pub fn simple(model: impl Into<String>, system: &str, user: &str) -> Self {
        let mut r = Self::new(model, vec![Message::user(user)]);
        if !system.is_empty() {
            r.system = Some(system.to_string());
        }
        r
    }
}

/// Token usage. `input_tokens` EXCLUDES cache reads/writes on every provider
/// (OpenAI-style `prompt_tokens` is normalized by subtracting cached tokens).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, o: &Usage) {
        self.input_tokens += o.input_tokens;
        self.output_tokens += o.output_tokens;
        self.cache_read_tokens += o.cache_read_tokens;
        self.cache_write_tokens += o.cache_write_tokens;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    ToolUse,
    StopSequence,
    Other(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatResponse {
    pub id: String,
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: StopReason,
    pub usage: Usage,
}

impl ChatResponse {
    /// All text blocks concatenated.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
    pub fn tool_uses(&self) -> Vec<(&str, &str, &Value)> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, name, input } => {
                    Some((id.as_str(), name.as_str(), input))
                }
                _ => None,
            })
            .collect()
    }
    /// Input of the first call to `name` (structured output via a forced tool).
    pub fn tool_input(&self, name: &str) -> Option<&Value> {
        self.tool_uses().into_iter().find(|(_, n, _)| *n == name).map(|(_, _, i)| i)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Text(String),
    ToolUseStart { id: String, name: String },
    /// Partial JSON for the tool call `id`.
    ToolInputDelta { id: String, partial_json: String },
}
