//! OpenAI-compatible chat client (`POST {base}/chat/completions`). Serves OpenAI,
//! OpenRouter, Groq, DeepSeek, Together, Mistral, xAI, Gemini's OpenAI endpoint,
//! NVIDIA NIM, and local runtimes (Ollama, LM Studio, llama.cpp server).

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::sync::Arc;

use rightkit_http::{Client, HttpError, Request};
use serde_json::{json, Map, Value};

use crate::credentials::CredentialSource;
use crate::error::LlmError;
use crate::provider::Provider;
use crate::raw::RawHandle;
use crate::types::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaxTokensField {
    /// `max_tokens` (most compatible servers).
    MaxTokens,
    /// `max_completion_tokens` (current OpenAI models).
    MaxCompletionTokens,
}

pub struct OpenAiCompatProvider {
    id: String,
    base_url: String,
    client: Client,
    credentials: Arc<dyn CredentialSource>,
    /// Local runtimes need no credential.
    requires_credential: bool,
    extra_headers: Vec<(String, String)>,
    max_tokens_field: MaxTokensField,
    observer: Option<CallObserver>,
    raw_capture: Option<usize>,
}

impl OpenAiCompatProvider {
    pub fn new(
        id: &str,
        base_url: &str,
        client: Client,
        credentials: Arc<dyn CredentialSource>,
    ) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
            credentials,
            requires_credential: true,
            extra_headers: Vec::new(),
            max_tokens_field: MaxTokensField::MaxTokens,
            observer: None,
            raw_capture: None,
        }
    }
    pub fn without_credential(mut self) -> Self {
        self.requires_credential = false;
        self
    }
    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.extra_headers.push((name.into(), value.into()));
        self
    }
    pub fn with_max_tokens_field(mut self, f: MaxTokensField) -> Self {
        self.max_tokens_field = f;
        self
    }
    /// Report every call that reaches the network to `f`: the exact body sent and the parsed
    /// response (with usage) or the error. The credential and auth header are never passed.
    /// See [`CallRecord`].
    pub fn with_observer(mut self, f: impl Fn(&CallRecord<'_>) + Send + Sync + 'static) -> Self {
        self.observer = Some(Arc::new(f));
        self
    }
    /// Keep the provider's raw response for non-streaming calls: status, request id and up to
    /// `max_body_bytes` of the body (0 keeps status and request id only). The observer gets it
    /// in [`CallRecord::raw`], and a `Truncated` or `Malformed` error carries it
    /// ([`LlmError::raw`]). Off by default, and streams are not captured. The body is the
    /// provider's answer to the prompt; see [`crate::raw`] for what it holds and who owns it.
    pub fn with_raw_capture(mut self, max_body_bytes: usize) -> Self {
        self.raw_capture = Some(max_body_bytes);
        self
    }

    /// The HTTP request, the credential text (when one was sent) so that error text from the
    /// provider can have it removed, and the body that was built.
    fn request(
        &self,
        req: &ChatRequest,
        stream: bool,
    ) -> Result<(Request, Option<String>, Value), LlmError> {
        req.check_images()?;
        req.check_schema()?;
        let mut body = build_body(req, stream, self.max_tokens_field);
        req.extra_body.merge_into(&mut body)?;
        // Replay after 429/5xx is safe: a chat completion has no server-side effect and
        // OpenAI-compatible providers document 429 and 5xx as retryable. The per-call
        // `CallRetry` may narrow or replace the client's attempt policy.
        let http = req.retry.apply(
            Request::post(format!("{}/chat/completions", self.base_url))
                .retry_unsafe_statuses(true)
                .header(
                    "Accept",
                    if stream {
                        "text/event-stream"
                    } else {
                        "application/json"
                    },
                ),
        );
        let (mut http, secret) = self.authorize(http)?;
        if let Some(limit) = req.timeout {
            // Per-call total budget; `deadline` also bounds retry attempts and sleeps.
            http = http.timeout(limit).deadline(limit);
        }
        Ok((http.json(&body)?, secret, body))
    }

    /// Adds the bearer credential (when this provider needs one) and the configured headers.
    fn authorize(&self, mut http: Request) -> Result<(Request, Option<String>), LlmError> {
        let mut secret = None;
        match self.credentials.credential(&self.id)? {
            Some(c) => {
                secret = Some(c.expose().to_string());
                http = http.bearer(c.expose());
            }
            None if self.requires_credential => {
                return Err(LlmError::MissingCredential(self.id.clone()))
            }
            None => {}
        }
        for (k, v) in &self.extra_headers {
            http = http.header(k.as_str(), v.as_str());
        }
        Ok((http, secret))
    }

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

    /// `GET {base}/models`: the model ids the endpoint lists. Doubles as a key check: a
    /// rejected credential (401 or 403) returns [`LlmError::Auth`]. Ids come back in the
    /// order the server listed them.
    pub fn list_models(&self) -> Result<Vec<String>, LlmError> {
        let http =
            Request::get(format!("{}/models", self.base_url)).header("Accept", "application/json");
        let (http, secret) = self.authorize(http)?;
        let resp = self.client.send(&http).map_err(|e| {
            match LlmError::from_http(e, secret.as_deref()) {
                LlmError::Status { status, message } if matches!(status, 401 | 403) => {
                    LlmError::Auth { status, message }
                }
                other => other,
            }
        })?;
        let v: Value = resp
            .json()
            .map_err(|e| LlmError::Malformed(e.to_string()))?;
        parse_model_ids(&v)
    }

    /// `Ok(())` when the endpoint accepts the credential. Same call as
    /// [`OpenAiCompatProvider::list_models`], without the list.
    pub fn validate_credential(&self) -> Result<(), LlmError> {
        self.list_models().map(|_| ())
    }
}

