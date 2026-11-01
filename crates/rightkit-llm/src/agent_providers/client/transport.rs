//! `ra-018` decomposition: `OpenAiCompatClient` HTTP request/stream bodies
//! and response decoding — moved verbatim out of `client.rs`. Request
//! building stays on the facade (`super::OpenAiCompatClient::build_chat_request`);
//! this module owns issuing the request, the retry/breaker-gated ladders
//! around it, and decoding the SSE response into `CompletedTurn` /
//! `ProviderStreamEvent`. Behavior, timing, and error classification are
//! unchanged; only the module boundary is new.
//! See `.audit/20260901-daemon-deep-r3/architect/decomposition-plans.md`.

use super::breaker::{breaker_allows_request, breaker_record_outcome, is_breaker_relevant_failure};
use super::retry::{
    observe_stream_breaker_outcome, parse_openai_rate_limit_headers,
    record_stream_establishment_outcome, retry_after_secs,
};
use super::wire::{
    can_continue_incomplete, continuation_messages, incomplete_output_error,
    is_incomplete_finish_reason, merge_continuation, strip_think_tags,
    validate_complete_tool_calls, Chunk, StreamAccum,
};
use super::{
    CompletedTurn, OpenAiCompatClient, ProviderRequestMetadata, ProviderResponseStream,
    ProviderStreamEvent,
};
use crate::agent_providers::transport::{
    semantic_progress_error, ProtocolFamily, ProviderError, SEMANTIC_PROGRESS_SECS,
};
use crate::reassembler::ToolCallChunkReassembler;
use futures::StreamExt;
use rightkit_agent_engine_protocol::ToolCall;

use super::{
    CapacityError, CapacityProfileV1, PreparedModalityV1, PreparedProviderRequestV1,
    PreparedRequestGateV1,
};

