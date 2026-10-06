//! OpenAI-compatible chat client (`POST {base}/chat/completions`). Serves OpenAI,
//! OpenRouter, Groq, DeepSeek, Together, Mistral, xAI, Gemini's OpenAI endpoint,
//! and local runtimes (Ollama, LM Studio, llama.cpp server).

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::sync::Arc;

use rightkit_http::{Client, HttpError, Request};
use serde_json::{json, Map, Value};

use crate::credentials::CredentialSource;
use crate::error::LlmError;
use crate::provider::Provider;
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
}

impl OpenAiCompatProvider {
    pub fn new(id: &str, base_url: &str, client: Client, credentials: Arc<dyn CredentialSource>) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
            credentials,
            requires_credential: true,
            extra_headers: Vec::new(),
            max_tokens_field: MaxTokensField::MaxTokens,
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

    fn request(&self, req: &ChatRequest, stream: bool) -> Result<Request, LlmError> {
        let body = build_body(req, stream, self.max_tokens_field);
        let mut http = Request::post(format!("{}/chat/completions", self.base_url))
            .header("Accept", if stream { "text/event-stream" } else { "application/json" });
        match self.credentials.credential(&self.id)? {
            Some(c) => http = http.bearer(c.expose()),
            None if self.requires_credential => return Err(LlmError::MissingCredential(self.id.clone())),
            None => {}
        }
        for (k, v) in &self.extra_headers {
            http = http.header(k.as_str(), v.as_str());
        }
        Ok(http.json(&body)?)
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
        push_message(&mut messages, m);
    }
    body.insert("messages".into(), Value::Array(messages));
    let key = match field {
        MaxTokensField::MaxTokens => "max_tokens",
        MaxTokensField::MaxCompletionTokens => "max_completion_tokens",
    };
    body.insert(key.into(), json!(req.max_tokens));
    if let Some(t) = req.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if !req.stop_sequences.is_empty() {
        body.insert("stop".into(), json!(req.stop_sequences));
    }
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.input_schema}})
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
    if stream {
        body.insert("stream".into(), json!(true));
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }
    Value::Object(body)
}

fn push_message(out: &mut Vec<Value>, m: &Message) {
    match m.role {
        Role::User => {
            let mut text = String::new();
            for b in &m.content {
                match b {
                    ContentBlock::Text { text: t } => text.push_str(t),
                    ContentBlock::ToolResult { tool_use_id, content, .. } => {
                        out.push(json!({"role": "tool", "tool_call_id": tool_use_id, "content": content}));
                    }
                    ContentBlock::ToolUse { .. } => {}
                }
            }
            if !text.is_empty() {
                out.push(json!({"role": "user", "content": text}));
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
                    ContentBlock::ToolResult { .. } => {}
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
    let cached = n(&u["prompt_tokens_details"]["cached_tokens"])
        .max(n(&u["prompt_cache_hit_tokens"])); // DeepSeek-style field
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
    let choice = v
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or_else(|| LlmError::Malformed("missing choices[0]".into()))?;
    let msg = &choice["message"];
    let mut content = Vec::new();
    if let Some(t) = msg["content"].as_str().filter(|t| !t.is_empty()) {
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
    Ok(ChatResponse {
        id: v["id"].as_str().unwrap_or_default().into(),
        model: v["model"].as_str().unwrap_or_default().into(),
        content,
        stop_reason: parse_finish(choice["finish_reason"].as_str()),
        usage: v.get("usage").map(parse_usage).unwrap_or_default(),
    })
}

impl Provider for OpenAiCompatProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn complete(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let resp = self.client.send(&self.request(req, false)?)?;
        let v: Value = resp.json().map_err(|e| LlmError::Malformed(e.to_string()))?;
        parse_response(&v)
    }

    fn stream(
        &self,
        req: &ChatRequest,
        on_event: &mut dyn FnMut(StreamEvent) -> ControlFlow<()>,
    ) -> Result<ChatResponse, LlmError> {
        let response = self.client.stream(&self.request(req, true)?)?;
        let mut id = String::new();
        let mut model = String::new();
        let mut text = String::new();
        let mut finish: Option<String> = None;
        let mut usage = Usage::default();
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
                let message = err["message"].as_str().unwrap_or("provider error").to_string();
                let kind = err["type"].as_str().unwrap_or("error").to_string();
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
                usage = parse_usage(u);
            }
            let Some(choice) = chunk["choices"].get(0) else { continue };
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
                        out.push(StreamEvent::ToolUseStart { id: entry.0.clone(), name: entry.1.clone() });
                    }
                    if let Some(a) = tc["function"]["arguments"].as_str().filter(|s| !s.is_empty()) {
                        entry.2.push_str(a);
                        out.push(StreamEvent::ToolInputDelta { id: entry.0.clone(), partial_json: a.to_string() });
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
            return Err(fail("stream ended before [DONE] or a finish reason".into(), delivered));
        }
        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(ContentBlock::text(text));
        }
        for (_, (cid, name, args)) in calls {
            content.push(ContentBlock::ToolUse { id: cid, name, input: tool_input(&args)? });
        }
        Ok(ChatResponse { id, model, content, stop_reason: parse_finish(finish.as_deref()), usage })
    }
}