impl std::fmt::Debug for OpenAiCompatProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The credential source and HTTP client are omitted: neither prints a key.
        f.debug_struct("OpenAiCompatProvider")
            .field("id", &self.id)
            .field("base_url", &crate::generation::redact_url(&self.base_url))
            .field("requires_credential", &self.requires_credential)
            .field("max_tokens_field", &self.max_tokens_field)
            .finish_non_exhaustive()
    }
}

pub fn build_body(req: &ChatRequest, stream: bool, field: MaxTokensField) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    let mut messages: Vec<Value> = Vec::new();
    if let Some(s) = &req.system {
        messages.push(json!({"role": "system", "content": s}));
    }
    for m in &req.messages {
        push_message(&mut messages, m, req.keep_empty_messages);
    }
    body.insert("messages".into(), Value::Array(messages));
    let key = match field {
        MaxTokensField::MaxTokens => "max_tokens",
        MaxTokensField::MaxCompletionTokens => "max_completion_tokens",
    };
    if !req.omit_max_tokens {
        body.insert(key.into(), json!(req.max_tokens));
    }
    if let Some(t) = req.temperature {
        body.insert("temperature".into(), f32_json(t));
    }
    if !req.stop_sequences.is_empty() {
        body.insert("stop".into(), json!(req.stop_sequences));
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                let mut tool = json!({"type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.input_schema}});
                if t.strict {
                    tool["function"]["strict"] = json!(true);
                }
                tool
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        if let Some(name) = &req.force_tool {
            body.insert(
                "tool_choice".into(),
                json!({"type": "function", "function": {"name": name}}),
            );
        }
    }
    if let Some(s) = req.response_schema() {
        body.insert(
            "response_format".into(),
            json!({"type": "json_schema", "json_schema": {
                "name": s.name, "schema": s.schema, "strict": s.strict}}),
        );
    }
    if stream {
        body.insert("stream".into(), json!(true));
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }
    Value::Object(body)
}

/// Maps one message. A user message with no text and no tool result is dropped, unless
/// `keep_empty` is set, in which case it is sent as an empty string.
fn push_message(out: &mut Vec<Value>, m: &Message, keep_empty: bool) {
    match m.role {
        Role::User => {
            let mut text = String::new();
            // In order, for a message that carries images (content becomes an array of parts).
            let mut parts: Vec<Value> = Vec::new();
            let mut has_image = false;
            let mut sent_tool_result = false;
            for b in &m.content {
                match b {
                    ContentBlock::Text { text: t } => {
                        text.push_str(t);
                        if !t.is_empty() {
                            parts.push(json!({"type": "text", "text": t}));
                        }
                    }
                    ContentBlock::Image { source } => {
                        has_image = true;
                        parts.push(
                            json!({"type": "image_url", "image_url": {"url": source.openai_url()}}),
                        );
                    }
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } => {
                        sent_tool_result = true;
                        out.push(json!({"role": "tool", "tool_call_id": tool_use_id, "content": content}));
                    }
                    ContentBlock::ToolUse { .. } => {}
                }
            }
            if has_image {
                out.push(json!({"role": "user", "content": parts}));
            } else if !text.is_empty() {
                out.push(json!({"role": "user", "content": text}));
            } else if keep_empty && !sent_tool_result {
                out.push(json!({"role": "user", "content": ""}));
            }
        }
        Role::Assistant => {
            let mut text = String::new();
            let mut calls = Vec::new();
            for b in &m.content {
                match b {
                    ContentBlock::Text { text: t } => text.push_str(t),
                    ContentBlock::ToolUse { id, name, input } => calls.push(json!({
                        "id": id, "type": "function",
                        "function": {"name": name, "arguments": input.to_string()}})),
                    ContentBlock::ToolResult { .. } | ContentBlock::Image { .. } => {}
                }
            }
            let mut msg = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) }});
            if !calls.is_empty() {
                msg["tool_calls"] = Value::Array(calls);
            }
            out.push(msg);
        }
    }
}