/// CRA-10 wiring point for `OpenAiCompatClient::build_chat_request`'s
/// output: turns the already-built `body` into a
/// [`PreparedProviderRequestV1`] over its exact serialized bytes and admits
/// it against `profile` before send. Callers must use the returned
/// `prepared.bytes` with `.body(prepared.bytes)` and
/// `prepared.content_type` — never re-send the original `.json(&body)` call,
/// which would reserialize and break the measured/digested/sent byte
/// equality CRA-10 requires.
pub(super) fn prepare_and_admit_chat_request(
    body: &impl serde::Serialize,
    content_type: &'static str,
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
        content_type,
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

const CHAT_REQUEST_SERIALIZER_GENERATION_V1: u64 = 1;

#[derive(Default)]
struct CodexStreamAccum {
    text: String,
    reasoning: String,
    tool_calls: std::collections::BTreeMap<String, (String, String)>,
    item_to_call: std::collections::BTreeMap<String, String>,
    finish_reason: Option<String>,
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_read_tokens: u32,
    reasoning_tokens: u32,
    got_terminal: bool,
}

fn apply_codex_event(
    event: serde_json::Value,
    accum: &mut CodexStreamAccum,
) -> Result<(), ProviderError> {
    let event_type = event
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    match event_type {
        "response.output_text.delta" => {
            if let Some(delta) = event.get("delta").and_then(serde_json::Value::as_str) {
                accum.text.push_str(delta);
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(delta) = event.get("delta").and_then(serde_json::Value::as_str) {
                accum.reasoning.push_str(delta);
            }
        }
        "response.output_item.added" => {
            let item = event.get("item").unwrap_or(&serde_json::Value::Null);
            if item.get("type").and_then(serde_json::Value::as_str) == Some("function_call") {
                let id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let name = item
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if !id.is_empty() {
                    accum.tool_calls.entry(id).or_insert((name, String::new()));
                }
                if let (Some(item_id), Some(call_id)) = (
                    item.get("id").and_then(serde_json::Value::as_str),
                    item.get("call_id").and_then(serde_json::Value::as_str),
                ) {
                    accum
                        .item_to_call
                        .insert(item_id.to_string(), call_id.to_string());
                }
            }
        }
        "response.function_call_arguments.delta" => {
            let raw_id = event
                .get("item_id")
                .or_else(|| event.get("call_id"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let id = accum
                .item_to_call
                .get(raw_id)
                .map(String::as_str)
                .unwrap_or(raw_id);
            if !id.is_empty() {
                accum
                    .tool_calls
                    .entry(id.to_string())
                    .or_insert((String::new(), String::new()))
                    .1
                    .push_str(
                        event
                            .get("delta")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default(),
                    );
            }
        }
        "response.function_call_arguments.done" => {
            let raw_id = event
                .get("item_id")
                .or_else(|| event.get("call_id"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            let id = accum
                .item_to_call
                .get(raw_id)
                .map(String::as_str)
                .unwrap_or(raw_id);
            if let Some((_, arguments)) = accum.tool_calls.get_mut(id) {
                if let Some(value) = event.get("arguments").and_then(serde_json::Value::as_str) {
                    *arguments = value.to_string();
                }
            }
        }
        "response.reasoning_summary_part.added" => {
            // A new summary part starts a new paragraph of the Thought text.
            if !accum.reasoning.is_empty()
                && !accum.reasoning.ends_with(
                    "

",
                )
            {
                accum.reasoning.push_str(
                    "

",
                );
            }
        }
        "response.output_item.done" => {
            let item = event.get("item").unwrap_or(&serde_json::Value::Null);
            // Some backends deliver the summary only on the completed
            // reasoning item, with no summary-text deltas.
            if item.get("type").and_then(serde_json::Value::as_str) == Some("reasoning")
                && accum.reasoning.trim().is_empty()
            {
                let parts: Vec<&str> = item
                    .get("summary")
                    .and_then(serde_json::Value::as_array)
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
                            .collect()
                    })
                    .unwrap_or_default();
                accum.reasoning = parts.join(
                    "

",
                );
            }
            if item.get("type").and_then(serde_json::Value::as_str) == Some("function_call") {
                let call_id = item
                    .get("call_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                if let Some((name, arguments)) = accum.tool_calls.get_mut(call_id) {
                    if let Some(value) = item.get("name").and_then(serde_json::Value::as_str) {
                        *name = value.to_string();
                    }
                    if let Some(value) = item.get("arguments").and_then(serde_json::Value::as_str) {
                        *arguments = value.to_string();
                    }
                }
            }
        }
        "response.completed" | "response.done" | "response.incomplete" => {
            let response = event.get("response").unwrap_or(&serde_json::Value::Null);
            // `response.incomplete` carries the cause in
            // `incomplete_details.reason` (`max_output_tokens` for the output
            // limit); the bare status `incomplete` never names a limit, so it
            // must not be the only reason surfaced.
            let status = response.get("status").and_then(serde_json::Value::as_str);
            let incomplete_reason = response
                .get("incomplete_details")
                .and_then(|details| details.get("reason"))
                .and_then(serde_json::Value::as_str)
                .filter(|reason| !reason.trim().is_empty());
            accum.finish_reason =
                if status == Some("incomplete") || event_type == "response.incomplete" {
                    incomplete_reason.or(status).map(str::to_owned)
                } else {
                    status.map(str::to_owned)
                };
            if let Some(usage) = response.get("usage") {
                let as_u32 = |value: Option<&serde_json::Value>| {
                    value.and_then(serde_json::Value::as_u64).unwrap_or(0) as u32
                };
                // The Responses API reports `input_tokens_details`; the older
                // singular spelling is kept so a recorded stream still parses.
                let cached = as_u32(
                    usage
                        .get("input_tokens_details")
                        .or_else(|| usage.get("input_token_details"))
                        .and_then(|value| value.get("cached_tokens")),
                );
                // `input_tokens` INCLUDES cached tokens on the Responses API;
                // normalize to the exclusive convention (input + cache_read =
                // full prompt) so cost and context accounting do not double count.
                accum.prompt_tokens = as_u32(usage.get("input_tokens")).saturating_sub(cached);
                accum.cache_read_tokens = cached;
                accum.completion_tokens = as_u32(usage.get("output_tokens"));
                accum.reasoning_tokens = as_u32(
                    usage
                        .get("output_tokens_details")
                        .and_then(|value| value.get("reasoning_tokens")),
                );
            }
            accum.got_terminal = true;
        }
        "response.failed" | "error" => {
            // `error` carries a top-level `message`; `response.failed` nests it
            // under `response.error.message`.
            let message = event
                .get("message")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    event
                        .get("response")
                        .and_then(|r| r.get("error"))
                        .and_then(|e| e.get("message"))
                        .and_then(serde_json::Value::as_str)
                })
                .or_else(|| {
                    event
                        .get("error")
                        .and_then(|e| e.get("message"))
                        .and_then(serde_json::Value::as_str)
                })
                .unwrap_or("Codex response failed");
            return Err(ProviderError::Provider(message.to_string()));
        }
        _ => {}
    }
    Ok(())
}

fn codex_responses_event_stream(
    response: crate::agent_providers::http::ProviderHttpResponse,
) -> ProviderResponseStream {
    codex_responses_event_stream_with_deadline(
        response,
        std::time::Duration::from_secs(SEMANTIC_PROGRESS_SECS),
    )
}

fn codex_responses_event_stream_with_deadline(
    response: crate::agent_providers::http::ProviderHttpResponse,
    progress_limit: std::time::Duration,
) -> ProviderResponseStream {
    let output = async_stream::try_stream! {
        let mut bytes = response.bytes_stream();
        let mut last_progress = std::time::Instant::now();
        let mut progress_mark = 0usize;
        let mut decoder = rightkit_http::SseDecoder::eager();
        let mut accum = CodexStreamAccum::default();
        let mut emitted = std::collections::HashSet::new();
        let mut delivered_text_bytes = 0;
        let mut delivered_reasoning_bytes = 0;
        loop {
            let remaining = progress_limit.saturating_sub(last_progress.elapsed());
            let next = match tokio::time::timeout(remaining, bytes.next()).await {
                Ok(next) => next,
                Err(_elapsed) => { Err(semantic_progress_error(progress_limit))?; unreachable!() }
            };
            let Some(chunk) = next else { break };
            let chunk = chunk.map_err(|error| ProviderError::Network(error.to_string()))?;
            for frame in decoder.push(&chunk).map_err(|error| ProviderError::Sse(error.to_string()))? {
                let payload = frame.data.trim();
                if payload.is_empty() || payload == "[DONE]" { continue; }
                let event: serde_json::Value = serde_json::from_str(payload)
                    .map_err(|_| ProviderError::Sse("invalid Codex Responses SSE JSON".into()))?;
                let event_type = event.get("type").and_then(serde_json::Value::as_str).unwrap_or("");
                apply_codex_event(event.clone(), &mut accum)?;
                let mark = accum.text.len()
                    + accum.reasoning.len()
                    + accum.tool_calls.values().map(|(n, a)| n.len() + a.len() + 1).sum::<usize>();
                if mark != progress_mark || accum.got_terminal {
                    progress_mark = mark;
                    last_progress = std::time::Instant::now();
                }
                if accum.text.len() > delivered_text_bytes {
                    yield ProviderStreamEvent::Text(accum.text[delivered_text_bytes..].to_string());
                    delivered_text_bytes = accum.text.len();
                }
                if accum.reasoning.len() > delivered_reasoning_bytes {
                    yield ProviderStreamEvent::Reasoning(accum.reasoning[delivered_reasoning_bytes..].to_string());
                    delivered_reasoning_bytes = accum.reasoning.len();
                }
                match event_type {
                    "response.function_call_arguments.done" | "response.output_item.done" => {
                        let raw_id = event.get("item_id").and_then(serde_json::Value::as_str)
                            .or_else(|| event.get("item").and_then(|v| v.get("call_id")).and_then(serde_json::Value::as_str));
                        let call_id = raw_id.and_then(|id| accum.item_to_call.get(id).map(String::as_str).or(Some(id)));
                        if let Some(id) = call_id {
                            if let Some((name, arguments_json)) = accum.tool_calls.get(id) {
                                let call = ToolCall { id: id.to_string(), name: name.clone(), arguments_json: if arguments_json.trim().is_empty() { "{}".into() } else { arguments_json.clone() } };
                                validate_complete_tool_calls(std::slice::from_ref(&call))?;
                                // Held until the terminal event: a failing stream must not leak a call.
                            }
                        }
                    }
                    _ => {}
                }
                if accum.got_terminal {
                    let calls = accum.tool_calls.iter().map(|(id, (name, arguments_json))| ToolCall {
                        id: id.clone(), name: name.clone(), arguments_json: if arguments_json.trim().is_empty() { "{}".into() } else { arguments_json.clone() },
                    }).collect::<Vec<_>>();
                    validate_complete_tool_calls(&calls)?;
                    for call in calls {
                        if emitted.insert(call.id.clone()) { yield ProviderStreamEvent::ToolCall(call); }
                    }
                    yield ProviderStreamEvent::Usage { prompt_tokens: accum.prompt_tokens, completion_tokens: accum.completion_tokens, cache_read_tokens: accum.cache_read_tokens, cache_creation_tokens: 0 };
                    yield ProviderStreamEvent::UsageDetail { reasoning_tokens: accum.reasoning_tokens };
                    yield ProviderStreamEvent::Finished(accum.finish_reason.clone());
                    return;
                }
            }
            if last_progress.elapsed() >= progress_limit {
                Err(semantic_progress_error(progress_limit))?;
            }
        }
        if decoder.has_pending() {
            Err(ProviderError::Sse("stream ended with an incomplete SSE frame".into()))?;
        }
        Err(ProviderError::Sse("Codex Responses stream ended before completion".into()))?;
    };
    Box::pin(output)
}

/// Prepare one immutable wire body from loaded model metadata. Every send and
/// transport retry below consumes this object, preventing a second serializer
/// from drifting away from the measured/digested bytes.
fn prepare_actual_chat_request(
    client: &OpenAiCompatClient,
    body: &impl serde::Serialize,
    metadata: &ProviderRequestMetadata,
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
    let draft_body = serde_json::to_value(body).map_err(|error| {
        super::RequestAuthorityErrorV1::InvalidResponse {
            reason: error.to_string(),
        }
    })?;
    if client.request_authority.is_none() && (!continuation || client.direct_dispatch) {
        let modality = if draft_body.to_string().contains("image_url") {
            PreparedModalityV1::TextPlusInlineImages
        } else {
            PreparedModalityV1::TextOnly
        };
        let (mut prepared, measured) = prepare_and_admit_chat_request(
            body,
            "application/json",
            client.base_url.clone(),
            client.model_id.clone(),
            CHAT_REQUEST_SERIALIZER_GENERATION_V1,
            metadata.tool_catalog_generation,
            modality,
            profile,
        )?;
        prepared.measured_tokens = Some(measured);
        return Ok(prepared);
    }
    super::prepare_authorized_request(
        draft_body,
        profile,
        CHAT_REQUEST_SERIALIZER_GENERATION_V1,
        metadata.tool_catalog_generation,
        client.request_authority.as_ref(),
        continuation,
    )
}

/// The ChatGPT Codex backend routes prompt-cache affinity by the
/// `session_id` header together with `prompt_cache_key`; without the header a
/// follow-up turn can land on a cold replica and report zero cached input.
/// The value is the engine's stable prefix key (or its session fallback).
fn codex_cache_affinity_headers(metadata: &ProviderRequestMetadata) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    if let Some(key) = metadata.stable_prefix_key.as_deref() {
        // A value that is not a legal header value is dropped by the HTTP layer
        // at send time; control characters are filtered here.
        if !key.chars().any(char::is_control) {
            headers.push(("session_id".to_string(), key.to_string()));
        }
    }
    headers
}

/// Append every pair to the request.
fn with_headers(
    mut request: rightkit_http::Request,
    headers: Vec<(String, String)>,
) -> rightkit_http::Request {
    for (name, value) in headers {
        request = request.header(name, value);
    }
    request
}

#[cfg(test)]
pub(super) fn codex_cache_affinity_headers_for_test(
    metadata: &ProviderRequestMetadata,
) -> Vec<(String, String)> {
    codex_cache_affinity_headers(metadata)
}

impl OpenAiCompatClient {
    pub(super) fn request_builder(&self, url: &str) -> rightkit_http::Request {
        let mut request = rightkit_http::Request::post(url).header("Accept-Encoding", "identity");
        for (name, value) in &self.public_headers {
            request = request.header(name.as_str(), value.as_str());
        }
        if self.direct_transport {
            if let Some((name, value)) = &self.direct_auth_header {
                request = request.secret_header(name.as_str(), value.as_str());
            }
            request
        } else {
            request
                .bearer(&self.api_key)
                .header(
                    "HTTP-Referer",
                    "https://github.com/adrdsouza/rightkit-agent",
                )
                .header("X-Title", "Agent")
        }
    }

    /// Send one attempt of an already-built request on this client's transport.
    pub(super) async fn send_request(
        &self,
        request: rightkit_http::Request,
    ) -> Result<crate::agent_providers::http::ProviderHttpResponse, rightkit_http::HttpError> {
        crate::agent_providers::http::send(&self.http, request).await
    }

    /// Stream one chat completion turn (no tools, no caching).
    pub async fn complete(
        &self,
        wire_messages: Vec<serde_json::Value>,
    ) -> Result<CompletedTurn, ProviderError> {
        self.complete_with_tools(wire_messages, vec![]).await
    }

    /// Stream one chat completion turn with tool definitions and prompt caching.
    ///
    /// Adds `cache_control: ephemeral` to the system message and the last tool definition
    /// so Anthropic-compatible providers (OpenRouter → Claude) can cache those stable blocks.
    /// Some OpenAI-compatible providers reject vendor extensions, so unsupported routes skip it.
    pub async fn complete_with_tools(
        &self,
        wire_messages: Vec<serde_json::Value>,
        tools: Vec<serde_json::Value>,
    ) -> Result<CompletedTurn, ProviderError> {
        self.complete_with_tools_metadata(wire_messages, tools, &ProviderRequestMetadata::default())
            .await
    }

    /// Stream one chat completion turn with request-only metadata. Stable
    /// prefix keys are sent only to OpenAI's first-party endpoint because
    /// compatible providers are not guaranteed to accept that field.
    ///
    /// P2-4 (P0-C Lane 2, 2g): wraps the real request in a per-host circuit
    /// breaker so a provider already failing hard is not hammered mid-session
    /// — see `crate::agent_providers::provider_resilience`. The retry/backoff ladder inside
    /// `complete_with_tools_metadata_inner` is unchanged; this is an outer
    /// layer around it, not a replacement.
    pub async fn complete_with_tools_metadata(
        &self,
        wire_messages: Vec<serde_json::Value>,
        tools: Vec<serde_json::Value>,
        metadata: &ProviderRequestMetadata,
    ) -> Result<CompletedTurn, ProviderError> {
        if !breaker_allows_request(&self.base_url) {
            return Err(ProviderError::Provider(format!(
                "circuit breaker open for {}: provider has been failing repeatedly, refusing this request to avoid hammering it",
                self.base_url
            )));
        }
        let result = match self
            .complete_with_tools_metadata_inner(
                wire_messages.clone(),
                tools.clone(),
                metadata,
                false,
            )
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
                if !can_continue_incomplete(&partial) {
                    Err(incomplete_output_error(&partial, false, None))
                } else {
                    match self
                        .complete_with_tools_metadata_inner(
                            continuation_messages(wire_messages, &partial.text),
                            tools,
                            metadata,
                            true,
                        )
                        .await
                    {
                        Ok(continued) => Ok(merge_continuation(partial, continued)),
                        Err(ProviderError::IncompleteOutput { .. }) => {
                            Err(incomplete_output_error(&partial, true, None))
                        }
                        Err(error @ ProviderError::Authority(_)) => Err(error),
                        Err(error) => Err(incomplete_output_error(
                            &partial,
                            true,
                            Some(error.to_string()),
                        )),
                    }
                }
            }
            Err(error) => Err(error),
        };
        // Only transient, provider-side outcomes move the breaker (P2-4 /
        // P0-C 2g): a permanent per-request failure (auth, malformed
        // request) shouldn't trip a breaker meant to protect against a
        // *degraded provider* — retrying it wouldn't help, and a corrected
        // subsequent request to the same host shouldn't be refused because
        // of it. Matches xai-circuit-breaker's `BreakerConfig` intent (its
        // "server" preset scopes failures to 429/5xx; "client" preset scopes
        // to 401 specifically) — this crate counts the transient class
        // (429/5xx/network) as failures for either wire protocol.
        let counts_as_success =
            result.is_ok() || !is_breaker_relevant_failure(result.as_ref().err());
        breaker_record_outcome(&self.base_url, counts_as_success);
        result
    }

    /// Start a provider stream that emits complete tool calls as soon as their
    /// argument JSON is assembled. The completed-turn method above remains
    /// the compatibility path for callers that need one aggregate value.
    pub async fn stream_with_tools_metadata(
        &self,
        wire_messages: Vec<serde_json::Value>,
        tools: Vec<serde_json::Value>,
        metadata: &ProviderRequestMetadata,
    ) -> Result<ProviderResponseStream, ProviderError> {
        let mut first = self
            .stream_once(wire_messages.clone(), tools.clone(), metadata, false)
            .await?;
        let client = self.clone();
        let metadata = metadata.clone();
        // Text-only output-limit stops continue once (mirrors the non-stream
        // path); a truncated tool call is never replayed or continued.
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
            let truncated = reason.as_deref().is_some_and(is_incomplete_finish_reason);
            if truncated && !saw_tool_call && !text.trim().is_empty() {
                let mut second = client
                    .stream_once(
                        continuation_messages(wire_messages, &text),
                        tools,
                        &metadata,
                        true,
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
        wire_messages: Vec<serde_json::Value>,
        tools: Vec<serde_json::Value>,
        metadata: &ProviderRequestMetadata,
        continuation: bool,
    ) -> Result<ProviderResponseStream, ProviderError> {
        if self.is_codex_responses_route() {
            let body = self.build_codex_request_body(wire_messages, tools, metadata);
            let prepared = prepare_actual_chat_request(self, &body, metadata, continuation)?;
            let url = format!("{}/codex/responses", self.base_url().trim_end_matches('/'));
            let mut physical = crate::agent_providers::transport::PhysicalAttemptCounter::default();
            let physical_attempt = physical.next();
            let result = self
                .send_request(
                    with_headers(
                        self.request_builder(&url)
                            .header("Accept", "text/event-stream")
                            .header("Content-Type", prepared.content_type)
                            .header("OpenAI-Beta", "responses=experimental"),
                        codex_cache_affinity_headers(metadata),
                    )
                    .body(prepared.bytes.clone()),
                )
                .await;
            (self.submission_observer)(
                crate::agent_providers::transport::observe_submission_attempt(
                    &prepared,
                    prepared
                        .measured_tokens
                        .expect("prepared request admitted before send"),
                    physical_attempt,
                    result.as_ref(),
                ),
            );
            let response = result.map_err(|error| ProviderError::from_send_error(&error))?;
            if !response.is_success() {
                return Err(ProviderError::Http {
                    status: response.status(),
                    body: response.text().await.unwrap_or_default(),
                });
            }
            return Ok(codex_responses_event_stream(response));
        }
        let body = self.build_chat_request(wire_messages, tools, metadata);
        let prepared = prepare_actual_chat_request(self, &body, metadata, continuation)?;
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        if !breaker_allows_request(&self.base_url) {
            return Err(ProviderError::Provider(format!(
                "circuit breaker open for {}: provider has been failing repeatedly, refusing this request to avoid hammering it",
                self.base_url
            )));
        }
        let response = record_stream_establishment_outcome(
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
                &metadata.retry_notifier,
                &self.base_url,
            )
            .await,
        )?;
        let base_url = self.base_url.clone();
        let progress_limit = self
            .semantic_progress_limit
            .unwrap_or(std::time::Duration::from_secs(SEMANTIC_PROGRESS_SECS));
        let output = async_stream::try_stream! {
            let mut bytes = response.bytes_stream();
            // CR-13: byte-oriented decoder — never `from_utf8_lossy` a raw
            // chunk directly, which corrupts a multibyte codepoint split
            // across a chunk boundary.
            let mut decoder = rightkit_http::SseDecoder::eager();
            let mut reassembler = ToolCallChunkReassembler::new();
            let mut emitted_tool_call_ids = std::collections::HashSet::new();
            let mut partial = CompletedTurn::default();
            // Complete tool calls are held until a terminal signal
            // (finish_reason or `[DONE]`); an EOF after complete arguments
            // must not look like a finished turn.
            let mut pending_calls: Vec<ToolCall> = Vec::new();
            let mut finish_reason = None;
            let mut got_done = false;
            // Semantic-progress deadline: keepalives and comment lines never
            // reset it; only text, reasoning, tool-call deltas or a terminal
            // signal do.
            let mut last_progress = std::time::Instant::now();
            loop {
                let remaining = progress_limit.saturating_sub(last_progress.elapsed());
                let next = match tokio::time::timeout(remaining, bytes.next()).await {
                    Ok(next) => next,
                    Err(_elapsed) => { Err(semantic_progress_error(progress_limit))?; unreachable!() }
                };
                let Some(chunk) = next else { break };
                let chunk = chunk.map_err(|error| ProviderError::Network(error.to_string()))?;
                let frames = decoder.push(&chunk).map_err(|error| ProviderError::Sse(error.to_string()))?;
                for frame in frames {
                    let payload = frame.data.trim();
                    if payload == "[DONE]" {
                        got_done = true;
                        break;
                    }
                    let chunk: Chunk = serde_json::from_str(payload)
                        .map_err(|error| ProviderError::Sse(error.to_string()))?;
                    if let Some(usage) = chunk.usage {
                        let normalized = usage.normalize();
                        let prompt_tokens = normalized.input_tokens;
                        let completion_tokens = normalized.output_tokens;
                        let cache_read_tokens = normalized.cache_read_tokens;
                        let cache_creation_tokens = normalized.cache_creation_tokens;
                        yield ProviderStreamEvent::Usage {
                            prompt_tokens,
                            completion_tokens,
                            cache_read_tokens,
                            cache_creation_tokens,
                        };
                        yield ProviderStreamEvent::UsageDetail {
                            reasoning_tokens: normalized.reasoning_tokens,
                        };
                        partial.prompt_tokens = prompt_tokens;
                        partial.completion_tokens = completion_tokens;
                        partial.cache_read_tokens = cache_read_tokens;
                        partial.cache_creation_tokens = cache_creation_tokens;
                        partial.reasoning_tokens = normalized.reasoning_tokens;
                    }
                    let Some(choice) = chunk.choices.as_ref().and_then(|choices| choices.first()) else {
                        continue;
                    };
                    if let Some(reason) = &choice.finish_reason {
                        finish_reason = Some(reason.clone());
                        partial.finish_reason = Some(reason.clone());
                        last_progress = std::time::Instant::now();
                    }
                    let Some(delta) = &choice.delta else {
                        continue;
                    };
                    if delta.reasoning_content.as_deref().or(delta.reasoning.as_deref()).is_some_and(|t| !t.is_empty())
                        || delta.content.as_deref().is_some_and(|t| !t.is_empty())
                        || delta.tool_calls.is_some()
                    {
                        last_progress = std::time::Instant::now();
                    }
                    // A provider can place both channels in one choice delta.
                    // Keep the semantic Thought event ahead of its answer so
                    // transcript projections receive canonical reasoning first.
                    if let Some(reasoning_text) = delta.reasoning_content.as_deref()
                        .or(delta.reasoning.as_deref())
                        .filter(|text| !text.is_empty())
                    {
                        let partial_reasoning = partial.reasoning.get_or_insert_with(String::new);
                        partial_reasoning.push_str(reasoning_text);
                        yield ProviderStreamEvent::Reasoning(reasoning_text.to_string());
                    }
                    if let Some(text) = delta.content.as_deref().filter(|text| !text.is_empty()) {
                        partial.text.push_str(text);
                        yield ProviderStreamEvent::Text(text.to_string());
                    }
                    if let Some(tool_calls) = &delta.tool_calls {
                        for raw in tool_calls {
                            reassembler
                                .process_delta(
                                    raw.index as u64,
                                    raw.id.as_deref(),
                                    raw.function.as_ref().and_then(|f| f.name.as_deref()),
                                    raw.function
                                        .as_ref()
                                        .and_then(|f| f.arguments.as_deref())
                                        .unwrap_or(""),
                                )
                                .map_err(|error| ProviderError::Sse(error.to_string()))?;
                            for ready in reassembler.drain_ready() {
                                let call = ToolCall {
                                    id: ready.id,
                                    name: ready.name,
                                    arguments_json: ready.arguments,
                                };
                                validate_complete_tool_calls(std::slice::from_ref(&call))?;
                                if !emitted_tool_call_ids.insert(call.id.clone()) {
                                    Err(ProviderError::Sse(format!(
                                        "provider emitted duplicate tool call id `{}`",
                                        call.id
                                    )))?;
                                }
                                pending_calls.push(call);
                            }
                        }
                    }
                }
                if got_done {
                    break;
                }
            }
            if decoder.has_pending() {
                Err(ProviderError::Sse("stream ended with an incomplete SSE frame".into()))?;
            }
            // RightKit's end-of-stream drain releases whatever is still pending in
            // index order and fails on a call with no id/name or with arguments
            // that are not JSON.
            let leftover = reassembler
                .drain_completed()
                .map_err(|error| ProviderError::Sse(error.to_string()))?;
            for ready in leftover {
                let call = ToolCall {
                    id: ready.id,
                    name: ready.name,
                    arguments_json: ready.arguments,
                };
                validate_complete_tool_calls(std::slice::from_ref(&call))?;
                if !emitted_tool_call_ids.insert(call.id.clone()) {
                    Err(ProviderError::Sse(format!(
                        "provider emitted duplicate tool call id `{}`",
                        call.id
                    )))?;
                }
                pending_calls.push(call);
            }
            // Completion needs a finish_reason, or a `[DONE]` that closed a
            // non-empty generation (some compatible servers omit the
            // finish_reason). A bare `[DONE]` with no content is an empty,
            // unproven response, never a success.
            let produced_output = !partial.text.is_empty()
                || partial.reasoning.as_ref().is_some_and(|text| !text.is_empty())
                || !pending_calls.is_empty();
            if finish_reason.is_none() && !(got_done && produced_output) {
                Err(ProviderError::Sse("stream ended before completion".into()))?;
            }
            for call in pending_calls {
                partial.tool_calls.push(call.clone());
                yield ProviderStreamEvent::ToolCall(call);
            }
            // Preserve provider truncation on streaming seam. Engine owns
            // final completeness gate after adapter delivery.
            yield ProviderStreamEvent::Finished(finish_reason);
        };
        Ok(observe_stream_breaker_outcome(base_url, Box::pin(output)))
    }

    pub(super) async fn complete_with_tools_metadata_inner(
        &self,
        wire_messages: Vec<serde_json::Value>,
        tools: Vec<serde_json::Value>,
        metadata: &ProviderRequestMetadata,
        continuation: bool,
    ) -> Result<CompletedTurn, ProviderError> {
        if self.is_codex_responses_route() {
            return self
                .complete_codex_responses(wire_messages, tools, metadata)
                .await;
        }
        let body = self.build_chat_request(wire_messages, tools, metadata);
        let prepared = prepare_actual_chat_request(self, &body, metadata, continuation)?;

        // Replay policy (published `rightkit-http` attempt-delivery semantics): the chat
        // completion is a paid, non-idempotent POST. An HTTP answer, 429 and 5xx included, is
        // surfaced as typed and never re-sent here (a 429 carries its real `Retry-After`); only a
        // failure before the request left (`NotSent`: name lookup, TCP connect, TLS) is retried,
        // on the reconnect backoff (5s doubling, capped 60s). A transport error or timeout after
        // the POST was sent may already have been delivered and billed, so it is final.
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let max_connection_attempts = 4u32;
        let mut connection_attempt = 0u32;
        let mut physical = crate::agent_providers::transport::PhysicalAttemptCounter::default();
        let resp = loop {
            let physical_attempt = physical.next();
            let result = self
                .send_request(
                    self.request_builder(&url)
                        .header("Content-Type", prepared.content_type)
                        .body(prepared.bytes.clone()),
                )
                .await;
            (self.submission_observer)(
                crate::agent_providers::transport::observe_submission_attempt(
                    &prepared,
                    prepared
                        .measured_tokens
                        .expect("prepared request admitted before send"),
                    physical_attempt,
                    result.as_ref(),
                ),
            );
            match result {
                Ok(r) if r.is_success() => break r,
                Ok(r) => {
                    let code = r.status();
                    // S:M2/S:M3 amendment item D: a 429 becomes a typed `RateLimited` error
                    // (carrying the real `Retry-After` when the route sent one, else the default
                    // transient hint), not a bare `Http`, so `retry_after_hint` /
                    // `classify_provider_error` work without callers re-parsing status codes.
                    if code == 429 {
                        let retry_after_secs = retry_after_secs(&r).unwrap_or(2);
                        return Err(ProviderError::RateLimited { retry_after_secs });
                    }
                    let body = r.text().await.unwrap_or_default();
                    return Err(ProviderError::Http { status: code, body });
                }
                Err(e) => {
                    connection_attempt += 1;
                    if crate::agent_providers::client::is_connect_phase_failure(&e)
                        && connection_attempt < max_connection_attempts
                    {
                        crate::agent_providers::client::wait_connection_retry(
                            &metadata.retry_notifier,
                            &self.base_url,
                            connection_attempt,
                            max_connection_attempts - 1,
                        )
                        .await;
                        continue;
                    }
                    return Err(ProviderError::from_send_error(&e));
                }
            }
        };

        // S:M2/S:M3 amendment: capture rate-limit metadata off the initial
        // response headers before the body is consumed by the stream loop —
        // OpenAI-compatible routes that expose `x-ratelimit-*` report it
        // here even on a streaming completion.
        let rate_limit_metadata = parse_openai_rate_limit_headers(resp.headers());
        if !resp.is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Http { status, body });
        }

        const CHUNK_TIMEOUT_SECS: u64 = 45;

        // The request has been delivered: the provider is generating (and billing) the turn. It is
        // never re-POSTed for a stream that went idle or ended early; the outcome is surfaced as a
        // typed `DeliveryUncertain` (with whether any output arrived) and the caller decides.
        let stream_resp = resp;
        let mut accum = 'stream_retry: {
            let mut accum = StreamAccum::default();

            let mut bytes_stream = stream_resp.bytes_stream();
            // CR-13: byte-oriented decoder — `rightkit_http::SseDecoder` buffers raw bytes.
            let mut decoder = rightkit_http::SseDecoder::eager();
            let mut got_done = false;
            let mut last_semantic = std::time::Instant::now();
            let progress_limit = self
                .semantic_progress_limit
                .unwrap_or(std::time::Duration::from_secs(SEMANTIC_PROGRESS_SECS));

            loop {
                // Per-chunk timeout: if no data arrives within CHUNK_TIMEOUT_SECS,
                // the stream is silently dead (known failure mode with some providers).
                let chunk_opt = tokio::time::timeout(
                    std::time::Duration::from_secs(CHUNK_TIMEOUT_SECS),
                    bytes_stream.next(),
                )
                .await;

                let chunk_result = match chunk_opt {
                    Ok(Some(cr)) => cr,
                    Ok(None) => break, // stream closed normally
                    Err(_elapsed) => {
                        // Timeout: no data for CHUNK_TIMEOUT_SECS seconds. The request was
                        // delivered, so it is not resent.
                        return Err(ProviderError::DeliveryUncertain {
                            reason: format!("SSE stream idle for {CHUNK_TIMEOUT_SECS}s"),
                            delivered_output: accum.has_provider_output(),
                        });
                    }
                };

                let chunk = chunk_result.map_err(|e| ProviderError::Network(e.to_string()))?;
                let frames = decoder
                    .push(&chunk)
                    .map_err(|error| ProviderError::Sse(error.to_string()))?;

                for frame in frames {
                    let payload = frame.data.trim();
                    if payload == "[DONE]" {
                        got_done = true;
                        break;
                    }

                    let chunk: Chunk = match serde_json::from_str(payload) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };

                    if let Some(u) = chunk.usage {
                        let normalized = u.normalize();
                        accum.prompt_tokens = normalized.input_tokens;
                        accum.completion_tokens = normalized.output_tokens;
                        accum.cache_read_tokens = normalized.cache_read_tokens;
                        accum.cache_creation_tokens = normalized.cache_creation_tokens;
                        accum.reasoning_tokens = normalized.reasoning_tokens;
                    }

                    let choice = match chunk.choices.as_ref().and_then(|c| c.first()) {
                        Some(c) => c,
                        None => continue,
                    };
                    if let Some(fr) = &choice.finish_reason {
                        accum.finish_reason = Some(fr.clone());
                    }
                    let Some(delta) = &choice.delta else {
                        continue;
                    };

                    if let Some(t) = &delta.content {
                        if !t.is_empty() {
                            accum.text.push_str(t);
                            last_semantic = std::time::Instant::now();
                        }
                    }
                    let r = delta
                        .reasoning_content
                        .as_deref()
                        .or(delta.reasoning.as_deref());
                    if let Some(r) = r {
                        if !r.is_empty() {
                            accum.reasoning.push_str(r);
                            last_semantic = std::time::Instant::now();
                        }
                    }

                    // Feed tool-call deltas through the RightKit reassembler.
                    if let Some(tcs) = &delta.tool_calls {
                        last_semantic = std::time::Instant::now();
                        for raw_tc in tcs {
                            accum.saw_tool_delta = true;
                            accum
                                .reassembler
                                .process_delta(
                                    raw_tc.index as u64,
                                    raw_tc.id.as_deref(),
                                    raw_tc.function.as_ref().and_then(|f| f.name.as_deref()),
                                    raw_tc
                                        .function
                                        .as_ref()
                                        .and_then(|f| f.arguments.as_deref())
                                        .unwrap_or(""),
                                )
                                .map_err(|error| ProviderError::Sse(error.to_string()))?;
                        }
                        if choice.finish_reason.as_deref() == Some("tool_calls") {
                            accum.drain_reassembled()?;
                        }
                    }
                }

                if got_done {
                    break;
                }
                // Keepalive comments reset the chunk timer above but not this
                // one: only text, reasoning or tool deltas count as progress.
                if last_semantic.elapsed() >= progress_limit {
                    return Err(semantic_progress_error(progress_limit));
                }
            }

            // Clean finish: finish_reason set, or `[DONE]` that closed a
            // non-empty generation. A bare `[DONE]` is an empty, unproven
            // response, never a success.
            if accum.finish_reason.is_some() || (got_done && accum.has_provider_output()) {
                break 'stream_retry accum;
            }
            if got_done {
                return Err(ProviderError::Sse(
                    "stream ended before completion: bare [DONE] with no content".into(),
                ));
            }

            // The stream ended without a completed answer. The request was delivered, so it is never
            // resent: with or without partial output the outcome is uncertain, and partial output is
            // not replayed (an unknown prefix would risk duplicate tool effects).
            return Err(ProviderError::DeliveryUncertain {
                reason: "provider stream ended before completion".into(),
                delivered_output: accum.has_provider_output(),
            });
        };

        // If finish_reason triggered but reassembler wasn't drained via delta:
        accum.drain_reassembled()?;

        // Some models inline chain-of-thought as <think>...</think> in the
        // content rather than a separate reasoning field. Strip it out of the
        // user-visible text and fold it into the reasoning channel.
        let (text, inline_reasoning) = strip_think_tags(&accum.text);
        if let Some(extra) = inline_reasoning {
            if accum.reasoning.is_empty() {
                accum.reasoning = extra;
            } else {
                accum.reasoning.push('\n');
                accum.reasoning.push_str(&extra);
            }
        }

        let turn = CompletedTurn {
            text,
            reasoning: if accum.reasoning.is_empty() {
                None
            } else {
                Some(accum.reasoning)
            },
            tool_calls: accum.tool_calls,
            finish_reason: accum.finish_reason,
            prompt_tokens: accum.prompt_tokens,
            completion_tokens: accum.completion_tokens,
            cache_read_tokens: accum.cache_read_tokens,
            cache_creation_tokens: accum.cache_creation_tokens,
            reasoning_tokens: accum.reasoning_tokens,
            protocol_family: Some(ProtocolFamily::OpenAiCompatible),
            rate_limit: rate_limit_metadata,
        };

        validate_complete_tool_calls(&turn.tool_calls)?;

        if turn
            .finish_reason
            .as_deref()
            .is_some_and(is_incomplete_finish_reason)
        {
            return Err(incomplete_output_error(&turn, false, None));
        }

        // Record redacted, versioned corpus when live capture is enabled.
        // Guarded by `RIGHTKIT_RECORD_REPLAY=1` so normal operation never
        // touches the filesystem. The corpus is written via
        // `crate::agent_providers::replay::maybe_record_turn` which redacts secrets and stamps
        // `version` + `recorded_at` + `redacted:true`.
        if crate::agent_providers::replay::is_record_mode() {
            // Use model_id as scenario hint plus epoch to avoid collisions.
            let epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let scenario = format!("live_{}_{epoch}", self.model_id.replace('/', "_"));
            let _ = crate::agent_providers::replay::maybe_record_turn(&scenario, &turn);
        }

        Ok(turn)
    }

    /// ChatGPT/Codex OAuth transport. This route is deliberately separate
    /// from chat-completions: Pi's contract is POST `/codex/responses` with
    /// Responses-shaped input and Responses SSE events.
    async fn complete_codex_responses(
        &self,
        wire_messages: Vec<serde_json::Value>,
        tools: Vec<serde_json::Value>,
        metadata: &ProviderRequestMetadata,
    ) -> Result<CompletedTurn, ProviderError> {
        let body = self.build_codex_request_body(wire_messages, tools, metadata);
        let prepared = prepare_actual_chat_request(self, &body, metadata, false)?;
        let url = format!("{}/codex/responses", self.base_url().trim_end_matches('/'));
        // MBR-008: this route must emit the same submission observation as the
        // chat/Anthropic ladders, or an authorized federated request leaves no
        // provider-boundary evidence row.
        let mut physical = crate::agent_providers::transport::PhysicalAttemptCounter::default();
        let physical_attempt = physical.next();
        let result = self
            .send_request(
                with_headers(
                    self.request_builder(&url)
                        .header("Accept", "text/event-stream")
                        .header("Content-Type", prepared.content_type)
                        .header("OpenAI-Beta", "responses=experimental"),
                    codex_cache_affinity_headers(metadata),
                )
                .body(prepared.bytes.clone()),
            )
            .await;
        (self.submission_observer)(
            crate::agent_providers::transport::observe_submission_attempt(
                &prepared,
                prepared
                    .measured_tokens
                    .expect("prepared request admitted before send"),
                physical_attempt,
                result.as_ref(),
            ),
        );
        let response = result.map_err(|error| ProviderError::from_send_error(&error))?;
        if !response.is_success() {
            return Err(ProviderError::Http {
                status: response.status(),
                body: response.text().await.unwrap_or_default(),
            });
        }
        let rate_limit_metadata = parse_openai_rate_limit_headers(response.headers());
        let mut bytes_stream = response.bytes_stream();
        let mut decoder = rightkit_http::SseDecoder::eager();
        let mut accum = CodexStreamAccum::default();
        while let Some(chunk) = bytes_stream.next().await {
            let chunk = chunk.map_err(|error| ProviderError::Network(error.to_string()))?;
            for frame in decoder
                .push(&chunk)
                .map_err(|error| ProviderError::Sse(error.to_string()))?
            {
                let payload = frame.data.trim();
                if payload.is_empty() || payload == "[DONE]" {
                    continue;
                }
                let event: serde_json::Value = serde_json::from_str(payload)
                    .map_err(|_| ProviderError::Sse("invalid Codex Responses SSE JSON".into()))?;
                apply_codex_event(event, &mut accum)?;
            }
            if accum.got_terminal {
                break;
            }
        }
        if !accum.got_terminal {
            return Err(ProviderError::Sse(
                "Codex Responses stream ended before completion".into(),
            ));
        }
        let tool_calls = accum
            .tool_calls
            .into_iter()
            .map(|(id, (name, arguments_json))| ToolCall {
                id,
                name,
                arguments_json: if arguments_json.trim().is_empty() {
                    "{}".into()
                } else {
                    arguments_json
                },
            })
            .collect::<Vec<_>>();
        let (text, inline_reasoning) = strip_think_tags(&accum.text);
        let mut reasoning = accum.reasoning;
        if let Some(inline) = inline_reasoning {
            if reasoning.is_empty() {
                reasoning = inline;
            } else {
                reasoning.push('\n');
                reasoning.push_str(&inline);
            }
        }
        let turn = CompletedTurn {
            text,
            reasoning: (!reasoning.is_empty()).then_some(reasoning),
            tool_calls,
            finish_reason: accum.finish_reason,
            prompt_tokens: accum.prompt_tokens,
            completion_tokens: accum.completion_tokens,
            cache_read_tokens: accum.cache_read_tokens,
            cache_creation_tokens: 0,
            reasoning_tokens: accum.reasoning_tokens,
            protocol_family: Some(ProtocolFamily::OpenAiCodexResponses),
            rate_limit: rate_limit_metadata,
        };
        validate_complete_tool_calls(&turn.tool_calls)?;
        if turn
            .finish_reason
            .as_deref()
            .is_some_and(is_incomplete_finish_reason)
        {
            return Err(incomplete_output_error(&turn, false, None));
        }
        Ok(turn)
    }
}

