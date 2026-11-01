//! WP-10 — Privacy / redaction helpers for the autonomous workflow control plane.
//!
//! ADR-0021 constraint: secret values MUST NEVER enter canonical hashes, journal
//! payloads, projections, model context, artifacts, diagnostics, telemetry,
//! exports, or effect receipts. This module provides pure, allocation-bounded
//! redaction helpers used by the journal writer (before any persistence), the
//! telemetry exporter (before any span emission), the import/export path
//! (before any serialization), and the run-input admission gate.
//!
//! All functions here are pure: no I/O, no clock, no global state, no
//! `unwrap()` in production code. The matcher functions are total (always
//! return a boolean) so callers can compose them in `SecretPatterns` without
//! needing `Option`.
#![allow(clippy::module_name_repetitions)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ─── Patterns ────────────────────────────────────────────────────────────────

/// The four v1 secret-pattern kinds. The set is closed: any other
/// "secret-shaped" string is the caller's responsibility (they should add a
/// new variant here so the v1 taxonomy stays reviewable).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum SecretKind {
    /// API keys (`sk-…`, `ghp_…`, bearer tokens, etc.).
    ApiKey,
    /// RFC 5322-shaped email addresses.
    Email,
    /// IPv4 dotted-quad addresses.
    Ipv4,
    /// PEM-encoded private keys (`-----BEGIN ... PRIVATE KEY-----`).
    PrivateKeyPem,
}

impl SecretKind {
    /// Stable lowercase identifier suitable for logging, error envelopes, and
    /// the import-side `kind` field in `SecretPattern`.
    pub const fn as_str(&self) -> &'static str {
        match self {
            SecretKind::ApiKey => "api_key",
            SecretKind::Email => "email",
            SecretKind::Ipv4 => "ipv4",
            SecretKind::PrivateKeyPem => "private_key_pem",
        }
    }
}

/// One pluggable secret pattern. The matcher is a plain function pointer
/// (`fn`) so the whole `SecretPatterns` collection stays `Clone + Debug`
/// without external state. Note: `fn` pointers are not `Serialize`, so
/// `SecretPattern` derives Debug/Clone/Copy/PartialEq/Eq only.
#[derive(Debug, Clone, Copy)]
pub struct SecretPattern {
    /// Discriminator; matches the literal string passed to
    /// [`SecretPatterns::with_kind`] and the on-the-wire `SecretPattern` JSON
    /// shape in the import schema.
    pub kind: &'static str,
    /// Total matcher: returns true when `s` contains a substring that looks
    /// like the named secret. Implementations must be pure.
    pub matcher: fn(&str) -> bool,
}

impl PartialEq for SecretPattern {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && std::ptr::fn_addr_eq(self.matcher, other.matcher)
    }
}

impl Eq for SecretPattern {}

// ─── Default matchers ────────────────────────────────────────────────────────

/// API-key matcher: recognises `sk-…`, `ghp_…`, `xox[baprs]-…`, and
/// bearer-style `Bearer …` / `Token …` prefixes. The matcher also flags
/// `key=…` / `api_key=…` assignments to catch the most common embed form.
///
/// `Bearer` / `bearer` matching is case-insensitive and does not require a
/// trailing space — `Authorization: Bearer <token>` and an embed like
/// `"bearer-token-here"` both flag.
pub fn matches_api_key(s: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "sk-", "sk_", "ghp_", "gho_", "ghs_", "ghu_", "ghr_", "xoxb-", "xoxa-", "xoxp-", "xoxr-",
        "xoxs-",
    ];
    for p in PREFIXES {
        if s.contains(p) {
            return true;
        }
    }
    // Lower-case once for every case-insensitive check below. Cheap relative
    // to the rest of the matcher and keeps each branch obvious.
    let lower = s.to_ascii_lowercase();
    if lower.contains("bearer") {
        return true;
    }
    if lower.contains("token=") {
        return true;
    }
    if lower.contains("api_key=") || lower.contains("apikey=") {
        return true;
    }
    false
}

