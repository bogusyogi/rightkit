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
                reason: format!(
                    "model `{}` has no entry in the model metadata table, so its context window is unknown; choose a model from the catalog",
                    client.model_id
                ),
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

    #[cfg(test)]
    pub(crate) fn with_semantic_progress_limit(mut self, limit: std::time::Duration) -> Self {
        self.semantic_progress_limit = Some(limit);
        self
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

    #[cfg(test)]
    fn request_body_for_test(
        &self,
        messages: Vec<Value>,
        tools: Vec<Value>,
        max_output_tokens: Option<u32>,
    ) -> Value {
        self.build_message_body(messages, tools, max_output_tokens)
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

#[cfg(test)]
fn anthropic_messages(messages: Vec<Value>) -> (Option<String>, Vec<Value>) {
    let (system, out) = anthropic_messages_split(messages);
    ((!system.is_empty()).then(|| system.join("\n\n")), out)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_image_parts_become_anthropic_base64_image_blocks() {
        let messages = vec![json!({"role":"user","content":[
            {"type":"text","text":"what is this?"},
            {"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}},
        ]})];
        let (_, out) = anthropic_messages(messages);
        let content = out[0]["content"].as_array().unwrap();
        assert_eq!(content[0], json!({"type":"text","text":"what is this?"}));
        assert_eq!(
            content[1],
            json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}})
        );
        assert!(!out[0].to_string().contains("image_url"));
    }

    #[test]
    fn invalid_images_degrade_to_text_notes_never_bad_blocks() {
        let oversize = format!("data:image/png;base64,{}", "A".repeat(8 * 1024 * 1024));
        for url in [
            "data:image/svg+xml;base64,AAAA",
            "data:image/png;base64,",
            "data:image/png,AAAA",
            "file:///etc/passwd",
            oversize.as_str(),
        ] {
            let block = anthropic_image_block(url);
            assert_eq!(block["type"], "text", "{url:.40}");
        }
        assert_eq!(
            anthropic_image_block("https://example.com/a.png")["source"]["type"],
            "url"
        );
    }

    #[test]
    fn string_user_content_is_untouched() {
        let (_, out) = anthropic_messages(vec![json!({"role":"user","content":"hello"})]);
        assert_eq!(out[0]["content"], "hello");
    }

    #[test]
    fn anthropic_authority_final_body_is_prepared_once_with_native_profile() {
        use crate::agent_providers::client::{
            ProviderRequestIdentityV1, RequestAuthorityBindingV1, RequestAuthorityErrorV1,
            RequestAuthorityInputV1, RequestAuthorityResponseV1, RequestAuthorityV1,
        };
        struct Authority(Mutex<Vec<RequestAuthorityInputV1>>);
        impl RequestAuthorityV1 for Authority {
            fn authorize(
                &self,
                input: RequestAuthorityInputV1,
            ) -> Result<RequestAuthorityResponseV1, RequestAuthorityErrorV1> {
                let mut final_body = input.draft_body.clone();
                final_body["system"] = json!("federated native context");
                let authorization_id = format!("native-{}", input.identity.generation);
                self.0.lock().unwrap().push(input);
                Ok(RequestAuthorityResponseV1 {
                    authorization_id,
                    final_body,
                })
            }
        }
        let authority = Arc::new(Authority(Mutex::new(Vec::new())));
        let identity = ProviderRequestIdentityV1 {
            session_id: "session".into(),
            task_id: "task".into(),
            request_id: "request".into(),
            generation: 6,
        };
        let client = AnthropicCompatClient::new(
            "key",
            "anthropic/claude-sonnet-4.5",
            "https://native.example",
        )
        .with_request_authority(RequestAuthorityBindingV1::new(
            identity.clone(),
            authority.clone(),
        ));
        let draft =
            client.build_message_body(vec![json!({"role":"user","content":"hello"})], vec![], None);
        let prepared = prepare_actual_message_request(&client, &draft, false).unwrap();
        let mut final_body = draft.clone();
        final_body["system"] = json!("federated native context");
        assert_eq!(prepared.bytes, serde_json::to_vec(&final_body).unwrap());
        assert_eq!(
            prepared.body_digest,
            rightkit_fs::digest::sha256_bytes(&prepared.bytes)
        );
        assert_eq!(prepared.request_identity, Some(identity));
        assert_eq!(prepared.authorization_id.as_deref(), Some("native-6"));
        let mut changed = draft;
        changed["messages"] = json!([{"role":"user","content":"continue"}]);
        let continued = prepare_actual_message_request(&client, &changed, true).unwrap();
        assert_eq!(continued.authorization_id.as_deref(), Some("native-7"));
        assert_ne!(prepared.body_digest, continued.body_digest);
        let inputs = authority.0.lock().unwrap();
        assert_eq!(inputs.len(), 2);
        assert_eq!(
            inputs[0].capacity_profile.route_id,
            "https://native.example"
        );
        assert!(inputs[0].capacity_profile.catalog_revision.is_some());
        assert!(inputs[1].continuation);
    }

    #[test]
    fn anthropic_changed_continuation_without_authority_is_public_typed_denial() {
        let client = AnthropicCompatClient::new(
            "key",
            "anthropic/claude-sonnet-4.5",
            "https://native.example",
        );
        let body = client.build_message_body(vec![], vec![], None);
        assert!(matches!(
            prepare_actual_message_request(&client, &body, true),
            Err(ProviderError::Authority(
                crate::agent_providers::client::RequestAuthorityErrorV1::ContinuationReauthorizationRequired
            ))
        ));
    }

    /// CR-13: LF and CRLF are equivalent SSE frame delimiters, even when the
    /// input is split at every possible byte offset inside Unicode content.
    #[test]
    fn decoder_handles_lf_crlf_and_split_utf8() {
        let data = r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"héllo — 你好 🎉"}}"#;
        for delimiter in ["\n\n", "\r\n\r\n"] {
            let bytes = format!("data: {data}{delimiter}").into_bytes();
            for split in 0..=bytes.len() {
                let mut decoder = AnthropicSseDecoder::new();
                let mut frames = decoder.push(&bytes[..split]).unwrap();
                frames.extend(decoder.push(&bytes[split..]).unwrap());
                assert_eq!(
                    frames,
                    vec![vec![data.to_string()]],
                    "split at byte {split}"
                );
                assert!(!decoder.has_incomplete_frame());
            }
        }
    }

    #[test]
    fn decoder_rejects_malformed_utf8_and_oversized_frames() {
        let mut malformed = AnthropicSseDecoder::new();
        assert!(malformed.push(b"data: \xff\n\n").is_err());

        let mut oversized = AnthropicSseDecoder::new();
        let line = vec![b'x'; AnthropicSseDecoder::MAX_FRAME_BYTES];
        let mut chunk = b"data: ".to_vec();
        chunk.extend_from_slice(&line);
        chunk.extend_from_slice(b"\ndata: x\n\n");
        let error = oversized.push(&chunk).expect_err("frame must be bounded");
        assert!(error.contains("SSE line exceeds limit"));
    }

    #[test]
    fn direct_request_honors_custom_auth_and_version_headers() {
        let client = AnthropicCompatClient::new_direct(
            "model",
            "https://direct.example/v1",
            Some(("Authorization".into(), "Bearer sentinel".into())),
            BTreeMap::from([("anthropic-version".into(), "2026-01-01".into())]),
            &["127.0.0.1:443".parse().unwrap()],
        );
        let request = client.request_builder("https://direct.example/v1/messages");
        let header = |name: &str| {
            request
                .headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(header("authorization"), Some("Bearer sentinel"));
        assert_eq!(header("anthropic-version"), Some("2026-01-01"));
    }

    #[test]
    fn maps_system_and_tool_schema_without_provider_guessing() {
        let (system, messages) = anthropic_messages(vec![
            json!({"role":"system","content":"rules"}),
            json!({"role":"user","content":"hello"}),
        ]);
        assert_eq!(system.as_deref(), Some("rules"));
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(anthropic_tool(json!({"type":"function","function":{"name":"read","description":"read","parameters":{"type":"object"}}})).unwrap()["input_schema"]["type"], "object");
    }

    #[test]
    fn normalizes_text_tools_usage_and_error_events() {
        let mut turn = CompletedTurn::default();
        let mut tools = Vec::new();
        apply_event(
            &mut turn,
            &mut tools,
            &json!({"type":"message_start","message":{"usage":{"input_tokens":3}}}),
        )
        .unwrap();
        apply_event(&mut turn, &mut tools, &json!({"type":"content_block_start","content_block":{"type":"tool_use","id":"tool-1","name":"read"}})).unwrap();
        apply_event(&mut turn, &mut tools, &json!({"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"path\":"}})).unwrap();
        apply_event(&mut turn, &mut tools, &json!({"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"\"x\"}"}})).unwrap();
        apply_event(
            &mut turn,
            &mut tools,
            &json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"done"}}),
        )
        .unwrap();
        apply_event(&mut turn, &mut tools, &json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":4}})).unwrap();
        assert_eq!(turn.text, "done");
        assert_eq!(turn.prompt_tokens, 3);
        assert_eq!(turn.completion_tokens, 4);
        assert_eq!(turn.finish_reason.as_deref(), Some("tool_use"));
        assert_eq!(
            tools[0],
            ("tool-1".into(), "read".into(), "{\"path\":\"x\"}".into())
        );
    }

    // ── P0-C 2c/2h: single cache breakpoint ─────────────────────────────

    #[test]
    fn single_breakpoint_lands_on_last_tool_when_tools_present() {
        let client = AnthropicCompatClient::new("key", "model", "https://api.anthropic.com");
        let body = client.request_body_for_test(
            vec![json!({"role": "system", "content": "rules"})],
            vec![json!({
                "type": "function",
                "function": {"name": "read", "description": "read", "parameters": {"type": "object"}}
            })],
            None,
        );
        assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
        // Render order is tools -> system -> messages: the system block must
        // carry its own breakpoint or the system prompt is never cached.
        assert_eq!(body["system"][0]["text"], "rules");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    }

    fn count_breakpoints(body: &Value) -> usize {
        let text = body.to_string();
        text.matches("\"cache_control\"").count()
    }

    #[test]
    fn breakpoints_cover_tools_system_and_rolling_history_within_limit() {
        let client = AnthropicCompatClient::new("key", "model", "https://api.anthropic.com");
        let tools = vec![json!({
            "type": "function",
            "function": {"name": "read", "description": "read", "parameters": {"type": "object"}}
        })];
        let turn1 = vec![
            json!({"role": "system", "content": "stable kernel"}),
            json!({"role": "system", "content": "volatile overlay v1"}),
            json!({"role": "user", "content": "hello"}),
        ];
        let body = client.request_body_for_test(turn1, tools.clone(), None);
        assert!(count_breakpoints(&body) <= 4, "{body}");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["system"][1]["cache_control"]["type"], "ephemeral");
        let last = &body["messages"][0]["content"][0];
        assert_eq!(last["text"], "hello");
        assert_eq!(last["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn cache_prefix_is_byte_stable_across_turns_when_only_history_grows() {
        let client = AnthropicCompatClient::new("key", "model", "https://api.anthropic.com");
        let tools = vec![json!({
            "type": "function",
            "function": {"name": "read", "description": "read", "parameters": {"type": "object"}}
        })];
        let sys = json!({"role": "system", "content": "stable kernel"});
        let first = client.request_body_for_test(
            vec![sys.clone(), json!({"role": "user", "content": "one"})],
            tools.clone(),
            None,
        );
        let second = client.request_body_for_test(
            vec![
                sys,
                json!({"role": "user", "content": "one"}),
                json!({"role": "assistant", "content": "ok"}),
                json!({"role": "user", "content": "two"}),
            ],
            tools,
            None,
        );
        // tools + system prefix identical byte-for-byte => cache read possible.
        assert_eq!(first["tools"], second["tools"]);
        assert_eq!(first["system"], second["system"]);
        // The first turn's user message is unchanged except for its moving
        // breakpoint marker; its text is the same prefix.
        assert_eq!(
            first["messages"][0]["content"][0]["text"],
            second["messages"][0]["content"]
                .as_str()
                .or_else(|| second["messages"][0]["content"][0]["text"].as_str())
                .unwrap()
        );
        // Only the trailing message carries the rolling breakpoint.
        assert!(second["messages"][0]["content"].is_string());
        assert_eq!(
            second["messages"][2]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert!(count_breakpoints(&second) <= 4);
    }

    #[test]
    fn late_system_message_does_not_mutate_the_cached_system_prefix() {
        let client = AnthropicCompatClient::new("key", "model", "https://api.anthropic.com");
        let turn1 = client.request_body_for_test(
            vec![
                json!({"role": "system", "content": "stable kernel"}),
                json!({"role": "user", "content": "one"}),
            ],
            vec![],
            None,
        );
        let turn2 = client.request_body_for_test(
            vec![
                json!({"role": "system", "content": "stable kernel"}),
                json!({"role": "user", "content": "one"}),
                json!({"role": "assistant", "content": "ok"}),
                json!({"role": "system", "content": "Triggered local skills"}),
                json!({"role": "user", "content": "two"}),
            ],
            vec![],
            None,
        );
        assert_eq!(turn1["system"], turn2["system"]);
        assert_eq!(turn2["messages"].as_array().unwrap().len(), 4);
        assert!(turn2["messages"][2]["content"]
            .as_str()
            .unwrap()
            .contains("Triggered local skills"));
    }

    #[test]
    fn message_delta_cache_usage_and_start_usage_fixture_parse() {
        // Realistic Anthropic stream usage: message_start then message_delta.
        let mut turn = CompletedTurn::default();
        let mut tools = Vec::new();
        apply_event(
            &mut turn,
            &mut tools,
            &json!({"type":"message_start","message":{"usage":{
                "input_tokens":12,"cache_creation_input_tokens":0,
                "cache_read_input_tokens":4096,"output_tokens":1}}}),
        )
        .unwrap();
        apply_event(
            &mut turn,
            &mut tools,
            &json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},
                    "usage":{"output_tokens":210}}),
        )
        .unwrap();
        assert_eq!(
            (
                turn.prompt_tokens,
                turn.cache_read_tokens,
                turn.cache_creation_tokens
            ),
            (12, 4096, 0)
        );
        assert_eq!(turn.completion_tokens, 210);
    }

    // S:M2/S:M3 amendment item B: native Anthropic "thinking" content
    // blocks surface into the same `reasoning` channel the OpenAI-compatible
    // path's `reasoning_content` uses, rather than being dropped.
    #[test]
    fn s_m2_thinking_deltas_surface_into_reasoning_channel() {
        let mut turn = CompletedTurn::default();
        let mut tools = Vec::new();
        apply_event(
            &mut turn,
            &mut tools,
            &json!({"type":"content_block_start","content_block":{"type":"thinking"}}),
        )
        .unwrap();
        apply_event(
            &mut turn,
            &mut tools,
            &json!({"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"first, "}}),
        )
        .unwrap();
        apply_event(
            &mut turn,
            &mut tools,
            &json!({"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"then this"}}),
        )
        .unwrap();
        assert_eq!(turn.reasoning.as_deref(), Some("first, then this"));
    }

    // S:M2/S:M3 amendment item C: rate-limit metadata parses from
    // Anthropic's `anthropic-ratelimit-*` headers onto the normalized
    // contract, independent of the OpenAI-compatible header parser.
    #[test]
    fn s_m2_parses_anthropic_rate_limit_headers() {
        let headers: Vec<(String, String)> = vec![
            ("anthropic-ratelimit-requests-remaining".into(), "17".into()),
            ("anthropic-ratelimit-tokens-remaining".into(), "5000".into()),
        ];

        let metadata = parse_anthropic_rate_limit_headers(&headers).expect("headers present");
        assert_eq!(metadata.remaining_requests, Some(17));
        assert_eq!(metadata.remaining_tokens, Some(5000));
    }

    #[test]
    fn s_m2_absent_anthropic_rate_limit_headers_yield_none() {
        let headers: Vec<(String, String)> = Vec::new();
        assert!(parse_anthropic_rate_limit_headers(&headers).is_none());
    }

    #[test]
    fn single_breakpoint_falls_back_to_system_when_no_tools() {
        let client = AnthropicCompatClient::new("key", "model", "https://api.anthropic.com");
        let body = client.request_body_for_test(
            vec![json!({"role": "system", "content": "rules"})],
            vec![],
            None,
        );
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["system"][0]["text"], "rules");
        assert_eq!(body["tools"].as_array().map(|a| a.len()), Some(0));
    }
}

#[cfg(test)]
mod parser_effect_tests {
    use super::*;
    use crate::agent_providers::ProviderStreamEvent;
    use futures::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    pub(super) async fn serve_sse(body: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        endpoint
    }

    fn anthropic_tool_stream(reason: &str) -> String {
        format!(
            "data: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":1}}}}}}\n\ndata: {{\"type\":\"content_block_start\",\"content_block\":{{\"type\":\"tool_use\",\"id\":\"tool-1\",\"name\":\"write_file\"}}}}\n\ndata: {{\"type\":\"content_block_delta\",\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":\"{{\\\"path\\\":\\\"out.txt\\\"}}\"}}}}\n\ndata: {{\"type\":\"content_block_stop\"}}\n\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"{reason}\"}}}}\n\ndata: {{\"type\":\"message_stop\"}}\n\n"
        )
    }

    #[tokio::test]
    async fn anthropic_real_parser_marks_incomplete_tool_generation() {
        for reason in [
            "length",
            "max_tokens",
            "max_output_tokens",
            "token_limit",
            "output_limit",
        ] {
            let endpoint = serve_sse(anthropic_tool_stream(reason)).await;
            let client =
                AnthropicCompatClient::new("test-key", "anthropic/claude-sonnet-4.5", endpoint);
            let mut stream = client
                .stream_with_tools(vec![], vec![], None)
                .await
                .unwrap();
            let mut saw_tool = false;
            let mut finished = None;
            while let Some(event) = stream.next().await {
                match event {
                    Ok(ProviderStreamEvent::ToolCall(_)) => saw_tool = true,
                    Ok(ProviderStreamEvent::Finished(reason)) => finished = reason,
                    Err(error) => panic!("unexpected parser error: {error:?}"),
                    Ok(_) => {}
                }
            }
            assert!(saw_tool);
            assert_eq!(finished.as_deref(), Some(reason));
        }
    }

    /// EFF-009: a genuinely truncated Anthropic wire fixture — the server
    /// sends a `data:` frame cut off mid-JSON (no terminating `\n\n`, no
    /// `message_stop`) and then closes the connection, exactly as a dropped
    /// upstream connection would. This must not be silently treated as a
    /// clean completion; the real decoder must surface
    /// `ProviderError::Sse("stream ended with an incomplete SSE frame")`.
    fn anthropic_truncated_mid_frame_stream() -> String {
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"partial ans".to_string()
    }

    #[tokio::test]
    async fn anthropic_real_parser_errors_on_truncated_mid_frame_stream() {
        let endpoint = serve_sse(anthropic_truncated_mid_frame_stream()).await;
        let client =
            AnthropicCompatClient::new("test-key", "anthropic/claude-sonnet-4.5", endpoint);
        let mut stream = client
            .stream_with_tools(vec![], vec![], None)
            .await
            .unwrap();
        let mut saw_error = false;
        while let Some(event) = stream.next().await {
            match event {
                Err(ProviderError::Sse(message)) => {
                    assert!(
                        message.contains("incomplete SSE frame"),
                        "unexpected SSE error: {message}"
                    );
                    saw_error = true;
                }
                Err(other) => panic!("expected typed incomplete-SSE-frame error, got {other:?}"),
                Ok(_) => {}
            }
        }
        assert!(
            saw_error,
            "a mid-frame-truncated stream must surface a decoder error, not silent success"
        );
    }

    #[tokio::test]
    async fn anthropic_real_parser_emits_complete_tool_generation() {
        let endpoint = serve_sse(anthropic_tool_stream("tool_use")).await;
        let client =
            AnthropicCompatClient::new("test-key", "anthropic/claude-sonnet-4.5", endpoint);
        let mut stream = client
            .stream_with_tools(vec![], vec![], None)
            .await
            .unwrap();
        let mut saw_tool = false;
        let mut finished = None;
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                ProviderStreamEvent::ToolCall(call) => saw_tool = call.name == "write_file",
                ProviderStreamEvent::Finished(reason) => finished = reason,
                _ => {}
            }
        }
        assert!(saw_tool);
        assert_eq!(finished.as_deref(), Some("tool_use"));
    }

    // ---- CR4 item 1: protocol fixtures, Anthropic Messages ----
    // Reuses this module's `serve_sse` and the real `stream_with_tools` path.

    /// Frames a scripted SSE body: each payload becomes one `data:` event.
    fn sse(frames: &[&str]) -> String {
        frames
            .iter()
            .map(|frame| format!("data: {frame}\n\n"))
            .collect()
    }

    struct FixtureOutcome {
        text: String,
        calls: Vec<ToolCall>,
        finished: Option<Option<String>>,
        error: Option<ProviderError>,
    }

    async fn fixture_outcome(mut stream: ProviderResponseStream) -> FixtureOutcome {
        let mut outcome = FixtureOutcome {
            text: String::new(),
            calls: Vec::new(),
            finished: None,
            error: None,
        };
        while let Some(event) = stream.next().await {
            match event {
                Ok(ProviderStreamEvent::Text(text)) => outcome.text.push_str(&text),
                Ok(ProviderStreamEvent::ToolCall(call)) => outcome.calls.push(call),
                Ok(ProviderStreamEvent::Finished(reason)) => outcome.finished = Some(reason),
                Ok(_) => {}
                Err(error) => {
                    outcome.error = Some(error);
                    break;
                }
            }
        }
        outcome
    }

    async fn anthropic_fixture(body: String) -> ProviderResponseStream {
        let endpoint = serve_sse(body).await;
        let client =
            AnthropicCompatClient::new("test-key", "anthropic/claude-sonnet-4.5", endpoint);
        client
            .stream_with_tools(vec![], vec![], None)
            .await
            .unwrap()
    }

    fn assert_sse_error(outcome: &FixtureOutcome, fragment: &str) {
        match &outcome.error {
            Some(ProviderError::Sse(message)) => {
                assert!(
                    message.contains(fragment),
                    "unexpected SSE error: {message}"
                )
            }
            other => panic!("expected ProviderError::Sse containing {fragment:?}, got {other:?}"),
        }
    }

    const AN_START: &str = r#"{"type":"message_start","message":{"usage":{"input_tokens":1}}}"#;
    const AN_TOOL_START: &str = r#"{"type":"content_block_start","content_block":{"type":"tool_use","id":"tool-1","name":"write_file"}}"#;
    const AN_ARGS_HEAD: &str = r#"{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#;
    const AN_ARGS_TAIL: &str = r#"{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"\"out.txt\"}"}}"#;
    const AN_ARGS_TRUNCATED: &str = r#"{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"path\":\"out"}}"#;
    const AN_BLOCK_STOP: &str = r#"{"type":"content_block_stop"}"#;
    const AN_TEXT_PARTIAL: &str =
        r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"partial"}}"#;
    const AN_DELTA_TOOL_USE: &str =
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"}}"#;
    const AN_DELTA_END_TURN: &str =
        r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#;
    const AN_MESSAGE_STOP: &str = r#"{"type":"message_stop"}"#;

    #[tokio::test]
    async fn anthropic_messages_valid_completion_emits_tool_call_once_with_complete_arguments() {
        let outcome = fixture_outcome(
            anthropic_fixture(sse(&[
                AN_START,
                AN_TOOL_START,
                AN_ARGS_HEAD,
                AN_ARGS_TAIL,
                AN_BLOCK_STOP,
                AN_DELTA_TOOL_USE,
                AN_MESSAGE_STOP,
            ]))
            .await,
        )
        .await;
        assert!(
            outcome.error.is_none(),
            "unexpected error: {:?}",
            outcome.error
        );
        assert_eq!(
            outcome.calls.len(),
            1,
            "tool call must be emitted exactly once"
        );
        assert_eq!(outcome.calls[0].id, "tool-1");
        assert_eq!(outcome.calls[0].name, "write_file");
        assert_eq!(outcome.calls[0].arguments_json, r#"{"path":"out.txt"}"#);
        assert_eq!(outcome.finished, Some(Some("tool_use".to_string())));
    }

    #[tokio::test]
    async fn anthropic_messages_bare_message_stop_is_incomplete_not_success() {
        let outcome = fixture_outcome(anthropic_fixture(sse(&[AN_MESSAGE_STOP])).await).await;
        assert!(outcome.text.is_empty() && outcome.calls.is_empty());
        assert_sse_error(&outcome, "before completion");
        assert!(
            outcome.finished.is_none(),
            "bare message_stop must not complete"
        );
    }

    #[tokio::test]
    async fn anthropic_messages_eof_without_completion_is_incomplete_not_completed() {
        let outcome =
            fixture_outcome(anthropic_fixture(sse(&[AN_START, AN_TEXT_PARTIAL])).await).await;
        assert_eq!(outcome.text, "partial");
        assert_sse_error(&outcome, "before completion");
        assert!(
            outcome.finished.is_none(),
            "abrupt EOF must not report Finished"
        );
    }

    #[tokio::test]
    async fn anthropic_messages_truncated_tool_arguments_never_surface_a_call() {
        let cases: Vec<(&str, String)> = vec![
            (
                "eof_after_partial_arguments",
                sse(&[AN_START, AN_TOOL_START, AN_ARGS_TRUNCATED]),
            ),
            (
                "block_stop_with_partial_arguments",
                sse(&[
                    AN_START,
                    AN_TOOL_START,
                    AN_ARGS_TRUNCATED,
                    AN_BLOCK_STOP,
                    AN_DELTA_TOOL_USE,
                    AN_MESSAGE_STOP,
                ]),
            ),
            (
                "frame_cut_mid_arguments",
                format!(
                    "{}data: {AN_ARGS_TRUNCATED}",
                    sse(&[AN_START, AN_TOOL_START])
                ),
            ),
        ];
        for (name, body) in cases {
            let outcome = fixture_outcome(anthropic_fixture(body).await).await;
            assert!(
                outcome.calls.is_empty(),
                "{name}: partial arguments surfaced as an executable tool call"
            );
            assert!(
                matches!(outcome.error, Some(ProviderError::Sse(_))),
                "{name}: truncated tool call must end in an SSE error"
            );
        }
    }

    #[tokio::test]
    async fn anthropic_messages_malformed_event_json_uses_rejection_path() {
        let cases: Vec<(&str, String)> = vec![
            ("non_json_payload", sse(&["nope"])),
            (
                "truncated_json_payload",
                "data: {\"type\":\"content_block_delta\"\n\n".to_string(),
            ),
        ];
        for (name, body) in cases {
            let outcome = fixture_outcome(anthropic_fixture(body).await).await;
            assert!(
                matches!(outcome.error, Some(ProviderError::Sse(_))),
                "{name}: malformed event must be rejected with ProviderError::Sse"
            );
            assert!(
                outcome.finished.is_none(),
                "{name}: must not report Finished"
            );
        }
    }

    #[tokio::test]
    async fn anthropic_messages_complete_tool_block_then_eof_is_not_a_completed_tool_turn() {
        let outcome = fixture_outcome(
            anthropic_fixture(sse(&[
                AN_START,
                AN_TOOL_START,
                AN_ARGS_HEAD,
                AN_ARGS_TAIL,
                AN_BLOCK_STOP,
            ]))
            .await,
        )
        .await;
        // The engine defers every yielded call until `Finished`
        // (engine_loop.rs deferred_calls), so the contract is that the stream
        // ends in an error and never reports Finished.
        assert_sse_error(&outcome, "before completion");
        assert!(outcome.finished.is_none(), "EOF must not report Finished");
    }

    #[tokio::test]
    async fn anthropic_messages_stop_reason_then_eof_without_message_stop_is_incomplete() {
        let outcome = fixture_outcome(
            anthropic_fixture(sse(&[AN_START, AN_TEXT_PARTIAL, AN_DELTA_END_TURN])).await,
        )
        .await;
        assert_sse_error(&outcome, "before completion");
        assert!(
            outcome.finished.is_none(),
            "EOF without message_stop must not report Finished"
        );
    }

    #[tokio::test]
    async fn anthropic_tool_call_is_not_yielded_at_content_block_stop() {
        // Complete tool block, then a stream cut: no call may surface.
        let outcome = fixture_outcome(
            anthropic_fixture(sse(&[
                AN_START,
                AN_TOOL_START,
                AN_ARGS_HEAD,
                AN_ARGS_TAIL,
                AN_BLOCK_STOP,
            ]))
            .await,
        )
        .await;
        assert!(
            outcome.calls.is_empty(),
            "call surfaced before message_stop"
        );
        assert_sse_error(&outcome, "before completion");
        assert!(outcome.finished.is_none());
    }

    #[tokio::test]
    async fn anthropic_tool_call_with_stop_reason_but_no_message_stop_is_typed_error() {
        let outcome = fixture_outcome(
            anthropic_fixture(sse(&[
                AN_START,
                AN_TOOL_START,
                AN_ARGS_HEAD,
                AN_ARGS_TAIL,
                AN_BLOCK_STOP,
                AN_DELTA_TOOL_USE,
            ]))
            .await,
        )
        .await;
        assert!(
            outcome.calls.is_empty(),
            "call surfaced without message_stop"
        );
        assert_sse_error(&outcome, "before completion");
        assert!(outcome.finished.is_none());
    }

    #[tokio::test]
    async fn anthropic_text_only_turn_completes_only_with_message_stop() {
        let ok = fixture_outcome(
            anthropic_fixture(sse(&[
                AN_START,
                AN_TEXT_PARTIAL,
                AN_DELTA_END_TURN,
                AN_MESSAGE_STOP,
            ]))
            .await,
        )
        .await;
        assert!(ok.error.is_none(), "unexpected error: {:?}", ok.error);
        assert_eq!(ok.text, "partial");
        assert_eq!(ok.finished, Some(Some("end_turn".to_string())));
        let cut = fixture_outcome(
            anthropic_fixture(sse(&[AN_START, AN_TEXT_PARTIAL, AN_DELTA_END_TURN])).await,
        )
        .await;
        assert_sse_error(&cut, "before completion");
        assert!(cut.finished.is_none());
    }

    /// Serves one canned SSE body per accepted connection, in order, and
    /// counts the requests received.
    async fn serve_sse_sequence(
        bodies: Vec<String>,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = count.clone();
        tokio::spawn(async move {
            for body in bodies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 4096];
                loop {
                    let read = socket.read(&mut chunk).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let length = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (endpoint, count)
    }

    const AN_DELTA_MAX_TOKENS: &str =
        r#"{"type":"message_delta","delta":{"stop_reason":"max_tokens"}}"#;
    const AN_TEXT_REST: &str =
        r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":" rest"}}"#;

    #[tokio::test]
    async fn anthropic_stream_text_max_tokens_continues_once_keeping_partial() {
        let first = sse(&[
            AN_START,
            AN_TEXT_PARTIAL,
            AN_DELTA_MAX_TOKENS,
            AN_MESSAGE_STOP,
        ]);
        let second = sse(&[AN_START, AN_TEXT_REST, AN_DELTA_END_TURN, AN_MESSAGE_STOP]);
        let (endpoint, count) = serve_sse_sequence(vec![first, second]).await;
        let client =
            AnthropicCompatClient::new("test-key", "anthropic/claude-sonnet-4.5", endpoint)
                .with_direct_dispatch();
        let outcome = fixture_outcome(
            client
                .stream_with_tools(vec![], vec![], None)
                .await
                .unwrap(),
        )
        .await;
        assert!(
            outcome.error.is_none(),
            "unexpected error: {:?}",
            outcome.error
        );
        assert_eq!(outcome.text, "partial rest");
        assert_eq!(outcome.finished, Some(Some("end_turn".to_string())));
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn anthropic_stream_repeated_max_tokens_is_bounded_to_one_continuation() {
        let limited = || {
            sse(&[
                AN_START,
                AN_TEXT_PARTIAL,
                AN_DELTA_MAX_TOKENS,
                AN_MESSAGE_STOP,
            ])
        };
        let (endpoint, count) = serve_sse_sequence(vec![limited(), limited()]).await;
        let client =
            AnthropicCompatClient::new("test-key", "anthropic/claude-sonnet-4.5", endpoint)
                .with_direct_dispatch();
        let outcome = fixture_outcome(
            client
                .stream_with_tools(vec![], vec![], None)
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(outcome.text, "partialpartial");
        assert_eq!(outcome.finished, Some(Some("max_tokens".to_string())));
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "no third request"
        );
    }

    #[tokio::test]
    async fn anthropic_stream_max_tokens_mid_tool_arguments_is_not_executed_or_continued() {
        let body = sse(&[
            AN_START,
            AN_TEXT_PARTIAL,
            AN_TOOL_START,
            AN_ARGS_TRUNCATED,
            AN_DELTA_MAX_TOKENS,
            AN_MESSAGE_STOP,
        ]);
        let (endpoint, count) = serve_sse_sequence(vec![body]).await;
        let client =
            AnthropicCompatClient::new("test-key", "anthropic/claude-sonnet-4.5", endpoint);
        let outcome = fixture_outcome(
            client
                .stream_with_tools(vec![], vec![], None)
                .await
                .unwrap(),
        )
        .await;
        assert!(
            outcome.error.is_none(),
            "unexpected error: {:?}",
            outcome.error
        );
        assert!(
            outcome.calls.is_empty(),
            "cut-off tool call must not surface"
        );
        assert_eq!(outcome.text, "partial");
        assert_eq!(outcome.finished, Some(Some("max_tokens".to_string())));
        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "never continued"
        );
    }

    async fn aggregate_result(body: String) -> Result<CompletedTurn, ProviderError> {
        let endpoint = serve_sse(body).await;
        let client =
            AnthropicCompatClient::new("test-key", "anthropic/claude-sonnet-4.5", endpoint);
        client.complete_with_tools(vec![], vec![], None).await
    }

    #[tokio::test]
    async fn anthropic_aggregate_text_turn_requires_message_stop() {
        let turn = aggregate_result(sse(&[
            AN_START,
            AN_TEXT_PARTIAL,
            AN_DELTA_END_TURN,
            AN_MESSAGE_STOP,
        ]))
        .await
        .expect("complete text turn must succeed");
        assert_eq!(turn.text, "partial");
        assert_eq!(turn.finish_reason.as_deref(), Some("end_turn"));

        match aggregate_result(sse(&[AN_START, AN_TEXT_PARTIAL, AN_DELTA_END_TURN])).await {
            Err(ProviderError::Sse(message)) => assert!(message.contains("before completion")),
            other => panic!(
                "EOF after stop_reason without message_stop must be Sse error, got {other:?}"
            ),
        }
    }

    #[tokio::test]
    async fn anthropic_aggregate_tool_turn_requires_message_stop() {
        let tool_events = [
            AN_START,
            AN_TOOL_START,
            AN_ARGS_HEAD,
            AN_ARGS_TAIL,
            AN_BLOCK_STOP,
            AN_DELTA_TOOL_USE,
        ];
        let mut complete = tool_events.to_vec();
        complete.push(AN_MESSAGE_STOP);
        let turn = aggregate_result(sse(&complete))
            .await
            .expect("complete tool turn");
        assert_eq!(turn.tool_calls.len(), 1);
        assert!(matches!(
            aggregate_result(sse(&tool_events)).await,
            Err(ProviderError::Sse(_))
        ));
    }

    #[tokio::test]
    async fn anthropic_tool_call_is_emitted_once_after_message_stop() {
        let outcome = fixture_outcome(
            anthropic_fixture(sse(&[
                AN_START,
                AN_TOOL_START,
                AN_ARGS_HEAD,
                AN_ARGS_TAIL,
                AN_BLOCK_STOP,
                AN_DELTA_TOOL_USE,
                AN_MESSAGE_STOP,
            ]))
            .await,
        )
        .await;
        assert!(
            outcome.error.is_none(),
            "unexpected error: {:?}",
            outcome.error
        );
        assert_eq!(outcome.calls.len(), 1);
        assert_eq!(outcome.finished, Some(Some("tool_use".to_string())));
    }
}

#[cfg(test)]
mod tool_error_fidelity_tests {
    use super::*;

    #[test]
    fn anthropic_tool_result_block_carries_is_error_only_when_failed() {
        let (_, out) = anthropic_messages_split(vec![
            json!({"role":"tool","tool_call_id":"a","content":"boom","is_error":true}),
            json!({"role":"tool","tool_call_id":"b","content":"fine"}),
        ]);
        let blocks = out[0]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["is_error"], true);
        assert!(blocks[1].get("is_error").is_none());
    }
}

#[cfg(test)]
mod semantic_progress_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn anthropic_ping_only_stream_hits_semantic_progress_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 8192];
            let _ = socket.read(&mut request).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
                .await;
            loop {
                if socket
                    .write_all(b"event: ping\ndata: {\"type\":\"ping\"}\n\n")
                    .await
                    .is_err()
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        });
        let client = AnthropicCompatClient::new("k", "anthropic/claude-sonnet-4.5", endpoint)
            .with_semantic_progress_limit(std::time::Duration::from_millis(300));
        let mut stream = client
            .stream_with_tools(vec![json!({"role":"user","content":"hi"})], vec![], None)
            .await
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("deadline must fire despite pings");
        assert!(
            matches!(result, Some(Err(ProviderError::Network(m))) if m.contains("semantic progress"))
        );
    }
}

