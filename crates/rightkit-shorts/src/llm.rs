//! LLM moment ranking: a provider trait, the Anthropic Messages API implementation,
//! the strict request/response shapes, and the validation that snaps whatever the model
//! says onto recogniser word boundaries.
//!
//! The model never sees or returns times. It gets sentences with ids and returns
//! `start_sentence` / `end_sentence` ids; [`crate::ranking`] resolves those to the
//! first and last non-filler words, so a proposal can only start at a word start and
//! end at a word end, and anything unresolvable is rejected with a reason.
//!
//! The API key is read from the OS secret store (service `app.orthiclabs.cutright`,
//! account `anthropic`) and from nowhere else: no environment variable, no file.
//! The HTTP transport and the request body are rightkit-llm's; the prompt, the tool
//! schema, the strict parse and the error mapping are CutRight's.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use rightkit_llm::{
    ChatRequest, ChatResponse, Client, ClientConfig, HttpError, LlmError, Provider, ProxyMode,
    RetryPolicy, StaticCredentials, ToolSpec,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::judge::JudgeVerdict;

pub const SECRET_SERVICE: &str = "app.orthiclabs.cutright";
pub const SECRET_ACCOUNT: &str = "anthropic";
pub const DEFAULT_MODEL: &str = "claude-sonnet-5-5";
pub const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const TOOL_NAME: &str = "report_moments";

/// rightkit-llm's Anthropic provider id; the key is looked up under this id.
const PROVIDER_ID: &str = "anthropic";
/// Whole-call deadline, as the former ureq global timeout (120 s, no retries).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentenceItem {
    /// `s0001`, `s0002`, ... stable across chunks (global sentence index + 1).
    pub id: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MomentRequest {
    pub sentences: Vec<SentenceItem>,
    pub max_moments: usize,
    pub min_duration_ms: i64,
    pub ideal_min_ms: i64,
    pub ideal_max_ms: i64,
    pub max_duration_ms: i64,
}

/// One ranked moment as the model must return it (strict: unknown fields rejected).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LlmMoment {
    pub start_sentence: String,
    pub end_sentence: String,
    /// The line that makes a viewer stay, quoted from the transcript.
    pub hook_text: String,
    pub reason: String,
    /// 0.0..=1.0, higher is better.
    pub score: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("no Anthropic key in the OS secret store ({0})")]
    NoKey(String),
    #[error("transport: {0}")]
    Transport(String),
    #[error("provider answered HTTP {0}")]
    Status(u16),
    #[error("malformed provider response: {0}")]
    Malformed(String),
}

pub trait MomentProvider {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    fn rank_moments(&self, request: &MomentRequest) -> Result<Vec<LlmMoment>, ProviderError>;
}

/// The strict JSON schema the model is held to (tool input schema).
pub fn moments_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["moments"],
        "properties": {
            "moments": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["start_sentence", "end_sentence", "hook_text", "reason", "score"],
                    "properties": {
                        "start_sentence": { "type": "string", "description": "id of the first sentence, e.g. s0012" },
                        "end_sentence": { "type": "string", "description": "id of the last sentence (same or later)" },
                        "hook_text": { "type": "string", "description": "the opening line that hooks the viewer, quoted exactly from the transcript" },
                        "reason": { "type": "string", "description": "one sentence on why this stands alone as a short" },
                        "score": { "type": "number", "description": "0.0 to 1.0, how strong the moment is" }
                    }
                }
            }
        }
    })
}

const SYSTEM: &str = "You pick the best self-contained moments of a talk for short vertical \
videos. You are given numbered transcript sentences with start/end seconds. Choose moments \
that open on a strong hook, make sense without the rest of the talk, and end on a finished \
thought. A moment is a run of consecutive sentences; give its first and last sentence id. \
Stay inside the duration limits. Do not overlap moments. Quote the hook line exactly. \
Answer only by calling the report_moments tool.";

