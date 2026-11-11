//! Untrusted content contract, on the core's wrapper (`rightkit_agent_gateway::content`). Every
//! field carrying text written by an email sender is returned as
//! `{"untrusted":true,"source":"email","text":"...","truncated":false}`, never as a bare string.
//! Bodies are stripped to visible text and capped. Addresses of the user's own accounts are the
//! only plain strings handlers emit.

use rightkit_agent_gateway::content::{self as core, Budget};
use serde_json::Value;

/// The `source` every wrapped field of this adapter names.
pub const SOURCE: &str = "email";
/// Characters of body text returned per message.
pub const BODY_CAP_PER_MESSAGE: usize = 20_000;
/// Characters of body text returned per response.
pub const BODY_CAP_PER_RESPONSE: usize = 100_000;
/// Cap for short sender-written fields (subject, names, snippets, addresses).
pub const FIELD_CAP: usize = 1_000;

/// A short sender-written field.
pub fn untrusted(text: &str) -> Value {
    core::untrusted(SOURCE, text, FIELD_CAP)
}

/// An optional sender-written field; `None` stays `null`.
pub fn untrusted_opt(text: Option<&str>) -> Value {
    text.map_or(Value::Null, untrusted)
}

/// Shared body allowance for one response.
pub struct BodyBudget(Budget);

impl BodyBudget {
    pub fn new() -> Self {
        Self::with_limit(BODY_CAP_PER_RESPONSE)
    }
    pub fn with_limit(remaining: usize) -> Self {
        BodyBudget(Budget::new(BODY_CAP_PER_MESSAGE, remaining))
    }
    /// A message body: HTML stripped to text, capped per message and by what
    /// is left of the response allowance. `truncated` is true when cut.
    pub fn body(&mut self, raw: &str, is_html: bool) -> Value {
        let text = if is_html {
            crate::host::host().html_to_text(raw)
        } else {
            raw.to_string()
        };
        self.0.take(SOURCE, &text)
    }
}

impl Default for BodyBudget {
    fn default() -> Self {
        Self::new()
    }
}