#[cfg(test)]
mod wire_conformance_tests {
    use super::*;
    use futures::StreamExt;

    fn client() -> AnthropicCompatClient {
        AnthropicCompatClient::new("k", "claude-sonnet-5-5", "http://127.0.0.1:1".to_string())
    }

    fn count_cc(body: &Value) -> usize {
        serde_json::to_string(body)
            .unwrap()
            .matches("\"cache_control\"")
            .count()
    }

    #[test]
    fn missing_tool_result_gets_synthetic_error_first_in_user_turn() {
        let body = client().request_body_for_test(
            vec![
                json!({"role":"user","content":"go"}),
                json!({"role":"assistant","content":"","tool_calls":[
                    {"id":"t1","function":{"name":"a","arguments":"{}"}},
                    {"id":"t2","function":{"name":"b","arguments":"{}"}}]}),
                json!({"role":"tool","tool_call_id":"t2","content":"ok"}),
            ],
            vec![],
            None,
        );
        let blocks = body["messages"][2]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["tool_use_id"], "t1");
        assert_eq!(blocks[0]["is_error"], true);
        assert_eq!(blocks[1]["tool_use_id"], "t2");
    }

    #[test]
    fn trailing_unanswered_tool_use_gets_user_result_turn() {
        let body = client().request_body_for_test(
            vec![
                json!({"role":"user","content":"go"}),
                json!({"role":"assistant","content":"","tool_calls":[
                    {"id":"t1","function":{"name":"a","arguments":"{}"}}]}),
            ],
            vec![],
            None,
        );
        assert_eq!(body["messages"][2]["role"], "user");
        assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "t1");
    }

    #[test]
    fn orphan_tool_result_is_dropped() {
        let body = client().request_body_for_test(
            vec![
                json!({"role":"user","content":"go"}),
                json!({"role":"assistant","content":"hi"}),
                json!({"role":"tool","tool_call_id":"ghost","content":"x"}),
                json!({"role":"user","content":"next"}),
            ],
            vec![],
            None,
        );
        let text = serde_json::to_string(&body["messages"]).unwrap();
        assert!(!text.contains("ghost"));
    }

    #[test]
    fn explicit_breakpoints_never_exceed_four() {
        let tools = vec![json!({"function":{"name":"a","parameters":{"type":"object"}}})];
        let body = client().request_body_for_test(
            vec![
                json!({"role":"system","content":"s1"}),
                json!({"role":"system","content":"s2"}),
                json!({"role":"system","content":"s3"}),
                json!({"role":"user","content":"hello"}),
            ],
            tools,
            None,
        );
        assert!(count_cc(&body) <= 4);
    }

    #[tokio::test]
    async fn signed_thinking_streamed_via_sse_is_replayed_verbatim_before_tool_use() {
        let frames = [
            r#"{"type":"message_start","message":{"usage":{"input_tokens":1}}}"#,
            r#"{"type":"content_block_start","content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","delta":{"type":"thinking_delta","thinking":"wire-conformance-reasoning"}}"#,
            r#"{"type":"content_block_delta","delta":{"type":"signature_delta","signature":"SIG123"}}"#,
            r#"{"type":"content_block_stop"}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
            r#"{"type":"message_stop"}"#,
        ];
        let body: String = frames.iter().map(|f| format!("data: {f}\n\n")).collect();
        let endpoint = super::parser_effect_tests::serve_sse(body).await;
        let c = AnthropicCompatClient::new("k", "anthropic/claude-sonnet-4.5", endpoint);
        let mut stream = c.stream_with_tools(vec![], vec![], None).await.unwrap();
        while stream.next().await.is_some() {}
        let out = client().request_body_for_test(
            vec![
                json!({"role":"user","content":"go"}),
                json!({"role":"assistant","content":"","reasoning_content":"wire-conformance-reasoning",
                    "tool_calls":[{"id":"t1","function":{"name":"a","arguments":"{}"}}]}),
                json!({"role":"tool","tool_call_id":"t1","content":"ok"}),
            ],
            vec![],
            None,
        );
        let blocks = out["messages"][1]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "thinking");
        assert_eq!(blocks[0]["signature"], "SIG123");
        assert_eq!(blocks[1]["type"], "tool_use");
    }
}