fn parse_usage(u: &Value) -> Usage {
    let n = |v: &Value| v.as_u64().unwrap_or(0);
    let prompt = n(&u["prompt_tokens"]);
    let cached =
        n(&u["prompt_tokens_details"]["cached_tokens"]).max(n(&u["prompt_cache_hit_tokens"])); // DeepSeek-style field
    Usage {
        input_tokens: prompt.saturating_sub(cached),
        output_tokens: n(&u["completion_tokens"]),
        cache_read_tokens: cached,
        cache_write_tokens: 0,
    }
}

fn parse_finish(s: Option<&str>) -> StopReason {
    match s {
        Some("stop") | None => StopReason::EndTurn,
        Some("length") => StopReason::MaxTokens,
        Some("tool_calls") | Some("function_call") => StopReason::ToolUse,
        Some(o) => StopReason::Other(o.to_string()),
    }
}

fn tool_input(args: &str) -> Result<Value, LlmError> {
    if args.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(args).map_err(|e| LlmError::Malformed(format!("tool arguments JSON: {e}")))
}

pub fn parse_response(v: &Value) -> Result<ChatResponse, LlmError> {
    let choices = match v.get("choices") {
        None | Some(Value::Null) => {
            return Err(LlmError::Malformed("missing choices field".into()))
        }
        Some(c) => c
            .as_array()
            .ok_or_else(|| LlmError::Malformed("choices is not an array".into()))?,
    };
    let choice = choices
        .first()
        .ok_or_else(|| LlmError::Malformed("empty choices array".into()))?;
    let msg = &choice["message"];
    let mut content = Vec::new();
    // `None` when `content` is null, absent or not a string; `Some("")` only for an explicit
    // empty string.
    let text = msg["content"].as_str();
    if let Some(t) = text.filter(|t| !t.is_empty()) {
        content.push(ContentBlock::text(t));
    }
    if let Some(calls) = msg["tool_calls"].as_array() {
        for c in calls {
            content.push(ContentBlock::ToolUse {
                id: c["id"].as_str().unwrap_or_default().into(),
                name: c["function"]["name"].as_str().unwrap_or_default().into(),
                input: tool_input(c["function"]["arguments"].as_str().unwrap_or(""))?,
            });
        }
    }
    let has_tool_use = content
        .iter()
        .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
    if !has_tool_use && text.is_none_or(str::is_empty) {
        let refusal = msg["refusal"].as_str().filter(|r| !r.is_empty());
        let filtered = choice["finish_reason"].as_str() == Some("content_filter");
        if refusal.is_some() || filtered {
            return Err(LlmError::Refused {
                message: refusal
                    .map(str::to_owned)
                    .unwrap_or_else(|| "finish_reason content_filter".into()),
            });
        }
        if text.is_none() {
            return Err(LlmError::Malformed(
                "empty completion: no text content and no tool call".into(),
            ));
        }
    }
    Ok(ChatResponse {
        id: v["id"].as_str().unwrap_or_default().into(),
        model: v["model"].as_str().unwrap_or_default().into(),
        content,
        stop_reason: parse_finish(choice["finish_reason"].as_str()),
        usage: v.get("usage").map(parse_usage).unwrap_or_default(),
        usage_known: v.get("usage").is_some(),
        metering: Metering::PerToken,
    })
}

/// Model ids from a `GET /models` answer (`{"data": [{"id": ...}]}`). Entries without a
/// non-empty `id` are skipped; a missing `data` array is [`LlmError::Malformed`].
pub fn parse_model_ids(v: &Value) -> Result<Vec<String>, LlmError> {
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| LlmError::Malformed("missing data array".into()))?;
    Ok(data
        .iter()
        .filter_map(|m| m["id"].as_str().filter(|s| !s.is_empty()))
        .map(str::to_owned)
        .collect())
}

