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
        CHAT_REQUEST_SERIALIZER_GENERATION_V1,
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