#[cfg(test)]
mod thinking_body_tests {
    use super::*;

    fn user(text: &str) -> Value {
        json!({"role": "user", "content": text})
    }

    fn body(
        model: &str,
        configure: impl FnOnce(AnthropicCompatClient) -> AnthropicCompatClient,
        max: Option<u32>,
    ) -> Value {
        let client = configure(AnthropicCompatClient::new(
            "key",
            model,
            "https://api.anthropic.com",
        ));
        client.request_body_for_test(vec![user("hi")], vec![], max)
    }

    #[test]
    fn adaptive_only_models_send_adaptive_summarized_and_never_enabled() {
        for model in [
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-fable-5-1",
            "claude-opus-4-8",
            "claude-opus-4-7",
        ] {
            let b = body(model, |c| c, None);
            assert_eq!(
                b["thinking"],
                json!({"type": "adaptive", "display": "summarized"}),
                "{model}"
            );
            assert!(b.get("output_config").is_none(), "{model}: {b}");
            assert_eq!(b["max_tokens"], 16_000, "{model}");
            assert!(!b.to_string().contains("budget_tokens"), "{model}");
        }
    }

    #[test]
    fn effort_reaches_output_config_not_the_thinking_object() {
        let b = body(
            "claude-opus-4-8",
            |c| c.with_effort("XHigh").with_thinking_budget_tokens(4096),
            None,
        );
        assert_eq!(b["output_config"], json!({"effort": "xhigh"}));
        assert!(b["thinking"].get("effort").is_none(), "{b}");
        assert!(b["thinking"].get("budget_tokens").is_none(), "{b}");
        // 4.6 has no xhigh, so it is lowered to high instead of 400ing.
        let b = body("claude-sonnet-4-6", |c| c.with_effort("xhigh"), None);
        assert_eq!(b["output_config"]["effort"], "high");
    }

