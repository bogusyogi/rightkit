//! Untrusted content. Text a third party wrote (a mail sender, a web page, another AI) leaves the
//! gateway marked as such, never as a bare string an agent could read as its instructions:
//!
//! * going out to an agent as data, wrapped:
//!   `{"untrusted":true,"source":"<source>","text":"...","truncated":false}` ([`untrusted`],
//!   [`Budget`]), capped by characters;
//! * going into a chat or a prompt as text, stamped: a fixed first line saying who it is from
//!   and that it is unverified ([`stamped`]).

use serde_json::{json, Value};

/// The wrapped form of `text` from `source`.
pub fn wrap(source: &str, text: String, truncated: bool) -> Value {
    json!({"untrusted": true, "source": source, "text": text, "truncated": truncated})
}

/// Drop the U+0001 and U+0002 sentinels (search-highlight marks, not text) from a value.
pub fn clean(s: &str) -> String {
    s.chars()
        .filter(|c| *c != '\u{1}' && *c != '\u{2}')
        .collect()
}

/// Cut `s` to at most `cap` characters, reporting whether it was cut.
pub fn cap_chars(s: String, cap: usize) -> (String, bool) {
    match s.char_indices().nth(cap) {
        Some((i, _)) => (s[..i].to_string(), true),
        None => (s, false),
    }
}

/// A short third-party field, cleaned and capped at `cap` characters, wrapped.
pub fn untrusted(source: &str, text: &str, cap: usize) -> Value {
    let (t, cut) = cap_chars(clean(text), cap);
    wrap(source, t, cut)
}

/// A shared allowance of characters for one response: each item is capped by its own limit and
/// by what is left of the allowance.
pub struct Budget {
    per_item: usize,
    remaining: usize,
}

impl Budget {
    pub fn new(per_item: usize, per_response: usize) -> Self {
        Budget {
            per_item,
            remaining: per_response,
        }
    }

    /// One item of text from `source`, cleaned, capped and wrapped; `truncated` is true when cut.
    pub fn take(&mut self, source: &str, text: &str) -> Value {
        let allowed = self.per_item.min(self.remaining);
        let (t, cut) = cap_chars(clean(text), allowed);
        self.remaining -= t.chars().count();
        wrap(source, t, cut)
    }

    /// Characters left in the allowance.
    pub fn remaining(&self) -> usize {
        self.remaining
    }
}

/// `text` with the `stamp` line first and a blank line after it.
pub fn stamped(stamp: &str, text: &str) -> String {
    format!("{stamp}\n\n{text}")
}