/// Email matcher: deliberately conservative (no RFC 5322 whitespace, no
/// quoted local parts). A local-part + `@` + dotted-domain shape is enough
/// for the redaction tier; precise validation is the receiver's job.
///
/// Walks every `@` in the input and tries to extract a valid local part to
/// the left and a valid dotted domain to the right. This means emails
/// embedded in prose (`"ping alice@example.com please"`) are matched even
/// though there is whitespace before the local part.
pub fn matches_email(s: &str) -> bool {
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'@' {
            continue;
        }
        // Local part: walk back to the first whitespace or `@` (exclusive).
        let mut start = i;
        while start > 0 {
            let prev = bytes[start - 1];
            if prev.is_ascii_whitespace() || prev == b'@' {
                break;
            }
            start -= 1;
        }
        // Domain: walk forward to the first whitespace or `@` (exclusive).
        let mut end = i + 1;
        while end < bytes.len() {
            let nxt = bytes[end];
            if nxt.is_ascii_whitespace() || nxt == b'@' {
                break;
            }
            end += 1;
        }
        // Both halves must be non-empty (so `@example.com` and `alice@` reject).
        if start >= i || end <= i + 1 {
            continue;
        }
        // Local part must contain at least one non-whitespace, non-`@` char —
        // already guaranteed by the backward scan above, but we re-check for
        // paranoia against any future refactor that lets a multi-byte char
        // slip through.
        let local = &bytes[start..i];
        if local.iter().any(|b| b.is_ascii_whitespace() || *b == b'@') {
            continue;
        }
        // Domain must contain at least one `.` and only label-like chars.
        let domain = &bytes[i + 1..end];
        let mut dot = false;
        let mut valid = true;
        for &b in domain {
            match b {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' => {}
                b'.' => dot = true,
                _ => {
                    valid = false;
                    break;
                }
            }
        }
        if valid && dot {
            return true;
        }
    }
    false
}

/// IPv4 matcher: four dotted decimal octets in `[0, 255]`.
///
/// Scans the input for any maximal run of digits and `.` characters and
/// validates the resulting token as a dotted-quad. This lets the matcher
/// recognise addresses embedded in prose (`"from 10.0.0.1 to 10.0.0.2"`)
/// without flagging ordinary words that happen to contain a dot.
pub fn matches_ipv4(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Find the end of the current candidate token (digits and `.` only).
        let mut end = i;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
            end += 1;
        }
        if end > i {
            // ASCII-only (digits + `.`), so `i` and `end` are valid UTF-8
            // boundaries and `s.get(..)` cannot fail here.
            if let Some(token) = s.get(i..end) {
                if is_dotted_quad(token) {
                    return true;
                }
            }
        }
        // Skip the non-token char we stopped at (or restart past the run).
        i = end + 1;
    }
    false
}

/// Validate that a token (already stripped of any non `0-9.` chars) is a
/// strict dotted-quad: exactly four octets, each 1–3 digits, in `[0, 255]`,
/// with no leading zero.
fn is_dotted_quad(token: &str) -> bool {
    let mut octets = token.split('.');
    let mut count = 0usize;
    for raw in octets.by_ref() {
        count += 1;
        if count > 4 {
            return false;
        }
        if raw.is_empty() || raw.len() > 3 {
            return false;
        }
        let Ok(n) = raw.parse::<u32>() else {
            return false;
        };
        if n > 255 {
            return false;
        }
        // Reject leading zeros to avoid octal-ish confusion.
        if raw.len() > 1 && raw.starts_with('0') {
            return false;
        }
    }
    count == 4
}

/// PEM private-key matcher: catches the standard block headers used by
/// OpenSSL, RSA, EC, PKCS8, and OpenSSH.
pub fn matches_private_key_pem(s: &str) -> bool {
    s.contains("-----BEGIN ") && s.contains("PRIVATE KEY-----")
}

/// The canonical `SecretPattern` for the four v1 kinds. Callers that need
/// more may compose their own list on top.
pub const DEFAULT_PATTERNS: &[SecretPattern] = &[
    SecretPattern {
        kind: "api_key",
        matcher: matches_api_key,
    },
    SecretPattern {
        kind: "email",
        matcher: matches_email,
    },
    SecretPattern {
        kind: "ipv4",
        matcher: matches_ipv4,
    },
    SecretPattern {
        kind: "private_key_pem",
        matcher: matches_private_key_pem,
    },
];

