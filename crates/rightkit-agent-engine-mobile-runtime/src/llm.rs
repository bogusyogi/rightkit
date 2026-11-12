//! Minimal provider adapter for the embedded runtime.
//!
//! # Why this exists instead of reusing the daemon's client
//!
//! The daemon's full adapter lives at `engine/host-app/src/provider_client.rs`
//! — inside the **binary**, with no `[lib]` target — and it is coupled to
//! `crate::inference_runtime` (route resolution, scheduling, failover
//! authority).
//! a binary, so it is unreachable from here.
//!
//! Extracting `provider_client` + `inference_runtime` into a shared crate is
//! the right long-term fix and would give mobile the full route/failover
//! story. It is deliberately **not** M0 work: M0's exit criterion is one real
//! provider turn on a physical iPhone, and 2.6k lines of daemon route
//! scheduling does not serve it while risking the daemon.
//!
//! So this adapter covers exactly the core path: transcript in, one
//! completed turn out. It has **no** failover, no route scheduling, no
//! reasoning-budget negotiation, and no stable prefix cache keys. Anything
//! relying on those must not assume they work here.
//!
//! Both provider clients accept OpenAI-shaped wire messages —
//! `AnthropicCompatClient` re-shapes them internally and lifts `system` out —
//! so one projection serves both protocols.
//!
//! On iOS, Apple's Foundation Models backend crosses a narrow host bridge.
//! It uses this same transcript/tool/session path, validates fixed bounds,
//! and never constructs a network client or accepts an API key.

use crate::{ProviderConfig, RuntimeError};
use rightkit_agent_coding_tools::registry::ToolRegistry;
use rightkit_agent_engine_core::llm::{LlmClient, LlmResponse};
use rightkit_agent_engine_protocol::ToolCall;
use rightkit_agent_engine_protocol::TranscriptMessage;
use rightkit_llm::agent_providers::anthropic_client::AnthropicCompatClient;
use rightkit_llm::agent_providers::client::{CompletedTurn, OpenAiCompatClient};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

#[cfg(target_os = "ios")]
use std::ffi::{c_char, CStr, CString};

/// Bounds applied before an on-device backend receives any work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnDeviceInferenceLimits {
    /// Maximum retained context, in caller-estimated tokens.
    pub max_context_tokens: usize,
    /// Maximum generated output token budget.
    pub max_output_tokens: u32,
    /// Maximum wall-clock budget passed to the backend.
    pub max_wall_clock: Duration,
}

impl Default for OnDeviceInferenceLimits {
    fn default() -> Self {
        Self {
            max_context_tokens: 8_192,
            max_output_tokens: 1_024,
            max_wall_clock: Duration::from_secs(30),
        }
    }
}

/// A transcript request for the bounded on-device inference seam.
#[derive(Debug, Clone)]
pub struct OnDeviceInferenceRequest {
    /// Transcript sent to the local model.
    pub messages: Vec<TranscriptMessage>,
    /// Caller-estimated token count for the retained context.
    pub context_tokens: usize,
    /// Requested output token budget.
    pub max_output_tokens: u32,
    /// Requested wall-clock budget.
    pub wall_clock: Duration,
    /// Exact tool schemas exposed by this host's shared engine contract.
    /// Native inference receives these rather than inventing a second mobile
    /// tool surface, then returns ordinary `ToolCall` values for the engine to
    /// validate and dispatch.
    pub tool_schemas: Vec<Value>,
}

/// Typed failures from on-device inference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnDeviceInferenceError {
    /// No model weights or real local backend are present in this workspace.
    NoLocalModelAvailable,
    /// The retained transcript exceeds the configured context bound.
    ContextLengthExceeded { requested: usize, maximum: usize },
    /// The requested output budget exceeds the configured bound.
    TokenBudgetExceeded { requested: u32, maximum: u32 },
    /// The requested wall-clock budget exceeds the configured bound.
    WallClockExceeded {
        requested: Duration,
        maximum: Duration,
    },
    /// The iOS host bridge could not complete a local Foundation Models turn.
    Bridge(String),
}

impl std::fmt::Display for OnDeviceInferenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoLocalModelAvailable => formatter.write_str("no local model available"),
            Self::ContextLengthExceeded { requested, maximum } => write!(
                formatter,
                "on-device context length {requested} exceeds limit {maximum}"
            ),
            Self::TokenBudgetExceeded { requested, maximum } => write!(
                formatter,
                "on-device token budget {requested} exceeds limit {maximum}"
            ),
            Self::WallClockExceeded { requested, maximum } => write!(
                formatter,
                "on-device wall clock {requested:?} exceeds limit {maximum:?}"
            ),
            Self::Bridge(message) => write!(formatter, "on-device inference bridge: {message}"),
        }
    }
}