    #[test]
    fn sonnet_4_6_still_uses_adaptive_not_deprecated_budget() {
        let b = body(
            "claude-sonnet-4-6",
            |c| c.with_thinking_budget_tokens(5000),
            None,
        );
        assert_eq!(b["thinking"]["type"], "adaptive");
        assert!(b["thinking"].get("budget_tokens").is_none(), "{b}");
    }

    #[test]
    fn manual_models_send_enabled_with_budget_below_max_tokens() {
        for model in [
            "claude-opus-4-5-20251101",
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5-20251001",
            "claude-opus-4-1-20250805",
        ] {
            let b = body(model, |c| c.with_thinking_budget_tokens(6000), None);
            assert_eq!(b["thinking"]["type"], "enabled", "{model}");
            assert_eq!(b["thinking"]["budget_tokens"], 6000, "{model}");
            assert!(b["max_tokens"].as_u64().unwrap() > 6000, "{model}: {b}");
            assert!(b["thinking"].get("display").is_none(), "{model}");
        }
        // Default budget when none is configured.
        let b = body("claude-sonnet-4-5", |c| c, None);
        assert_eq!(
            b["thinking"],
            json!({"type": "enabled", "budget_tokens": 8192})
        );
        assert!(b["max_tokens"].as_u64().unwrap() > 8192);
        // Only Opus 4.5 takes effort among the manual models.
        let b = body("claude-opus-4-5-20251101", |c| c.with_effort("max"), None);
        assert_eq!(b["output_config"]["effort"], "high");
        let b = body("claude-haiku-4-5", |c| c.with_effort("high"), None);
        assert!(b.get("output_config").is_none(), "{b}");
    }

