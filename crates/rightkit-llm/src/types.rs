use std::time::Duration;

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
        Self {
            role: Role::User,
            content: vec![ContentBlock::text(text)],
        }
    }
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: vec![ContentBlock::text(text)],
        }
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
    /// Ask the provider for schema-conforming tool input ("strict" tool use). Sent as
    /// `"strict": true` on the tool (Anthropic) or the function (OpenAI-compatible) only
    /// when set; omitted otherwise, so requests are unchanged for callers that leave it off.
    /// Providers reject schemas they cannot enforce; that surfaces as a request error.
    #[serde(default, skip_serializing_if = "is_false")]
    pub strict: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl ToolSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            input_schema,
            strict: false,
        }
    }

    pub fn strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }
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
    /// Leave the output-length field out of the OpenAI-compatible request body, so the
    /// provider applies its own limit. Default `false`: `max_tokens` is always sent. The
    /// Anthropic Messages API requires the field, so it ignores this flag and sends
    /// `max_tokens`. Set through [`ChatRequest::without_max_tokens`].
    pub omit_max_tokens: bool,
    /// Total time budget for this call, overriding the client's `request_timeout` (60 s).
    /// Applied per call as `Request::timeout` and `Request::deadline`, so retries share it.
    /// For a stream it caps the whole stream body. The client's `response_timeout` still
    /// bounds the wait for response headers. `None` keeps the client's settings.
    pub timeout: Option<Duration>,
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
            omit_max_tokens: false,
            timeout: None,
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
    /// Set the output limit and send it (clears [`ChatRequest::without_max_tokens`]).
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self.omit_max_tokens = false;
        self
    }
    /// Do not send the output-length field to OpenAI-compatible providers. See
    /// [`ChatRequest::omit_max_tokens`]; Anthropic still receives `max_tokens`.
    pub fn without_max_tokens(mut self) -> Self {
        self.omit_max_tokens = true;
        self
    }
    /// Set the sampling temperature (sent only when set).
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }
    /// Per-call total timeout. See [`ChatRequest::timeout`] for what it bounds.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
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

/// How a route is paid for, which decides whether a price lookup applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Metering {
    /// Billed per token through an API key (the HTTP providers).
    #[default]
    PerToken,
    /// Covered by a subscription (an installed, signed-in assistant). Price
    /// lookups do not apply, whatever token counts the tool reports.
    Subscription,
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
    /// Token usage. Meaningful only when `usage_known` is true; otherwise it is
    /// all zeros as a placeholder and must not be summed or priced.
    pub usage: Usage,
    /// False when the route did not report token counts (never "zero tokens").
    pub usage_known: bool,
    pub metering: Metering,
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
        self.tool_uses()
            .into_iter()
            .find(|(_, n, _)| *n == name)
            .map(|(_, _, i)| i)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    Text(String),
    ToolUseStart {
        id: String,
        name: String,
    },
    /// Partial JSON for the tool call `id`.
    ToolInputDelta {
        id: String,
        partial_json: String,
    },
}
