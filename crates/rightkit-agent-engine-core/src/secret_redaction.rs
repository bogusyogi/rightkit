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
use rightkit_agent_tools::coding_tools::ToolRegistry;
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
    ) -> rightkit_agent_tools::coding_tools::registry::PromotedToolState {
        self.inner.promoted_tool_state_snapshot()
    }

    fn active_route_label(&self) -> Option<String> {
        self.inner.active_route_label()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ScriptedLlm;

    // Fake patterns only; assembled at runtime so no literal looks like a key.
    fn fake_sk() -> String {
        format!("sk-{}", "FAKE".repeat(8))
    }
    fn fake_ghp() -> String {
        format!("ghp_{}", "x1".repeat(15))
    }
    fn fake_aws() -> String {
        format!("AKIA{}", "FAKEFAKEFAKEFAKE")
    }

    #[test]
    fn redacts_known_shapes_and_restores_exactly() {
        let r = SecretRedactor::new();
        let text = format!(
            "key {} and {} then {} end",
            fake_sk(),
            fake_ghp(),
            fake_aws()
        );
        let red = r.redact(&text);
        assert!(!red.contains(&fake_sk()));
        assert!(!red.contains(&fake_ghp()));
        assert!(!red.contains(&fake_aws()));
        assert!(red.contains("[[CR_SECRET_APIKEY_1]]"));
        assert_eq!(r.restore(&red), text);
    }

    #[test]
    fn placeholders_are_stable_and_redaction_is_idempotent() {
        let r = SecretRedactor::new();
        let a = r.redact(&format!("a {}", fake_sk()));
        let b = r.redact(&format!("b {}", fake_sk()));
        assert_eq!(a.trim_start_matches("a "), b.trim_start_matches("b "));
        assert_eq!(r.redact(&a), a);
    }

    #[test]
    fn redacts_credential_assignments_and_json_fields() {
        let r = SecretRedactor::new();
        let text = "DB_PASSWORD=hunter2hunter2 and {\"api_key\": \"fake-value-12345\"}";
        let red = r.redact(text);
        assert!(!red.contains("hunter2hunter2"));
        assert!(!red.contains("fake-value-12345"));
        assert!(red.contains("DB_PASSWORD="));
        assert_eq!(r.restore(&red), text);
    }

    #[test]
    fn short_assignment_values_and_plain_text_are_untouched() {
        let r = SecretRedactor::new();
        let t = "TOKEN=abc and the sk-short prefix, plus ordinary prose.";
        assert_eq!(r.redact(t), t);
    }

    #[test]
    fn redacts_private_key_block_and_bearer_and_jwt() {
        let r = SecretRedactor::new();
        let pem = "-----BEGIN PRIVATE KEY-----\nFAKEFAKEFAKE\n-----END PRIVATE KEY-----";
        let jwt = format!(
            "eyJ{}.{}.{}",
            "a".repeat(12),
            "b".repeat(12),
            "c".repeat(12)
        );
        let text = format!("{pem}\nAuthorization: Bearer fakebearertoken123456\n{jwt}");
        let red = r.redact(&text);
        assert!(!red.contains("FAKEFAKEFAKE"));
        assert!(!red.contains("fakebearertoken123456"));
        assert!(!red.contains(&jwt));
        assert_eq!(r.restore(&red), text);
    }

    #[test]
    fn registered_known_secret_is_redacted_anywhere() {
        let r = SecretRedactor::new();
        r.register_known("plainfakesecret");
        let red = r.redact("use plainfakesecret here");
        assert!(!red.contains("plainfakesecret"));
        assert_eq!(r.restore(&red), "use plainfakesecret here");
    }

    #[test]
    fn debug_never_prints_values() {
        let r = SecretRedactor::new();
        r.redact(&fake_sk());
        let dbg = format!("{r:?}");
        assert!(!dbg.contains("FAKE"));
        assert!(dbg.contains("secrets: 1"));
    }

    #[test]
    fn restore_leaves_unknown_placeholders() {
        let r = SecretRedactor::new();
        assert_eq!(r.restore("[[CR_SECRET_X_99]]"), "[[CR_SECRET_X_99]]");
    }

    fn user(content: String) -> TranscriptMessage {
        TranscriptMessage::User {
            id: "u1".into(),
            created_at: "t".into(),
            content,
            trust: None,
            pinned: false,
            attachments: vec![],
        }
    }

    #[test]
    fn messages_are_redacted_without_mutating_input() {
        let r = SecretRedactor::new();
        let msgs = vec![user(format!("token {}", fake_ghp()))];
        let out = r.redact_messages(&msgs);
        match (&msgs[0], &out[0]) {
            (
                TranscriptMessage::User { content: a, .. },
                TranscriptMessage::User { content: b, .. },
            ) => {
                assert!(a.contains(&fake_ghp()));
                assert!(!b.contains(&fake_ghp()));
            }
            _ => panic!("variant changed"),
        }
    }

    #[tokio::test]
    async fn redacting_llm_hides_secret_from_provider_and_restores_tool_args() {
        let secret = fake_sk();
        let redactor = Arc::new(SecretRedactor::new());
        let placeholder = redactor.redact(&secret);
        let scripted = Arc::new(ScriptedLlm::new(vec![LlmResponse::text(format!(
            "using {placeholder}"
        ))
        .with_tool_call(ToolCall {
            id: "c1".into(),
            name: "bash".into(),
            arguments_json: format!("{{\"cmd\":\"echo {placeholder}\"}}"),
        })]));
        let llm = RedactingLlm::new(scripted, redactor);
        let resp = llm
            .complete(&[user(format!("my key is {secret}"))])
            .await
            .unwrap();
        assert_eq!(resp.text, format!("using {secret}"));
        assert!(resp.tool_calls[0].arguments_json.contains(&secret));
    }

    #[test]
    fn stream_restores_placeholder_split_across_chunks() {
        let r = SecretRedactor::new();
        let secret = fake_sk();
        let ph = r.redact(&secret);
        let (a, b) = ph.split_at(ph.len() / 2);
        let mut carry = String::new();
        let mut emitted = String::new();
        for chunk in [format!("go {a}"), format!("{b} done")] {
            for ev in map_stream_item(&r, &mut carry, Ok(LlmStreamEvent::Text(chunk))) {
                if let Ok(LlmStreamEvent::Text(t)) = ev {
                    emitted.push_str(&t);
                }
            }
        }
        for ev in map_stream_item(&r, &mut carry, Ok(LlmStreamEvent::Finished(None))) {
            if let Ok(LlmStreamEvent::Text(t)) = ev {
                emitted.push_str(&t);
            }
        }
        assert_eq!(emitted, format!("go {secret} done"));
    }
}