    #[test]
    fn manual_budget_is_kept_below_an_explicit_max_output_tokens() {
        let b = body(
            "claude-sonnet-4-5",
            |c| c.with_thinking_budget_tokens(8000),
            Some(4096),
        );
        assert_eq!(b["max_tokens"], 4096);
        assert!(
            b["thinking"]["budget_tokens"].as_u64().unwrap() < 4096,
            "{b}"
        );
        assert!(
            b["thinking"]["budget_tokens"].as_u64().unwrap() >= 1024,
            "{b}"
        );
    }

    #[test]
    fn models_without_thinking_send_no_thinking_fields() {
        for model in [
            "claude-3-5-sonnet-20241022",
            "claude-3-haiku-20240307",
            "model",
            "gpt-5",
        ] {
            let b = body(
                model,
                |c| c.with_effort("high").with_thinking_budget_tokens(4096),
                None,
            );
            assert!(b.get("thinking").is_none(), "{model}: {b}");
            assert!(b.get("output_config").is_none(), "{model}: {b}");
            assert_eq!(b["max_tokens"], 4096, "{model}");
        }
    }

    #[test]
    fn disabled_thinking_uses_each_models_off_switch() {
        let b = body("claude-opus-5", |c| c.with_thinking_disabled(), None);
        assert_eq!(b["thinking"], json!({"type": "disabled"}));
        let b = body("claude-sonnet-5-5", |c| c.with_thinking_disabled(), None);
        assert_eq!(b["thinking"], json!({"type": "between_tools"}));
        let b = body("claude-opus-5-5", |c| c.with_thinking_disabled(), None);
        assert!(b.get("thinking").is_none(), "{b}");
        let b = body("claude-haiku-4-5", |c| c.with_thinking_disabled(), None);
        assert!(b.get("thinking").is_none(), "{b}");
    }

