use std::sync::Arc;
use std::time::Duration;

use rightkit_http::{Request, RetryPolicy};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::error::LlmError;
use crate::image::{Frame, ImageSource};
use crate::raw::RawHandle;

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
    /// An image or video frame, valid in a user message. See [`crate::image`].
    Image {
        source: ImageSource,
    },
}

impl ContentBlock {
    pub fn text(t: impl Into<String>) -> Self {
        ContentBlock::Text { text: t.into() }
    }
    /// An image from raw bytes (base64-encoded here). `media_type` is one of
    /// [`crate::image::SUPPORTED_MEDIA_TYPES`].
    pub fn image_bytes(media_type: impl Into<String>, bytes: &[u8]) -> Self {
        ContentBlock::Image {
            source: ImageSource::from_bytes(media_type, bytes),
        }
    }
    /// An image the provider fetches from an `http` or `https` URL.
    pub fn image_url(url: impl Into<String>) -> Self {
        ContentBlock::Image {
            source: ImageSource::url(url),
        }
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
    /// A user turn from explicit blocks (text and images in the order given).
    pub fn user_blocks(content: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::User,
            content,
        }
    }
    /// A user turn with a text prompt followed by images.
    pub fn user_with_images(text: impl Into<String>, images: Vec<ImageSource>) -> Self {
        let mut content = vec![ContentBlock::text(text)];
        content.extend(images.into_iter().map(|source| ContentBlock::Image { source }));
        Self {
            role: Role::User,
            content,
        }
    }
    /// A user turn with a text prompt, then each frame preceded by its timestamp label
    /// (`Frame at 1.500 s:`), in the order given.
    pub fn user_with_frames(text: impl Into<String>, frames: &[Frame]) -> Self {
        let mut content = vec![ContentBlock::text(text)];
        for f in frames {
            content.extend(f.blocks());
        }
        Self {
            role: Role::User,
            content,
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
    /// Retry behaviour for this call. [`CallRetry::Client`] (the default) keeps the client's
    /// policy and the provider's replay rule. See [`CallRetry`].
    pub retry: CallRetry,
    /// Extra top-level fields merged into the built request body. See [`ExtraBody`].
    pub extra_body: ExtraBody,
    /// Return [`LlmError::Truncated`] instead of a normal response when the provider stopped
    /// at its output limit. Default `false`. See [`ChatRequest::reject_truncated_output`].
    pub reject_truncated: bool,
    /// Send user messages that have no text as an empty string instead of dropping them.
    /// Default `false`: an empty user message is left out of the OpenAI-compatible body.
    /// Anthropic rejects empty content, so with `true` an empty message fails with
    /// [`LlmError::InvalidRequest`] before anything is sent. See
    /// [`ChatRequest::keep_empty_messages`].
    pub keep_empty_messages: bool,
    /// Wrap a failure that happened after the provider reported or began to bill usage in
    /// [`LlmError::Billed`], so the spend is visible on the error. Default `false`: errors
    /// keep their exact variants. [`LlmError::billing`] and `Attempt::billing` answer
    /// either way. See [`ChatRequest::report_failed_usage`].
    pub report_failed_usage: bool,
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
            retry: CallRetry::Client,
            extra_body: ExtraBody::default(),
            reject_truncated: false,
            keep_empty_messages: false,
            report_failed_usage: false,
        }
    }
    /// True when any message carries an image block.
    pub fn has_images(&self) -> bool {
        self.messages
            .iter()
            .flat_map(|m| &m.content)
            .any(|b| matches!(b, ContentBlock::Image { .. }))
    }
    /// Check every image block locally ([`ImageSource::validate`]). Both HTTP providers call
    /// this before building a request; images outside user messages are rejected too.
    pub(crate) fn check_images(&self) -> Result<(), LlmError> {
        for m in &self.messages {
            for b in &m.content {
                if let ContentBlock::Image { source } = b {
                    if m.role != Role::User {
                        return Err(LlmError::InvalidRequest(
                            "image blocks are valid only in user messages".into(),
                        ));
                    }
                    source.validate()?;
                }
            }
        }
        Ok(())
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
    /// Set the retry behaviour for this call. See [`CallRetry`].
    pub fn with_retry(mut self, retry: CallRetry) -> Self {
        self.retry = retry;
        self
    }
    /// Make exactly one attempt: no status replay and no transport retry for this call.
    pub fn without_retries(self) -> Self {
        self.with_retry(CallRetry::Disabled)
    }
    /// Add a top-level body field. A key the crate already sets for this request (for example
    /// `model`, `messages`, `max_tokens`, `tools`, `stream`) is an error at send time, not a
    /// silent replacement; use [`ChatRequest::with_extra_override`] to replace one on purpose.
    /// Replaces an earlier value for the same key, and clears an earlier override for it.
    pub fn with_extra(mut self, key: impl Into<String>, value: Value) -> Self {
        let key = key.into();
        self.extra_body.overrides.retain(|k| k != &key);
        self.extra_body.fields.insert(key, value);
        self
    }
    /// Add a top-level body field that replaces a field the crate sets for this request.
    /// Use only when the replacement is deliberate.
    pub fn with_extra_override(mut self, key: impl Into<String>, value: Value) -> Self {
        let key = key.into();
        if !self.extra_body.overrides.contains(&key) {
            self.extra_body.overrides.push(key.clone());
        }
        self.extra_body.fields.insert(key, value);
        self
    }
    /// Choose what happens to a user message with no text. `false` (default) drops it from
    /// the OpenAI-compatible body, as before. `true` sends it as an empty string; Anthropic
    /// has no empty content, so `true` makes an empty message an
    /// [`LlmError::InvalidRequest`] there, before sending.
    pub fn keep_empty_messages(mut self, keep: bool) -> Self {
        self.keep_empty_messages = keep;
        self
    }
    /// Make a call that fails after dispatch report the usage it incurred. When the provider
    /// had reported token counts by the time of the failure (a truncated or refused answer,
    /// a stream that broke after its usage frame, a cancelled stream), the error is returned
    /// as [`LlmError::Billed`] carrying those counts; look through it with
    /// [`LlmError::root`]. Failures with no counts are returned bare, and
    /// [`LlmError::billing`] still classifies them as `Spend::Unknown` or `Spend::NotSpent`.
    pub fn report_failed_usage(mut self) -> Self {
        self.report_failed_usage = true;
        self
    }
    /// Treat a provider stop at the output limit as [`LlmError::Truncated`]. By default the
    /// truncated text is returned with [`StopReason::MaxTokens`]. Applies to streams too.
    pub fn reject_truncated_output(mut self) -> Self {
        self.reject_truncated = true;
        self
    }
}

/// JSON number for an `f32` request field, written as its shortest decimal that round-trips
/// the `f32` (`0.3` goes on the wire as `0.3`, not `0.30000001192092896`).
pub(crate) fn f32_json(v: f32) -> Value {
    // `Display` for f32 is the shortest round-tripping decimal; parsing that as f64 keeps the
    // digits, since f64 serialises its shortest form. NaN and infinity become JSON null, as
    // `json!` already did.
    match v.to_string().parse::<f64>() {
        Ok(d) => json!(d),
        Err(_) => json!(v as f64),
    }
}

/// Per-call retry behaviour, honoured by both HTTP providers.
///
/// Providers always allow status replay for chat calls (a chat completion has no
/// server-side effect), so the replay rule is unchanged; this enum only selects the attempt
/// policy:
///
/// - [`CallRetry::Client`] (default): the client's `ClientConfig::retry`, unchanged.
/// - [`CallRetry::Disabled`]: one attempt, no retry for any status or transport error.
/// - [`CallRetry::Policy`]: this policy replaces the client's for this call. Its
///   `retry_statuses` decide what is retried, so include `529` yourself for Anthropic.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum CallRetry {
    #[default]
    Client,
    Disabled,
    Policy(RetryPolicy),
}

