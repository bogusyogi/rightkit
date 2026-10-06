//! Free-text PII scrubbing for strings that leave the device (crash reports,
//! outbox payloads, support bundles).
//!
//! Key-based redaction (`RedactionPolicy`) protects known-sensitive fields.
//! This module protects *values*: panic messages, backtraces and error strings
//! that can carry home directories, e-mail addresses, credentials or quoted
//! user content.

use serde_json::Value;

const USER_DIR_MARKERS: &[&str] = &["/Users/", "/home/", "\\Users\\", "/Documents and Settings/"];
const USER_PLACEHOLDER: &str = "<user>";

/// Replace identifying fragments in free text. Shape is preserved so reports
/// stay diagnosable: `/Users/ada/app/src/main.rs:4` becomes
/// `/Users/<user>/app/src/main.rs:4`.
pub fn scrub_text(input: &str) -> String {
    let mut text = input.to_string();
    for home in home_dirs() {
        text = text.replace(&home, "~");
    }
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    for ch in text.chars() {
        if is_delimiter(ch) {
            out.push_str(&scrub_word(&word));
            word.clear();
            out.push(ch);
        } else {
            word.push(ch);
        }
    }
    out.push_str(&scrub_word(&word));
    out
}

/// Std panic messages quote the offending value in backticks or single quotes
/// (a bad string slice prints the whole string), and that value can be user
/// content. Keep the message's shape, drop what it quotes.
pub fn redact_quoted(msg: &str) -> String {
    let chars: Vec<char> = msg.chars().collect();
    let mut out = String::with_capacity(msg.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Backticks always wrap a value; single quotes only a short one
        // (`'é'`), so an apostrophe in prose is left alone.
        let max = match c {
            '`' => usize::MAX,
            '\'' => 8,
            _ => 0,
        };
        if max > 0 {
            if let Some(len) = chars[i + 1..]
                .iter()
                .take(max.saturating_add(1))
                .position(|&n| n == c)
            {
                if len <= max {
                    out.push(c);
                    out.push_str(&format!("<redacted:{len}>"));
                    out.push(c);
                    i += len + 2;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Scrub every string leaf of a JSON value in place.
pub fn scrub_value(value: &mut Value) {
    match value {
        Value::String(text) => {
            let scrubbed = scrub_text(text);
            if scrubbed != *text {
                *text = scrubbed;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(scrub_value),
        Value::Object(map) => map.values_mut().for_each(scrub_value),
        _ => {}
    }
}

fn home_dirs() -> Vec<String> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .filter(|path| path.len() > 3 && path != "/root")
        .collect()
}

fn is_delimiter(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '"' | '\'' | '`' | '(' | ')' | '<' | '>' | ',' | ';' | '=' | '[' | ']' | '{' | '}'
        )
}

fn scrub_word(word: &str) -> String {
    if word.is_empty() {
        return String::new();
    }
    if looks_like_email(word) {
        return "[redacted:email]".into();
    }
    if looks_like_credential(word) {
        return "[redacted:credential]".into();
    }
    if looks_like_ipv4(word) {
        return "[redacted:ip]".into();
    }
    mask_user_segment(word)
}

fn looks_like_email(word: &str) -> bool {
    let trimmed = word.trim_matches(|c: char| matches!(c, '.' | ':'));
    let Some((local, domain)) = trimmed.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !domain.contains('@')
        && local
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-'))
}

fn looks_like_credential(word: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "sk-",
        "sk_",
        "gsk_",
        "ghp_",
        "gho_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "AKIA",
        "AIza",
        "nvapi-",
        "eyJ",
    ];
    if word.len() >= 12 && PREFIXES.iter().any(|prefix| word.starts_with(prefix)) {
        return true;
    }
    // Long opaque blobs: hex or base64-ish runs of 32+ characters.
    word.len() >= 32
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '-' | '_' | '='))
        && word.chars().any(|c| c.is_ascii_digit())
        && word.chars().any(|c| c.is_ascii_alphabetic())
}

fn looks_like_ipv4(word: &str) -> bool {
    let host = word.split(':').next().unwrap_or(word);
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok())
}

/// `/Users/ada/x` becomes `/Users/<user>/x` anywhere inside the word.
fn mask_user_segment(word: &str) -> String {
    let mut result = word.to_string();
    for marker in USER_DIR_MARKERS {
        let separator = if marker.starts_with('\\') { '\\' } else { '/' };
        let mut from = 0;
        while let Some(found) = result[from..].find(marker) {
            let start = from + found + marker.len();
            let end = result[start..]
                .find(separator)
                .map(|offset| start + offset)
                .unwrap_or(result.len());
            let name = &result[start..end];
            if !name.is_empty() && name != USER_PLACEHOLDER && name != "Shared" {
                result.replace_range(start..end, USER_PLACEHOLDER);
                from = start + USER_PLACEHOLDER.len();
            } else {
                from = end.max(start);
            }
            if from >= result.len() {
                break;
            }
        }
    }
    result
}