impl std::error::Error for OnDeviceInferenceError {}

/// A product-owned implementation of on-device inference.
#[async_trait::async_trait]
pub trait OnDeviceInferenceBackend: Send + Sync {
    /// Execute one request after the seam has validated all bounds.
    async fn infer(
        &self,
        request: OnDeviceInferenceRequest,
    ) -> Result<LlmResponse, OnDeviceInferenceError>;
}

/// Validate all on-device limits without invoking a backend.
pub fn validate_on_device_request(
    request: &OnDeviceInferenceRequest,
    limits: OnDeviceInferenceLimits,
) -> Result<(), OnDeviceInferenceError> {
    if request.context_tokens > limits.max_context_tokens {
        return Err(OnDeviceInferenceError::ContextLengthExceeded {
            requested: request.context_tokens,
            maximum: limits.max_context_tokens,
        });
    }
    if request.max_output_tokens > limits.max_output_tokens {
        return Err(OnDeviceInferenceError::TokenBudgetExceeded {
            requested: request.max_output_tokens,
            maximum: limits.max_output_tokens,
        });
    }
    if request.wall_clock > limits.max_wall_clock {
        return Err(OnDeviceInferenceError::WallClockExceeded {
            requested: request.wall_clock,
            maximum: limits.max_wall_clock,
        });
    }
    Ok(())
}

/// Invoke an on-device backend only after context, token, and wall-clock
/// bounds have passed.
pub async fn infer_on_device<B: OnDeviceInferenceBackend + ?Sized>(
    backend: &B,
    request: OnDeviceInferenceRequest,
    limits: OnDeviceInferenceLimits,
) -> Result<LlmResponse, OnDeviceInferenceError> {
    validate_on_device_request(&request, limits)?;
    backend.infer(request).await
}

/// Placeholder backend used until the workspace contains a real local model.
pub struct NoLocalModelBackend;

#[async_trait::async_trait]
impl OnDeviceInferenceBackend for NoLocalModelBackend {
    async fn infer(
        &self,
        _request: OnDeviceInferenceRequest,
    ) -> Result<LlmResponse, OnDeviceInferenceError> {
        Err(OnDeviceInferenceError::NoLocalModelAvailable)
    }
}

/// An `LlmClient` adapter for a bounded on-device backend.
pub struct OnDeviceLlmClient {
    backend: Arc<dyn OnDeviceInferenceBackend>,
    limits: OnDeviceInferenceLimits,
    tool_schemas: Vec<Value>,
}

/// Construct an unavailable on-device client without claiming model readiness.
pub fn unavailable_on_device_client(limits: OnDeviceInferenceLimits) -> Arc<dyn LlmClient> {
    Arc::new(OnDeviceLlmClient {
        backend: Arc::new(NoLocalModelBackend),
        limits,
        tool_schemas: Vec::new(),
    })
}

/// Construct the real iOS-native inference route. On non-iOS hosts the same
/// provider selection stays deliberately unavailable; this keeps desktop
/// tests/linking free of an Apple-framework symbol while never falling back to
/// a network transport.
pub fn native_on_device_client(
    github_configured: bool,
    cloudflare_configured: bool,
    limits: OnDeviceInferenceLimits,
) -> Arc<dyn LlmClient> {
    #[cfg(target_os = "ios")]
    {
        Arc::new(OnDeviceLlmClient {
            backend: Arc::new(FoundationModelsBridgeBackend),
            limits,
            tool_schemas: tool_schemas_for_host(github_configured, cloudflare_configured),
        })
    }

    #[cfg(not(target_os = "ios"))]
    {
        let _ = (github_configured, cloudflare_configured);
        unavailable_on_device_client(limits)
    }
}

/// Foundation Models lives in Swift. This narrow ABI transports only the
/// transcript, shared tool schemas, and fixed bounds; it has no URL, key, or
/// network-capable input. Swift returns regular the host app tool calls, so Rust
/// still owns validation, approvals, execution, events, and persistence.
#[cfg(target_os = "ios")]
struct FoundationModelsBridgeBackend;

