//! Anthropic Messages API client (`POST {base}/v1/messages`), non-streaming and SSE.

use std::ops::ControlFlow;
use std::sync::Arc;

use rightkit_http::{Client, ClientConfig, HttpError, Request, RetryPolicy};
use serde_json::{json, Map, Value};

use crate::cache::{estimate_tokens, plan_breakpoints, CacheableBlock, MAX_BREAKPOINTS};
use crate::credentials::CredentialSource;
use crate::error::LlmError;
use crate::provider::Provider;
use crate::raw::RawHandle;
use crate::types::*;

pub const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Message-block slots reserved for conversation breakpoints in `CacheMode::Auto`.
const MESSAGE_BREAKPOINTS: usize = 2;

pub struct AnthropicProvider {
    id: String,
    base_url: String,
    client: Client,
    credentials: Arc<dyn CredentialSource>,
    extra_headers: Vec<(String, String)>,
    observer: Option<CallObserver>,
    raw_capture: Option<usize>,
}

/// Default client config for Anthropic: also retries 529 (overloaded).
pub fn default_client_config() -> ClientConfig {
    let mut retry = RetryPolicy::default();
    retry.retry_statuses.push(529);
    ClientConfig {
        retry,
        ..ClientConfig::default()
    }
}

impl AnthropicProvider {
    pub fn new(base_url: &str, client: Client, credentials: Arc<dyn CredentialSource>) -> Self {
        Self {
            id: "anthropic".into(),
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
            credentials,
            extra_headers: Vec::new(),
            observer: None,
            raw_capture: None,
        }
    }
    pub fn with_id(mut self, id: &str) -> Self {
        self.id = id.to_string();
        self
    }
    /// e.g. `anthropic-beta`.
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.extra_headers.push((name.into(), value.into()));
        self
    }
    /// Report every call that reaches the network to `f`. See [`CallRecord`].
    pub fn with_observer(mut self, f: impl Fn(&CallRecord<'_>) + Send + Sync + 'static) -> Self {
        self.observer = Some(Arc::new(f));
        self
    }
    /// Keep the provider's raw response for non-streaming calls: status, request id (the
    /// `request-id` header) and up to `max_body_bytes` of the body (0 keeps status and request
    /// id only). The observer gets it in [`CallRecord::raw`], and a `Truncated` or `Malformed`
    /// error carries it ([`LlmError::raw`]). Off by default, and streams are not captured. The
    /// body is the provider's answer to the prompt; see [`crate::raw`] for what it holds and who
    /// owns it.
    pub fn with_raw_capture(mut self, max_body_bytes: usize) -> Self {
        self.raw_capture = Some(max_body_bytes);
        self
    }

    /// The HTTP request, the credential text so provider error text can have it removed, and
    /// the body that was built.
    fn request(
        &self,
        req: &ChatRequest,
        stream: bool,
    ) -> Result<(Request, Option<String>, Value), LlmError> {
        check_empty_messages(req)?;
        let credential = self
            .credentials
            .credential(&self.id)?
            .ok_or_else(|| LlmError::MissingCredential(self.id.clone()))?;
        let secret = credential.expose().to_string();
        let mut body = build_body(req, stream);
        req.extra_body.merge_into(&mut body)?;
        // Replay after 429/5xx/529 is safe: a rejected or failed Messages call is not billed
        // or applied server-side, and Anthropic documents these statuses as retryable. The
        // per-call `CallRetry` may narrow or replace the client's attempt policy.
        let http = req.retry.apply(
            Request::post(format!("{}/v1/messages", self.base_url))
                .retry_unsafe_statuses(true)
                .secret_header("x-api-key", credential.expose())
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header(
                    "Accept",
                    if stream {
                        "text/event-stream"
                    } else {
                        "application/json"
                    },
                ),
        );
        let mut http = http.json(&body)?;
        for (k, v) in &self.extra_headers {
            http = http.header(k.as_str(), v.as_str());
        }
        if let Some(limit) = req.timeout {
            // Per-call total budget; `deadline` also bounds retry attempts and sleeps.
            http = http.timeout(limit).deadline(limit);
        }
        Ok((http, Some(secret), body))
    }
}

