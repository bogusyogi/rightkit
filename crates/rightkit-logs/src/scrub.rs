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

/// Options for [`scrub_text_with`]. The default keeps path shape, as
/// [`scrub_text`] does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrubOptions {
    /// Replace every absolute path token (`/…`, `~/…`, `C:\…`, `\\server\…`)
    /// with `[path]`, the way the CodeRight support bundle does. Off by default.
    pub redact_paths: bool,
}

/// Replace identifying fragments in free text. Shape is preserved so reports
/// stay diagnosable: `/Users/ada/app/src/main.rs:4` becomes
/// `/Users/<user>/app/src/main.rs:4`.
pub fn scrub_text(input: &str) -> String {
    scrub_text_with(input, &ScrubOptions::default())
}

/// [`scrub_text`] with options. With `ScrubOptions::default()` the output is
/// identical to `scrub_text`.
pub fn scrub_text_with(input: &str, options: &ScrubOptions) -> String {
    let mut text = input.to_string();
    for home in home_dirs() {
        text = text.replace(&home, "~");
    }
    let text = redact_bearer(&text);
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    for ch in text.chars() {
        if is_delimiter(ch) {
            out.push_str(&scrub_word(&word, options));
            word.clear();
            out.push(ch);
        } else {
            word.push(ch);
        }
    }
    out.push_str(&scrub_word(&word, options));
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

fn scrub_word(word: &str, options: &ScrubOptions) -> String {
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
    if options.redact_paths && looks_like_absolute_path(word) {
        return "[path]".into();
    }
    mask_user_segment(word)
}

/// Whole-token path shapes: `/x` (not `//`, which is a comment or a URL
/// tail), `~/x`, a Windows drive `C:\x`, and a UNC `\\server\x`. A URL such
/// as `https://x/y` or a ratio such as `1/2` does not start with one of these.
fn looks_like_absolute_path(word: &str) -> bool {
    let bytes = word.as_bytes();
    if let Some(rest) = word.strip_prefix("~/") {
        return !rest.is_empty();
    }
    if let Some(rest) = word.strip_prefix("\\\\") {
        return rest.chars().next().is_some_and(|c| c != '\\');
    }
    if word.starts_with('/') {
        return word.len() > 1 && !word.starts_with("//");
    }
    bytes.len() > 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\'
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

/// `Bearer <credential>` (case-insensitive, whole-word marker): the token after
/// the marker is a credential whatever its shape, so it is dropped and the
/// marker stays. A marker with no token after it is left alone.
fn redact_bearer(text: &str) -> String {
    const MARKER: &str = "bearer";
    // ASCII lowercasing keeps every byte offset of `text`.
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut search = 0;
    while let Some(found) = lower[search..].find(MARKER) {
        let start = search + found;
        let end = start + MARKER.len();
        search = end;
        let whole_word = text[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_word_char(c))
            && text[end..].chars().next().is_none_or(|c| !is_word_char(c));
        if !whole_word {
            continue;
        }
        let mut token_start = end;
        if text[token_start..].starts_with(':') {
            token_start += 1;
        }
        let tail = &text[token_start..];
        let gap = tail.len() - tail.trim_start().len();
        if gap == 0 {
            continue;
        }
        token_start += gap;
        let token_len = text[token_start..]
            .find(char::is_whitespace)
            .unwrap_or(text.len() - token_start);
        if token_len == 0 {
            continue;
        }
        let token_end = token_start + token_len;
        out.push_str(&text[cursor..token_start]);
        out.push_str("[redacted:credential]");
        cursor = token_end;
        search = token_end;
    }
    out.push_str(&text[cursor..]);
    out
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn looks_like_credential(word: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "sk-",
        "sk_",
        "gsk_",
        "csk-",
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
    // Short `or-` prefix: a digit and a length floor keep ordinary hyphenated
    // words out.
    if word.len() >= 20 && word.starts_with("or-") && word.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    if looks_like_telegram_token(word) {
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

/// Bot token shape `<numeric id>:<secret>`, as issued for Telegram bots.
fn looks_like_telegram_token(word: &str) -> bool {
    let Some((id, secret)) = word.split_once(':') else {
        return false;
    };
    id.len() >= 6
        && id.chars().all(|c| c.is_ascii_digit())
        && secret.len() >= 20
        && secret
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
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
