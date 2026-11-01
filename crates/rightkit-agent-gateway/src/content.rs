//! Untrusted content contract. Every field carrying text written by an email
//! sender is returned as
//! `{"untrusted":true,"source":"email","text":"...","truncated":false}`, never
//! as a bare string. Bodies are stripped to visible text and capped. Addresses
//! of the user's own accounts are the only plain strings handlers emit.

use serde_json::{json, Value};

/// Characters of body text returned per message.
pub const BODY_CAP_PER_MESSAGE: usize = 20_000;
/// Characters of body text returned per response.
pub const BODY_CAP_PER_RESPONSE: usize = 100_000;
/// Cap for short sender-written fields (subject, names, snippets, addresses).
pub const FIELD_CAP: usize = 1_000;

fn wrap(text: String, truncated: bool) -> Value {
    json!({"untrusted": true, "source": "email", "text": text, "truncated": truncated})
}

/// Drop the search-highlight sentinels (not mail text) from a value.
fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| *c != '\u{1}' && *c != '\u{2}')
        .collect()
}

/// Cut `s` to at most `cap` characters, reporting whether it was cut.
fn cap_chars(s: String, cap: usize) -> (String, bool) {
    match s.char_indices().nth(cap) {
        Some((i, _)) => (s[..i].to_string(), true),
        None => (s, false),
    }
}

/// A short sender-written field.
pub fn untrusted(text: &str) -> Value {
    let (t, cut) = cap_chars(clean(text), FIELD_CAP);
    wrap(t, cut)
}

/// An optional sender-written field; `None` stays `null`.
pub fn untrusted_opt(text: Option<&str>) -> Value {
    text.map_or(Value::Null, untrusted)
}

/// Shared body allowance for one response.
pub struct BodyBudget {
    remaining: usize,
}

impl BodyBudget {
    pub fn new() -> Self {
        Self::with_limit(BODY_CAP_PER_RESPONSE)
    }
    pub fn with_limit(remaining: usize) -> Self {
        BodyBudget { remaining }
    }
    /// A message body: HTML stripped to text, capped per message and by what
    /// is left of the response allowance. `truncated` is true when cut.
    pub fn body(&mut self, raw: &str, is_html: bool) -> Value {
        let text = if is_html {
            crate::host::host().html_to_text(raw)
        } else {
            raw.to_string()
        };
        let allowed = BODY_CAP_PER_MESSAGE.min(self.remaining);
        let (t, cut) = cap_chars(clean(&text), allowed);
        self.remaining -= t.chars().count();
        wrap(t, cut)
    }
}

impl Default for BodyBudget {
    fn default() -> Self {
        Self::new()
    }
}