impl Provider for OpenAiCompatProvider {
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
            |v| v.get("usage").map(parse_usage),
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
        let mut usage = Usage::default();
        let result = self
            .stream_call(req, &http, secret.as_deref(), &mut usage, on_event)
            .map_err(|e| {
                // The usage frame is the last chunk; a stream that fails or is cancelled after
                // it was billed for what the provider counted.
                if req.report_failed_usage && usage != Usage::default() {
                    billed(
                        e,
                        &AttemptBilling::new(Spend::Reported(usage), Metering::PerToken),
                    )
                } else {
                    e
                }
            });
        self.observe(true, &body, &result, None);
        result
    }
}

impl OpenAiCompatProvider {
    fn stream_call(
        &self,
        req: &ChatRequest,
        http: &Request,
        key: Option<&str>,
        usage: &mut Usage,
        on_event: &mut dyn FnMut(StreamEvent) -> ControlFlow<()>,
    ) -> Result<ChatResponse, LlmError> {
        let response = self
            .client
            .stream(http)
            .map_err(|e| LlmError::from_http(e, key))?;
        let mut id = String::new();
        let mut model = String::new();
        let mut text = String::new();
        let mut finish: Option<String> = None;
        // index -> (id, name, args)
        let mut calls: BTreeMap<u64, (String, String, String)> = BTreeMap::new();
        let mut delivered = false;
        let mut done = false;

        let fail = |m: String, delivered: bool| {
            if delivered {
                LlmError::MidStream(m)
            } else {
                LlmError::Http(HttpError::Stream(m))
            }
        };

        for item in response.sse() {
            let ev = item.map_err(|e| {
                if delivered {
                    LlmError::MidStream(e.to_string())
                } else {
                    LlmError::Http(e)
                }
            })?;
            if ev.data.trim() == "[DONE]" {
                done = true;
                break;
            }
            let chunk: Value = serde_json::from_str(&ev.data)
                .map_err(|e| fail(format!("bad SSE JSON: {e}"), delivered))?;
            if let Some(err) = chunk.get("error").filter(|e| !e.is_null()) {
                let message = crate::error::redact_secret(
                    err["message"].as_str().unwrap_or("provider error"),
                    key,
                );
                let kind =
                    crate::error::redact_secret(err["type"].as_str().unwrap_or("error"), key);
                return Err(if delivered {
                    LlmError::MidStream(format!("{kind}: {message}"))
                } else {
                    LlmError::Provider { kind, message }
                });
            }
            if let Some(s) = chunk["id"].as_str() {
                id = s.into();
            }
            if let Some(s) = chunk["model"].as_str() {
                model = s.into();
            }
            if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
                *usage = parse_usage(u);
            }
            let Some(choice) = chunk["choices"].get(0) else {
                continue;
            };
            let delta = &choice["delta"];
            let mut out = Vec::new();
            if let Some(t) = delta["content"].as_str().filter(|t| !t.is_empty()) {
                text.push_str(t);
                out.push(StreamEvent::Text(t.to_string()));
            }
            if let Some(tcs) = delta["tool_calls"].as_array() {
                for tc in tcs {
                    let idx = tc["index"].as_u64().unwrap_or(0);
                    let entry = calls.entry(idx).or_default();
                    if let Some(i) = tc["id"].as_str().filter(|s| !s.is_empty()) {
                        entry.0 = i.to_string();
                    }
                    if let Some(n) = tc["function"]["name"].as_str().filter(|s| !s.is_empty()) {
                        entry.1 = n.to_string();
                        out.push(StreamEvent::ToolUseStart {
                            id: entry.0.clone(),
                            name: entry.1.clone(),
                        });
                    }
                    if let Some(a) = tc["function"]["arguments"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                    {
                        entry.2.push_str(a);
                        out.push(StreamEvent::ToolInputDelta {
                            id: entry.0.clone(),
                            partial_json: a.to_string(),
                        });
                    }
                }
            }
            if let Some(f) = choice["finish_reason"].as_str() {
                finish = Some(f.to_string());
            }
            for e in out {
                delivered = true;
                if on_event(e).is_break() {
                    return Err(LlmError::Cancelled);
                }
            }
        }
        if !done && finish.is_none() {
            return Err(fail(
                "stream ended before [DONE] or a finish reason".into(),
                delivered,
            ));
        }
        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(ContentBlock::text(text));
        }
        for (_, (cid, name, args)) in calls {
            content.push(ContentBlock::ToolUse {
                id: cid,
                name,
                input: tool_input(&args)?,
            });
        }
        check_output(
            ChatResponse {
                id,
                model,
                content,
                stop_reason: parse_finish(finish.as_deref()),
                usage: *usage,
                usage_known: *usage != Usage::default(),
                metering: Metering::PerToken,
            },
            req,
        )
    }
}
