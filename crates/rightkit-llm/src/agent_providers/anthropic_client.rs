#![allow(clippy::result_large_err)]

//! Streaming Anthropic-compatible client, normalized to [`CompletedTurn`].
//!
//! This stays deliberately separate from the OpenAI wire client: Direct
//! connections select a protocol rather than relying on provider-name guesses.
//!
//! RIGHTKIT-SERIOUS: rightkit-llm has no async cancellable Anthropic stream into a completed turn (thinking replay, multi-anchor cache_control, request authority, submission receipts); the agent cannot run on a blocking provider (report P-1)

use crate::agent_providers::{
    CapacityError, CapacityProfileV1, CompletedTurn, PreparedModalityV1, PreparedProviderRequestV1,
    PreparedRequestGateV1, ProviderError, ProviderResponseStream, ProviderStreamEvent,
};
use futures::StreamExt;
use rightkit_agent_engine_protocol::ToolCall;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Most bytes of one Anthropic SSE body: 4 MiB, or 64 MiB with the host's `large_sse_limit`.
fn max_sse_bytes() -> usize {
    if crate::agent_providers::host::host_options().large_sse_limit {
        64 * 1024 * 1024
    } else {
        4 * 1024 * 1024
    }
}

fn sse_limit_message() -> &'static str {
    if crate::agent_providers::host::host_options().large_sse_limit {
        "stream exceeded 64MiB"
    } else {
        "stream exceeded 4MiB"
    }
}

/// How long Anthropic keeps a cached prefix, as pi's `cacheRetention`.
///
/// - `Long`: one hour (`ttl: "1h"`), for a chat a person is attached to. People pause longer than
///   the 5-minute window, and every use refreshes the entry. A 1-hour write costs 2x the input price.
/// - `Short`: the 5-minute window (write 1.25x), the default. For subagents, headless runs and other
///   unattended work that sends its next request within seconds: the dearer write buys nothing.
/// - `None`: no breakpoints at all, so nothing is written (and nothing is read). For a single request
///   whose prefix will not be seen again, which would only pay the write premium.
///
/// Reads cost the same for every lifetime. All breakpoints in one request share one lifetime (a
/// mismatch is an HTTP 400).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheRetention {
    None,
    #[default]
    Short,
    Long,
}

/// The `cache_control` value for every breakpoint in a native Anthropic request, or `None` when the
/// request must not cache.
fn cache_control_marker(retention: CacheRetention) -> Option<Value> {
    match retention {
        CacheRetention::None => None,
        CacheRetention::Short => Some(json!({"type": "ephemeral"})),
        CacheRetention::Long => Some(
            match crate::agent_providers::cache_rules::ANTHROPIC_NATIVE.extended_ttl {
                Some(ttl) => json!({"type": "ephemeral", "ttl": ttl}),
                None => json!({"type": "ephemeral"}),
            },
        ),
    }
}

/// CRA-10 wiring point for `build_message_body`'s output: prepares the
/// already-built Anthropic `body` into a [`PreparedProviderRequestV1`] over
/// its exact serialized bytes (`content-type: application/json`) and admits it against
/// `profile` before the request is issued. `route_fingerprint` should
/// identify the exact Anthropic route/base URL in use.
fn prepare_and_admit_message_body(
    body: &Value,
    route_fingerprint: impl Into<String>,
    model_id: impl Into<String>,
    serializer_generation: u64,
    tool_catalog_generation: Option<u64>,
    modality: PreparedModalityV1,
    profile: CapacityProfileV1,
) -> Result<(PreparedProviderRequestV1, u64), CapacityError> {
    let bytes = serde_json::to_vec(body).unwrap_or_default();
    let prepared = PreparedProviderRequestV1::new(
        bytes,
        "application/json",
        route_fingerprint,
        model_id,
        serializer_generation,
        tool_catalog_generation,
        modality,
    );
    let gate = PreparedRequestGateV1::new(profile);
    let measured = gate.admit(&prepared)?;
    Ok((prepared, measured))
}

const ANTHROPIC_MESSAGE_SERIALIZER_GENERATION_V1: u64 = 1;

fn prepare_actual_message_request(
    client: &AnthropicCompatClient,
    body: &Value,
    continuation: bool,
) -> Result<PreparedProviderRequestV1, ProviderError> {
    let profile = crate::agent_providers::model_metadata::send_capacity_profile(
        &client.model_id,
        &client.base_url,
    )?;
    if client.request_authority.is_none() && (!continuation || client.direct_dispatch) {
        let rendered_body = body.to_string();
        let modality = if rendered_body.contains("image_url")
            || rendered_body.contains("\"type\":\"image\"")
        {
            PreparedModalityV1::TextPlusInlineImages
        } else {
            PreparedModalityV1::TextOnly
        };
        let (mut prepared, measured) = prepare_and_admit_message_body(
            body,
            client.base_url.clone(),
            client.model_id.clone(),
            ANTHROPIC_MESSAGE_SERIALIZER_GENERATION_V1,
            None,
            modality,
            profile,
        )?;
        prepared.measured_tokens = Some(measured);
        return Ok(prepared);
    }
    crate::agent_providers::client::prepare_authorized_request(
        body.clone(),
        profile,
        ANTHROPIC_MESSAGE_SERIALIZER_GENERATION_V1,
        None,
        client.request_authority.as_ref(),
        continuation,
    )
}

/// Anthropic SSE framing over `rightkit_http::SseDecoder` (WHATWG framing):
/// a frame ends at an empty line, LF, CR and CRLF are accepted, a multibyte
/// codepoint split across chunks is never corrupted, and both a line and an
/// aggregate event are bounded by the shared provider framing ceiling. Each
/// returned frame holds the frame's `data` payload (the `data:` prefix is
/// already removed by the decoder).
struct AnthropicSseDecoder {
    inner: rightkit_http::SseDecoder,
}

impl AnthropicSseDecoder {
    /// 1 MiB: generously larger than any legitimate SSE line or frame (a full
    /// tool-call-arguments chunk included), small enough that a runaway stream
    /// cannot exhaust memory before tripping it.
    const MAX_FRAME_BYTES: usize = 1 << 20;

