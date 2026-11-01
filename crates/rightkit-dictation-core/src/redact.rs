//! Redaction of diagnostic events and engine payloads before they reach logs or telemetry.
//!
//! Content fields (transcripts, prompts, selections, window titles) are replaced with a marker;
//! secret-shaped tokens under any key are masked; free-text messages go through
//! `rightkit_logs::scrub_text` plus the payload floor below.

use serde_json::Value;

const DIAGNOSTIC_REDACTION_MARKER: &str = "[redacted:diagnostics]";

/// Remove user content, target-window metadata, and secret-shaped values from
/// an ordinary local diagnostic event before it reaches disk or stderr.
///
/// This is unconditional: privacy is the baseline, not an optional
/// mode. AI-payload logging is a separate, explicit opt-in path and does not
/// call this function.
pub fn redact_diagnostic_event(mut value: Value) -> Value {
    redact_diagnostic_value(&mut value);
    value
}

/// Redact provider credentials from locally retained AI payloads while keeping
/// ordinary transcript fields readable. This stays separate from ordinary
/// diagnostics, whose content fields are always blanket-redacted.
pub fn redact_payload_value(value: Value, max_text_chars: usize) -> Value {
    match value {
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| {
                    let value = if payload_secret_key(&key) {
                        Value::String("[redacted]".into())
                    } else {
                        redact_payload_value(value, max_text_chars)
                    };
                    (key, value)
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|value| redact_payload_value(value, max_text_chars))
                .collect(),
        ),
        Value::String(value) => Value::String(redact_payload_text(&value, max_text_chars)),
        value => value,
    }
}

/// Redact secret-shaped tokens in either plain provider output or JSON text.
pub fn redact_payload_text(value: &str, max_text_chars: usize) -> String {
    let value: String = value.chars().take(max_text_chars).collect();
    if let Ok(parsed) = serde_json::from_str::<Value>(&value) {
        return serde_json::to_string(&redact_payload_value(parsed, max_text_chars))
            .unwrap_or_else(|_| redact_payload_secret_tokens(&value));
    }
    redact_payload_secret_tokens(&value)
}

fn payload_secret_key(key: &str) -> bool {
    [
        "authorization",
        "api_key",
        "apikey",
        "access_token",
        "refresh_token",
        "id_token",
        "client_secret",
        "password",
        "password_hash",
        "credential",
    ]
    .contains(&key.to_ascii_lowercase().as_str())
}

fn redact_payload_secret_tokens(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    let mut redact_next = false;
    let bytes = value.as_bytes();
    while cursor < bytes.len() {
        if !is_payload_secret_token_char(bytes[cursor]) {
            let Some(character) = value[cursor..].chars().next() else {
                break;
            };
            output.push(character);
            cursor += character.len_utf8();
            continue;
        }
        let start = cursor;
        while cursor < bytes.len() && is_payload_secret_token_char(bytes[cursor]) {
            cursor += 1;
        }
        let token = &value[start..cursor];
        if redact_next || looks_like_payload_secret(token) {
            output.push_str("[redacted]");
        } else {
            output.push_str(token);
        }
        redact_next = token.eq_ignore_ascii_case("bearer");
    }
    output
}

fn is_payload_secret_token_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
}

/// The app's own credential-prefix floor for payload tokens (16+ characters), compared
/// case-insensitively so a mixed-case copy cannot bypass redaction. It overlaps the owner scrubber
/// (`rightkit_logs::scrub_text`, case-sensitive, splits words only on whitespace, quotes, brackets and
/// `,;=`) on purpose: the owner decides the shapes it knows, this list keeps the floor the app
/// had before the owner was adopted.
const PAYLOAD_FLOOR_PREFIXES: &[&str] = &[
    "sk-",
    "gsk_",
    "ghp_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxr-",
    "xoxs-",
    "aiza",
    "akia",
    "eyj",
];

/// The same floor for the unknown-key diagnostic values and the operational `message` (12+
/// characters), case-insensitive.
const DIAGNOSTIC_FLOOR_PREFIXES: &[&str] = &[
    "sk-",
    "sk_",
    "gsk_",
    "csk_",
    "rk_",
    "ghp_",
    "gho_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "aiza",
    "akia",
    "eyj",
];

fn has_floor_prefix(token: &str, prefixes: &[&str], min_len: usize) -> bool {
    let lower = token.to_ascii_lowercase();
    token.len() >= min_len && prefixes.iter().any(|prefix| lower.starts_with(prefix))
}

/// The value of a `label:value` or `label=value` token (after the last `:` or `=`), without
/// wrapping punctuation. The owner scrubber does not split on `:`, so `key:sk-...` is one word to it.
fn label_value(token: &str) -> &str {
    let wrap = |c: char| {
        matches!(
            c,
            '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '<' | '>' | '{' | '}'
        )
    };
    let core = token.trim_matches(wrap);
    match core.rfind([':', '=']) {
        Some(i) => core[i + 1..].trim_matches(wrap),
        None => core,
    }
}