// ─── Pattern set ─────────────────────────────────────────────────────────────

/// A closed set of [`SecretPattern`]s. Build with [`SecretPatterns::defaults`]
/// or compose your own with [`SecretPatterns::with_kind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretPatterns {
    patterns: Vec<SecretPattern>,
}

impl SecretPatterns {
    /// All four v1 defaults.
    pub fn defaults() -> Self {
        Self {
            patterns: DEFAULT_PATTERNS.to_vec(),
        }
    }

    /// Start with defaults and append `extra`.
    #[must_use]
    pub fn with_extra(extra: SecretPattern) -> Self {
        let mut s = Self::defaults();
        s.patterns.push(extra);
        s
    }

    /// Construct a set containing exactly one named pattern. Useful in
    /// tests and in policy builders that want a narrow redaction surface.
    #[must_use]
    pub fn with_kind(kind: SecretKind, matcher: fn(&str) -> bool) -> Self {
        let kind_str = match kind {
            SecretKind::ApiKey => "api_key",
            SecretKind::Email => "email",
            SecretKind::Ipv4 => "ipv4",
            SecretKind::PrivateKeyPem => "private_key_pem",
        };
        Self {
            patterns: vec![SecretPattern {
                kind: kind_str,
                matcher,
            }],
        }
    }

    /// Borrow the underlying patterns.
    pub fn as_slice(&self) -> &[SecretPattern] {
        &self.patterns
    }

    /// True when any registered pattern matches `s`. Short-circuits.
    pub fn any_match(&self, s: &str) -> bool {
        self.patterns.iter().any(|p| (p.matcher)(s))
    }
}

impl Default for SecretPatterns {
    fn default() -> Self {
        Self::defaults()
    }
}

// ─── Redaction ───────────────────────────────────────────────────────────────

/// Replacement token written in place of any matched substring.
pub const REDACTED_TOKEN: &str = "[REDACTED]";

/// In-place redact any string-typed value (and every string nested under
/// arrays / objects) using the registered patterns. Non-string scalars
/// (`number`, `bool`, `null`) are left untouched; only string segments can
/// leak secrets in practice.
pub fn redact_node_output(value: &mut Value, secrets: &SecretPatterns) {
    redact_in_place(value, secrets);
}

/// Convenience wrapper used by the admission gate: same semantics as
/// [`redact_node_output`] but takes a borrowed slice so callers that build
/// a transient pattern set on the hot path don't have to allocate a
/// [`SecretPatterns`] first.
pub fn redact_run_input(value: &mut Value, patterns: &[SecretPattern]) {
    let set = SecretPatterns {
        patterns: patterns.to_vec(),
    };
    redact_in_place(value, &set);
}