#[cfg(target_os = "ios")]
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct FoundationModelsBridgeRequest<'a> {
    messages: &'a [TranscriptMessage],
    tool_schemas: &'a [Value],
    max_output_tokens: u32,
    wall_clock_milliseconds: u64,
}

#[cfg(target_os = "ios")]
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct FoundationModelsBridgeResponse {
    #[serde(default)]
    text: String,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
    #[serde(default)]
    finish_reason: Option<String>,
    #[serde(default)]
    prompt_tokens: u32,
    #[serde(default)]
    completion_tokens: u32,
    #[serde(default)]
    error: Option<String>,
}

#[cfg(target_os = "ios")]
unsafe extern "C" {
    fn host_ios_foundation_models_infer(request_json: *const c_char) -> *mut c_char;
    fn host_ios_foundation_models_free_string(response_json: *mut c_char);
}

#[cfg(target_os = "ios")]
fn invoke_foundation_models_bridge(
    request: OnDeviceInferenceRequest,
) -> Result<LlmResponse, OnDeviceInferenceError> {
    let bridge_request = FoundationModelsBridgeRequest {
        messages: &request.messages,
        tool_schemas: &request.tool_schemas,
        max_output_tokens: request.max_output_tokens,
        wall_clock_milliseconds: request.wall_clock.as_millis().min(u128::from(u64::MAX)) as u64,
    };
    let json = serde_json::to_string(&bridge_request)
        .map_err(|error| OnDeviceInferenceError::Bridge(format!("encode request: {error}")))?;
    let input = CString::new(json)
        .map_err(|_| OnDeviceInferenceError::Bridge("request contains NUL".into()))?;
    let response = unsafe { host_ios_foundation_models_infer(input.as_ptr()) };
    if response.is_null() {
        return Err(OnDeviceInferenceError::Bridge(
            "Foundation Models bridge returned no response".into(),
        ));
    }
    let raw = unsafe { CStr::from_ptr(response) }
        .to_string_lossy()
        .into_owned();
    unsafe { host_ios_foundation_models_free_string(response) };
    let response: FoundationModelsBridgeResponse = serde_json::from_str(&raw)
        .map_err(|error| OnDeviceInferenceError::Bridge(format!("decode response: {error}")))?;
    if let Some(error) = response.error.filter(|value| !value.is_empty()) {
        return Err(OnDeviceInferenceError::Bridge(error));
    }
    Ok(LlmResponse {
        text: response.text,
        reasoning: response.reasoning,
        tool_calls: response.tool_calls,
        finish_reason: response.finish_reason,
        prompt_tokens: response.prompt_tokens,
        completion_tokens: response.completion_tokens,
        cache_read_tokens: 0,
        cache_creation_tokens: 0,
    })
}

#[cfg(target_os = "ios")]
#[async_trait::async_trait]
impl OnDeviceInferenceBackend for FoundationModelsBridgeBackend {
    async fn infer(
        &self,
        request: OnDeviceInferenceRequest,
    ) -> Result<LlmResponse, OnDeviceInferenceError> {
        tokio::task::spawn_blocking(move || invoke_foundation_models_bridge(request))
            .await
            .map_err(|error| OnDeviceInferenceError::Bridge(format!("bridge task: {error}")))?
    }
}

/// Which wire protocol a route speaks.
enum Route {
    OpenAi(Box<OpenAiCompatClient>),
    Anthropic(Box<AnthropicCompatClient>),
    Responses(Box<ResponsesClient>),
}

struct ResponsesClient {
    http: rightkit_http::AsyncClient,
    extra_headers: Vec<(String, String)>,
    endpoint: String,
    api_key: String,
    model: String,
    reasoning_effort: Option<String>,
}

/// One provider route plus the tool schemas this host actually offers.
pub struct MobileLlmClient {
    route: Route,
    /// Ordered fallback routes (ENG-31), tried in order only after `route`'s
    /// own internal retry ladder is exhausted. Empty for hosts that never
    /// configured `fallback_models` — behavior is then identical to before
    /// this field existed.
    fallback: Vec<Route>,
    /// Frozen at construction from the host's registry, so the schema the
    /// model sees can never drift from the tools the host can run.
    tool_schemas: Vec<Value>,
    max_output_tokens: Option<u32>,
}