/// Anthropic has no empty content. With `keep_empty_messages` set, a message with no text is
/// rejected here, before anything is sent. Without the option the message is sent unchanged,
/// as before, and the API's own error applies.
fn check_empty_messages(req: &ChatRequest) -> Result<(), LlmError> {
    if !req.keep_empty_messages {
        return Ok(());
    }
    let empty = req.messages.iter().position(|m| {
        m.content
            .iter()
            .all(|b| matches!(b, ContentBlock::Text { text } if text.is_empty()))
    });
    match empty {
        Some(i) => Err(LlmError::InvalidRequest(format!(
            "message {i} has no content; Anthropic rejects empty messages (keep_empty_messages is set)"
        ))),
        None => Ok(()),
    }
}

/// Build the exact JSON body sent to the API.
pub fn build_body(req: &ChatRequest, stream: bool) -> Value {
    let auto = req.cache == CacheMode::Auto;
    let ephemeral = || json!({"type": "ephemeral"});
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    body.insert("max_tokens".into(), json!(req.max_tokens));
    if let Some(t) = req.temperature {
        body.insert("temperature".into(), f32_json(t));
    }
    if !req.stop_sequences.is_empty() {
        body.insert("stop_sequences".into(), json!(req.stop_sequences));
    }
    if stream {
        body.insert("stream".into(), json!(true));
    }

    let mut slots = MAX_BREAKPOINTS;
    if !req.tools.is_empty() {
        let mut tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut tool = json!({"name": t.name, "description": t.description, "input_schema": t.input_schema});
                if t.strict {
                    tool["strict"] = json!(true);
                }
                tool
            })
            .collect();
        if auto {
            tools.last_mut().unwrap()["cache_control"] = ephemeral();
            slots -= 1;
        }
        body.insert("tools".into(), Value::Array(tools));
        if let Some(name) = &req.force_tool {
            body.insert("tool_choice".into(), json!({"type": "tool", "name": name}));
        }
    }
    if let Some(system) = &req.system {
        if auto {
            body.insert(
                "system".into(),
                json!([{"type": "text", "text": system, "cache_control": ephemeral()}]),
            );
            slots -= 1;
        } else {
            body.insert("system".into(), json!(system));
        }
    }

    let mut messages: Vec<Value> = req.messages.iter().map(message_json).collect();
    if auto {
        let mut coords = Vec::new();
        let mut blocks = Vec::new();
        for (mi, m) in req.messages.iter().enumerate() {
            for (bi, b) in m.content.iter().enumerate() {
                coords.push((mi, bi));
                blocks.push(CacheableBlock {
                    id: format!("m{mi}.{bi}"),
                    approx_tokens: block_tokens(b),
                    volatile: false,
                });
            }
        }
        for idx in plan_breakpoints(&blocks, slots.min(MESSAGE_BREAKPOINTS)) {
            let (mi, bi) = coords[idx];
            messages[mi]["content"][bi]["cache_control"] = ephemeral();
        }
    }
    body.insert("messages".into(), Value::Array(messages));
    Value::Object(body)
}

fn block_tokens(b: &ContentBlock) -> u32 {
    match b {
        ContentBlock::Text { text } => estimate_tokens(text),
        ContentBlock::ToolResult { content, .. } => estimate_tokens(content),
        ContentBlock::ToolUse { input, .. } => estimate_tokens(&input.to_string()),
    }
}

fn message_json(m: &Message) -> Value {
    let content: Vec<Value> = m
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => json!({"type": "text", "text": text}),
            ContentBlock::ToolUse { id, name, input } => {
                json!({"type": "tool_use", "id": id, "name": name, "input": input})
            }
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => {
                let mut v =
                    json!({"type": "tool_result", "tool_use_id": tool_use_id, "content": content});
                if *is_error {
                    v["is_error"] = json!(true);
                }
                v
            }
        })
        .collect();
    json!({"role": match m.role { Role::User => "user", Role::Assistant => "assistant" }, "content": content})
}

fn parse_usage(v: &Value) -> Usage {
    let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    Usage {
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cache_read_tokens: n("cache_read_input_tokens"),
        cache_write_tokens: n("cache_creation_input_tokens"),
    }
}

fn parse_stop(s: Option<&str>) -> StopReason {
    match s {
        Some("end_turn") | None => StopReason::EndTurn,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("tool_use") => StopReason::ToolUse,
        Some("stop_sequence") => StopReason::StopSequence,
        Some(other) => StopReason::Other(other.to_string()),
    }
}