    fn new() -> Self {
        Self {
            inner: rightkit_http::SseDecoder::with_limits(
                Self::MAX_FRAME_BYTES,
                Self::MAX_FRAME_BYTES,
            ),
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<String>>, String> {
        let events = self.inner.push(chunk).map_err(|error| error.to_string())?;
        Ok(events.into_iter().map(|event| vec![event.data]).collect())
    }

    /// True when the stream holds a partial line or a `data:` frame still
    /// waiting for its terminating blank line.
    fn has_incomplete_frame(&self) -> bool {
        self.inner.has_pending()
    }
}

#[derive(Clone)]
pub struct AnthropicCompatClient {
    http: rightkit_http::AsyncClient,
    auth_header: Option<(String, String)>,
    public_headers: BTreeMap<String, String>,
    model_id: String,
    base_url: String,
    submission_observer: crate::agent_providers::transport::SubmissionObserver,
    request_authority: Option<crate::agent_providers::client::RequestAuthorityBindingV1>,
    direct_dispatch: bool,
    /// Test seam for the semantic-progress deadline; `None` uses the default.
    semantic_progress_limit: Option<std::time::Duration>,
    /// Operator thinking/effort settings; shaped per model at request time.
    thinking: crate::agent_providers::anthropic_thinking::ThinkingSettings,
    cache_retention: CacheRetention,
    deferred_tools: Vec<Value>,
    retry_notifier: rightkit_agent_engine_protocol::RetryNotifier,
    /// Opt-in thinking-block binding (`None`: nothing is sent, nothing is dropped).
    thinking_binding: Option<ThinkingBlockBinding>,
}

impl AnthropicCompatClient {
    pub fn new(
        api_key: impl Into<String>,
        model_id: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        let base_url = base_url.into();
        Self {
            // No proxy and no redirects: the key goes to the configured host only.
            http: crate::agent_providers::http::provider_client(
                &base_url,
                crate::agent_providers::http::Route::NoProxy,
            ),
            auth_header: Some(("x-api-key".into(), api_key.into())),
            public_headers: BTreeMap::new(),
            model_id: model_id.into(),
            base_url,
            submission_observer: Arc::new(|_| {}),
            request_authority: None,
            direct_dispatch: false,
            semantic_progress_limit: None,
            thinking: Default::default(),
            cache_retention: CacheRetention::default(),
            deferred_tools: Vec::new(),
            retry_notifier: Default::default(),
            thinking_binding: None,
        }
    }

    pub fn new_direct(
        model_id: impl Into<String>,
        base_url: impl Into<String>,
        auth_header: Option<(String, String)>,
        public_headers: BTreeMap<String, String>,
        resolved_addresses: &[std::net::SocketAddr],
    ) -> Self {
        let base_url = base_url.into();
        Self {
            http: crate::agent_providers::http::provider_client(
                &base_url,
                crate::agent_providers::http::Route::Pinned(resolved_addresses),
            ),
            auth_header,
            public_headers,
            model_id: model_id.into(),
            base_url,
            submission_observer: Arc::new(|_| {}),
            request_authority: None,
            direct_dispatch: false,
            semantic_progress_limit: None,
            thinking: Default::default(),
            cache_retention: CacheRetention::default(),
            deferred_tools: Vec::new(),
            retry_notifier: Default::default(),
            thinking_binding: None,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Set the reasoning effort ("low" | "medium" | "high" | "xhigh" | "max";
    /// "none" turns thinking off where the model allows it). Sent as
    /// `output_config.effort` and clamped to the model's supported levels.
    pub fn with_effort(mut self, effort: impl Into<String>) -> Self {
        let effort = effort.into();
        self.thinking.effort =
            (!effort.trim().is_empty()).then(|| effort.trim().to_ascii_lowercase());
        self
    }

    /// Set the manual thinking budget used by models that only accept
    /// `{type:"enabled", budget_tokens}`; adaptive models ignore it.
    pub fn with_thinking_budget_tokens(mut self, tokens: u32) -> Self {
        self.thinking.budget_tokens = (tokens > 0).then_some(tokens);
        self
    }

    /// Turn thinking off wherever the model has an off switch.
    pub fn with_thinking_disabled(mut self) -> Self {
        self.thinking.disabled = true;
        self
    }

    /// Set how long Anthropic keeps the cached prefix (see [`CacheRetention`]). The default is
    /// [`CacheRetention::Short`], the 5-minute `ephemeral` marker every request carried before.
    pub fn with_cache_retention(mut self, retention: CacheRetention) -> Self {
        self.cache_retention = retention;
        self
    }

    /// Tool definitions (OpenAI function shape) that are listed with `defer_loading` and announced
    /// to the model by `tool_addition` blocks after a `tool_search` call, instead of being sent in
    /// full. Ignored on models without inline tool additions. Empty by default.
    pub fn with_deferred_tools(mut self, definitions: Vec<Value>) -> Self {
        self.deferred_tools = definitions;
        self
    }

    /// Report stream-establishment retries (429/overload waits) through `notifier`.
    pub fn with_retry_notifier(
        mut self,
        notifier: rightkit_agent_engine_protocol::RetryNotifier,
    ) -> Self {
        self.retry_notifier = notifier;
        self
    }

    /// Opt in to thinking-block binding: sends beta `thinking-binding-controls-2026-08-01` and
    /// `thinking.block_binding.prefix_mismatch_behavior` (`error` or `drop_block`), and removes
    /// the thinking blocks of another model or provider from the request (never from the stored
    /// history). Off by default.
    pub fn with_thinking_block_binding(mut self, binding: ThinkingBlockBinding) -> Self {
        self.thinking_binding = Some(binding);
        self
    }

    fn inline_tools_enabled(&self) -> bool {
        !self.deferred_tools.is_empty() && model_supports_inline_tools(&self.model_id)
    }

    /// Identify one OpenCode conversation without impersonating its client.
    pub fn with_opencode_session(mut self, session_id: &str) -> Self {
        self.public_headers
            .insert("x-opencode-session".into(), session_id.into());
        self.public_headers.insert(
            "user-agent".into(),
            format!("{}/0.1", super::host::user_agent_product()),
        );
        self
    }

    /// Attach provider-local observation for each physical submission.
    pub fn with_submission_observer(
        mut self,
        observer: crate::agent_providers::transport::SubmissionObserver,
    ) -> Self {
        self.submission_observer = observer;
        self
    }

    pub fn with_request_authority(
        mut self,
        binding: crate::agent_providers::client::RequestAuthorityBindingV1,
    ) -> Self {
        self.request_authority = Some(binding);
        self
    }

    /// Mark this client as direct (degraded-context) dispatch: there is no
    /// per-turn authority, so a continuation is admitted by capacity alone.
    pub fn with_direct_dispatch(mut self) -> Self {
        self.direct_dispatch = true;
        self
    }

    fn request_builder(&self, url: &str) -> rightkit_http::Request {
        let mut request = rightkit_http::Request::post(url).header("accept", "text/event-stream");
        if let Some((name, value)) = &self.auth_header {
            request = request.secret_header(name.as_str(), value.as_str());
        }
        let has_anthropic_version = self
            .public_headers
            .keys()
            .any(|name| name.eq_ignore_ascii_case("anthropic-version"));
        if !has_anthropic_version {
            request = request.header("anthropic-version", "2023-06-01");
        }
        // The body carries inline-tools fields whenever `inline_tools_enabled()` and the thinking
        // binding field whenever a binding is set, so each beta flag must be sent then: appended to
        // a user-configured `anthropic-beta`, never dropped or duplicated.
        let mut required_betas: Vec<&str> = Vec::new();
        if self.inline_tools_enabled() {
            required_betas.push(INLINE_TOOLS_BETA);
        }
        if self.thinking_binding.is_some() {
            required_betas.push(THINKING_BINDING_BETA);
        }
        let mut has_beta = false;
        for (name, value) in &self.public_headers {
            if !required_betas.is_empty() && name.eq_ignore_ascii_case("anthropic-beta") {
                has_beta = true;
                let present: Vec<&str> = value.split(',').map(str::trim).collect();
                let missing: Vec<&str> = required_betas
                    .iter()
                    .copied()
                    .filter(|beta| !present.contains(beta))
                    .collect();
                if !missing.is_empty() {
                    let merged = format!("{value},{}", missing.join(","));
                    request = request.header(name.as_str(), merged.as_str());
                    continue;
                }
            }
            request = request.header(name.as_str(), value.as_str());
        }
        if !required_betas.is_empty() && !has_beta {
            request = request.header("anthropic-beta", required_betas.join(","));
        }
        request
    }

    /// Build the Anthropic Messages API request body. Pure — no I/O — so the
    /// cache-linkage shape (P0-C 2c/2h) is unit-testable without a network
    /// mock.
    fn build_message_body(
        &self,
        messages: Vec<Value>,
        tools: Vec<Value>,
        max_output_tokens: Option<u32>,
    ) -> Value {
        let options = *crate::agent_providers::host::host_options();
        let inline = self.inline_tools_enabled();
        let deferred_names: std::collections::HashSet<String> = if inline {
            self.deferred_tools
                .iter()
                .filter_map(|def| def.pointer("/function/name").and_then(Value::as_str))
                .filter(|name| valid_tool_name(name))
                .map(str::to_string)
                .collect()
        } else {
            std::collections::HashSet::new()
        };
        let messages = if inline {
            insert_tool_additions(messages, &deferred_names)
        } else {
            messages
        };
        // With the host's `history_repair`, a call another vendor issued can carry an id this API
        // refuses (`^[a-zA-Z0-9_-]+$`, 64 long); the same stable mapping goes on the call and its
        // result, so pairing holds.
        let mut messages = messages;
        if options.history_repair {
            crate::agent_providers::history_transform::normalise_wire_ids(&mut messages);
        }
        // Thinking-block binding, layer 1: this is the request copy; the stored history is never
        // edited. Thinking of another model or provider is left out of this request only.
        if self.thinking_binding.is_some() {
            drop_foreign_thinking(&mut messages, &self.model_id);
        }
        let (system_blocks, mut messages) = anthropic_messages_split(messages);
        repair_tool_pairing(&mut messages);
        let mut tools = tools
            .into_iter()
            .filter(|tool| {
                !tool
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| deferred_names.contains(name))
            })
            .filter_map(anthropic_tool)
            .collect::<Vec<_>>();
        // Anthropic rejects a request whose tools repeat a name ("tools: Tool names must be
        // unique", HTTP 400); a promoted deferred tool can already be in the list. With the host's
        // `anthropic_request_hygiene` the first is kept.
        let mut seen_tool_names = std::collections::HashSet::new();
        if options.anthropic_request_hygiene {
            tools.retain(|tool| {
                tool.get("name")
                    .and_then(Value::as_str)
                    .is_none_or(|name| seen_tool_names.insert(name.to_string()))
            });
        } else {
            // Not filtered, but a deferred definition still must not repeat one of them.
            seen_tool_names.extend(
                tools
                    .iter()
                    .filter_map(|tool| tool.get("name").and_then(Value::as_str))
                    .map(str::to_string),
            );
        }
        let thinking_plan = crate::agent_providers::anthropic_thinking::plan(
            &self.model_id,
            &self.thinking,
            max_output_tokens,
            tool_loop_lacks_thinking_block(&messages),
        );
        let mut body = json!({
            "model": self.model_id,
            "messages": messages,
            "max_tokens": thinking_plan.max_tokens,
            "stream": true,
        });
        if let Some(thinking) = thinking_plan.thinking {
            body["thinking"] = thinking;
            if let Some(binding) = self.thinking_binding {
                if let Some(object) = body["thinking"].as_object_mut() {
                    if object.get("type").and_then(Value::as_str) != Some("disabled") {
                        object.insert(
                            "block_binding".into(),
                            json!({"prefix_mismatch_behavior": binding.wire()}),
                        );
                    }
                }
            }
        }
        if let Some(effort) = thinking_plan.effort {
            body["output_config"] = json!({"effort": effort});
        }
        // Anthropic renders the prompt as tools -> system -> messages, so a
        // breakpoint caches everything BEFORE it. Up to four are allowed; we
        // use at most four:
        //   1. last tool        - tool schemas alone survive a system change
        //   2. first system     - tools + the stable leading system block
        //   3. last system      - tools + all system blocks (volatile host
        //                         overlays follow the stable block and may
        //                         change between turns, so 2 still hits then)
        //   4. last message     - rolling breakpoint over the whole
        //                         conversation so far (history grows
        //                         append-only between turns)
        // Previously only the last tool was marked, which left the system
        // prompt and all history uncached on Anthropic routes.
        let marker = cache_control_marker(self.cache_retention);
        if let (Some(last), Some(marker)) = (tools.last_mut(), &marker) {
            last["cache_control"] = marker.clone();
        }
        if inline {
            // After the breakpoint: a deferred definition cannot carry `cache_control`.
            for def in self.deferred_tools.iter().filter(|def| {
                def.pointer("/function/name")
                    .and_then(Value::as_str)
                    .is_some_and(valid_tool_name)
            }) {
                if let Some(mut tool) = anthropic_tool(def.clone()) {
                    // Names must be unique across the whole array (a registry can list one twice).
                    let unique = tool
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|name| seen_tool_names.insert(name.to_string()));
                    if unique {
                        tool["defer_loading"] = Value::Bool(true);
                        tools.push(tool);
                    }
                }
            }
        }
        if !system_blocks.is_empty() {
            let last_index = system_blocks.len() - 1;
            body["system"] = Value::Array(
                system_blocks
                    .iter()
                    .enumerate()
                    .map(|(index, text)| {
                        let mut block = json!({"type": "text", "text": text});
                        if index == 0 || index == last_index {
                            if let Some(marker) = &marker {
                                block["cache_control"] = marker.clone();
                            }
                        }
                        block
                    })
                    .collect(),
            );
        }
        if let Some(marker) = &marker {
            if options.anthropic_request_hygiene {
                // The API allows four markers in all; spend a spare one on a second message
                // breakpoint.
                let used = tools
                    .iter()
                    .filter(|tool| tool.get("cache_control").is_some())
                    .count()
                    + body["system"].as_array().map_or(0, |blocks| {
                        blocks
                            .iter()
                            .filter(|block| block.get("cache_control").is_some())
                            .count()
                    });
                mark_trailing_message_breakpoint(
                    &mut messages,
                    marker,
                    used + 1
                        < crate::agent_providers::cache_rules::ANTHROPIC_NATIVE.max_breakpoints,
                );
            } else {
                mark_last_message_breakpoint(&mut messages, marker);
            }
        }
        body["messages"] = Value::Array(messages);
        body["tools"] = json!(tools);
        body
    }

    pub async fn complete_with_tools(
        &self,
        messages: Vec<Value>,
        tools: Vec<Value>,
        max_output_tokens: Option<u32>,
    ) -> Result<CompletedTurn, ProviderError> {
        // P2-4 / P0-C 2g: same per-host circuit breaker as the OpenAI-compat
        // client (`crate::agent_providers::client`), so a degraded Anthropic-protocol route is
        // not hammered mid-session either.
        let Some(mut admission) =
            crate::agent_providers::client::breaker_admit(&self.base_url, &self.model_id)
        else {
            return Err(ProviderError::Provider(format!(
                "circuit breaker open for {}: provider has been failing repeatedly, refusing this request to avoid hammering it",
                self.base_url
            )));
        };
        let result = match self
            .complete_with_tools_inner(messages.clone(), tools.clone(), max_output_tokens, false)
            .await
        {
            Ok(turn) => Ok(turn),
            Err(ProviderError::IncompleteOutput {
                partial_text,
                partial_reasoning,
                partial_tool_calls,
                prompt_tokens,
                completion_tokens,
                cache_read_tokens,
                cache_creation_tokens,
                finish_reason,
                ..
            }) => {
                let partial = CompletedTurn {
                    text: partial_text,
                    reasoning: partial_reasoning,
                    tool_calls: partial_tool_calls,
                    prompt_tokens,
                    completion_tokens,
                    cache_read_tokens,
                    cache_creation_tokens,
                    finish_reason: Some(finish_reason),
                    ..Default::default()
                };
                if !crate::agent_providers::client::can_continue_incomplete(&partial) {
                    Err(crate::agent_providers::client::incomplete_output_error(
                        &partial, false, None,
                    ))
                } else {
                    match self
                        .complete_with_tools_inner(
                            crate::agent_providers::client::continuation_messages(
                                messages,
                                &partial.text,
                            ),
                            tools,
                            max_output_tokens,
                            true,
                        )
                        .await
                    {
                        Ok(continued) => Ok(crate::agent_providers::client::merge_continuation(
                            partial, continued,
                        )),
                        Err(ProviderError::IncompleteOutput { .. }) => {
                            Err(crate::agent_providers::client::incomplete_output_error(
                                &partial, true, None,
                            ))
                        }
                        Err(error @ ProviderError::Authority(_)) => Err(error),
                        Err(error) => Err(crate::agent_providers::client::incomplete_output_error(
                            &partial,
                            true,
                            Some(error.to_string()),
                        )),
                    }
                }
            }
            Err(error) => Err(error),
        };
        // Same transient-only failure scoping as the OpenAI-compat client —
        // see `crate::agent_providers::client::is_breaker_relevant_failure`.
        let counts_as_success = result.is_ok()
            || !crate::agent_providers::client::is_breaker_relevant_failure(result.as_ref().err());
        admission.record(counts_as_success);
        result
    }

    /// Start an Anthropic Messages stream and emit each tool block once its
    /// input JSON is complete. The aggregate `complete_with_tools` method is
    /// retained for compatibility callers.
    pub async fn stream_with_tools(
        &self,
        messages: Vec<Value>,
        tools: Vec<Value>,
        max_output_tokens: Option<u32>,
    ) -> Result<ProviderResponseStream, ProviderError> {
        let dropped_tool = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut first = self
            .stream_once(
                messages.clone(),
                tools.clone(),
                max_output_tokens,
                false,
                dropped_tool.clone(),
            )
            .await?;
        let client = self.clone();
        // A text-only `max_tokens` stop continues once, keeping the partial
        // output (mirrors `complete_with_tools`); a cut-off tool call is
        // never executed, replayed or continued.
        let output = async_stream::try_stream! {
            let mut text = String::new();
            let mut saw_tool_call = false;
            let mut finished: Option<Option<String>> = None;
            let mut content_shown = false;
            let mut stream_retries = 0u32;
            let retry_stream_errors = crate::agent_providers::host::host_options().status_retries;
            while let Some(event) = first.next().await {
                // With the host's `status_retries`, a transient error inside the stream, before
                // any content, sends the request again (an overload event arrives after a 200).
                let event = match event {
                    Ok(event) => event,
                    Err(error) if retry_stream_errors
                        && !content_shown
                        && stream_retries < STREAM_ERROR_MAX_RETRIES
                        && stream_error_is_transient(&error) =>
                    {
                        stream_retries += 1;
                        let delay = match &error {
                            ProviderError::RateLimited { retry_after_secs } => {
                                std::time::Duration::from_secs((*retry_after_secs).clamp(1, 30))
                            }
                            _ => std::time::Duration::from_secs(5 * (1u64 << (stream_retries - 1).min(4))),
                        };
                        crate::agent_providers::client::wait_status_retry(
                            &client.retry_notifier,
                            &client.base_url,
                            error.to_string(),
                            stream_retries,
                            STREAM_ERROR_MAX_RETRIES,
                            delay,
                        )
                        .await;
                        first = client
                            .stream_once(
                                messages.clone(),
                                tools.clone(),
                                max_output_tokens,
                                false,
                                dropped_tool.clone(),
                            )
                            .await?;
                        continue;
                    }
                    Err(error) => Err(error)?,
                };
                match event {
                    ProviderStreamEvent::Finished(reason) => finished = Some(reason),
                    other => {
                        match &other {
                            ProviderStreamEvent::Text(delta) => {
                                content_shown = true;
                                text.push_str(delta)
                            }
                            ProviderStreamEvent::Reasoning(_) => content_shown = true,
                            ProviderStreamEvent::ToolCall(_) => {
                                content_shown = true;
                                saw_tool_call = true
                            }
                            _ => {}
                        }
                        yield other;
                    }
                }
            }
            let reason = finished.unwrap_or(None);
            let truncated = reason
                .as_deref()
                .is_some_and(crate::agent_providers::client::is_incomplete_finish_reason);
            let tool_cut = saw_tool_call
                || dropped_tool.load(std::sync::atomic::Ordering::SeqCst);
            if truncated && !tool_cut && !text.trim().is_empty() {
                let mut second = client
                    .stream_once(
                        crate::agent_providers::client::continuation_messages(messages, &text),
                        tools,
                        max_output_tokens,
                        true,
                        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    )
                    .await?;
                while let Some(event) = second.next().await {
                    yield event?;
                }
            } else {
                yield ProviderStreamEvent::Finished(reason);
            }
        };
        Ok(Box::pin(output))
    }

    async fn stream_once(
        &self,
        messages: Vec<Value>,
        tools: Vec<Value>,
        max_output_tokens: Option<u32>,
        continuation: bool,
        dropped_tool: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<ProviderResponseStream, ProviderError> {
        let Some(mut admission) =
            crate::agent_providers::client::breaker_admit(&self.base_url, &self.model_id)
        else {
            return Err(ProviderError::Provider(format!(
                "circuit breaker open for {}: provider has been failing repeatedly, refusing this request to avoid hammering it",
                self.base_url
            )));
        };
        let body = self.build_message_body(messages, tools, max_output_tokens);
        let prepared = prepare_actual_message_request(self, &body, continuation)?;
        let url = format!("{}/messages", self.base_url.trim_end_matches('/'));
        // CR-12: same transient-retry ladder the OpenAI-compatible client
        // uses to establish its stream — previously this was a single-shot
        // request with no retry classification at all.
        let response = crate::agent_providers::client::record_stream_establishment_outcome(
            &mut admission,
            crate::agent_providers::transport::send_observed_stream_request_with_retry(
                &self.http,
                || {
                    self.request_builder(&url)
                        .header("Content-Type", prepared.content_type)
                        .body(prepared.bytes.clone())
                },
                &prepared,
                &self.submission_observer,
                &self.retry_notifier,
                &self.base_url,
            )
            .await,
        )?;
        let options = *crate::agent_providers::host::host_options();
        let reported_rate_limit = if options.usage_and_rate_limit_events {
            parse_anthropic_rate_limit_headers(response.headers())
        } else {
            None
        };
        let progress_limit_cfg =
            self.semantic_progress_limit
                .unwrap_or(std::time::Duration::from_secs(
                    crate::agent_providers::transport::SEMANTIC_PROGRESS_SECS,
                ));
        let logged_model = self.model_id.clone();
        let output = async_stream::try_stream! {
            if let Some(rate_limit) = reported_rate_limit {
                yield ProviderStreamEvent::RateLimit(rate_limit);
            }
            let mut stream = response.bytes_stream();
            let mut previews = crate::agent_providers::partial_tool_args::ToolArgsPreviews::default();
            // CR-13: the shared byte-oriented decoder preserves split UTF-8,
            // accepts LF and CRLF, and bounds both incomplete lines and the
            // aggregate frame before JSON decoding.
            let mut decoder = AnthropicSseDecoder::new();
            let mut total = 0usize;
            let mut tools: Vec<(String, String, String, bool)> = Vec::new();
            let mut emitted_tool_call_ids = std::collections::HashSet::new();
            let mut finish_reason = None;
            let mut prompt_tokens = 0;
            let mut completion_tokens = 0;
            let mut partial = CompletedTurn::default();
            let mut thinking_tracker = ThinkingTracker::default();
            let mut got_stop = false;
            let progress_limit = progress_limit_cfg;
            let mut last_progress = std::time::Instant::now();
            loop {
                let remaining = progress_limit.saturating_sub(last_progress.elapsed());
                let next = match tokio::time::timeout(remaining, stream.next()).await {
                    Ok(next) => next,
                    Err(_elapsed) => { Err(crate::agent_providers::transport::semantic_progress_error(progress_limit))?; unreachable!() }
                };
                let Some(chunk) = next else { break };
                let chunk = chunk.map_err(|error| ProviderError::Network(error.to_string()))?;
                total = total.saturating_add(chunk.len());
                if total > max_sse_bytes() {
                    Err(ProviderError::Sse(sse_limit_message().into()))?;
                }
                let frames = decoder.push(&chunk).map_err(ProviderError::Sse)?;
                for frame in frames {
                    for line in frame.iter().map(String::as_str) {
                        let value: Value = serde_json::from_str(line.trim())
                            .map_err(|error| ProviderError::Sse(error.to_string()))?;
                        thinking_tracker.observe(&value);
                        // `ping` and `message_start` are liveness, not progress.
                        if !matches!(value.get("type").and_then(Value::as_str), Some("ping" | "message_start")) {
                            last_progress = std::time::Instant::now();
                        }
                        match value.get("type").and_then(Value::as_str).unwrap_or_default() {
                            "content_block_start"
                                if value.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") =>
                            {
                                let block = &value["content_block"];
                                tools.push((
                                    block.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
                                    block.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                                    String::new(),
                                    false,
                                ));
                            }
                            "content_block_delta" => match value.pointer("/delta/type").and_then(Value::as_str) {
                                Some("text_delta") => {
                                    if let Some(text) = value.pointer("/delta/text").and_then(Value::as_str).filter(|text| !text.is_empty()) {
                                        partial.text.push_str(text);
                                        yield ProviderStreamEvent::Text(text.to_string());
                                    }
                                }
                                Some("thinking_delta") => {
                                    // S:M2/S:M3 amendment item B: surface native
                                    // thinking content into the same reasoning
                                    // channel the OpenAI-compatible path uses.
                                    if let Some(fragment) = value.pointer("/delta/thinking").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                                        push_reasoning(&mut partial.reasoning, fragment);
                                        yield ProviderStreamEvent::Reasoning(fragment.to_string());
                                    }
                                }
                                Some("input_json_delta") => {
                                    let slot = tools.len().to_string();
                                    if let Some((id, name, arguments, _)) = tools.last_mut() {
                                        let fragment = value.pointer("/delta/partial_json").and_then(Value::as_str).unwrap_or_default();
                                        arguments.push_str(fragment);
                                        if options.partial_tool_argument_events {
                                            if let Some(preview) = previews.push(&slot, id, name, fragment) {
                                                yield preview;
                                            }
                                        }
                                    }
                                }
                                _ => {}
                            },
                            "content_block_stop" => {
                                // Tool calls are buffered until `message_stop` with a
                                // stop_reason; a stream cut after a block stop must
                                // not surface an executable call (issue #63).
                            }
                            "message_delta" => {
                                finish_reason = value.pointer("/delta/stop_reason").and_then(Value::as_str).map(str::to_string);
                                partial.finish_reason = finish_reason.clone();
                                if let Some(usage) = value.get("usage") {
                                    completion_tokens = usage.get("output_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
                                    partial.completion_tokens = completion_tokens;
                                    // Newer API revisions repeat the cumulative input/cache
                                    // counters on message_delta; prefer them when non-zero.
                                    let (delta_read, delta_creation) = cache_usage(usage);
                                    if delta_read > 0 { partial.cache_read_tokens = delta_read; }
                                    if delta_creation > 0 { partial.cache_creation_tokens = delta_creation; }
                                }
                            }
                            "message_stop" => {
                                got_stop = true;
                            }
                            "message_start" => {
                                log_message_start_transformations(&value, &logged_model);
                                if let Some(usage) = value.pointer("/message/usage") {
                                    prompt_tokens = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
                                    let (cache_read, cache_creation) = cache_usage(usage);
                                    partial.cache_read_tokens = cache_read;
                                    partial.cache_creation_tokens = cache_creation;
                                    partial.prompt_tokens = prompt_tokens;
                                }
                            }
                            "error" => {
                                Err(stream_error_event(&value))?;
                            }
                            _ => {}
                        }
                    }
                }
                if last_progress.elapsed() >= progress_limit {
                    Err(crate::agent_providers::transport::semantic_progress_error(progress_limit))?;
                }
            }
            if decoder.has_incomplete_frame() {
                Err(ProviderError::Sse("stream ended with an incomplete SSE frame".into()))?;
            }
            thinking_tracker.finish();
            // Explicit completion needs both a stop_reason and `message_stop`.
            if !got_stop || finish_reason.is_none() {
                Err(ProviderError::Sse("stream ended before completion".into()))?;
            }
            let output_truncated = finish_reason
                .as_deref()
                .is_some_and(crate::agent_providers::client::is_incomplete_finish_reason);
            for (id, name, arguments, emitted) in &tools {
                let arguments = if options.truncated_tool_call_recovery && arguments.trim().is_empty() {
                    "{}"
                } else {
                    arguments.as_str()
                };
                if !emitted {
                    if serde_json::from_str::<Value>(arguments).is_ok() && !id.is_empty() && !name.is_empty() {
                        if !emitted_tool_call_ids.insert(id.clone()) {
                            Err(ProviderError::Sse(format!(
                                "provider emitted duplicate tool call id `{id}`"
                            )))?;
                        }
                        let call = rightkit_agent_engine_protocol::ToolCall {
                            id: id.clone(), name: name.clone(), arguments_json: arguments.to_string(),
                        };
                        partial.tool_calls.push(call.clone());
                        yield ProviderStreamEvent::ToolCall(call);
                    } else if output_truncated {
                        // Cut off mid-arguments by the output limit: the call
                        // is never executed or continued; the turn keeps its
                        // partial text and ends with the limit reason.
                        dropped_tool.store(true, std::sync::atomic::Ordering::SeqCst);
                    } else {
                        Err(ProviderError::Sse("stream ended with an incomplete tool call".into()))?;
                    }
                }
            }
            yield ProviderStreamEvent::Usage {
                prompt_tokens,
                completion_tokens,
                cache_read_tokens: partial.cache_read_tokens,
                cache_creation_tokens: partial.cache_creation_tokens,
            };
            // The window ran out mid-answer: with the host's `extended_overflow_markers` the text
            // is cut, so report an overflow (which reactive compaction handles), never a
            // finished turn.
            if options.extended_overflow_markers
                && finish_reason.as_deref() == Some("model_context_window_exceeded")
            {
                Err(ProviderError::Provider(
                    "finish_reason: model_context_window_exceeded".into(),
                ))?;
            }
            // Preserve provider truncation on streaming seam. Engine owns
            // final completeness gate after adapter delivery.
            yield ProviderStreamEvent::Finished(finish_reason);
        };
        Ok(
            crate::agent_providers::client::observe_stream_breaker_outcome(
                admission,
                Box::pin(output),
            ),
        )
    }

    async fn complete_with_tools_inner(
        &self,
        messages: Vec<Value>,
        tools: Vec<Value>,
        max_output_tokens: Option<u32>,
        continuation: bool,
    ) -> Result<CompletedTurn, ProviderError> {
        let body = self.build_message_body(messages, tools, max_output_tokens);
        let prepared = prepare_actual_message_request(self, &body, continuation)?;
        let url = format!("{}/messages", self.base_url.trim_end_matches('/'));
        let response = crate::agent_providers::http::send(
            &self.http,
            self.request_builder(&url)
                .header("Content-Type", prepared.content_type)
                .body(prepared.bytes.clone()),
        )
        .await
        .map_err(|error| {
            (self.submission_observer)(
                crate::agent_providers::transport::observe_submission_attempt(
                    &prepared,
                    prepared
                        .measured_tokens
                        .expect("prepared request admitted before send"),
                    1,
                    Err(&error),
                ),
            );
            ProviderError::from_send_error(&error)
        })?;
        (self.submission_observer)(
            crate::agent_providers::transport::observe_stream_establishment_attempt(
                &prepared,
                prepared
                    .measured_tokens
                    .expect("prepared request admitted before send"),
                1,
                Ok(&response),
            ),
        );
        if !response.is_success() {
            let options = crate::agent_providers::host::host_options();
            if !options.quota_errors && !options.status_retries {
                return Err(ProviderError::Http {
                    status: response.status(),
                    body: response.text().await.unwrap_or_default(),
                });
            }
            let status = response.status();
            let hint = crate::agent_providers::client::retry_after_secs(&response);
            let body = response.text().await.unwrap_or_default();
            if options.quota_errors
                && (status == 402
                    || (matches!(status, 400 | 429)
                        && crate::agent_providers::transport::is_quota_exhausted_body(&body)))
            {
                return Err(ProviderError::QuotaExceeded(body));
            }
            if status == 429 {
                return Err(ProviderError::RateLimited {
                    retry_after_secs: hint.unwrap_or(2),
                });
            }
            return Err(ProviderError::Http { status, body });
        }
        parse_sse(response).await
    }
}

/// Like [`anthropic_messages`] but keeps each system message as its own block
/// so the request builder can put cache breakpoints on the stable leading
/// block(s) while volatile overlays that follow them stay outside the cached span.
fn anthropic_messages_split(messages: Vec<Value>) -> (Vec<String>, Vec<Value>) {
    let mut system = Vec::new();
    let mut out = Vec::new();
    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user");
        let content = message.get("content").cloned().unwrap_or(Value::Null);
        if role == "system" {
            if content.is_array() && message.get("inline_tool_additions").is_some() {
                // A mid-conversation system message of `tool_addition` blocks (see
                // `insert_tool_additions`) is sent as a system turn in place; before any history
                // there is nothing for it to follow.
                if !out.is_empty() {
                    out.push(json!({"role": "system", "content": content}));
                }
                continue;
            }
            if let Some(text) = content.as_str() {
                if out.is_empty() {
                    system.push(text.to_string());
                } else {
                    // A system message after history (for example a late skill
                    // hint) stays in position as a tagged user turn. Hoisting it
                    // into `system` would rewrite the head of the prompt and
                    // invalidate the prompt cache for every later turn.
                    out.push(json!({
                        "role": "user",
                        "content": format!("{SYSTEM_REMINDER_OPEN}{text}{SYSTEM_REMINDER_CLOSE}"),
                    }));
                }
            }
            continue;
        }
        if role == "tool" {
            // Normalized tool results ride Anthropic's native `tool_result`
            // block inside a user turn; consecutive results (parallel calls)
            // share one user message so every `tool_use` is answered together.
            let mut block = json!({
                "type": "tool_result",
                "tool_use_id": message.get("tool_call_id").and_then(Value::as_str).unwrap_or_default(),
                "content": content,
            });
            if message.get("is_error").and_then(Value::as_bool) == Some(true) {
                block["is_error"] = Value::Bool(true);
            }
            let appended = out
                .last_mut()
                .filter(|last| {
                    last.get("role").and_then(Value::as_str) == Some("user")
                        && last
                            .get("content")
                            .and_then(Value::as_array)
                            .is_some_and(|blocks| {
                                blocks.iter().all(|b| {
                                    b.get("type").and_then(Value::as_str) == Some("tool_result")
                                })
                            })
                })
                .and_then(|last| last.get_mut("content").and_then(Value::as_array_mut))
                .map(|blocks| blocks.push(block.clone()))
                .is_some();
            if !appended {
                out.push(json!({"role": "user", "content": [block]}));
            }
            continue;
        }
        let role = if role == "assistant" {
            "assistant"
        } else {
            "user"
        };
        if role == "assistant" {
            let mut content = anthropic_assistant_content(&message, content);
            if crate::agent_providers::host::host_options().anthropic_request_hygiene {
                // The API refuses blank text blocks and an assistant turn that ends in thinking.
                if let Some(blocks) = content.as_array_mut() {
                    blocks.retain(|b| {
                        b.get("type").and_then(Value::as_str) != Some("text")
                            || b.get("citations").is_some_and(|c| !c.is_null())
                            || !b
                                .get("text")
                                .and_then(Value::as_str)
                                .is_some_and(|text| text.trim().is_empty())
                    });
                    while blocks.last().is_some_and(|b| {
                        matches!(
                            b.get("type").and_then(Value::as_str),
                            Some("thinking" | "redacted_thinking")
                        )
                    }) {
                        blocks.pop();
                    }
                    if blocks.is_empty() {
                        continue;
                    }
                }
            }
            out.push(json!({
                "role": role,
                "content": content,
            }));
        } else {
            out.push(json!({"role": role, "content": anthropic_user_content(content)}));
        }
    }
    (system, out)
}

/// Largest decoded image Anthropic accepts per block.
const ANTHROPIC_MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// Map normalized user content parts to Anthropic blocks: `image_url` data
/// URLs become base64 `image` sources (png, jpeg, gif, webp only, within the
/// per-image size limit); http(s) URLs become `url` sources. A part that
/// fails validation is replaced by a text note, never sent as a bad block.
fn anthropic_user_content(content: Value) -> Value {
    let Some(parts) = content.as_array() else {
        return content;
    };
    Value::Array(
        parts
            .iter()
            .map(|part| {
                let Some(url) = part
                    .get("image_url")
                    .and_then(|image| image.get("url").or(Some(image)))
                    .and_then(Value::as_str)
                    .filter(|_| part.get("type").and_then(Value::as_str) == Some("image_url"))
                else {
                    return part.clone();
                };
                anthropic_image_block(url)
            })
            .collect(),
    )
}

fn anthropic_image_block(url: &str) -> Value {
    if let Some(rest) = url.strip_prefix("data:") {
        let Some((meta, data)) = rest.split_once(',') else {
            return json!({"type":"text","text":"[image omitted: malformed data URL]"});
        };
        let media_type = meta.strip_suffix(";base64").unwrap_or("");
        if !matches!(
            media_type,
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            return json!({"type":"text","text":format!("[image omitted: unsupported type {media_type:?}]")});
        }
        let decoded_len = data.trim_end_matches('=').len() * 3 / 4;
        if data.is_empty() || decoded_len > ANTHROPIC_MAX_IMAGE_BYTES {
            return json!({"type":"text","text":"[image omitted: empty or over the 5 MiB limit]"});
        }
        return json!({"type":"image","source":{"type":"base64","media_type":media_type,"data":data}});
    }
    if url.starts_with("https://") || url.starts_with("http://") {
        return json!({"type":"image","source":{"type":"url","url":url}});
    }
    json!({"type":"text","text":"[image omitted: unsupported source]"})
}

/// Restore provider-native Anthropic blocks from the normalized assistant
/// transcript shape used by the shared engine.
fn anthropic_assistant_content(message: &Value, content: Value) -> Value {
    // Already-native content (including any signed thinking blocks) must keep
    // its exact structure when it reaches this boundary.
    if content.is_array() {
        return content;
    }
    let mut blocks = Vec::new();
    if let Some(thinking) = message
        .get("reasoning_content")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        if let Some(native) = recall_signed_thinking(thinking) {
            // Verbatim signed blocks captured from the original stream.
            blocks.extend(native);
        } else if let Some(signature) = message.get("signature").and_then(Value::as_str) {
            blocks.push(json!({"type":"thinking", "thinking":thinking, "signature":signature}));
        }
        // Unsigned thinking is invalid on Anthropic's wire; it is omitted.
    }
    let hygiene = crate::agent_providers::host::host_options().anthropic_request_hygiene;
    if let Some(text) = content.as_str().filter(|text| {
        if hygiene {
            !text.trim().is_empty()
        } else {
            !text.is_empty()
        }
    }) {
        blocks.push(json!({"type":"text", "text":text}));
    }
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            let function = call.get("function").cloned().unwrap_or(Value::Null);
            let input = function
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                .unwrap_or_else(|| json!({}));
            blocks.push(json!({
                "type":"tool_use",
                "id":call.get("id").and_then(Value::as_str).unwrap_or_default(),
                "name":function.get("name").and_then(Value::as_str).unwrap_or_default(),
                "input":input,
            }));
        }
    }
    Value::Array(blocks)
}