pub fn user_prompt(req: &MomentRequest) -> String {
    let mut s = format!(
        "Duration limits for a moment: at least {} s, at most {} s, ideally {}-{} s. \
         Return up to {} moments, best first.\n\nTranscript sentences:\n",
        req.min_duration_ms / 1000,
        req.max_duration_ms / 1000,
        req.ideal_min_ms / 1000,
        req.ideal_max_ms / 1000,
        req.max_moments
    );
    for x in &req.sentences {
        s.push_str(&sentence_line(x));
    }
    s
}

/// One sentence as the prompt shows it: id, times in seconds, text.
pub fn sentence_line(x: &SentenceItem) -> String {
    format!(
        "{} [{:.1}-{:.1}] {}\n",
        x.id,
        x.start_ms as f64 / 1000.0,
        x.end_ms as f64 / 1000.0,
        x.text
    )
}

/// Characters of [`sentence_line`], for the token budget.
pub fn sentence_chars(x: &SentenceItem) -> usize {
    sentence_line(x).chars().count()
}

/// Characters of the prompt that does not depend on the sentences: the system prompt and
/// the header. Pass a request whose `sentences` is empty.
pub fn fixed_prompt_chars(shell: &MomentRequest) -> usize {
    SYSTEM.chars().count() + user_prompt(shell).chars().count()
}

/// The chat request for one chunk: the system prompt, one user turn, and the forced
/// strict `report_moments` tool, as rightkit-llm's `ChatRequest` expresses it.
fn moments_request(model: &str, max_tokens: u32, req: &MomentRequest) -> ChatRequest {
    let mut chat = ChatRequest::simple(model, SYSTEM, &user_prompt(req));
    chat.max_tokens = max_tokens;
    chat.tools.push(
        ToolSpec::new(
            TOOL_NAME,
            "Report the ranked short-form moments.",
            moments_schema(),
        )
        .strict(true),
    );
    chat.force_tool = Some(TOOL_NAME.to_string());
    chat
}

/// The Messages API request body exactly as rightkit-llm builds and sends it: a forced
/// strict tool call so the answer is JSON that matches [`moments_schema`].
pub fn build_request_body(model: &str, max_tokens: u32, req: &MomentRequest) -> Value {
    rightkit_llm::anthropic::build_body(&moments_request(model, max_tokens, req), false)
}

/// Finds the forced tool call in a Messages API response and parses it strictly.
pub fn parse_response(response: &Value) -> Result<Vec<LlmMoment>, ProviderError> {
    let bad = |m: &str| ProviderError::Malformed(m.to_string());
    let block = response
        .get("content")
        .and_then(Value::as_array)
        .and_then(|c| {
            c.iter().find(|b| {
                b.get("type").and_then(Value::as_str) == Some("tool_use")
                    && b.get("name").and_then(Value::as_str) == Some(TOOL_NAME)
            })
        })
        .ok_or_else(|| bad("no report_moments tool call in the response"))?;
    let input = block
        .get("input")
        .ok_or_else(|| bad("tool call has no input"))?;
    parse_moments(input)
}

/// The forced tool call's input from a rightkit-llm response, parsed strictly.
fn tool_moments(response: &ChatResponse) -> Result<Vec<LlmMoment>, ProviderError> {
    let bad = |m: &str| ProviderError::Malformed(m.to_string());
    let input = response
        .tool_input(TOOL_NAME)
        .ok_or_else(|| bad("no report_moments tool call in the response"))?;
    if input.is_null() {
        return Err(bad("tool call has no input"));
    }
    parse_moments(input)
}

/// Strict parse of the tool input into the typed [`JudgeVerdict`]; see that type for
/// the rules. Returns the moments only.
pub fn parse_moments(input: &Value) -> Result<Vec<LlmMoment>, ProviderError> {
    JudgeVerdict::from_tool_input(input).map(|v| v.moments)
}