impl CallRetry {
    pub(crate) fn apply(&self, request: Request) -> Request {
        match self {
            CallRetry::Client => request,
            CallRetry::Disabled => request.retry(RetryPolicy::none()),
            CallRetry::Policy(policy) => request.retry(policy.clone()),
        }
    }
}

/// Caller-supplied top-level JSON fields, merged into the body the crate builds (for example
/// `reasoning_effort`, `include_reasoning`, or OpenRouter's `provider`).
///
/// Rule: a key the crate did not set is added. A key the crate did set for this request
/// makes the call fail with [`LlmError::InvalidRequest`] before anything is sent, unless the
/// key was added with [`ChatRequest::with_extra_override`], which replaces it. Nothing is
/// merged into nested objects.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExtraBody {
    /// Fields to merge into the top level of the body.
    pub fields: Map<String, Value>,
    /// Keys in `fields` allowed to replace a field the crate sets.
    pub overrides: Vec<String>,
}

impl ExtraBody {
    pub(crate) fn merge_into(&self, body: &mut Value) -> Result<(), LlmError> {
        let Some(object) = body.as_object_mut() else {
            return Ok(());
        };
        for (key, value) in &self.fields {
            if object.contains_key(key) && !self.overrides.contains(key) {
                return Err(LlmError::InvalidRequest(format!(
                    "extra body field '{key}' is already set by the request; use with_extra_override to replace it"
                )));
            }
            object.insert(key.clone(), value.clone());
        }
        Ok(())
    }
}