/// A provider route plus the ordered fallback model slugs to fail over to
/// once the primary is exhausted (ENG-31). Fallback entries reuse the
/// primary's protocol, base URL, API key, and reasoning effort — only the
/// model slug differs, matching how a single provider account typically
/// offers several models behind one key and endpoint.
///
/// No `ProviderRoute` type exists elsewhere in the codebase (checked
/// `registry::ModelRoute`, `rightkit_llm::failover` route entries, and
/// `mobile_runtime::ProviderConfig` — none carry a fallback list), so this
/// is the mobile-runtime-local home for it; `client_for` keeps working
/// unchanged for hosts that never touch this type.
#[derive(Debug, Clone)]
pub struct ProviderRoute {
    pub config: ProviderConfig,
    /// Model slugs to try, in order, after `config.model` is exhausted.
    pub fallback_models: Vec<String>,
}

/// Build a client for the host's chosen provider, with no fallback routes.
/// Preserved as-is for existing callers; see [`client_for_route`] for
/// failover-aware construction.
pub fn client_for(
    config: &ProviderConfig,
    github_configured: bool,
    cloudflare_configured: bool,
) -> Result<Arc<dyn LlmClient>, RuntimeError> {
    if matches!(
        config.protocol.trim().to_ascii_lowercase().as_str(),
        "on-device" | "foundation-models"
    ) {
        if config.model.trim().is_empty() {
            return Err(RuntimeError::Config("on-device model is empty".into()));
        }
        return Ok(native_on_device_client(
            github_configured,
            cloudflare_configured,
            OnDeviceInferenceLimits::default(),
        ));
    }
    let route = build_route(config)?;
    Ok(Arc::new(MobileLlmClient {
        route,
        fallback: Vec::new(),
        tool_schemas: tool_schemas_for_host(github_configured, cloudflare_configured),
        // Anthropic requires an explicit ceiling; OpenAI-compatible ignores it.
        max_output_tokens: Some(8_192),
    }))
}

/// Build a provider client for bounded orchestration assists. Unlike a
/// session client it exposes no tool schemas, so recap, goal-audit, & diff
/// narration requests cannot solicit a workspace mutation.
pub fn assist_client_for(config: &ProviderConfig) -> Result<Arc<dyn LlmClient>, RuntimeError> {
    if matches!(
        config.protocol.trim().to_ascii_lowercase().as_str(),
        "on-device" | "foundation-models"
    ) {
        let limits = OnDeviceInferenceLimits {
            max_output_tokens: 512,
            ..OnDeviceInferenceLimits::default()
        };
        #[cfg(target_os = "ios")]
        {
            return Ok(Arc::new(OnDeviceLlmClient {
                backend: Arc::new(FoundationModelsBridgeBackend),
                limits,
                tool_schemas: Vec::new(),
            }));
        }
        #[cfg(not(target_os = "ios"))]
        {
            return Ok(unavailable_on_device_client(limits));
        }
    }
    let route = build_route(config)?;
    Ok(Arc::new(MobileLlmClient {
        route,
        fallback: Vec::new(),
        tool_schemas: Vec::new(),
        max_output_tokens: Some(512),
    }))
}

/// Build a client for `route.config`, additionally consulting
/// `route.fallback_models` (ENG-31) when the primary model's own retry
/// ladder is exhausted. A fallback route that itself fails to construct
/// (bad model slug format, etc.) aborts construction of the whole client —
/// better to fail loudly at session start than silently drop a fallback the
/// host explicitly configured.
pub fn client_for_route(
    route: &ProviderRoute,
    github_configured: bool,
    cloudflare_configured: bool,
) -> Result<Arc<dyn LlmClient>, RuntimeError> {
    let primary = build_route(&route.config)?;
    let mut fallback = Vec::with_capacity(route.fallback_models.len());
    for model in &route.fallback_models {
        let mut config = route.config.clone();
        model.clone_into(&mut config.model);
        fallback.push(build_route(&config)?);
    }
    Ok(Arc::new(MobileLlmClient {
        route: primary,
        fallback,
        tool_schemas: tool_schemas_for_host(github_configured, cloudflare_configured),
        max_output_tokens: Some(8_192),
    }))
}

