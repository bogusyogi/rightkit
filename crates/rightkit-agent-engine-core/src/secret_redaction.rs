//! A195: reversible secret redaction on the model boundary.
//!
//! Secrets (API keys, tokens, private keys, `NAME=value` credential
//! assignments) found in transcript text are replaced with stable
//! `[[CR_SECRET_<KIND>_<n>]]` placeholders before a request reaches the
//! provider, and the placeholders are restored in the provider's response so
//! tool calls and prose the user sees locally carry the real value again.
//!
//! Invariants:
//! - The raw value lives only in this process's memory (inside
//!   [`SecretRedactor`]); `Debug` prints counts, never values, and nothing here
//!   logs or serializes a value.
//! - Placeholders are stable within a session: the same value always maps to
//!   the same placeholder, so redacting an already-restored history again is
//!   idempotent and prompt-cache prefixes stay byte-identical.
//! - Anything that persists text must call [`SecretRedactor::redact`] (or
//!   [`SecretRedactor::redact_messages`]) first; restore is for local display
//!   and local tool execution only.

use crate::llm::{LlmClient, LlmRequestMetadata, LlmResponse, LlmResponseStream, LlmStreamEvent};
use async_trait::async_trait;
use futures::StreamExt;
use rightkit_agent_engine_protocol::{ToolCall, TranscriptMessage};
use rightkit_agent_coding_tools::ToolRegistry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rightkit_agent_engine_protocol::secret_scan::{
    find_secrets, SECRET_PLACEHOLDER_CLOSE as PLACEHOLDER_CLOSE,
    SECRET_PLACEHOLDER_OPEN as PLACEHOLDER_OPEN,
};

const MAX_PLACEHOLDER_LEN: usize = 64;

#[derive(Default)]
struct State {
    by_value: HashMap<String, String>,
    by_placeholder: HashMap<String, String>,
    known: Vec<String>,
    counter: usize,
}

/// Session-scoped reversible redactor. Cheap to share behind an `Arc`.
#[derive(Default)]
pub struct SecretRedactor {
    state: Mutex<State>,
}

impl std::fmt::Debug for SecretRedactor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.state.lock().map(|s| s.by_value.len()).unwrap_or(0);
        write!(f, "SecretRedactor {{ secrets: {n} }}")
    }
}

/// Index at which a possibly-incomplete placeholder begins at the tail of
/// `s`, or `s.len()` when the tail is safe to emit.
fn hold_back_index(s: &str) -> usize {
    if let Some(i) = s.rfind("[[") {
        if !s[i..].contains(PLACEHOLDER_CLOSE) && s.len() - i <= MAX_PLACEHOLDER_LEN {
            return i;
        }
    }
    if s.ends_with('[') {
        return s.len() - 1;
    }
    s.len()
}