/// Whether the owner scrubber changes `token`, i.e. it has a credential, email, IP or user-path
/// shape the owner knows.
fn owner_scrub_changes(token: &str) -> bool {
    rightkit_logs::scrub_text(token) != token
}

/// Floor prefix on the token or on the value after its label.
fn floor_token(token: &str, prefixes: &[&str], min_len: usize) -> bool {
    has_floor_prefix(token, prefixes, min_len)
        || has_floor_prefix(label_value(token), prefixes, min_len)
}

/// Floor prefix, or an owner shape on the token or on the value after its label.
fn secret_token(token: &str, prefixes: &[&str], min_len: usize) -> bool {
    let value = label_value(token);
    floor_token(token, prefixes, min_len)
        || owner_scrub_changes(token)
        || (value != token && owner_scrub_changes(value))
}

fn looks_like_payload_secret(value: &str) -> bool {
    secret_token(value, PAYLOAD_FLOOR_PREFIXES, 16)
}

fn redact_diagnostic_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if key.eq_ignore_ascii_case("message") {
                    // `message` is the operational payload of every local
                    // telemetry event (`key=value` diagnostics, error strings).
                    // Blanket redaction makes the entire local log useless, so
                    // scrub it token-wise instead — same privacy posture as the
                    // sidecar stderr scrubber. Emitters must never place raw
                    // user content in `message`; content belongs under the
                    // blanket-redacted keys below.
                    if let Value::String(text) = child {
                        *text = scrub_diagnostic_message(text);
                    } else {
                        redact_diagnostic_value(child);
                    }
                } else if sensitive_diagnostic_key(key) {
                    *child = Value::String(DIAGNOSTIC_REDACTION_MARKER.to_string());
                } else {
                    redact_diagnostic_value(child);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_diagnostic_value(item);
            }
        }
        Value::String(text) if secret_shaped(text) => {
            *text = DIAGNOSTIC_REDACTION_MARKER.to_string();
        }
        _ => {}
    }
}

fn sensitive_diagnostic_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    matches!(
        key.as_str(),
        "authorization"
            | "body"
            | "completion"
            | "content"
            | "cookie"
            | "last_transcript"
            | "phrase"
            | "process_name"
            | "query"
            | "raw_text"
            | "recognized_text"
            | "selected_text"
            | "target"
            | "text"
            | "transcript"
            | "window"
            | "window_title"
    ) || [
        "api_key",
        "clipboard",
        "password",
        "prompt",
        "refresh_token",
        "secret",
        "token",
        "transcript",
        "window",
    ]
    .iter()
    .any(|fragment| key.contains(fragment))
}

/// Token-wise scrub for the operational `message` field: keep `key=value`
/// diagnostics and error text, redact tokens that name or carry sensitive
/// material (the app's keyword and prefix floors), and run every other token through the owner's
/// `rightkit_logs::scrub_text` (credential shapes, emails, IPs, user-path segments).
fn scrub_diagnostic_message(text: &str) -> String {
    const SENSITIVE_MESSAGE_FRAGMENTS: &[&str] = &[
        "authorization",
        "bearer",
        "api_key",
        "api-key",
        "apikey",
        "token",
        "transcript",
        "prompt",
        "content",
        "password",
        "secret",
        "clipboard",
        "phrase",
        "window_title",
        "title=",
        "text=",
    ];
    let mut scrubbed = String::with_capacity(text.len());
    let mut redact_next = false;
    for token in text.split_inclusive(char::is_whitespace) {
        let trimmed_len = token.trim_end_matches(char::is_whitespace).len();
        let (body, suffix) = token.split_at(trimmed_len);
        let lower = body.to_ascii_lowercase();
        let sensitive = SENSITIVE_MESSAGE_FRAGMENTS
            .iter()
            .any(|fragment| lower.contains(fragment))
            || floor_token(body, DIAGNOSTIC_FLOOR_PREFIXES, 12);
        if redact_next || sensitive {
            scrubbed.push_str(DIAGNOSTIC_REDACTION_MARKER);
        } else {
            scrubbed.push_str(&rightkit_logs::scrub_text(body));
        }
        scrubbed.push_str(suffix);
        redact_next = lower == "bearer" || lower.ends_with("=bearer") || lower.ends_with(":bearer");
    }
    scrubbed
}

fn secret_shaped(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    if lower.contains("bearer ")
        || lower.contains("authorization=")
        || lower.contains("api_key=")
        || lower.contains("api-key=")
        || lower.contains("access_token=")
        || lower.contains("refresh_token=")
        || lower.contains("secret=")
    {
        return true;
    }

    value
        .split(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}'
                )
        })
        .any(|token| {
            let token = token.trim_matches(|character: char| matches!(character, ':' | '='));
            secret_token(token, DIAGNOSTIC_FLOOR_PREFIXES, 12)
        })
}

#[cfg(test)]
#[path = "redact_tests.rs"]
mod tests;