#[cfg(test)]
mod parser_effect_tests {
    use super::*;
    use crate::agent_providers::ProviderStreamEvent;
    use futures::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn codex_responses_events_accumulate_text_tool_usage_without_credentials() {
        let mut accum = CodexStreamAccum::default();
        apply_codex_event(
            serde_json::json!({"type":"response.output_text.delta","delta":"hello"}),
            &mut accum,
        )
        .unwrap();
        apply_codex_event(
            serde_json::json!({"type":"response.output_item.added","item":{"type":"function_call","id":"fc-item-1","call_id":"call-1","name":"read_file"}}),
            &mut accum,
        )
        .unwrap();
        apply_codex_event(
            serde_json::json!({"type":"response.function_call_arguments.delta","item_id":"fc-item-1","delta":"{}"}),
            &mut accum,
        )
        .unwrap();
        apply_codex_event(
            serde_json::json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":4,"output_tokens":2,"input_token_details":{"cached_tokens":1}}}}),
            &mut accum,
        )
        .unwrap();
        assert_eq!(accum.text, "hello");
        assert_eq!(accum.tool_calls["call-1"].0, "read_file");
        assert_eq!(accum.tool_calls["call-1"].1, "{}");
        assert_eq!(
            (
                accum.prompt_tokens,
                accum.completion_tokens,
                accum.cache_read_tokens
            ),
            (3, 2, 1)
        );
        assert!(accum.got_terminal);
    }