    #[test]
    fn manual_thinking_is_skipped_when_tool_loop_assistant_has_no_thinking_block() {
        let client =
            AnthropicCompatClient::new("key", "claude-sonnet-4-5", "https://api.anthropic.com");
        let tool_turn = |first: Value| {
            vec![
                user("go"),
                json!({"role": "assistant", "content": [first, {"type": "tool_use", "id": "t1", "name": "read", "input": {}}]}),
                json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]}),
            ]
        };
        let plain = client.build_message_body(
            tool_turn(json!({"type": "text", "text": "x"})),
            vec![],
            None,
        );
        assert!(plain.get("thinking").is_none(), "{plain}");
        let signed = client.build_message_body(
            tool_turn(json!({"type": "thinking", "thinking": "t", "signature": "sig"})),
            vec![],
            None,
        );
        assert_eq!(signed["thinking"]["type"], "enabled", "{signed}");
        assert_eq!(signed["messages"][1]["content"][0]["signature"], "sig");
    }

    #[test]
    fn signed_thinking_replay_is_preserved_with_adaptive_thinking_on() {
        let client =
            AnthropicCompatClient::new("key", "claude-opus-4-8", "https://api.anthropic.com");
        let messages = vec![
            user("go"),
            json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "plan", "signature": "sig"},
                {"type": "tool_use", "id": "t1", "name": "read", "input": {}}
            ]}),
            json!({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]}),
        ];
        let b = client.build_message_body(messages, vec![], None);
        assert_eq!(b["thinking"]["type"], "adaptive");
        assert_eq!(b["messages"][1]["content"][0]["type"], "thinking");
        assert_eq!(b["messages"][1]["content"][0]["signature"], "sig");
    }
}