pub fn parse_response(v: &Value) -> Result<ChatResponse, LlmError> {
    let content = v
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| LlmError::Malformed("missing content array".into()))?;
    let mut blocks = Vec::new();
    for b in content {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => blocks.push(ContentBlock::text(b["text"].as_str().unwrap_or_default())),
            Some("tool_use") => blocks.push(ContentBlock::ToolUse {
                id: b["id"].as_str().unwrap_or_default().into(),
                name: b["name"].as_str().unwrap_or_default().into(),
                input: b.get("input").cloned().unwrap_or(Value::Null),
            }),
            _ => {} // thinking and other block types are not surfaced
        }
    }
    Ok(ChatResponse {
        id: v["id"].as_str().unwrap_or_default().into(),
        model: v["model"].as_str().unwrap_or_default().into(),
        content: blocks,
        stop_reason: parse_stop(v["stop_reason"].as_str()),
        usage: v.get("usage").map(parse_usage).unwrap_or_default(),
        usage_known: v.get("usage").is_some(),
        metering: Metering::PerToken,
    })
}

impl std::fmt::Debug for AnthropicProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The credential source and HTTP client are omitted: neither prints a key.
        f.debug_struct("AnthropicProvider")
            .field("id", &self.id)
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let (http, secret, body) = self.request(req, false)?;
        let (result, raw) = crate::raw::exchange(
            &self.client,
            &http,
            secret.as_deref(),
            self.raw_capture,
            req,
            parse_response,
        );
        self.observe(false, &body, &result, raw.as_ref());
        result
    }

    fn stream(
        &self,
        req: &ChatRequest,
        on_event: &mut dyn FnMut(StreamEvent) -> ControlFlow<()>,
    ) -> Result<ChatResponse, LlmError> {
        let (http, secret, body) = self.request(req, true)?;
        let result = self.stream_call(req, &http, secret.as_deref(), on_event);
        self.observe(true, &body, &result, None);
        result
    }
}

impl AnthropicProvider {
    fn observe(
        &self,
        stream: bool,
        body: &Value,
        result: &Result<ChatResponse, LlmError>,
        raw: Option<&RawHandle>,
    ) {
        if let Some(observer) = &self.observer {
            observer(&CallRecord {
                provider: &self.id,
                stream,
                request_body: body,
                result: result.as_ref(),
                raw,
            });
        }
    }

    fn stream_call(
        &self,
        req: &ChatRequest,
        http: &Request,
        key: Option<&str>,
        on_event: &mut dyn FnMut(StreamEvent) -> ControlFlow<()>,
    ) -> Result<ChatResponse, LlmError> {
        let response = self
            .client
            .stream(http)
            .map_err(|e| LlmError::from_http(e, key))?;
        let mut asm = Assembler::default();
        let mut delivered = false;
        for item in response.sse() {
            let ev = match item {
                Ok(e) => e,
                Err(e) => return Err(late(e, delivered)),
            };
            if ev.event == "ping" {
                continue;
            }
            let data: Value = serde_json::from_str(&ev.data)
                .map_err(|e| late_msg(format!("bad SSE JSON: {e}"), delivered))?;
            let kind = data
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or(&ev.event)
                .to_string();
            match kind.as_str() {
                "error" => {
                    let e = &data["error"];
                    let (k, m) = (
                        crate::error::redact_secret(e["type"].as_str().unwrap_or("error"), key),
                        crate::error::redact_secret(e["message"].as_str().unwrap_or_default(), key),
                    );
                    return Err(if delivered {
                        LlmError::MidStream(format!("{k}: {m}"))
                    } else {
                        LlmError::Provider {
                            kind: k,
                            message: m,
                        }
                    });
                }
                "message_stop" => return asm.finish().and_then(|r| check_output(r, req)),
                _ => {
                    for out in asm.apply(&kind, &data) {
                        delivered = true;
                        if on_event(out).is_break() {
                            return Err(LlmError::Cancelled);
                        }
                    }
                }
            }
        }
        Err(late_msg(
            "stream ended before message_stop".into(),
            delivered,
        ))
    }
}

fn late(e: HttpError, delivered: bool) -> LlmError {
    if delivered {
        LlmError::MidStream(e.to_string())
    } else {
        LlmError::Http(e)
    }
}