/// Maps a rightkit-llm failure onto CutRight's provider errors. Transport text is
/// scrubbed of the key as well as by rightkit-http's own redaction.
fn map_llm_error(e: LlmError, key: &str) -> ProviderError {
    match e {
        LlmError::Http(HttpError::Status { status, .. }) => ProviderError::Status(status),
        LlmError::Http(HttpError::Json(m)) | LlmError::Malformed(m) => ProviderError::Malformed(m),
        LlmError::MissingCredential(p) => {
            ProviderError::NoKey(format!("no credential for provider '{p}'"))
        }
        other => {
            let text = other.to_string();
            let text = if key.is_empty() {
                text
            } else {
                text.replace(key, "<redacted>")
            };
            ProviderError::Transport(text)
        }
    }
}

/// Anthropic Messages API provider. The transport is rightkit-llm's; the key is held
/// in memory only and never shown by `Debug`.
pub struct AnthropicProvider {
    model: String,
    key: String,
    url: String,
    max_tokens: u32,
    provider: rightkit_llm::anthropic::AnthropicProvider,
}

impl fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicProvider")
            .field("model", &self.model)
            .field("url", &self.url)
            .field("key", &"<redacted>")
            .finish()
    }
}

impl AnthropicProvider {
    /// `model` defaults to [`DEFAULT_MODEL`].
    pub fn new(model: Option<&str>, key: String) -> Self {
        // Pre-port policy: one attempt, 120 s whole-call deadline, environment proxy
        // variables honoured (ureq's default), and the key scrubbed from error text.
        let config = ClientConfig {
            request_timeout: REQUEST_TIMEOUT,
            connect_timeout: Some(REQUEST_TIMEOUT),
            response_timeout: Some(REQUEST_TIMEOUT),
            retry: RetryPolicy::none(),
            proxy: ProxyMode::Environment,
            ..ClientConfig::default()
        }
        .redact_values(vec![key.clone()]);
        let client = Client::new(config);
        let credentials = Arc::new(StaticCredentials::default().with(PROVIDER_ID, &key));
        Self {
            model: model.unwrap_or(DEFAULT_MODEL).to_string(),
            key,
            url: ANTHROPIC_URL.into(),
            max_tokens: 4096,
            provider: rightkit_llm::anthropic::AnthropicProvider::new(
                rightkit_llm::anthropic::ANTHROPIC_BASE_URL,
                client,
                credentials,
            ),
        }
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Reads the key from the OS secret store; `NoKey` when there is none (or this
    /// platform has no reader), which the caller turns into a heuristic fallback.
    pub fn from_secret_store(model: Option<&str>) -> Result<Self, ProviderError> {
        read_secret().map(|key| Self::new(model, key))
    }
}

/// Background read: never prompts. `KeyringCompat` keeps keys written by the
/// earlier keyring-based reader readable on Windows.
fn read_secret() -> Result<String, ProviderError> {
    use rightkit_credentials::{Config, Namespace, Store, WindowsTarget};
    let ns = Namespace::new(SECRET_SERVICE)
        .map_err(|e| ProviderError::NoKey(format!("secret store unavailable: {e}")))?
        .windows_target(WindowsTarget::KeyringCompat);
    let config = Config::new(ns).guard_mode(rightkit_credentials::GuardMode::Deny);
    #[cfg(test)]
    let config = config.guard_when(true, rightkit_credentials::GuardMode::Deny);
    let secret = Store::open(config)
        .get(SECRET_ACCOUNT)
        .map_err(|e| ProviderError::NoKey(format!("secret store error: {e}")))?;
    match secret.as_ref().map(|s| s.expose_str()) {
        Some(Ok(v)) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => Err(ProviderError::NoKey(
            "no entry for app.orthiclabs.cutright / anthropic".into(),
        )),
    }
}

impl MomentProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn rank_moments(&self, request: &MomentRequest) -> Result<Vec<LlmMoment>, ProviderError> {
        let chat = moments_request(&self.model, self.max_tokens, request);
        let response = self
            .provider
            .complete(&chat)
            .map_err(|e| map_llm_error(e, &self.key))?;
        tool_moments(&response)
    }
}