/// What a provider reports to a [`CallObserver`] once per call that reaches the network.
pub struct CallRecord<'a> {
    /// Provider id (for example `openai` or the preset name).
    pub provider: &'a str,
    /// True for a streamed call.
    pub stream: bool,
    /// The exact JSON body sent, after extra fields were merged. The credential and auth
    /// headers are never part of it.
    pub request_body: &'a Value,
    /// The parsed response, with its usage, or the error the call ended with. Error text has
    /// the credential removed.
    pub result: Result<&'a ChatResponse, &'a LlmError>,
    /// The provider's raw response (status, request id and body bytes up to the cap), for a
    /// non-streaming call that got an HTTP response, whether it then succeeded or failed.
    /// `None` when raw capture is off (the default), for a streamed call (streams are not
    /// captured), and when no response arrived (a transport failure or a timeout). The same
    /// handle is attached to a `Truncated` or `Malformed` error of this call. The body is the
    /// provider's answer to the prompt and belongs to the app from here on; the credential
    /// text is replaced in it, and the request's auth headers are never part of it. See
    /// [`crate::raw`].
    pub raw: Option<&'a RawHandle>,
}

/// Callback for an on-device payload ledger. Called synchronously on the calling thread, so
/// it should be quick. A panic propagates to the caller.
pub type CallObserver = Arc<dyn Fn(&CallRecord<'_>) + Send + Sync>;

/// Shared post-response check for `ChatRequest::reject_truncated`.
pub(crate) fn check_output(
    response: ChatResponse,
    req: &ChatRequest,
) -> Result<ChatResponse, LlmError> {
    if req.reject_truncated && response.stop_reason == StopReason::MaxTokens {
        let output_tokens = response.usage_known.then_some(response.usage.output_tokens);
        let error = LlmError::Truncated { output_tokens };
        return Err(if req.report_failed_usage {
            billed(error, &response.billing())
        } else {
            error
        });
    }
    Ok(response)
}

/// Wrap `error` in [`LlmError::Billed`] when `billing` carries reported usage; otherwise
/// return it unchanged.
pub(crate) fn billed(error: LlmError, billing: &AttemptBilling) -> LlmError {
    match billing.spend {
        Spend::Reported(_) => LlmError::Billed {
            error: Box::new(error),
            billing: *billing,
        },
        _ => error,
    }
}

/// What a call cost, or may have cost, when it ended. Present on every attempt record
/// (`Attempt::billing`), derivable from any error ([`LlmError::billing`]) and from a
/// response ([`ChatResponse::billing`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptBilling {
    pub spend: Spend,
    pub metering: Metering,
}

impl AttemptBilling {
    pub fn new(spend: Spend, metering: Metering) -> Self {
        Self { spend, metering }
    }
    /// Dispatched, nothing known about the usage, billed per token.
    pub fn unknown() -> Self {
        Self::new(Spend::Unknown, Metering::PerToken)
    }
    /// The usage the route reported, if any. Never zero-filled.
    pub fn usage(&self) -> Option<Usage> {
        match self.spend {
            Spend::Reported(u) => Some(u),
            _ => None,
        }
    }
    /// True when spend may have been incurred and a caller counting spend must not treat
    /// the attempt as free: reported usage, or dispatched with unknown usage on a per-token
    /// route.
    pub fn may_have_spend(&self) -> bool {
        self.metering == Metering::PerToken && !matches!(self.spend, Spend::NotSpent)
    }
}

/// Spend state of one attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spend {
    /// The request was not sent, or the provider rejected it (HTTP 4xx other than 408)
    /// before any generation. Providers document these as unbilled.
    NotSpent,
    /// The route reported these token counts for the attempt.
    Reported(Usage),
    /// The request was dispatched and may have been billed, but no counts arrived
    /// (transport failure, timeout, 5xx, a stream cut before its usage frame).
    Unknown,
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
    /// The billing state of this response: `Reported` when `usage_known`, else `Unknown`.
    pub fn billing(&self) -> AttemptBilling {
        AttemptBilling::new(
            if self.usage_known {
                Spend::Reported(self.usage)
            } else {
                Spend::Unknown
            },
            self.metering,
        )
    }
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