/// Resolve one [`ProviderConfig`] into a wire [`Route`]. Shared by
/// [`client_for`] and [`client_for_route`] so primary and fallback routes go
/// through identical validation.
fn build_route(config: &ProviderConfig) -> Result<Route, RuntimeError> {
    if config.api_key.trim().is_empty() {
        return Err(RuntimeError::Config("provider apiKey is empty".into()));
    }
    if config.model.trim().is_empty() {
        return Err(RuntimeError::Config("provider model is empty".into()));
    }
    let endpoint = url::Url::parse(config.base_url.trim())
        .map_err(|_| RuntimeError::Config("provider baseUrl must be a valid HTTPS URL".into()))?;
    if endpoint.scheme() != "https" || endpoint.host_str().is_none() {
        return Err(RuntimeError::Config(
            "provider baseUrl must use HTTPS".into(),
        ));
    }
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        return Err(RuntimeError::Config(
            "provider baseUrl must not contain credentials".into(),
        ));
    }
    let reasoning_effort = normalize_reasoning_effort(config.reasoning_effort.as_deref())?;
    let opencode_session = (endpoint.host_str() == Some("opencode.ai"))
        .then_some(config.session_id.as_deref())
        .flatten();
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(session_id) = opencode_session {
        if session_id.chars().any(char::is_control) {
            return Err(RuntimeError::Config(
                "session ID contains invalid header characters".into(),
            ));
        }
        headers.push(("x-opencode-session".to_string(), session_id.to_string()));
    }

    let route = match config.protocol.to_ascii_lowercase().as_str() {
        "openai" | "openai-compatible" if uses_responses_api(config) => {
            Route::Responses(Box::new(ResponsesClient {
                http: responses_http_client(&config.base_url)
                    .map_err(|error| RuntimeError::Config(format!("HTTP client: {error}")))?,
                extra_headers: headers,
                endpoint: format!("{}/responses", config.base_url.trim_end_matches('/')),
                api_key: config.api_key.clone(),
                model: config.model.clone(),
                reasoning_effort: reasoning_effort.clone(),
            }))
        }
        "openai" | "openai-compatible" => {
            let mut client = OpenAiCompatClient::new(
                config.api_key.clone(),
                config.model.clone(),
                config.base_url.clone(),
            );
            if let Some(session_id) = opencode_session {
                client = client.with_opencode_session(session_id);
            }
            if let Some(effort) = reasoning_effort {
                client = if effort == "none" {
                    client.with_reasoning_disabled()
                } else {
                    client.with_effort(effort)
                };
            }
            Route::OpenAi(Box::new(client))
        }
        "anthropic" | "anthropic-compatible" => {
            let mut client = AnthropicCompatClient::new(
                config.api_key.clone(),
                config.model.clone(),
                config.base_url.clone(),
            );
            if let Some(session_id) = opencode_session {
                client = client.with_opencode_session(session_id);
            }
            Route::Anthropic(Box::new(client))
        }
        other => {
            return Err(RuntimeError::Config(format!(
                "unsupported provider protocol '{other}'; expected 'openai' or 'anthropic'"
            )))
        }
    };

    Ok(route)
}

fn normalize_reasoning_effort(raw: Option<&str>) -> Result<Option<String>, RuntimeError> {
    let Some(raw) = raw else { return Ok(None) };
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() || normalized == "auto" {
        return Ok(None);
    }
    if matches!(
        normalized.as_str(),
        "none" | "low" | "medium" | "high" | "xhigh"
    ) {
        return Ok(Some(normalized));
    }
    Err(RuntimeError::Config(format!(
        "unknown provider reasoningEffort '{raw}'; expected auto, none, low, medium, high, or xhigh"
    )))
}

/// OpenCode Zen publishes Muse through Responses API while HY3 and most free
/// routes use Chat Completions. Endpoint selection is model metadata encoded
/// by Zen today; keep it narrow so ordinary OpenAI-compatible models retain
/// their existing wire contract.
fn uses_responses_api(config: &ProviderConfig) -> bool {
    config.base_url.contains("opencode.ai/zen") && config.model.starts_with("muse-")
}

/// Wire tool schemas for every tool the non-`host` registry actually
/// registered. A tool absent here is absent from the model's options — the
/// mobile capability profile is enforced by construction, not by refusal.
fn tool_schemas_for_host(github_configured: bool, cloudflare_configured: bool) -> Vec<Value> {
    ToolRegistry::build_default()
        .all()
        .iter()
        .filter(|tool| {
            crate::is_mobile_supported_tool(tool.name(), github_configured, cloudflare_configured)
        })
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name(),
                    "description": tool.description(),
                    "parameters": tool.input_schema(),
                }
            })
        })
        .collect()
}