/// Enforce Anthropic's tool pairing contract: every `tool_use` in an assistant
/// turn must be answered by a `tool_result` (same id) at the START of the next
/// user turn, and a `tool_result` must answer a `tool_use` of the turn right
/// before it. Missing results (interrupted or compacted turns) get a synthetic
/// error result; orphan results are dropped. Either violation is a 400.
/// Docs: https://platform.claude.com/docs/en/agents-and-tools/tool-use/overview
fn repair_tool_pairing(messages: &mut Vec<Value>) {
    let mut index = 0;
    let mut prev_ids: Vec<String> = Vec::new();
    while index < messages.len() {
        let role = messages[index]
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if role == "assistant" {
            prev_ids = messages[index]
                .get("content")
                .and_then(Value::as_array)
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
                        .filter_map(|b| b.get("id").and_then(Value::as_str).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if !prev_ids.is_empty() {
                let next_is_user = messages
                    .get(index + 1)
                    .and_then(|m| m.get("role").and_then(Value::as_str))
                    == Some("user");
                if !next_is_user {
                    messages.insert(index + 1, json!({"role": "user", "content": []}));
                }
                let next = &mut messages[index + 1];
                if let Some(text) = next
                    .get("content")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                {
                    next["content"] = json!([{"type": "text", "text": text}]);
                }
                if let Some(blocks) = next.get_mut("content").and_then(Value::as_array_mut) {
                    let answered: Vec<String> = blocks
                        .iter()
                        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                        .filter_map(|b| {
                            b.get("tool_use_id")
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                        .collect();
                    let mut front: Vec<Value> = prev_ids
                        .iter()
                        .filter(|id| !answered.contains(id))
                        .map(|id| {
                            json!({
                                "type": "tool_result",
                                "tool_use_id": id,
                                "content": "Tool call produced no recorded result (interrupted).",
                                "is_error": true,
                            })
                        })
                        .collect();
                    // tool_results must precede any other block in the user turn.
                    let (results, others): (Vec<Value>, Vec<Value>) =
                        blocks.drain(..).partition(|b| {
                            b.get("type").and_then(Value::as_str) == Some("tool_result")
                        });
                    front.extend(results);
                    front.extend(others);
                    *blocks = front;
                }
            }
            index += 1;
            continue;
        }
        if role == "user" {
            if let Some(blocks) = messages[index]
                .get_mut("content")
                .and_then(Value::as_array_mut)
            {
                let before = blocks.len();
                blocks.retain(|b| {
                    b.get("type").and_then(Value::as_str) != Some("tool_result")
                        || b.get("tool_use_id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| prev_ids.iter().any(|p| p == id))
                });
                if blocks.is_empty() && before > 0 {
                    messages.remove(index);
                    continue;
                }
            }
        }
        prev_ids.clear();
        index += 1;
    }
}

/// Wrapping of a system message that follows history, sent in place as a user turn.
const SYSTEM_REMINDER_OPEN: &str = "<system-reminder>\n";
const SYSTEM_REMINDER_CLOSE: &str = "\n</system-reminder>";

/// A user turn that `anthropic_messages_split` built from a system message after history (the goal
/// reminder, provider-boundary overlays, a late skill hint). The engine adds the volatile ones at
/// the tail of each request only, so the next request does not repeat them at that position.
fn is_system_reminder_turn(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("user")
        && message
            .get("content")
            .and_then(Value::as_str)
            .is_some_and(|text| {
                text.starts_with(SYSTEM_REMINDER_OPEN) && text.ends_with(SYSTEM_REMINDER_CLOSE)
            })
}

/// A message that can carry the rolling breakpoint: not a mid-conversation system message (a tool
/// addition) and not a per-request reminder turn.
fn is_stable_message(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) != Some("system")
        && !is_system_reminder_turn(message)
}

/// Mark the last content block of the last stable message as a rolling cache
/// breakpoint so the whole conversation so far is read from cache next turn.
/// Thinking blocks cannot carry `cache_control` and are skipped.
///
/// With `second`, also mark the last stable message before the latest assistant turn: that is where
/// the previous request put its breakpoint. A turn that appends more than the 20-block lookback
/// (parallel tool calls) would otherwise leave the earlier entry out of reach and rewrite everything.
fn mark_trailing_message_breakpoint(messages: &mut [Value], marker: &Value, second: bool) {
    // A breakpoint on a per-request tail is unreachable next turn, so the whole history would be written again.
    let Some(last_index) = messages.iter().rposition(is_stable_message) else {
        return;
    };
    mark_message_breakpoint(&mut messages[last_index], marker);
    if second {
        let previous = messages[..last_index]
            .iter()
            .rposition(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
            .and_then(|assistant| messages[..assistant].iter().rposition(is_stable_message));
        if let Some(previous) = previous {
            mark_message_breakpoint(&mut messages[previous], marker);
        }
    }
}

fn mark_message_breakpoint(last: &mut Value, marker: &Value) {
    let marker = marker.clone();
    match last.get("content").cloned() {
        Some(Value::String(text)) if !text.is_empty() => {
            last["content"] = json!([{"type": "text", "text": text, "cache_control": marker}]);
        }
        Some(Value::Array(_)) => {
            if let Some(block) = last
                .get_mut("content")
                .and_then(Value::as_array_mut)
                .and_then(|blocks| blocks.last_mut())
            {
                let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
                if !matches!(kind, "thinking" | "redacted_thinking") {
                    block["cache_control"] = marker;
                }
            }
        }
        _ => {}
    }
}

/// The one message breakpoint of a request without the host's `anthropic_request_hygiene`: the
/// last message, whatever it is.
fn mark_last_message_breakpoint(messages: &mut [Value], marker: &Value) {
    if let Some(last) = messages.last_mut() {
        mark_message_breakpoint(last, marker);
    }
}

/// Beta header for inline tool additions (`tool_addition` blocks in a mid-conversation system message).
const INLINE_TOOLS_BETA: &str = "inline-tools-2026-09-15";

/// Beta header for `thinking.block_binding`.
const THINKING_BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";

/// What Anthropic does when a request's thinking blocks no longer match the prefix they were
/// issued under: refuse it (`Error`) or drop the mismatched block (`DropBlock`). Sent as
/// `thinking.block_binding.prefix_mismatch_behavior`; see
/// [`AnthropicCompatClient::with_thinking_block_binding`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingBlockBinding {
    Error,
    DropBlock,
}

impl ThinkingBlockBinding {
    /// The wire value of `prefix_mismatch_behavior`.
    pub fn wire(self) -> &'static str {
        match self {
            ThinkingBlockBinding::Error => "error",
            ThinkingBlockBinding::DropBlock => "drop_block",
        }
    }
}

/// True only for an HTTP 400 that reports a thinking-block binding mismatch; the caller flips
/// that one session to [`ThinkingBlockBinding::DropBlock`] and resends once.
pub fn is_thinking_binding_mismatch(error: &ProviderError) -> bool {
    match error {
        ProviderError::Http { status: 400, body } => {
            let body = body.to_ascii_lowercase();
            body.contains("block_binding") || (body.contains("thinking") && body.contains("prefix"))
        }
        _ => false,
    }
}

/// Remove the thinking of assistant turns issued by another model or provider from the request
/// copy: `reasoning_content`, `reasoning` and every `thinking` / `redacted_thinking` block. A
/// turn with no `origin`, or a matching one, is left as it is.
fn drop_foreign_thinking(messages: &mut [Value], model_id: &str) {
    for message in messages.iter_mut() {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let foreign = message.get("origin").is_some_and(|origin| {
            origin
                .get("model")
                .and_then(Value::as_str)
                .is_some_and(|model| model != model_id)
                || origin
                    .get("provider")
                    .and_then(Value::as_str)
                    .is_some_and(|provider| provider != "anthropic")
        });
        if !foreign {
            continue;
        }
        if let Some(object) = message.as_object_mut() {
            object.remove("reasoning_content");
            object.remove("reasoning");
        }
        if let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) {
            blocks.retain(|block| {
                !matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("thinking" | "redacted_thinking")
                )
            });
        }
    }
}

