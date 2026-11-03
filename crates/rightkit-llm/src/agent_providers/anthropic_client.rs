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
use std::sync::{Arc, Mutex};

const MAX_SSE_BYTES: usize = 4 * 1024 * 1024;

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
    let Some(capability) = crate::agent_providers::model_metadata::capability_of(&client.model_id)
    else {
        return Err(ProviderError::Capacity(
            CapacityError::CapacityUnavailable {
                route_id: client.base_url.clone(),
                reason: format!("no loaded model metadata for {}", client.model_id),
            },
        ));
    };
    let profile = capability.capacity_profile(
        client.base_url.clone(),
        Some(
            crate::agent_providers::model_metadata::catalog_status()
                .version
                .clone(),
        ),
    );
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

    /// Identify one OpenCode conversation without impersonating its client.
    pub fn with_opencode_session(mut self, session_id: &str) -> Self {
        self.public_headers
            .insert("x-opencode-session".into(), session_id.into());
        self.public_headers
            .insert("user-agent".into(), "Agent/0.1".into());
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
        for (name, value) in &self.public_headers {
            request = request.header(name.as_str(), value.as_str());
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
        let (system_blocks, mut messages) = anthropic_messages_split(messages);
        repair_tool_pairing(&mut messages);
        let mut tools = tools
            .into_iter()
            .filter_map(anthropic_tool)
            .collect::<Vec<_>>();
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
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = json!({"type": "ephemeral"});
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
                            block["cache_control"] = json!({"type": "ephemeral"});
                        }
                        block
                    })
                    .collect(),
            );
        }
        mark_trailing_message_breakpoint(&mut messages);
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
        if !crate::agent_providers::client::breaker_allows_request(&self.base_url) {
            return Err(ProviderError::Provider(format!(
                "circuit breaker open for {}: provider has been failing repeatedly, refusing this request to avoid hammering it",
                self.base_url
            )));
        }
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
        crate::agent_providers::client::breaker_record_outcome(&self.base_url, counts_as_success);
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
            while let Some(event) = first.next().await {
                match event? {
                    ProviderStreamEvent::Finished(reason) => finished = Some(reason),
                    other => {
                        match &other {
                            ProviderStreamEvent::Text(delta) => text.push_str(delta),
                            ProviderStreamEvent::ToolCall(_) => saw_tool_call = true,
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
        if !crate::agent_providers::client::breaker_allows_request(&self.base_url) {
            return Err(ProviderError::Provider(format!(
                "circuit breaker open for {}: provider has been failing repeatedly, refusing this request to avoid hammering it",
                self.base_url
            )));
        }
        let body = self.build_message_body(messages, tools, max_output_tokens);
        let prepared = prepare_actual_message_request(self, &body, continuation)?;
        let url = format!("{}/messages", self.base_url.trim_end_matches('/'));
        // CR-12: same transient-retry ladder the OpenAI-compatible client
        // uses to establish its stream — previously this was a single-shot
        // request with no retry classification at all.
        let response = crate::agent_providers::client::record_stream_establishment_outcome(
            &self.base_url,
            crate::agent_providers::transport::send_observed_stream_request_with_retry(
                &self.http,
                || {
                    self.request_builder(&url)
                        .header("Content-Type", prepared.content_type)
                        .body(prepared.bytes.clone())
                },
                &prepared,
                &self.submission_observer,
                &rightkit_agent_engine_protocol::RetryNotifier::default(),
                &self.base_url,
            )
            .await,
        )?;
        let base_url = self.base_url.clone();
        let progress_limit_cfg =
            self.semantic_progress_limit
                .unwrap_or(std::time::Duration::from_secs(
                    crate::agent_providers::transport::SEMANTIC_PROGRESS_SECS,
                ));
        let output = async_stream::try_stream! {
            let mut stream = response.bytes_stream();
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
                if total > MAX_SSE_BYTES {
                    Err(ProviderError::Sse("stream exceeded 4MiB".into()))?;
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
                                    if let Some((_, _, arguments, _)) = tools.last_mut() {
                                        arguments.push_str(value.pointer("/delta/partial_json").and_then(Value::as_str).unwrap_or_default());
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
                                if let Some(usage) = value.pointer("/message/usage") {
                                    prompt_tokens = usage.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
                                    let (cache_read, cache_creation) = cache_usage(usage);
                                    partial.cache_read_tokens = cache_read;
                                    partial.cache_creation_tokens = cache_creation;
                                    partial.prompt_tokens = prompt_tokens;
                                }
                            }
                            "error" => {
                                Err(ProviderError::Provider(value.get("error").and_then(|error| error.get("message")).and_then(Value::as_str).unwrap_or("anthropic stream error").to_string()))?;
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
                if !emitted {
                    if serde_json::from_str::<Value>(arguments).is_ok() && !id.is_empty() && !name.is_empty() {
                        if !emitted_tool_call_ids.insert(id.clone()) {
                            Err(ProviderError::Sse(format!(
                                "provider emitted duplicate tool call id `{id}`"
                            )))?;
                        }
                        let call = rightkit_agent_engine_protocol::ToolCall {
                            id: id.clone(), name: name.clone(), arguments_json: arguments.clone(),
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
            // Preserve provider truncation on streaming seam. Engine owns
            // final completeness gate after adapter delivery.
            yield ProviderStreamEvent::Finished(finish_reason);
        };
        Ok(
            crate::agent_providers::client::observe_stream_breaker_outcome(
                base_url,
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
            crate::agent_providers::transport::observe_submission_attempt(
                &prepared,
                prepared
                    .measured_tokens
                    .expect("prepared request admitted before send"),
                1,
                Ok(&response),
            ),
        );
        if !response.is_success() {
            return Err(ProviderError::Http {
                status: response.status(),
                body: response.text().await.unwrap_or_default(),
            });
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
                        "content": format!("<system-reminder>\n{text}\n</system-reminder>"),
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
            out.push(json!({
                "role": role,
                "content": anthropic_assistant_content(&message, content),
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
    if let Some(text) = content.as_str().filter(|text| !text.is_empty()) {
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

/// Mark the last content block of the last message as a rolling cache
/// breakpoint so the whole conversation so far is read from cache next turn.
/// Thinking blocks cannot carry `cache_control` and are skipped.
fn mark_trailing_message_breakpoint(messages: &mut [Value]) {
    let Some(last) = messages.last_mut() else {
        return;
    };
    let marker = json!({"type": "ephemeral"});
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

fn anthropic_tool(tool: Value) -> Option<Value> {
    let function = tool.get("function")?;
    Some(json!({
        "name": function.get("name")?,
        "description": crate::agent_providers::client::bounded_tool_description(function),
        "input_schema": function.get("parameters").cloned().unwrap_or_else(|| json!({"type":"object","properties":{}})),
    }))
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
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| ProviderError::Network(error.to_string()))?;
        total = total.saturating_add(chunk.len());
        if total > MAX_SSE_BYTES {
            return Err(ProviderError::Sse("stream exceeded 4MiB".into()));
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
        "error" => {
            return Err(ProviderError::Provider(
                value
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("anthropic stream error")
                    .to_string(),
            ))
        }
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
static SIGNED_THINKING: Mutex<Vec<(String, Vec<Value>)>> = Mutex::new(Vec::new());
const SIGNED_THINKING_CAP: usize = 256;

fn remember_signed_thinking(blocks: Vec<Value>) {
    let key: String = blocks
        .iter()
        .filter_map(|block| block.get("thinking").and_then(Value::as_str))
        .collect();
    let key = key.trim().to_string();
    if key.is_empty() || !blocks.iter().any(|b| b.get("signature").is_some()) {
        return;
    }
    if let Ok(mut store) = SIGNED_THINKING.lock() {
        store.retain(|(existing, _)| existing != &key);
        store.push((key, blocks));
        if store.len() > SIGNED_THINKING_CAP {
            store.remove(0);
        }
    }
}

fn recall_signed_thinking(reasoning: &str) -> Option<Vec<Value>> {
    let store = SIGNED_THINKING.lock().ok()?;
    store
        .iter()
        .find(|(key, _)| key == reasoning.trim())
        .map(|(_, blocks)| blocks.clone())
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