    #[test]
    fn responses_usage_normalizes_inclusive_input_and_reads_reasoning() {
        // Realistic Responses API `response.completed` usage block.
        let mut accum = CodexStreamAccum::default();
        apply_codex_event(
            serde_json::json!({"type":"response.completed","response":{"status":"completed","usage":{
                "input_tokens":2006,"input_tokens_details":{"cached_tokens":1920},
                "output_tokens":300,"output_tokens_details":{"reasoning_tokens":192},
                "total_tokens":2306}}}),
            &mut accum,
        )
        .unwrap();
        assert_eq!(accum.prompt_tokens, 86, "uncached input only");
        assert_eq!(accum.cache_read_tokens, 1920);
        assert_eq!(accum.completion_tokens, 300);
        assert_eq!(accum.reasoning_tokens, 192);
    }

    #[test]
    fn chat_completions_usage_fixture_normalizes_cached_and_reasoning() {
        let usage: crate::agent_providers::client::wire::UsageSummary = serde_json::from_str(
            r#"{"prompt_tokens":2006,"completion_tokens":300,"total_tokens":2306,
                "prompt_tokens_details":{"cached_tokens":1920,"audio_tokens":0},
                "completion_tokens_details":{"reasoning_tokens":192,"audio_tokens":0,
                  "accepted_prediction_tokens":0,"rejected_prediction_tokens":0}}"#,
        )
        .unwrap();
        let n = usage.normalize();
        assert_eq!(
            (n.input_tokens, n.cache_read_tokens, n.cache_creation_tokens),
            (86, 1920, 0)
        );
        assert_eq!((n.output_tokens, n.reasoning_tokens), (300, 192));
    }

    #[test]
    fn anthropic_style_gateway_usage_passes_through_unchanged() {
        let usage: crate::agent_providers::client::wire::UsageSummary = serde_json::from_str(
            r#"{"prompt_tokens":12,"completion_tokens":40,
                "cache_read_input_tokens":1000,"cache_creation_input_tokens":200}"#,
        )
        .unwrap();
        let n = usage.normalize();
        assert_eq!(
            (n.input_tokens, n.cache_read_tokens, n.cache_creation_tokens),
            (12, 1000, 200)
        );
    }

    #[test]
    fn openrouter_cache_write_is_split_out_of_input() {
        let usage: crate::agent_providers::client::wire::UsageSummary = serde_json::from_str(
            r#"{"prompt_tokens":1500,"completion_tokens":10,
                "prompt_tokens_details":{"cached_tokens":1000,"cache_write_tokens":300}}"#,
        )
        .unwrap();
        let n = usage.normalize();
        assert_eq!(
            (n.input_tokens, n.cache_read_tokens, n.cache_creation_tokens),
            (200, 1000, 300)
        );
    }

    #[test]
    fn responses_usage_reports_cached_prompt_size_under_the_documented_key() {
        // The Responses API spells it `input_tokens_details`.
        let mut accum = CodexStreamAccum::default();
        apply_codex_event(
            serde_json::json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":4638,"output_tokens":6,"input_tokens_details":{"cached_tokens":4096}}}}),
            &mut accum,
        )
        .unwrap();
        // Exclusive convention: input + cache_read is the full 4638-token prompt.
        assert_eq!((accum.prompt_tokens, accum.cache_read_tokens), (542, 4096));
    }

    #[test]
    fn prepared_admission_uses_exact_serializer_bytes_and_never_a_byte_quota() {
        let body = serde_json::json!({"model":"moonshotai/kimi-k2.6","messages":[{"role":"user","content":"héllo"}]});
        let profile = CapacityProfileV1::new(8_192, 1, "moonshotai/kimi-k2.6", "route", None);
        let (prepared, measured) = prepare_and_admit_chat_request(
            &body,
            "application/json",
            "route",
            "moonshotai/kimi-k2.6",
            CHAT_REQUEST_SERIALIZER_GENERATION_V1,
            None,
            PreparedModalityV1::TextOnly,
            profile,
        )
        .expect("catalog-sized body admits");
        assert_eq!(prepared.bytes, serde_json::to_vec(&body).unwrap());
        assert_eq!(prepared.content_type, "application/json");
        assert_eq!(
            prepared.body_digest,
            rightkit_fs::digest::sha256_bytes(&prepared.bytes)
        );
        assert_eq!(
            measured,
            super::super::capacity::measure_o200k_base(&String::from_utf8_lossy(&prepared.bytes))
        );
        assert_ne!(
            measured,
            (prepared.bytes.len() / 4) as u64,
            "negative control: bytes/4 is not admission"
        );
        let retry = prepared.clone();
        assert_eq!(
            retry.body_digest, prepared.body_digest,
            "identical retry retains identity"
        );
        assert_eq!(
            retry.bytes, prepared.bytes,
            "identical retry retains sent bytes"
        );
    }

    #[test]
    fn over_budget_and_changed_continuation_fail_before_transport() {
        let body = serde_json::json!({"messages":[{"role":"user","content":"a deliberately nonempty prompt"}]});
        let profile = CapacityProfileV1::new(1, 0, "model", "route", None);
        let (prepared, _) = prepare_and_admit_chat_request(
            &body,
            "application/json",
            "route",
            "model",
            CHAT_REQUEST_SERIALIZER_GENERATION_V1,
            None,
            PreparedModalityV1::TextOnly,
            CapacityProfileV1::new(8_192, 0, "model", "route", None),
        )
        .unwrap();
        let gate = PreparedRequestGateV1::new(profile);
        assert!(matches!(
            gate.admit(&prepared),
            Err(CapacityError::FinalInsertionOverBudget { .. })
        ));
        assert!(matches!(
            gate.admit_continuation(&prepared, None),
            Err(CapacityError::ContinuationReauthorizationRequired)
        ));
    }

    async fn serve_sse(body: String) -> String {
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

    /// Serves `body` to every connection that arrives within a window and counts them: a resent
    /// request would show up as a second connection.
    async fn serve_sse_counting(body: String) -> (String, tokio::task::JoinHandle<usize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let served = tokio::spawn(async move {
            let mut count = 0usize;
            while let Ok(Ok((mut socket, _))) =
                tokio::time::timeout(std::time::Duration::from_millis(1500), listener.accept())
                    .await
            {
                count += 1;
                let mut request = vec![0u8; 16 * 1024];
                let _ = socket.read(&mut request).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
            count
        });
        (endpoint, served)
    }

    #[tokio::test]
    async fn a_stream_that_ends_after_output_is_uncertain_and_never_resent() {
        // One text chunk, then the connection closes with no finish_reason and no [DONE].
        let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n"
            .to_string();
        let (endpoint, served) = serve_sse_counting(body).await;
        let client = OpenAiCompatClient::new("test-key", "test-model", endpoint);
        let error = client
            .complete_with_tools_metadata(vec![], vec![], &Default::default())
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                ProviderError::DeliveryUncertain {
                    delivered_output: true,
                    ..
                }
            ),
            "{error:?}"
        );
        assert_eq!(
            served.await.unwrap(),
            1,
            "the delivered request must not be resent"
        );
    }

    #[tokio::test]
    async fn a_stream_that_ends_with_no_output_is_uncertain_and_never_resent() {
        let (endpoint, served) = serve_sse_counting(String::new()).await;
        let client = OpenAiCompatClient::new("test-key", "test-model", endpoint);
        let error = client
            .complete_with_tools_metadata(vec![], vec![], &Default::default())
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                ProviderError::DeliveryUncertain {
                    delivered_output: false,
                    ..
                }
            ),
            "{error:?}"
        );
        assert_eq!(
            served.await.unwrap(),
            1,
            "an empty stream is not a license to resend"
        );
    }

    async fn serve_sse_capturing_request(
        body: String,
    ) -> (String, tokio::sync::oneshot::Receiver<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (captured_tx, captured_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                let Some(headers_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..headers_end + 4]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    })
                    .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if request.len() >= headers_end + 4 + content_length {
                    break;
                }
            }
            let _ = captured_tx.send(request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        (endpoint, captured_rx)
    }

    async fn codex_response(endpoint: &str) -> crate::agent_providers::http::ProviderHttpResponse {
        let http = crate::agent_providers::http::provider_client(
            endpoint,
            crate::agent_providers::http::Route::Ambient,
        );
        crate::agent_providers::http::send(&http, rightkit_http::Request::get(endpoint))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn codex_native_stream_yields_before_terminal_and_drop_closes_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (closed_tx, closed_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request).await;
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"first\"}\n\n").await.unwrap();
            let mut byte = [0_u8; 1];
            let _ = socket.read(&mut byte).await;
            let _ = closed_tx.send(());
        });
        let response = codex_response(&endpoint).await;
        let mut stream = codex_responses_event_stream(response);
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
                .await
                .expect("text delta must arrive before terminal event")
                .unwrap()
                .unwrap(),
            ProviderStreamEvent::Text(text) if text == "first"
        ));
        drop(stream);
        tokio::time::timeout(std::time::Duration::from_secs(2), closed_rx)
            .await
            .expect("drop should promptly release HTTP connection")
            .unwrap();
    }

    #[tokio::test]
    async fn codex_native_stream_emits_validated_tool_once_and_terminal_events() {
        let body = concat!(
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"item-1\",\"call_id\":\"call-1\",\"name\":\"read_file\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item-1\",\"delta\":\"{}\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item-1\",\"arguments\":\"{}\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"item-1\",\"call_id\":\"call-1\",\"name\":\"read_file\",\"arguments\":\"{}\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":4,\"output_tokens\":2,\"input_token_details\":{\"cached_tokens\":1}}}}\n\n"
        );
        let endpoint = serve_sse(body.into()).await;
        let response = codex_response(&endpoint).await;
        let mut stream = codex_responses_event_stream(response);
        let mut calls = 0;
        let mut usage = None;
        let mut finished = false;
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                ProviderStreamEvent::ToolCall(call) => {
                    calls += 1;
                    assert_eq!(call.name, "read_file");
                }
                ProviderStreamEvent::Usage {
                    prompt_tokens,
                    completion_tokens,
                    cache_read_tokens,
                    ..
                } => usage = Some((prompt_tokens, completion_tokens, cache_read_tokens)),
                ProviderStreamEvent::Finished(Some(reason)) => {
                    assert_eq!(reason, "completed");
                    finished = true;
                }
                _ => {}
            }
        }
        assert_eq!(calls, 1);
        assert_eq!(usage, Some((3, 2, 1)));
        assert!(finished);
    }

    #[tokio::test]
    async fn codex_native_stream_rejects_truncated_sse_and_malformed_function_arguments() {
        let endpoint =
            serve_sse("data: {\"type\":\"response.output_text.delta\",\"delta\":\"part".into())
                .await;
        let mut stream = codex_responses_event_stream(codex_response(&endpoint).await);
        let mut saw_error = false;
        while let Some(event) = stream.next().await {
            if matches!(event, Err(ProviderError::Sse(_))) {
                saw_error = true;
            }
        }
        assert!(saw_error);

        let body = concat!(
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"item-1\",\"call_id\":\"call-1\",\"name\":\"read_file\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item-1\",\"delta\":\"{bad\"}\n\n",
            "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item-1\",\"arguments\":\"{bad\"}\n\n"
        );
        let endpoint = serve_sse(body.into()).await;
        let mut stream = codex_responses_event_stream(codex_response(&endpoint).await);
        assert!(matches!(stream.next().await, Some(Err(_))));
    }

    async fn codex_collect(body: &str) -> Vec<Result<ProviderStreamEvent, ProviderError>> {
        let endpoint = serve_sse(body.into()).await;
        let mut stream = codex_responses_event_stream(codex_response(&endpoint).await);
        let mut out = Vec::new();
        while let Some(event) = stream.next().await {
            out.push(event);
        }
        out
    }

    #[tokio::test]
    async fn codex_native_bare_done_terminator_is_a_typed_error_not_completion() {
        let events = codex_collect("data: [DONE]\n\n").await;
        assert!(matches!(events.last(), Some(Err(ProviderError::Sse(_)))));
        assert!(!events
            .iter()
            .any(|e| matches!(e, Ok(ProviderStreamEvent::Finished(_)))));
    }

    #[tokio::test]
    async fn codex_native_clean_eof_after_text_without_terminal_is_an_error() {
        let events =
            codex_collect("data: {\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}\n\n")
                .await;
        assert!(matches!(events.first(), Some(Ok(ProviderStreamEvent::Text(t))) if t == "hi"));
        assert!(matches!(events.last(), Some(Err(ProviderError::Sse(_)))));
    }

    #[tokio::test]
    async fn codex_native_eof_with_partial_tool_arguments_never_emits_the_call() {
        let body = concat!(
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"i1\",\"call_id\":\"c1\",\"name\":\"read_file\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"i1\",\"delta\":\"{\\\"path\\\":\\\"a\"}\n\n"
        );
        let events = codex_collect(body).await;
        assert!(!events
            .iter()
            .any(|e| matches!(e, Ok(ProviderStreamEvent::ToolCall(_)))));
        assert!(matches!(events.last(), Some(Err(_))));
    }

    #[tokio::test]
    async fn codex_native_terminal_with_unfinished_partial_arguments_is_rejected() {
        let body = concat!(
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"i1\",\"call_id\":\"c1\",\"name\":\"read_file\"}}\n\n",
            "data: {\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"i1\",\"delta\":\"{\\\"path\\\":\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{}}}\n\n"
        );
        let events = codex_collect(body).await;
        assert!(!events.iter().any(|e| matches!(
            e,
            Ok(ProviderStreamEvent::ToolCall(_) | ProviderStreamEvent::Finished(_))
        )));
        assert!(matches!(events.last(), Some(Err(_))));
    }

    #[tokio::test]
    async fn codex_native_response_failed_surfaces_nested_error_message() {
        let events = codex_collect(
            "data: {\"type\":\"response.failed\",\"response\":{\"status\":\"failed\",\"error\":{\"code\":\"server_error\",\"message\":\"upstream exploded\"}}}\n\n",
        )
        .await;
        assert!(matches!(
            events.last(),
            Some(Err(ProviderError::Provider(m))) if m == "upstream exploded"
        ));
    }

    #[tokio::test]
    async fn codex_native_error_event_surfaces_top_level_message() {
        let events =
            codex_collect("data: {\"type\":\"error\",\"message\":\"rate limited\"}\n\n").await;
        assert!(matches!(
            events.last(),
            Some(Err(ProviderError::Provider(m))) if m == "rate limited"
        ));
    }

    #[tokio::test]
    async fn codex_native_tool_call_is_held_until_terminal_and_never_leaks_on_failure() {
        let body = concat!(
            "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"i1\",\"call_id\":\"c1\",\"name\":\"read_file\"}}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"id\":\"i1\",\"call_id\":\"c1\",\"name\":\"read_file\",\"arguments\":\"{}\"}}\n\n",
            "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"late failure\"}}}\n\n"
        );
        let events = codex_collect(body).await;
        assert!(!events
            .iter()
            .any(|e| matches!(e, Ok(ProviderStreamEvent::ToolCall(_)))));
        assert!(matches!(
            events.last(),
            Some(Err(ProviderError::Provider(_)))
        ));
    }

    #[tokio::test]
    async fn codex_native_keepalive_only_stream_hits_semantic_progress_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request).await;
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n")
                .await;
            loop {
                if socket.write_all(b": keepalive\n\n").await.is_err() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        });
        let response = codex_response(&endpoint).await;
        let mut stream = codex_responses_event_stream_with_deadline(
            response,
            std::time::Duration::from_millis(300),
        );
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("deadline must fire despite keepalives");
        assert!(
            matches!(result, Some(Err(ProviderError::Network(m))) if m.contains("semantic progress"))
        );
    }

    #[tokio::test]
    async fn codex_native_stream_preserves_incomplete_terminal_status() {
        let endpoint = serve_sse(
            "data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"usage\":{}}}\n\n".into(),
        )
        .await;
        let mut stream = codex_responses_event_stream(codex_response(&endpoint).await);
        let mut finished = None;
        while let Some(event) = stream.next().await {
            if let ProviderStreamEvent::Finished(reason) = event.unwrap() {
                finished = reason;
            }
        }
        assert_eq!(finished.as_deref(), Some("incomplete"));
    }

    #[tokio::test]
    async fn codex_incomplete_max_output_tokens_maps_to_output_limit_reason() {
        let body = sse(&[
            CX_TEXT_PARTIAL,
            r#"{"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{}}}"#,
        ]);
        let outcome = fixture_outcome(codex_fixture(body).await).await;
        assert!(
            outcome.error.is_none(),
            "unexpected error: {:?}",
            outcome.error
        );
        assert_eq!(outcome.text, "partial");
        let reason = outcome.finished.clone().flatten().expect("finished reason");
        assert_eq!(reason, "max_output_tokens");
        assert!(is_incomplete_finish_reason(&reason));
        // A non-limit incomplete cause (content filter) is not a limit stop.
        let filtered = sse(&[
            r#"{"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"content_filter"},"usage":{}}}"#,
        ]);
        let outcome = fixture_outcome(codex_fixture(filtered).await).await;
        let reason = outcome.finished.flatten().expect("finished reason");
        assert!(!is_incomplete_finish_reason(&reason));
    }

    #[tokio::test]
    async fn codex_output_limit_with_cut_tool_arguments_never_surfaces_a_call() {
        let outcome = fixture_outcome(
            codex_fixture(sse(&[
                CX_ITEM_ADDED,
                CX_ARGS_TRUNCATED,
                r#"{"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{}}}"#,
            ]))
            .await,
        )
        .await;
        assert!(
            outcome.calls.is_empty(),
            "cut-off tool call must not surface"
        );
        assert!(
            outcome.error.is_some(),
            "cut-off tool call must fail, not finish"
        );
    }

    /// Serves one canned SSE body per accepted connection, in order, and
    /// records each request body.
    async fn serve_sse_sequence(
        bodies: Vec<String>,
    ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = seen.clone();
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
                let end = request
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .unwrap_or(0)
                    + 4;
                captured
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&request[end..]).into_owned());
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (endpoint, seen)
    }

    #[tokio::test]
    async fn openai_stream_text_output_limit_continues_once_keeping_partial() {
        let first = sse(&[
            r#"{"choices":[{"delta":{"content":"part"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#,
            "[DONE]",
        ]);
        let second = sse(&[
            r#"{"choices":[{"delta":{"content":"ial done"},"finish_reason":null}]}"#,
            r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]);
        let (endpoint, seen) = serve_sse_sequence(vec![first, second]).await;
        let client = OpenAiCompatClient::new("test-key", "moonshotai/kimi-k2.6", endpoint)
            .with_direct_dispatch();
        let outcome = fixture_outcome(
            client
                .stream_with_tools_metadata(vec![], vec![], &Default::default())
                .await
                .unwrap(),
        )
        .await;
        assert!(
            outcome.error.is_none(),
            "unexpected error: {:?}",
            outcome.error
        );
        assert_eq!(outcome.text, "partial done");
        assert_eq!(outcome.finished, Some(Some("stop".to_string())));
        let requests = seen.lock().unwrap();
        assert_eq!(requests.len(), 2, "exactly one continuation request");
        assert!(
            requests[1].contains("part"),
            "continuation carries the partial"
        );
    }

    #[tokio::test]
    async fn openai_stream_repeated_output_limit_is_bounded_to_one_continuation() {
        let limited = |text: &str| {
            sse(&[
                &format!(
                    r#"{{"choices":[{{"delta":{{"content":"{text}"}},"finish_reason":null}}]}}"#
                ),
                r#"{"choices":[{"delta":{},"finish_reason":"length"}]}"#,
                "[DONE]",
            ])
        };
        let (endpoint, seen) = serve_sse_sequence(vec![limited("a"), limited("b")]).await;
        let client = OpenAiCompatClient::new("test-key", "moonshotai/kimi-k2.6", endpoint)
            .with_direct_dispatch();
        let outcome = fixture_outcome(
            client
                .stream_with_tools_metadata(vec![], vec![], &Default::default())
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(outcome.text, "ab");
        assert_eq!(outcome.finished, Some(Some("length".to_string())));
        assert_eq!(seen.lock().unwrap().len(), 2, "no third request");
    }

    #[tokio::test]
    async fn openai_stream_output_limit_with_tool_call_does_not_continue() {
        let (endpoint, seen) = serve_sse_sequence(vec![openai_tool_stream("length")]).await;
        let client = OpenAiCompatClient::new("test-key", "moonshotai/kimi-k2.6", endpoint);
        let outcome = fixture_outcome(
            client
                .stream_with_tools_metadata(vec![], vec![], &Default::default())
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(outcome.finished, Some(Some("length".to_string())));
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "tool-bearing turn is never continued"
        );
    }

    #[tokio::test]
    async fn openai_chat_keepalive_only_stream_hits_semantic_progress_deadline() {
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
                if socket.write_all(b": keepalive\n\n").await.is_err() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        });
        let client = OpenAiCompatClient::new("test-key", "moonshotai/kimi-k2.6", endpoint)
            .with_semantic_progress_limit(std::time::Duration::from_millis(300));
        let mut stream = client
            .stream_with_tools_metadata(
                vec![serde_json::json!({"role":"user","content":"hi"})],
                vec![],
                &ProviderRequestMetadata::default(),
            )
            .await
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), stream.next())
            .await
            .expect("deadline must fire despite keepalives");
        assert!(
            matches!(result, Some(Err(ProviderError::Network(m))) if m.contains("semantic progress"))
        );
    }

    #[tokio::test]
    async fn stream_sends_the_admitted_serialized_bytes_and_content_type() {
        let sse =
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
                .to_string();
        let (endpoint, captured) = serve_sse_capturing_request(sse).await;
        let client = OpenAiCompatClient::new("test-key", "moonshotai/kimi-k2.6", endpoint);
        let messages = vec![serde_json::json!({"role":"user","content":"exact bytes"})];
        let expected = serde_json::to_vec(&client.build_chat_request(
            messages.clone(),
            vec![],
            &ProviderRequestMetadata::default(),
        ))
        .unwrap();
        let mut stream = client
            .stream_with_tools_metadata(messages, vec![], &ProviderRequestMetadata::default())
            .await
            .unwrap();
        while stream.next().await.is_some() {}
        let request = captured.await.unwrap();
        let headers_end = request
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap();
        let headers = String::from_utf8_lossy(&request[..headers_end]);
        assert!(headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case("content-type: application/json")));
        assert_eq!(&request[headers_end + 4..], expected.as_slice());
    }

    #[tokio::test]
    async fn federated_stream_sends_only_authority_body_with_matching_observation() {
        use crate::agent_providers::client::{
            ProviderRequestIdentityV1, RequestAuthorityBindingV1, RequestAuthorityErrorV1,
            RequestAuthorityInputV1, RequestAuthorityResponseV1, RequestAuthorityV1,
        };
        struct Authority;
        impl RequestAuthorityV1 for Authority {
            fn authorize(
                &self,
                input: RequestAuthorityInputV1,
            ) -> Result<RequestAuthorityResponseV1, RequestAuthorityErrorV1> {
                assert!(input.capacity_profile.window_tokens > 0);
                assert!(input.capacity_profile.catalog_revision.is_some());
                let mut final_body = input.draft_body;
                final_body["messages"] =
                    serde_json::json!([{"role":"user","content":"federated final héllo"}]);
                Ok(RequestAuthorityResponseV1 {
                    authorization_id: "opaque-openai".into(),
                    final_body,
                })
            }
        }
        let sse =
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
                .to_string();
        let (endpoint, captured) = serve_sse_capturing_request(sse).await;
        let observations = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = observations.clone();
        let identity = ProviderRequestIdentityV1 {
            session_id: "session".into(),
            task_id: "task".into(),
            request_id: "request".into(),
            generation: 8,
        };
        let client = OpenAiCompatClient::new("test-key", "moonshotai/kimi-k2.6", endpoint)
            .with_request_authority(RequestAuthorityBindingV1::new(
                identity.clone(),
                std::sync::Arc::new(Authority),
            ))
            .with_submission_observer(std::sync::Arc::new(move |observation| {
                sink.lock().unwrap().push(observation)
            }));
        let mut stream = client
            .stream_with_tools_metadata(
                vec![serde_json::json!({"role":"user","content":"draft must be replaced"})],
                vec![],
                &ProviderRequestMetadata::default(),
            )
            .await
            .unwrap();
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        let request = captured.await.unwrap();
        let body_start = request
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .unwrap()
            + 4;
        let observations = observations.lock().unwrap();
        assert_eq!(observations.len(), 1);
        let observation = &observations[0];
        assert_eq!(observation.request_identity.as_ref(), Some(&identity));
        assert_eq!(
            observation.authorization_id.as_deref(),
            Some("opaque-openai")
        );
        assert_eq!(
            observation.prepared.bytes.as_slice(),
            &request[body_start..]
        );
        assert_eq!(
            observation.prepared.body_digest,
            rightkit_fs::digest::sha256_bytes(&request[body_start..])
        );
        let body: serde_json::Value = serde_json::from_slice(&request[body_start..]).unwrap();
        assert_eq!(body["messages"][0]["content"], "federated final héllo");
        assert_eq!(observation.attempt, 1);
        assert_eq!(
            observation.state,
            crate::agent_providers::transport::SubmissionState::Submitted
        );
    }

    fn openai_tool_stream(reason: &str) -> String {
        let call = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"write_file","arguments":"{\"path\":\"out.txt\"}"}}]},"finish_reason":null}]}