/// Report the response's `input_transformations` (non-empty array) to the host's logger.
fn log_message_start_transformations(value: &Value, fallback_model: &str) {
    if let Some(array) = value
        .pointer("/message/input_transformations")
        .filter(|array| array.as_array().is_some_and(|items| !items.is_empty()))
    {
        let model = value
            .pointer("/message/model")
            .and_then(Value::as_str)
            .unwrap_or(fallback_model);
        crate::agent_providers::host::log_input_transformations(model, array);
    }
}

/// Anthropic's tool-name rule: 1 to 128 characters of letters, digits, `_` and `-`.
fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Models that accept inline tool additions (not Claude Sonnet 5).
fn model_supports_inline_tools(model: &str) -> bool {
    [
        "claude-haiku-5-5",
        "claude-sonnet-5-5",
        "claude-opus-5-5",
        "claude-fable-5-2",
        "claude-fable-5-1",
    ]
    .iter()
    .any(|prefix| model.starts_with(prefix))
}

/// Put a `tool_addition` system message after the turn that gave the model each deferred tool
/// (the `tool_search` result that listed it), so a promoted tool never edits the tools array.
///
/// The message is placed where the API allows it: after a `user` turn (a tool-result turn counts),
/// before the next assistant message or at the end. It is flushed at the next assistant message,
/// so it never sits between a `tool_use` and its `tool_result`, and its position depends only on
/// history, which makes it identical on every later request.
fn insert_tool_additions(
    messages: Vec<Value>,
    deferred: &std::collections::HashSet<String>,
) -> Vec<Value> {
    let search_ids: std::collections::HashSet<String> = messages
        .iter()
        .filter(|m| m.get("role").and_then(Value::as_str) == Some("assistant"))
        .filter_map(|m| m.get("tool_calls").and_then(Value::as_array))
        .flatten()
        .filter(|call| {
            call.pointer("/function/name").and_then(Value::as_str) == Some("tool_search")
        })
        .filter_map(|call| call.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    if search_ids.is_empty() {
        return messages;
    }
    let mut announced: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pending: Vec<String> = Vec::new();
    let mut out: Vec<Value> = Vec::with_capacity(messages.len() + 2);
    let flush = |out: &mut Vec<Value>, pending: &mut Vec<String>| {
        if pending.is_empty() {
            return;
        }
        let blocks: Vec<Value> = pending
            .drain(..)
            .map(|name| json!({"type": "tool_addition", "tool": {"type": "tool_reference", "name": name}}))
            .collect();
        out.push(json!({"role": "system", "content": blocks, "inline_tool_additions": true}));
    };
    for message in messages {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        if role == "assistant" {
            flush(&mut out, &mut pending);
        }
        if role == "tool" {
            let id = message
                .get("tool_call_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if search_ids.contains(id)
                && message.get("is_error").and_then(Value::as_bool) != Some(true)
            {
                // The result may be wrapped in a trust envelope, so look for each deferred tool's
                // `"name":"<tool>"` entry in the text instead of parsing it as bare JSON.
                let text = match message.get("content") {
                    Some(Value::String(text)) => text.clone(),
                    Some(other) => other.to_string(),
                    None => String::new(),
                };
                let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
                let mut names: Vec<&String> = deferred
                    .iter()
                    .filter(|name| {
                        compact.contains(&format!("\"name\":\"{name}\""))
                            || compact.contains(&format!("\\\"name\\\":\\\"{name}\\\""))
                    })
                    .collect();
                names.sort();
                for name in names {
                    if announced.insert(name.clone()) {
                        pending.push(name.clone());
                    }
                }
            }
        }
        out.push(message);
    }
    flush(&mut out, &mut pending);
    out
}

fn anthropic_tool(tool: Value) -> Option<Value> {
    let function = tool.get("function")?;
    Some(json!({
        "name": function.get("name")?,
        "description": crate::agent_providers::client::bounded_tool_description(function),
        "input_schema": if crate::agent_providers::host::host_options().anthropic_request_hygiene {
            normalized_input_schema(function.get("parameters"))
        } else {
            function.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object","properties":{}}))
        },
    }))
}

/// Anthropic requires every tool's `input_schema` to be an object schema with `type` and no top-level
/// `oneOf`/`anyOf`/`allOf`. Some registered tools use a top-level `oneOf` to offer several call
/// shapes; merge the branches into one object (union of properties, `required` only where every
/// branch agrees) so the model still sees every parameter name and enum instead of an empty schema.
fn normalized_input_schema(parameters: Option<&Value>) -> Value {
    let Some(Value::Object(map)) = parameters else {
        return json!({"type":"object","properties":{}});
    };
    let mut schema = map.clone();
    let mut merged_properties = serde_json::Map::new();
    let mut shared_required: Option<Vec<String>> = None;
    for combinator in ["oneOf", "anyOf", "allOf"] {
        let Some(Value::Array(branches)) = schema.remove(combinator) else {
            continue;
        };
        for branch in branches {
            if let Some(Value::Object(properties)) = branch.get("properties") {
                for (name, definition) in properties {
                    merged_properties
                        .entry(name.clone())
                        .or_insert_with(|| definition.clone());
                }
            }
            let required: Vec<String> = branch
                .get("required")
                .and_then(Value::as_array)
                .map(|names| {
                    names
                        .iter()
                        .filter_map(|name| name.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            shared_required = Some(match shared_required {
                None => required,
                Some(current) => current
                    .into_iter()
                    .filter(|name| required.contains(name))
                    .collect(),
            });
        }
    }
    schema.insert("type".into(), json!("object"));
    let mut properties = match schema.remove("properties") {
        Some(Value::Object(existing)) => existing,
        _ => serde_json::Map::new(),
    };
    for (name, definition) in merged_properties {
        properties.entry(name).or_insert(definition);
    }
    schema.insert("properties".into(), Value::Object(properties));
    if let Some(required) = shared_required.filter(|names| !names.is_empty()) {
        schema.insert("required".into(), json!(required));
    }
    Value::Object(schema)
}

/// `S:M2`/`S:M3` amendment item C: Anthropic reports rate-limit budget via
/// `anthropic-ratelimit-{requests,tokens}-remaining` (integer counts) and
/// `anthropic-ratelimit-{requests,tokens}-reset` (an RFC 3339 timestamp).
/// This crate has no date-parsing dependency, so the reset headers are
/// captured only when a route already sends a plain integer-seconds form;
/// an RFC 3339 timestamp is left `None` rather than pulling in a new crate
/// dependency for a best-effort metadata field.
pub(crate) fn parse_anthropic_rate_limit_headers(
    headers: &[(String, String)],
) -> Option<crate::agent_providers::transport::RateLimitMetadata> {
    use crate::agent_providers::http::header_u64;
    let metadata = crate::agent_providers::transport::RateLimitMetadata {
        remaining_requests: header_u64(headers, "anthropic-ratelimit-requests-remaining"),
        remaining_tokens: header_u64(headers, "anthropic-ratelimit-tokens-remaining"),
        reset_requests_secs: header_u64(headers, "anthropic-ratelimit-requests-reset"),
        reset_tokens_secs: header_u64(headers, "anthropic-ratelimit-tokens-reset"),
    };
    (!metadata.is_empty()).then_some(metadata)
}

async fn parse_sse(
    response: crate::agent_providers::http::ProviderHttpResponse,
) -> Result<CompletedTurn, ProviderError> {
    let rate_limit = parse_anthropic_rate_limit_headers(response.headers());
    let mut stream = response.bytes_stream();
    // CR-13: use the same bounded byte-oriented LF/CRLF decoder as the
    // streaming path, so aggregate and incremental callers share framing.
    let mut decoder = AnthropicSseDecoder::new();
    let mut total = 0usize;
    let mut turn = CompletedTurn {
        protocol_family: Some(crate::agent_providers::transport::ProtocolFamily::AnthropicNative),
        rate_limit,
        ..Default::default()
    };
    let mut tool_calls: Vec<(String, String, String)> = Vec::new();
    let mut thinking_tracker = ThinkingTracker::default();
    let mut got_message_stop = false;
    let idle =
        std::time::Duration::from_secs(crate::agent_providers::transport::SEMANTIC_PROGRESS_SECS);
    let idle_everywhere = crate::agent_providers::host::host_options().idle_timeout_everywhere;
    loop {
        let next = if idle_everywhere {
            tokio::time::timeout(idle, stream.next())
                .await
                .map_err(|_elapsed| {
                    crate::agent_providers::transport::semantic_progress_error(idle)
                })?
        } else {
            stream.next().await
        };
        let Some(chunk) = next else { break };
        let chunk = chunk.map_err(|error| ProviderError::Network(error.to_string()))?;
        total = total.saturating_add(chunk.len());
        if total > max_sse_bytes() {
            return Err(ProviderError::Sse(sse_limit_message().into()));
        }
        let frames = decoder.push(&chunk).map_err(ProviderError::Sse)?;
        for frame in frames {
            for line in frame.iter().map(String::as_str) {
                let value: Value = serde_json::from_str(line.trim())
                    .map_err(|error| ProviderError::Sse(error.to_string()))?;
                thinking_tracker.observe(&value);
                if value.get("type").and_then(Value::as_str) == Some("message_stop") {
                    got_message_stop = true;
                }
                apply_event(&mut turn, &mut tool_calls, &value)?;
            }
        }
    }
    thinking_tracker.finish();
    if decoder.has_incomplete_frame() {
        return Err(ProviderError::Sse(
            "stream ended with an incomplete SSE frame".into(),
        ));
    }
    // Success requires `message_stop` for every turn, text-only included; a
    // stop_reason followed by EOF is a cut stream, not a completion.
    if !got_message_stop {
        return Err(ProviderError::Sse("stream ended before completion".into()));
    }
    turn.tool_calls = tool_calls
        .into_iter()
        .map(|(id, name, arguments_json)| {
            let arguments_json = if crate::agent_providers::host::host_options()
                .truncated_tool_call_recovery
                && arguments_json.trim().is_empty()
            {
                "{}".to_string()
            } else {
                arguments_json
            };
            if id.is_empty()
                || name.is_empty()
                || serde_json::from_str::<Value>(&arguments_json).is_err()
            {
                return Err(ProviderError::Sse(
                    "stream ended with an incomplete tool call".into(),
                ));
            }
            Ok(ToolCall {
                id,
                name,
                arguments_json,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    crate::agent_providers::client::validate_complete_tool_calls(&turn.tool_calls)?;
    if turn.finish_reason.is_none() {
        return Err(ProviderError::Sse("stream ended before completion".into()));
    }
    if crate::agent_providers::host::host_options().extended_overflow_markers
        && turn.finish_reason.as_deref() == Some("model_context_window_exceeded")
    {
        return Err(ProviderError::Provider(
            "finish_reason: model_context_window_exceeded".into(),
        ));
    }
    if turn
        .finish_reason
        .as_deref()
        .is_some_and(crate::agent_providers::client::is_incomplete_finish_reason)
    {
        return Err(crate::agent_providers::client::incomplete_output_error(
            &turn, false, None,
        ));
    }
    Ok(turn)
}

fn apply_event(
    turn: &mut CompletedTurn,
    tools: &mut Vec<(String, String, String)>,
    value: &Value,
) -> Result<(), ProviderError> {
    match value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
    {
        "content_block_start"
            if value.pointer("/content_block/type").and_then(Value::as_str) == Some("tool_use") =>
        {
            let block = &value["content_block"];
            tools.push((
                block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                String::new(),
            ));
        }
        "content_block_delta" => match value.pointer("/delta/type").and_then(Value::as_str) {
            Some("text_delta") => turn.text.push_str(
                value
                    .pointer("/delta/text")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            // S:M2/S:M3 amendment item B: Anthropic's native "thinking"
            // content blocks are the Anthropic-native reasoning channel —
            // fold them into `CompletedTurn::reasoning`, the same field the
            // OpenAI-compatible path's `reasoning_content` lands in, rather
            // than dropping them.
            Some("thinking_delta") => {
                push_reasoning(
                    &mut turn.reasoning,
                    value
                        .pointer("/delta/thinking")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                );
            }
            Some("input_json_delta") => {
                if let Some((_, _, arguments)) = tools.last_mut() {
                    arguments.push_str(
                        value
                            .pointer("/delta/partial_json")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    );
                }
            }
            _ => {}
        },
        "message_delta" => {
            turn.finish_reason = value
                .pointer("/delta/stop_reason")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(usage) = value.get("usage") {
                turn.completion_tokens = usage
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32;
                let (delta_read, delta_creation) = cache_usage(usage);
                if delta_read > 0 {
                    turn.cache_read_tokens = delta_read;
                }
                if delta_creation > 0 {
                    turn.cache_creation_tokens = delta_creation;
                }
            }
        }
        "message_start" => {
            // No client model here; the response's own `/message/model` names it.
            log_message_start_transformations(value, "");
            if let Some(usage) = value.pointer("/message/usage") {
                turn.prompt_tokens = usage
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as u32;
                let (cache_read, cache_creation) = cache_usage(usage);
                turn.cache_read_tokens = cache_read;
                turn.cache_creation_tokens = cache_creation;
            }
        }
        "error" => return Err(stream_error_event(value)),
        _ => {}
    }
    Ok(())
}

/// True when the request ends inside a tool loop (trailing user turn holds
/// only `tool_result` blocks) and the assistant turn that issued those calls
/// does not begin with a thinking block. Manual extended thinking requires the
/// final assistant turn to start with one, so such a request must not enable
/// it (for example after a restart dropped the in-memory signed blocks).
fn tool_loop_lacks_thinking_block(messages: &[Value]) -> bool {
    let is_tool_results = |m: &Value| {
        m.get("role").and_then(Value::as_str) == Some("user")
            && m.get("content")
                .and_then(Value::as_array)
                .is_some_and(|blocks| {
                    !blocks.is_empty()
                        && blocks
                            .iter()
                            .all(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
                })
    };
    let mut end = messages.len();
    while end > 0 && is_tool_results(&messages[end - 1]) {
        end -= 1;
    }
    if end == messages.len() || end == 0 {
        return false;
    }
    let assistant = &messages[end - 1];
    if assistant.get("role").and_then(Value::as_str) != Some("assistant") {
        return false;
    }
    let first = assistant
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.first())
        .and_then(|b| b.get("type"))
        .and_then(Value::as_str);
    !matches!(first, Some("thinking" | "redacted_thinking"))
}

/// Native thinking blocks (with their signatures) captured from a stream,
/// keyed by the normalized reasoning text the engine journals. The normalized
/// transcript only carries plain reasoning text, so replay restores the exact
/// signed blocks by looking the text back up here; a block without its
/// signature is rejected by the API, so unsigned thinking is never replayed.
static SIGNED_THINKING: crate::agent_providers::reasoning_store::ReasoningStore =
    crate::agent_providers::reasoning_store::ReasoningStore::new("signed-thinking.jsonl", 256);

fn remember_signed_thinking(blocks: Vec<Value>) {
    let key: String = blocks
        .iter()
        .filter_map(|block| block.get("thinking").and_then(Value::as_str))
        .collect();
    let key = key.trim().to_string();
    if key.is_empty() || !blocks.iter().any(|b| b.get("signature").is_some()) {
        return;
    }
    SIGNED_THINKING.remember(key, blocks);
}

fn recall_signed_thinking(reasoning: &str) -> Option<Vec<Value>> {
    SIGNED_THINKING.recall(reasoning.trim())
}

/// How many times a transient error inside the stream (before any content) sends the request again.
const STREAM_ERROR_MAX_RETRIES: u32 = 3;

/// An overload, server error or rate limit: what an HTTP 529, 5xx or 429 would be retried for.
fn stream_error_is_transient(error: &ProviderError) -> bool {
    matches!(
        error,
        ProviderError::RateLimited { .. }
            | ProviderError::Http {
                status: 500 | 502 | 503 | 529,
                ..
            }
    )
}

/// An in-stream `error` event as a typed error. Without the host's `status_retries` or
/// `quota_errors` every event is [`ProviderError::Provider`] with its message. With them, callers
/// tell a retryable overload or rate limit from a fatal refusal (the same classes the HTTP status
/// path gives); `billing_error` is [`ProviderError::QuotaExceeded`] only with `quota_errors`. An
/// unknown type stays `Provider`.
fn stream_error_event(value: &Value) -> ProviderError {
    let options = crate::agent_providers::host::host_options();
    let error = value.get("error");
    let kind = error
        .and_then(|error| error.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let message = error
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("anthropic stream error");
    if !options.status_retries && !options.quota_errors {
        return ProviderError::Provider(message.to_string());
    }
    let body = if kind.is_empty() {
        message.to_string()
    } else {
        format!("{kind}: {message}")
    };
    match kind {
        "overloaded_error" => ProviderError::Http { status: 529, body },
        "api_error" => ProviderError::Http { status: 500, body },
        "rate_limit_error" => ProviderError::RateLimited {
            retry_after_secs: 2,
        },
        "authentication_error" => ProviderError::Auth,
        "billing_error" if options.quota_errors => ProviderError::QuotaExceeded(body),
        _ => ProviderError::Provider(message.to_string()),
    }
}

/// Assembles thinking / redacted_thinking blocks from raw stream events.
#[derive(Default)]
struct ThinkingTracker {
    current: Option<Value>,
    done: Vec<Value>,
}

impl ThinkingTracker {
    fn observe(&mut self, value: &Value) {
        match value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "content_block_start" => {
                self.flush();
                let block = &value["content_block"];
                match block.get("type").and_then(Value::as_str) {
                    Some("thinking") => {
                        self.current = Some(json!({
                            "type": "thinking",
                            "thinking": block.get("thinking").and_then(Value::as_str).unwrap_or_default(),
                        }));
                        if let Some(sig) = block.get("signature").and_then(Value::as_str) {
                            if !sig.is_empty() {
                                self.current.as_mut().unwrap()["signature"] = json!(sig);
                            }
                        }
                    }
                    Some("redacted_thinking") => self.done.push(block.clone()),
                    _ => {}
                }
            }
            "content_block_delta" => {
                let Some(current) = self.current.as_mut() else {
                    return;
                };
                match value.pointer("/delta/type").and_then(Value::as_str) {
                    Some("thinking_delta") => {
                        let add = value
                            .pointer("/delta/thinking")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let existing = current["thinking"].as_str().unwrap_or_default().to_string();
                        current["thinking"] = json!(format!("{existing}{add}"));
                    }
                    Some("signature_delta") => {
                        let add = value
                            .pointer("/delta/signature")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        let existing = current
                            .get("signature")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        current["signature"] = json!(format!("{existing}{add}"));
                    }
                    _ => {}
                }
            }
            "content_block_stop" => self.flush(),
            _ => {}
        }
    }

    fn flush(&mut self) {
        if let Some(block) = self.current.take() {
            self.done.push(block);
        }
    }

    fn finish(&mut self) {
        self.flush();
        remember_signed_thinking(std::mem::take(&mut self.done));
    }
}

/// Append one reasoning fragment into the shared `Option<String>` reasoning
/// channel, initializing it lazily. `S:M2`/`S:M3` amendment item B.
fn push_reasoning(reasoning: &mut Option<String>, fragment: &str) {
    if fragment.is_empty() {
        return;
    }
    match reasoning {
        Some(existing) => existing.push_str(fragment),
        None => *reasoning = Some(fragment.to_string()),
    }
}

/// Anthropic reports cache consumption with initial message usage. Keep both
/// counters separate so normalized turns preserve cache-hit versus cache-write
/// accounting for the session ledger.
fn cache_usage(usage: &Value) -> (u32, u32) {
    (
        usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
    )
}

#[cfg(test)]
mod thinking_binding_tests {
    use super::*;

    const MODEL: &str = "claude-sonnet-4-5";

    fn client() -> AnthropicCompatClient {
        AnthropicCompatClient::new("key", MODEL, "https://api.anthropic.com/v1")
    }

    fn user_message() -> Vec<Value> {
        vec![json!({"role": "user", "content": "hi"})]
    }

    fn beta_headers(request: &rightkit_http::Request) -> Vec<String> {
        request
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
            .map(|(_, value)| value.clone())
            .collect()
    }

    #[test]
    fn wire_values() {
        assert_eq!(ThinkingBlockBinding::Error.wire(), "error");
        assert_eq!(ThinkingBlockBinding::DropBlock.wire(), "drop_block");
    }

    #[test]
    fn body_carries_prefix_mismatch_behavior() {
        for (binding, wire) in [
            (ThinkingBlockBinding::Error, "error"),
            (ThinkingBlockBinding::DropBlock, "drop_block"),
        ] {
            let body = client()
                .with_thinking_block_binding(binding)
                .build_message_body(user_message(), Vec::new(), None);
            assert_eq!(
                body.pointer("/thinking/block_binding/prefix_mismatch_behavior")
                    .and_then(Value::as_str),
                Some(wire)
            );
        }
    }

    #[test]
    fn body_has_no_block_binding_without_the_builder() {
        let body = client().build_message_body(user_message(), Vec::new(), None);
        assert!(body.get("thinking").is_some());
        assert!(body.pointer("/thinking/block_binding").is_none());
    }

    #[test]
    fn beta_is_sent_once_and_merged() {
        let bound = client().with_thinking_block_binding(ThinkingBlockBinding::Error);
        let request = bound.request_builder("https://api.anthropic.com/v1/messages");
        assert_eq!(beta_headers(&request), vec![THINKING_BINDING_BETA.to_string()]);

        let mut merged = bound.clone();
        merged
            .public_headers
            .insert("anthropic-beta".into(), "foo".into());
        let request = merged.request_builder("https://api.anthropic.com/v1/messages");
        assert_eq!(
            beta_headers(&request),
            vec!["foo,thinking-binding-controls-2026-08-01".to_string()]
        );

        let mut present = bound.clone();
        present.public_headers.insert(
            "anthropic-beta".into(),
            "foo, thinking-binding-controls-2026-08-01".into(),
        );
        let request = present.request_builder("https://api.anthropic.com/v1/messages");
        assert_eq!(
            beta_headers(&request),
            vec!["foo, thinking-binding-controls-2026-08-01".to_string()]
        );

        let unbound = client().request_builder("https://api.anthropic.com/v1/messages");
        assert!(beta_headers(&unbound).is_empty());
    }

    #[test]
    fn beta_combines_with_inline_tools() {
        let tool = json!({"type": "function", "function": {"name": "lookup", "parameters": {}}});
        let combined = AnthropicCompatClient::new(
            "key",
            "claude-sonnet-5-5",
            "https://api.anthropic.com/v1",
        )
        .with_deferred_tools(vec![tool])
        .with_thinking_block_binding(ThinkingBlockBinding::DropBlock);
        assert!(combined.inline_tools_enabled());
        let request = combined.request_builder("https://api.anthropic.com/v1/messages");
        assert_eq!(
            beta_headers(&request),
            vec![format!("{INLINE_TOOLS_BETA},{THINKING_BINDING_BETA}")]
        );
    }

    #[test]
    fn foreign_thinking_is_removed_and_same_model_kept() {
        let history = vec![
            json!({"role": "user", "content": "one"}),
            json!({
                "role": "assistant",
                "origin": {"model": "claude-opus-4-5", "provider": "anthropic"},
                "reasoning_content": "foreign reasoning",
                "content": [
                    {"type": "thinking", "thinking": "t", "signature": "SIG_FOREIGN"},
                    {"type": "redacted_thinking", "data": "RED_FOREIGN"},
                    {"type": "text", "text": "answer one"}
                ]
            }),
            json!({"role": "user", "content": "two"}),
            json!({
                "role": "assistant",
                "origin": {"model": MODEL, "provider": "anthropic"},
                "content": [
                    {"type": "thinking", "thinking": "t", "signature": "SIG_SAME"},
                    {"type": "text", "text": "answer two"}
                ]
            }),
            json!({"role": "user", "content": "three"}),
        ];
        let before = history.clone();
        let body = client()
            .with_thinking_block_binding(ThinkingBlockBinding::Error)
            .build_message_body(history.clone(), Vec::new(), None);
        let wire = body["messages"].to_string();
        assert!(!wire.contains("SIG_FOREIGN"));
        assert!(!wire.contains("RED_FOREIGN"));
        assert!(!wire.contains("foreign reasoning"));
        assert!(wire.contains("SIG_SAME"));
        assert!(wire.contains("answer one"));
        assert_eq!(history, before);
        assert!(before[1].to_string().contains("SIG_FOREIGN"));

        // Without the builder nothing is dropped.
        let plain = client().build_message_body(history, Vec::new(), None);
        assert!(plain["messages"].to_string().contains("SIG_FOREIGN"));
    }

    #[test]
    fn mismatch_matcher() {
        let http = |status: u16, body: &str| ProviderError::Http {
            status,
            body: body.to_string(),
        };
        assert!(is_thinking_binding_mismatch(&http(
            400,
            r#"{"error":{"message":"thinking.block_binding: prefix changed"}}"#
        )));
        assert!(is_thinking_binding_mismatch(&http(
            400,
            "Thinking block does not match the Prefix"
        )));
        assert!(!is_thinking_binding_mismatch(&http(400, "max_tokens too large")));
        assert!(!is_thinking_binding_mismatch(&http(529, "block_binding")));
        assert!(!is_thinking_binding_mismatch(&ProviderError::RateLimited {
            retry_after_secs: 1
        }));
    }
}