/// Project the engine transcript onto OpenAI-shaped wire messages.
///
/// Compaction and fold markers are projected as system notes rather than
/// dropped: a model that cannot see that history was summarized will happily
/// re-derive what was already decided.
fn project(messages: &[TranscriptMessage]) -> Vec<Value> {
    let mut wire = Vec::with_capacity(messages.len());
    for message in messages {
        if !message.is_provider_visible() {
            continue;
        }
        match message {
            TranscriptMessage::System { content, .. } => {
                wire.push(json!({ "role": "system", "content": content }));
            }
            TranscriptMessage::User { content, .. } => {
                wire.push(json!({ "role": "user", "content": content }));
            }
            TranscriptMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                let mut entry = json!({ "role": "assistant", "content": content });
                if !tool_calls.is_empty() {
                    entry["tool_calls"] = Value::Array(
                        tool_calls
                            .iter()
                            .map(|call| {
                                json!({
                                    "id": call.id,
                                    "type": "function",
                                    "function": {
                                        "name": call.name,
                                        "arguments": call.arguments_json,
                                    }
                                })
                            })
                            .collect(),
                    );
                }
                wire.push(entry);
            }
            TranscriptMessage::Tool {
                tool_call_id,
                summary,
                ..
            } => {
                wire.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_call_id,
                    "content": summary,
                }));
            }
            TranscriptMessage::Compact {
                summary,
                dropped_count,
                ..
            } => {
                wire.push(json!({
                    "role": "system",
                    "content": format!(
                        "[compacted: {dropped_count} earlier turn(s) summarized]\n{summary}"
                    ),
                }));
            }
            TranscriptMessage::Fold {
                summary,
                subtask_id,
                dropped_count,
                ..
            } => {
                wire.push(json!({
                    "role": "system",
                    "content": format!(
                        "[folded subtask {subtask_id}: {dropped_count} turn(s) summarized]\n{summary}"
                    ),
                }));
            }
        }
    }
    wire
}

fn responses_tools(schemas: &[Value]) -> Vec<Value> {
    schemas
        .iter()
        .map(|schema| {
            let function = &schema["function"];
            json!({
                "type": "function",
                "name": function["name"],
                "description": function["description"],
                "parameters": function["parameters"],
                "strict": false
            })
        })
        .collect()
}

fn project_responses(messages: &[TranscriptMessage]) -> Vec<Value> {
    let mut input = Vec::new();
    for message in messages {
        if !message.is_provider_visible() {
            continue;
        }
        match message {
            TranscriptMessage::System { content, .. } => {
                input.push(json!({"role": "system", "content": content}));
            }
            TranscriptMessage::User { content, .. } => {
                input.push(json!({"role": "user", "content": content}));
            }
            TranscriptMessage::Assistant {
                content,
                tool_calls,
                ..
            } => {
                if !content.is_empty() {
                    input.push(json!({"role": "assistant", "content": content}));
                }
                for call in tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": call.arguments_json
                    }));
                }
            }
            TranscriptMessage::Tool {
                tool_call_id,
                summary,
                ..
            } => input.push(json!({
                "type": "function_call_output",
                "call_id": tool_call_id,
                "output": summary
            })),
            TranscriptMessage::Compact {
                summary,
                dropped_count,
                ..
            } => input.push(json!({
                "role": "system",
                "content": format!(
                    "[compacted: {dropped_count} earlier turn(s) summarized]\n{summary}"
                )
            })),
            TranscriptMessage::Fold {
                summary,
                subtask_id,
                dropped_count,
                ..
            } => input.push(json!({
                "role": "system",
                "content": format!(
                    "[folded subtask {subtask_id}: {dropped_count} turn(s) summarized]\n{summary}"
                )
            })),
        }
    }
    input
}

impl ResponsesClient {
    fn request_body(&self, messages: &[TranscriptMessage], tool_schemas: &[Value]) -> Value {
        let mut body = json!({
            "model": self.model,
            "input": project_responses(messages),
            "tools": responses_tools(tool_schemas),
            "tool_choice": "auto"
        });
        if let Some(effort) = &self.reasoning_effort {
            body["reasoning"] = json!({ "effort": effort });
        }
        body
    }