"#;
        format!(
            "{call}data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"{reason}\"}}]}}\n\ndata: [DONE]\n\n"
        )
    }

    #[tokio::test]
    async fn openai_real_parser_marks_incomplete_tool_generation() {
        for reason in [
            "length",
            "max_tokens",
            "max_output_tokens",
            "token_limit",
            "output_limit",
        ] {
            let endpoint = serve_sse(openai_tool_stream(reason)).await;
            let client = OpenAiCompatClient::new("test-key", "moonshotai/kimi-k2.6", endpoint);
            let mut stream = client
                .stream_with_tools_metadata(vec![], vec![], &Default::default())
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

    /// EFF-009: a genuinely truncated OpenAI-compatible wire fixture — the
    /// server sends a `data:` frame cut off mid-JSON (no terminating `\n\n`,
    /// no `[DONE]`) and then closes the connection, exactly as a dropped
    /// upstream connection would. This must not be silently treated as a
    /// clean completion; the real `rightkit_http::SseDecoder` + stream loop must
    /// surface `ProviderError::Sse("stream ended with an incomplete SSE
    /// frame")` (see `SseDecoder::has_pending`).
    fn openai_truncated_mid_frame_stream() -> String {
        r#"data: {"choices":[{"delta":{"content":"partial ans"#.to_string()
    }

    #[tokio::test]
    async fn openai_real_parser_errors_on_truncated_mid_frame_stream() {
        let endpoint = serve_sse(openai_truncated_mid_frame_stream()).await;
        let client = OpenAiCompatClient::new("test-key", "moonshotai/kimi-k2.6", endpoint);
        let mut stream = client
            .stream_with_tools_metadata(vec![], vec![], &Default::default())
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
    async fn openai_real_parser_emits_complete_tool_generation() {
        let endpoint = serve_sse(openai_tool_stream("tool_calls")).await;
        let client = OpenAiCompatClient::new("test-key", "moonshotai/kimi-k2.6", endpoint);
        let mut stream = client
            .stream_with_tools_metadata(vec![], vec![], &Default::default())
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
        assert_eq!(finished.as_deref(), Some("tool_calls"));
    }

    // ---- CR4 item 1: protocol fixtures, OpenAI-compatible chat completions ----
    // Reuses this module's `serve_sse` (scripted bytes over a real socket,
    // then close, so EOF is genuine) and the real `stream_with_tools_metadata`.

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

    async fn openai_chat_fixture(body: String) -> ProviderResponseStream {
        let endpoint = serve_sse(body).await;
        let client = OpenAiCompatClient::new("test-key", "moonshotai/kimi-k2.6", endpoint);
        client
            .stream_with_tools_metadata(vec![], vec![], &Default::default())
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

    const OA_TOOL_START: &str = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"write_file","arguments":"{\"path\":"}}]},"finish_reason":null}]}"#;
    const OA_TOOL_TAIL: &str = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"out.txt\"}"}}]},"finish_reason":null}]}"#;
    const OA_TOOL_TRUNCATED: &str = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"write_file","arguments":"{\"path\":\"out"}}]},"finish_reason":null}]}"#;
    const OA_FINISH_TOOL_CALLS: &str = r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#;
    const OA_TEXT_PARTIAL: &str =
        r#"{"choices":[{"delta":{"content":"partial"},"finish_reason":null}]}"#;

    #[tokio::test]
    async fn openai_chat_valid_completion_emits_tool_call_once_with_complete_arguments() {
        let outcome = fixture_outcome(
            openai_chat_fixture(sse(&[
                OA_TOOL_START,
                OA_TOOL_TAIL,
                OA_FINISH_TOOL_CALLS,
                "[DONE]",
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
        assert_eq!(outcome.calls[0].id, "call-1");
        assert_eq!(outcome.calls[0].name, "write_file");
        assert_eq!(outcome.calls[0].arguments_json, r#"{"path":"out.txt"}"#);
        assert_eq!(outcome.finished, Some(Some("tool_calls".to_string())));
    }

    #[tokio::test]
    async fn openai_chat_bare_done_terminator_is_incomplete_not_success() {
        let outcome = fixture_outcome(openai_chat_fixture(sse(&["[DONE]"])).await).await;
        assert!(outcome.text.is_empty() && outcome.calls.is_empty());
        assert_sse_error(&outcome, "before completion");
        assert!(outcome.finished.is_none(), "bare [DONE] must not complete");
    }

    #[tokio::test]
    async fn openai_chat_eof_without_terminator_is_incomplete_not_completed() {
        let outcome = fixture_outcome(openai_chat_fixture(sse(&[OA_TEXT_PARTIAL])).await).await;
        assert_eq!(outcome.text, "partial");
        assert_sse_error(&outcome, "before completion");
        assert!(
            outcome.finished.is_none(),
            "abrupt EOF must not report Finished"
        );
    }

    #[tokio::test]
    async fn openai_chat_truncated_tool_arguments_never_surface_a_call() {
        let cases: Vec<(&str, String)> = vec![
            ("eof_after_partial_arguments", sse(&[OA_TOOL_TRUNCATED])),
            (
                "finish_tool_calls_with_partial_arguments",
                sse(&[OA_TOOL_TRUNCATED, OA_FINISH_TOOL_CALLS, "[DONE]"]),
            ),
            (
                "frame_cut_mid_arguments",
                format!("data: {OA_TOOL_TRUNCATED}"),
            ),
        ];
        for (name, body) in cases {
            let outcome = fixture_outcome(openai_chat_fixture(body).await).await;
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
    async fn openai_chat_malformed_event_json_uses_rejection_path() {
        let cases: Vec<(&str, String)> = vec![
            ("non_json_payload", sse(&["not-json"])),
            (
                "truncated_json_payload",
                "data: {\"choices\":[{\"delta\":{\"content\":\"x\"\n\n".to_string(),
            ),
        ];
        for (name, body) in cases {
            let outcome = fixture_outcome(openai_chat_fixture(body).await).await;
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
    async fn openai_chat_complete_arguments_then_eof_is_not_a_completed_tool_turn() {
        let outcome =
            fixture_outcome(openai_chat_fixture(sse(&[OA_TOOL_START, OA_TOOL_TAIL])).await).await;
        // The engine defers every yielded call until `Finished`
        // (engine_loop.rs deferred_calls), so the contract is that the stream
        // ends in an error and never reports Finished.
        assert_sse_error(&outcome, "before completion");
        assert!(outcome.finished.is_none(), "EOF must not report Finished");
    }

    #[tokio::test]
    async fn openai_chat_tool_call_is_held_until_terminal_signal() {
        // Complete arguments then EOF: the call must never be surfaced.
        let outcome =
            fixture_outcome(openai_chat_fixture(sse(&[OA_TOOL_START, OA_TOOL_TAIL])).await).await;
        assert!(
            outcome.calls.is_empty(),
            "tool call surfaced before any terminal signal"
        );
        assert_sse_error(&outcome, "before completion");
        assert!(outcome.finished.is_none());
    }

    #[tokio::test]
    async fn openai_chat_tool_call_released_on_bare_done_after_output() {
        let outcome = fixture_outcome(
            openai_chat_fixture(sse(&[OA_TOOL_START, OA_TOOL_TAIL, "[DONE]"])).await,
        )
        .await;
        assert!(
            outcome.error.is_none(),
            "unexpected error: {:?}",
            outcome.error
        );
        assert_eq!(outcome.calls.len(), 1);
        assert_eq!(outcome.finished, Some(None));
    }

    #[tokio::test]
    async fn openai_chat_done_with_only_usage_is_empty_not_success() {
        let usage =
            r#"{"choices":[],"usage":{"prompt_tokens":3,"completion_tokens":0,"total_tokens":3}}"#;
        let outcome = fixture_outcome(openai_chat_fixture(sse(&[usage, "[DONE]"])).await).await;
        assert!(outcome.calls.is_empty() && outcome.text.is_empty());
        assert_sse_error(&outcome, "before completion");
        assert!(outcome.finished.is_none());
    }

    #[tokio::test]
    async fn openai_chat_non_stream_bare_done_is_incomplete_not_success() {
        let endpoint = serve_sse(sse(&["[DONE]"])).await;
        let client = OpenAiCompatClient::new("test-key", "moonshotai/kimi-k2.6", endpoint);
        let result = client
            .complete_with_tools_metadata(vec![], vec![], &Default::default())
            .await;
        match result {
            Err(ProviderError::Sse(message)) => {
                assert!(
                    message.contains("before completion"),
                    "unexpected: {message}"
                )
            }
            other => panic!("bare [DONE] must be a typed incomplete error, got {other:?}"),
        }
    }

    // ---- CR4 item 1: protocol fixtures, OpenAI Responses (Codex route) ----

    async fn codex_fixture(body: String) -> ProviderResponseStream {
        let endpoint = serve_sse(body).await;
        codex_responses_event_stream(codex_response(&endpoint).await)
    }

    const CX_TEXT_PARTIAL: &str = r#"{"type":"response.output_text.delta","delta":"partial"}"#;
    const CX_ITEM_ADDED: &str = r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"item-1","call_id":"call-1","name":"write_file"}}"#;
    const CX_ARGS_HEAD: &str = r#"{"type":"response.function_call_arguments.delta","item_id":"item-1","delta":"{\"path\":"}"#;
    const CX_ARGS_TAIL: &str = r#"{"type":"response.function_call_arguments.delta","item_id":"item-1","delta":"\"out.txt\"}"}"#;
    const CX_ARGS_TRUNCATED: &str = r#"{"type":"response.function_call_arguments.delta","item_id":"item-1","delta":"{\"path\":\"out"}"#;
    const CX_ARGS_DONE: &str = r#"{"type":"response.function_call_arguments.done","item_id":"item-1","arguments":"{\"path\":\"out.txt\"}"}"#;
    const CX_ITEM_DONE: &str = r#"{"type":"response.output_item.done","item":{"type":"function_call","id":"item-1","call_id":"call-1","name":"write_file","arguments":"{\"path\":\"out.txt\"}"}}"#;
    const CX_COMPLETED: &str = r#"{"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":4,"output_tokens":2,"input_token_details":{"cached_tokens":1}}}}"#;

    #[tokio::test]
    async fn codex_responses_valid_completion_emits_tool_call_once_with_complete_arguments() {
        let outcome = fixture_outcome(
            codex_fixture(sse(&[
                CX_ITEM_ADDED,
                CX_ARGS_HEAD,
                CX_ARGS_TAIL,
                CX_ARGS_DONE,
                CX_ITEM_DONE,
                CX_COMPLETED,
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
        assert_eq!(outcome.calls[0].id, "call-1");
        assert_eq!(outcome.calls[0].name, "write_file");
        assert_eq!(outcome.calls[0].arguments_json, r#"{"path":"out.txt"}"#);
        assert_eq!(outcome.finished, Some(Some("completed".to_string())));
    }

    #[tokio::test]
    async fn codex_responses_bare_done_terminator_is_incomplete_not_success() {
        let outcome = fixture_outcome(codex_fixture(sse(&["[DONE]"])).await).await;
        assert!(outcome.text.is_empty() && outcome.calls.is_empty());
        assert_sse_error(&outcome, "before completion");
        assert!(outcome.finished.is_none(), "bare [DONE] must not complete");
    }

    #[tokio::test]
    async fn codex_responses_eof_without_completed_event_is_incomplete_not_completed() {
        let outcome = fixture_outcome(codex_fixture(sse(&[CX_TEXT_PARTIAL])).await).await;
        assert_eq!(outcome.text, "partial");
        assert_sse_error(&outcome, "before completion");
        assert!(
            outcome.finished.is_none(),
            "abrupt EOF must not report Finished"
        );
    }

    #[tokio::test]
    async fn codex_responses_truncated_tool_arguments_never_surface_a_call() {
        let cases: Vec<(&str, String)> = vec![
            (
                "eof_after_partial_arguments",
                sse(&[CX_ITEM_ADDED, CX_ARGS_TRUNCATED]),
            ),
            (
                "completed_with_partial_arguments",
                sse(&[CX_ITEM_ADDED, CX_ARGS_TRUNCATED, CX_COMPLETED]),
            ),
            (
                "frame_cut_mid_arguments",
                format!("{}data: {CX_ARGS_TRUNCATED}", sse(&[CX_ITEM_ADDED])),
            ),
        ];
        for (name, body) in cases {
            let outcome = fixture_outcome(codex_fixture(body).await).await;
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
    async fn codex_responses_malformed_event_json_uses_rejection_path() {
        let cases: Vec<(&str, String)> = vec![
            ("non_json_payload", sse(&["nope"])),
            (
                "truncated_json_payload",
                "data: {\"type\":\"response.output_text.delta\"\n\n".to_string(),
            ),
        ];
        for (name, body) in cases {
            let outcome = fixture_outcome(codex_fixture(body).await).await;
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
}