impl SecretRedactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an exact value (for example a configured session secret) that
    /// must always be redacted regardless of its shape. Short values (< 6
    /// bytes) are ignored: they would shred ordinary text.
    pub fn register_known(&self, value: &str) {
        if value.len() < 6 {
            return;
        }
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !st.known.iter().any(|k| k == value) {
            st.known.push(value.to_string());
            st.known.sort_by_key(|k| std::cmp::Reverse(k.len()));
        }
    }

    fn placeholder(&self, kind: &str, value: &str) -> String {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = st.by_value.get(value) {
            return p.clone();
        }
        st.counter += 1;
        let p = format!("{PLACEHOLDER_OPEN}{kind}_{}{PLACEHOLDER_CLOSE}", st.counter);
        st.by_value.insert(value.to_string(), p.clone());
        st.by_placeholder.insert(p.clone(), value.to_string());
        p
    }

    /// Replace every detected secret in `text` with its placeholder.
    pub fn redact(&self, text: &str) -> String {
        let text = self.redact_known(text);
        let spans = find_secrets(&text);
        if spans.is_empty() {
            return text;
        }
        let mut out = String::with_capacity(text.len());
        let mut copied = 0usize;
        for sp in spans {
            out.push_str(&text[copied..sp.start]);
            out.push_str(&self.placeholder(sp.kind, &text[sp.start..sp.end]));
            copied = sp.end;
        }
        out.push_str(&text[copied..]);
        out
    }

    fn redact_known(&self, text: &str) -> String {
        let known = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.known.clone()
        };
        let mut out = text.to_string();
        for k in known {
            if out.contains(&k) {
                let p = self.placeholder("KNOWN", &k);
                out = out.replace(&k, &p);
            }
        }
        out
    }

    /// Replace every known placeholder in `text` with its raw value. Unknown
    /// placeholders are left untouched.
    pub fn restore(&self, text: &str) -> String {
        if !text.contains(PLACEHOLDER_OPEN) {
            return text.to_string();
        }
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(i) = rest.find(PLACEHOLDER_OPEN) {
            out.push_str(&rest[..i]);
            let after = &rest[i..];
            match after.find(PLACEHOLDER_CLOSE) {
                Some(e) => {
                    let ph = &after[..e + PLACEHOLDER_CLOSE.len()];
                    match st.by_placeholder.get(ph) {
                        Some(raw) => out.push_str(raw),
                        None => out.push_str(ph),
                    }
                    rest = &after[e + PLACEHOLDER_CLOSE.len()..];
                }
                None => {
                    out.push_str(after);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// Redact every text-bearing field of the transcript messages. The input
    /// is untouched; a redacted copy is returned.
    pub fn redact_messages(&self, messages: &[TranscriptMessage]) -> Vec<TranscriptMessage> {
        messages
            .iter()
            .cloned()
            .map(|mut m| {
                match &mut m {
                    TranscriptMessage::System { content, .. }
                    | TranscriptMessage::User { content, .. } => {
                        *content = self.redact(content);
                    }
                    TranscriptMessage::Assistant {
                        content,
                        reasoning_content,
                        tool_calls,
                        ..
                    } => {
                        *content = self.redact(content);
                        if let Some(r) = reasoning_content {
                            *r = self.redact(r);
                        }
                        for c in tool_calls.iter_mut() {
                            c.arguments_json = self.redact(&c.arguments_json);
                        }
                    }
                    TranscriptMessage::Tool { summary, .. }
                    | TranscriptMessage::Compact { summary, .. }
                    | TranscriptMessage::Fold { summary, .. } => {
                        *summary = self.redact(summary);
                    }
                }
                m
            })
            .collect()
    }

    fn restore_call(&self, mut call: ToolCall) -> ToolCall {
        call.arguments_json = self.restore(&call.arguments_json);
        call
    }

    fn restore_response(&self, mut r: LlmResponse) -> LlmResponse {
        r.text = self.restore(&r.text);
        r.reasoning = r.reasoning.map(|t| self.restore(&t));
        r.tool_calls = r
            .tool_calls
            .into_iter()
            .map(|c| self.restore_call(c))
            .collect();
        r
    }
}

fn flush(r: &SecretRedactor, carry: &mut String, out: &mut Vec<Result<LlmStreamEvent, String>>) {
    if !carry.is_empty() {
        out.push(Ok(LlmStreamEvent::Text(r.restore(&std::mem::take(carry)))));
    }
}

fn map_stream_item(
    r: &SecretRedactor,
    carry: &mut String,
    item: Result<LlmStreamEvent, String>,
) -> Vec<Result<LlmStreamEvent, String>> {
    let mut out = Vec::new();
    match item {
        Err(e) => {
            flush(r, carry, &mut out);
            out.push(Err(e));
        }
        Ok(LlmStreamEvent::Text(t)) => {
            carry.push_str(&t);
            let idx = hold_back_index(carry);
            let emit: String = carry[..idx].to_string();
            *carry = carry[idx..].to_string();
            if !emit.is_empty() {
                out.push(Ok(LlmStreamEvent::Text(r.restore(&emit))));
            }
        }
        Ok(LlmStreamEvent::Reasoning(t)) => out.push(Ok(LlmStreamEvent::Reasoning(r.restore(&t)))),
        Ok(LlmStreamEvent::ToolCall(c)) => {
            flush(r, carry, &mut out);
            out.push(Ok(LlmStreamEvent::ToolCall(r.restore_call(c))));
        }
        Ok(LlmStreamEvent::Response(resp)) => {
            flush(r, carry, &mut out);
            out.push(Ok(LlmStreamEvent::Response(r.restore_response(resp))));
        }
        Ok(ev @ (LlmStreamEvent::Finished(_) | LlmStreamEvent::OutputLimitCheckpoint { .. })) => {
            flush(r, carry, &mut out);
            out.push(Ok(ev));
        }
        Ok(other) => out.push(Ok(other)),
    }
    out
}

/// [`LlmClient`] decorator: the wrapped provider only ever sees placeholders;
/// everything it returns is restored before the engine sees it.
pub struct RedactingLlm {
    inner: Arc<dyn LlmClient>,
    redactor: Arc<SecretRedactor>,
}

impl RedactingLlm {
    pub fn new(inner: Arc<dyn LlmClient>, redactor: Arc<SecretRedactor>) -> Self {
        Self { inner, redactor }
    }
}

#[async_trait]
impl LlmClient for RedactingLlm {
    async fn complete(&self, messages: &[TranscriptMessage]) -> Result<LlmResponse, String> {
        let redacted = self.redactor.redact_messages(messages);
        let resp = self.inner.complete(&redacted).await?;
        Ok(self.redactor.restore_response(resp))
    }

    async fn complete_with_metadata(
        &self,
        messages: &[TranscriptMessage],
        metadata: &LlmRequestMetadata,
    ) -> Result<LlmResponse, String> {
        let redacted = self.redactor.redact_messages(messages);
        let resp = self
            .inner
            .complete_with_metadata(&redacted, metadata)
            .await?;
        Ok(self.redactor.restore_response(resp))
    }

    async fn stream_with_metadata(
        &self,
        messages: &[TranscriptMessage],
        metadata: &LlmRequestMetadata,
    ) -> Result<LlmResponseStream, String> {
        let redacted = self.redactor.redact_messages(messages);
        let inner = self.inner.stream_with_metadata(&redacted, metadata).await?;
        let r = self.redactor.clone();
        let mapped = inner
            .scan(String::new(), move |carry, item| {
                futures::future::ready(Some(map_stream_item(&r, carry, item)))
            })
            .flat_map(futures::stream::iter);
        Ok(Box::pin(mapped))
    }

    fn image_budget_policy(&self) -> crate::image_eviction::ImageBudgetPolicy {
        self.inner.image_budget_policy()
    }

    fn image_request_accounting(
        &self,
        messages: &[TranscriptMessage],
        metadata: &LlmRequestMetadata,
    ) -> crate::image_eviction::ImageRequestAccounting {
        self.inner
            .image_request_accounting(&self.redactor.redact_messages(messages), metadata)
    }

    fn replace_tool_registry(&self, tools: Arc<ToolRegistry>) {
        self.inner.replace_tool_registry(tools)
    }

    fn promoted_tool_state_snapshot(
        &self,
    ) -> rightkit_agent_coding_tools::registry::PromotedToolState {
        self.inner.promoted_tool_state_snapshot()
    }

    fn active_route_label(&self) -> Option<String> {
        self.inner.active_route_label()
    }
}