fn redact_in_place(value: &mut Value, secrets: &SecretPatterns) {
    match value {
        Value::String(s) => {
            if secrets.any_match(s) {
                redact_string_segments(s, secrets);
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_in_place(item, secrets);
            }
        }
        Value::Object(map) => {
            for (_k, v) in map {
                redact_in_place(v, secrets);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn redact_string_segments(s: &mut String, secrets: &SecretPatterns) {
    // Walk char-by-char and rebuild. Allocation-bounded: each matcher
    // returns a `bool` over the whole string, so we run matchers once and
    // then split on the smallest token that matches.
    for p in secrets.as_slice() {
        let matcher = p.matcher;
        if !matcher(s) {
            continue;
        }
        // Find the largest contiguous ASCII run that contains the matcher
        // hit and replace it. We do this by walking the string and
        // collapsing every maximal non-whitespace segment that matches.
        let mut output = String::with_capacity(s.len());
        let mut buf = String::new();
        for ch in s.chars() {
            if ch.is_whitespace() {
                if matcher(&buf) {
                    output.push_str(REDACTED_TOKEN);
                } else {
                    output.push_str(&buf);
                }
                buf.clear();
                output.push(ch);
            } else {
                buf.push(ch);
            }
        }
        if matcher(&buf) {
            output.push_str(REDACTED_TOKEN);
        } else {
            output.push_str(&buf);
        }
        *s = output;
        // Once we've replaced once per pattern kind, the remaining content
        // may still match other kinds; the outer loop handles that.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn api_key_matches_common_prefixes() {
        assert!(matches_api_key("Authorization: Bearer sk-abcdef"));
        assert!(matches_api_key("token=ghp_abcdef123"));
        assert!(matches_api_key("api_key=hunter2"));
        assert!(!matches_api_key("plain english sentence"));
    }

    #[test]
    fn email_matches_minimal_shape() {
        assert!(matches_email("contact alice@example.com today"));
        assert!(!matches_email("no at sign here"));
        assert!(!matches_email("@example.com"));
        assert!(!matches_email("alice@"));
        assert!(!matches_email("a@b")); // no dot
    }

    #[test]
    fn ipv4_matches_dotted_quad() {
        assert!(matches_ipv4("connect to 10.0.0.1 now"));
        assert!(matches_ipv4("0.0.0.0"));
        assert!(!matches_ipv4("256.0.0.1"));
        assert!(!matches_ipv4("1.2.3"));
        assert!(!matches_ipv4("01.2.3.4")); // leading zero
    }

    #[test]
    fn pem_matches_known_headers() {
        assert!(matches_private_key_pem(concat!(
            "--",
            "---BEGIN RSA PRIVATE KEY-----\nABCD\n-----END RSA PRIVATE KEY-----"
        )));
        assert!(matches_private_key_pem(concat!(
            "--",
            "---BEGIN OPENSSH PRIVATE KEY-----"
        )));
        assert!(!matches_private_key_pem("not a key"));
    }

    #[test]
    fn default_patterns_cover_all_kinds() {
        let p = SecretPatterns::defaults();
        assert_eq!(p.as_slice().len(), 4);
        assert!(p.any_match("contact alice@example.com"));
        assert!(p.any_match("key=ghp_abc"));
        assert!(p.any_match("host 10.0.0.5"));
        assert!(p.any_match(concat!("--", "---BEGIN PRIVATE KEY-----")));
        assert!(!p.any_match("nothing to redact"));
    }

    #[test]
    fn redacts_api_key_inside_string() {
        let mut v = json!({ "auth": "Bearer sk-secret-key" });
        redact_node_output(&mut v, &SecretPatterns::defaults());
        let s = v["auth"].as_str().expect("string");
        assert!(!s.contains("sk-secret-key"), "got: {s}");
        assert!(s.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_email_inside_nested_object() {
        let mut v = json!({
            "level1": { "level2": ["ping alice@example.com please"] }
        });
        redact_node_output(&mut v, &SecretPatterns::defaults());
        let s = v["level1"]["level2"][0].as_str().expect("string");
        assert!(!s.contains("alice@example.com"), "got: {s}");
        assert!(s.contains("[REDACTED]"));
    }

    #[test]
    fn redacts_ipv4_keeps_untouched_tokens() {
        let mut v = json!({ "trace": "from 10.0.0.1 to 10.0.0.2" });
        redact_node_output(&mut v, &SecretPatterns::defaults());
        let s = v["trace"].as_str().expect("string");
        assert!(!s.contains("10.0.0.1"), "got: {s}");
        assert!(!s.contains("10.0.0.2"), "got: {s}");
        assert!(s.contains("[REDACTED]"));
    }

    #[test]
    fn redact_run_input_redacts_with_borrowed_slice() {
        let mut v = json!({ "k": "email me at bob@example.com" });
        redact_run_input(&mut v, DEFAULT_PATTERNS);
        let s = v["k"].as_str().expect("string");
        assert!(!s.contains("bob@example.com"), "got: {s}");
        assert!(s.contains("[REDACTED]"));
    }

    #[test]
    fn leaves_non_strings_alone() {
        let mut v = json!({ "n": 42, "b": true, "x": null });
        redact_node_output(&mut v, &SecretPatterns::defaults());
        assert_eq!(v["n"], json!(42));
        assert_eq!(v["b"], json!(true));
        assert_eq!(v["x"], json!(null));
    }

    #[test]
    fn secret_patterns_default_constructor_matches_arc() {
        let a = SecretPatterns::defaults();
        let b = SecretPatterns::default();
        assert_eq!(a, b);
    }
}