fn late_msg(m: String, delivered: bool) -> LlmError {
    if delivered {
        LlmError::MidStream(m)
    } else {
        LlmError::Http(HttpError::Stream(m))
    }
}

#[derive(Default)]
struct Assembler {
    id: String,
    model: String,
    usage: Usage,
    stop: Option<String>,
    blocks: Vec<Slot>,
}

enum Slot {
    Text(String),
    Tool {
        id: String,
        name: String,
        json: String,
    },
    Other,
}

impl Assembler {
    fn apply(&mut self, kind: &str, d: &Value) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        match kind {
            "message_start" => {
                let m = &d["message"];
                self.id = m["id"].as_str().unwrap_or_default().into();
                self.model = m["model"].as_str().unwrap_or_default().into();
                if let Some(u) = m.get("usage") {
                    self.usage = parse_usage(u);
                }
            }
            "content_block_start" => {
                let idx = d["index"].as_u64().unwrap_or(0) as usize;
                let cb = &d["content_block"];
                while self.blocks.len() <= idx {
                    self.blocks.push(Slot::Other);
                }
                self.blocks[idx] = match cb["type"].as_str() {
                    Some("text") => {
                        let t = cb["text"].as_str().unwrap_or_default().to_string();
                        if !t.is_empty() {
                            out.push(StreamEvent::Text(t.clone()));
                        }
                        Slot::Text(t)
                    }
                    Some("tool_use") => {
                        let (id, name) = (
                            cb["id"].as_str().unwrap_or_default().to_string(),
                            cb["name"].as_str().unwrap_or_default().to_string(),
                        );
                        out.push(StreamEvent::ToolUseStart {
                            id: id.clone(),
                            name: name.clone(),
                        });
                        Slot::Tool {
                            id,
                            name,
                            json: String::new(),
                        }
                    }
                    _ => Slot::Other,
                };
            }
            "content_block_delta" => {
                let idx = d["index"].as_u64().unwrap_or(0) as usize;
                let delta = &d["delta"];
                match (self.blocks.get_mut(idx), delta["type"].as_str()) {
                    (Some(Slot::Text(t)), Some("text_delta")) => {
                        let s = delta["text"].as_str().unwrap_or_default();
                        t.push_str(s);
                        out.push(StreamEvent::Text(s.to_string()));
                    }
                    (Some(Slot::Tool { id, json, .. }), Some("input_json_delta")) => {
                        let s = delta["partial_json"].as_str().unwrap_or_default();
                        json.push_str(s);
                        out.push(StreamEvent::ToolInputDelta {
                            id: id.clone(),
                            partial_json: s.to_string(),
                        });
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(s) = d["delta"]["stop_reason"].as_str() {
                    self.stop = Some(s.to_string());
                }
                if let Some(u) = d.get("usage") {
                    let nu = parse_usage(u);
                    // message_delta carries cumulative output tokens; other counters
                    // are only overwritten when present.
                    self.usage.output_tokens = nu.output_tokens.max(self.usage.output_tokens);
                    if u.get("input_tokens").is_some() {
                        self.usage.input_tokens = nu.input_tokens;
                    }
                    if u.get("cache_read_input_tokens").is_some() {
                        self.usage.cache_read_tokens = nu.cache_read_tokens;
                    }
                    if u.get("cache_creation_input_tokens").is_some() {
                        self.usage.cache_write_tokens = nu.cache_write_tokens;
                    }
                }
            }
            _ => {}
        }
        out
    }

    fn finish(self) -> Result<ChatResponse, LlmError> {
        let mut content = Vec::new();
        for slot in self.blocks {
            match slot {
                Slot::Text(t) => content.push(ContentBlock::text(t)),
                Slot::Tool { id, name, json } => {
                    let input = if json.trim().is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&json)
                            .map_err(|e| LlmError::Malformed(format!("tool input JSON: {e}")))?
                    };
                    content.push(ContentBlock::ToolUse { id, name, input });
                }
                Slot::Other => {}
            }
        }
        Ok(ChatResponse {
            id: self.id,
            model: self.model,
            content,
            stop_reason: parse_stop(self.stop.as_deref()),
            usage: self.usage,
            usage_known: self.usage != Usage::default(),
            metering: Metering::PerToken,
        })
    }
}