    async fn complete(
        &self,
        messages: &[TranscriptMessage],
        tool_schemas: &[Value],
    ) -> Result<LlmResponse, String> {
        let body = self.request_body(messages, tool_schemas);
        // A paid, non-idempotent POST: an HTTP answer (429 and 5xx included) is final and is
        // never re-sent, because a 5xx may already have been processed and billed. rightkit-http
        // re-sends it only for a failure before the request left (connect phase).
        let mut request = rightkit_http::Request::post(self.endpoint.clone())
            .bearer(&self.api_key)
            .json(&body)
            .map_err(|error| format!("Responses request failed: {error}"))?;
        for (name, value) in &self.extra_headers {
            request = request.header(name.as_str(), value.as_str());
        }
        let response = self
            .http
            .send_any(&request)
            .await
            .map_err(|error| format!("Responses request failed: {error}"))?;
        let status = response.status;
        let body = response.text();
        if !(200..300).contains(&status) {
            return Err(format!(
                "Responses API returned {}: {}",
                status,
                body.chars().take(300).collect::<String>()
            ));
        }
        let value: Value = serde_json::from_str(&body)
            .map_err(|error| format!("Decode Responses result: {error}"))?;
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut tool_calls = Vec::new();
        for item in value["output"].as_array().into_iter().flatten() {
            match item["type"].as_str() {
                Some("message") => {
                    for content in item["content"].as_array().into_iter().flatten() {
                        if content["type"] == "output_text" {
                            if let Some(fragment) = content["text"].as_str() {
                                text.push_str(fragment);
                            }
                        }
                    }
                }
                Some("reasoning") => {
                    for summary in item["summary"].as_array().into_iter().flatten() {
                        if let Some(fragment) = summary["text"].as_str() {
                            if !reasoning.is_empty() {
                                reasoning.push('\n');
                            }
                            reasoning.push_str(fragment);
                        }
                    }
                }
                Some("function_call") => {
                    let Some(id) = item["call_id"].as_str() else {
                        continue;
                    };
                    let Some(name) = item["name"].as_str() else {
                        continue;
                    };
                    tool_calls.push(ToolCall {
                        id: id.to_owned(),
                        name: name.to_owned(),
                        arguments_json: item["arguments"].as_str().unwrap_or("{}").to_owned(),
                    });
                }
                _ => {}
            }
        }
        let prompt_tokens = value["usage"]["input_tokens"].as_u64().unwrap_or(0) as u32;
        let completion_tokens = value["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32;
        Ok(LlmResponse {
            text,
            reasoning: (!reasoning.is_empty()).then_some(reasoning),
            finish_reason: Some(
                if tool_calls.is_empty() {
                    "stop"
                } else {
                    "tool_calls"
                }
                .into(),
            ),
            tool_calls,
            prompt_tokens,
            completion_tokens,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        })
    }
}

/// A Responses call is a paid POST, so no HTTP status is ever retried: `retry_statuses` is empty
/// and the request does not opt in to unsafe status retries. The attempts that remain are the
/// connect-phase re-sends rightkit-http makes for a non-idempotent request that never left (name
/// lookup, TCP connect, TLS), bounded so a suspended phone never waits indefinitely in one turn.
fn responses_retry_policy() -> rightkit_http::RetryPolicy {
    rightkit_http::RetryPolicy {
        max_attempts: 3,
        initial_backoff: std::time::Duration::from_millis(250),
        max_backoff: std::time::Duration::from_secs(1),
        multiplier: 2.0,
        jitter: 0.0,
        retry_statuses: Vec::new(),
        honor_retry_after: true,
        max_retry_after: std::time::Duration::from_secs(60),
        defer_over_cap: false,
        // None keeps the legacy single shared counter (only `max_attempts` binds).
        max_status_attempts: None,
        max_transport_attempts: None,
    }
}

/// HTTP client for the Responses route: HTTPS endpoint only (validated by the caller),
/// environment proxy honoured, redirects not followed.
fn responses_http_client(base_url: &str) -> Result<rightkit_http::AsyncClient, String> {
    rightkit_http::AsyncClient::new(responses_client_config(base_url))
        .map_err(|error| error.to_string())
}

fn responses_client_config(base_url: &str) -> rightkit_http::ClientConfig {
    rightkit_http::ClientConfig {
        user_agent: "Agent/0.1".into(),
        connect_timeout: Some(std::time::Duration::from_secs(10)),
        request_timeout: std::time::Duration::from_secs(120),
        response_timeout: Some(std::time::Duration::from_secs(120)),
        retry: responses_retry_policy(),
        proxy: rightkit_http::ProxyMode::Environment,
        https_only: base_url.trim_start().starts_with("https://"),
        ..rightkit_http::ClientConfig::default()
    }
}

fn into_llm_response(turn: CompletedTurn) -> LlmResponse {
    LlmResponse {
        text: turn.text,
        reasoning: turn.reasoning,
        tool_calls: turn.tool_calls,
        finish_reason: turn.finish_reason,
        prompt_tokens: turn.prompt_tokens,
        completion_tokens: turn.completion_tokens,
        cache_read_tokens: turn.cache_read_tokens,
        cache_creation_tokens: turn.cache_creation_tokens,
    }
}

fn estimated_context_tokens(messages: &[TranscriptMessage]) -> usize {
    let bytes = project(messages)
        .iter()
        .map(|value| value.to_string())
        .map(|text| text.len())
        .sum::<usize>();
    bytes.saturating_add(3) / 4
}

#[async_trait::async_trait]
impl LlmClient for OnDeviceLlmClient {
    async fn complete(&self, messages: &[TranscriptMessage]) -> Result<LlmResponse, String> {
        let request = OnDeviceInferenceRequest {
            messages: messages.to_vec(),
            context_tokens: estimated_context_tokens(messages),
            max_output_tokens: self.limits.max_output_tokens,
            wall_clock: self.limits.max_wall_clock,
            tool_schemas: self.tool_schemas.clone(),
        };
        infer_on_device(self.backend.as_ref(), request, self.limits)
            .await
            .map_err(|error| error.to_string())
    }
}

impl MobileLlmClient {
    /// Run one completion attempt against `route`, classifying any failure
    /// (ENG-31) so the caller can decide whether to fail over. Reuses each
    /// client's own internal retry/backoff ladder unchanged — this function
    /// adds no second timer, it only runs *after* that ladder gives up.
    async fn complete_via(
        &self,
        route: &Route,
        messages: &[TranscriptMessage],
        wire: &[Value],
    ) -> Result<LlmResponse, (bool, String)> {
        match route {
            Route::OpenAi(client) => client
                .complete_with_tools(wire.to_vec(), self.tool_schemas.clone())
                .await
                .map(into_llm_response)
                .map_err(|error| (error_allows_failover(&error), error.to_string())),
            Route::Anthropic(client) => client
                .complete_with_tools(
                    wire.to_vec(),
                    self.tool_schemas.clone(),
                    self.max_output_tokens,
                )
                .await
                .map(into_llm_response)
                .map_err(|error| (error_allows_failover(&error), error.to_string())),
            Route::Responses(client) => client
                .complete(messages, &self.tool_schemas)
                .await
                // A string error cannot say whether the request was delivered: never fail over on it.
                .map_err(|message| (false, message)),
        }
    }
}

/// Fail over to another route only when the failed request provably did no work: a connect-phase
/// failure (`NotDelivered`) or an explicit refusal (HTTP 400, 401 or 403, or the typed `Auth`).
/// Everything else may have been delivered and billed, or is not a definite refusal, and is exactly
/// one paid request: 408, 425, 429, any 5xx, a post-send `Network` error, a cut stream, an
/// in-body provider error, `DeliveryUncertain`, and capacity or authority errors.
fn error_allows_failover(error: &rightkit_llm::agent_providers::transport::ProviderError) -> bool {
    use rightkit_llm::agent_providers::transport::ProviderError;
    match error {
        ProviderError::Http { status, .. } => matches!(*status, 400 | 401 | 403),
        ProviderError::NotDelivered(_) | ProviderError::Auth => true,
        _ => false,
    }
}

#[async_trait::async_trait]
impl LlmClient for MobileLlmClient {
    async fn complete(&self, messages: &[TranscriptMessage]) -> Result<LlmResponse, String> {
        let wire = project(messages);
        let (primary, fallback): (&Route, &[Route]) = (&self.route, &self.fallback);
        // ENG-31: try the primary route first. Nothing below this layer has
        // dispatched a tool call or applied any other effect yet — tool
        // dispatch happens one layer up in the engine's turn loop, after
        // `complete` returns — so trying additional fallback routes here can
        // never duplicate an already-committed effect; at most it repeats an
        // HTTP request that never got a usable response.
        match self.complete_via(primary, messages, &wire).await {
            Ok(response) => return Ok(response),
            Err((may_fail_over, message)) => {
                if !may_fail_over || fallback.is_empty() {
                    // An outcome that may have been delivered (post-send error, 408/425/429/5xx, a
                    // cut stream) must not become a second paid request on another route.
                    return Err(message);
                }
                let mut last_error = message;
                for fallback_route in fallback {
                    match self.complete_via(fallback_route, messages, &wire).await {
                        Ok(response) => return Ok(response),
                        Err((next_may_fail_over, next_message)) => {
                            last_error = next_message;
                            if !next_may_fail_over {
                                break;
                            }
                        }
                    }
                }
                Err(last_error)
            }
        }
    }
}
