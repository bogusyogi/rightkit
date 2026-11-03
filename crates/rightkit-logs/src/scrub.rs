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
    scrub_inner(input, options, false).0
}

/// [`scrub_text`] that also returns how many fragments it replaced. Each home directory, `Bearer`
/// token, redacted word and masked user-name segment counts once.
///
/// The text is the same as `scrub_text`'s, with one difference for the end of the input: a
/// credential prefix that ends it (`sk-`, `ghp_`, `AKIA`, ...) is redacted at any length. The
/// input may be the cut-off start of a longer string, and a length limit would otherwise leave the
/// start of the key readable. Mid-text words keep `scrub_text`'s length floor.
pub fn scrub_text_counted(input: &str) -> (String, usize) {
    scrub_inner(input, &ScrubOptions::default(), true)
}

/// Scrubs `input`; `truncated` applies the end-of-input credential rule of
/// [`scrub_text_counted`]. Returns the text and the number of replacements.
fn scrub_inner(input: &str, options: &ScrubOptions, truncated: bool) -> (String, usize) {
    let mut count = 0usize;
    let mut text = input.to_string();
    for home in home_dirs() {
        let found = text.matches(&home).count();
        if found > 0 {
            count += found;
            text = text.replace(&home, "~");
        }
    }
    // URLs/OAuth params first, while `://`, `?`, `&` and `=` structure is intact:
    // the word pass below splits on those delimiters and would lose it.
    let text = redact_url_text(&text);
    let (text, bearer) = redact_bearer(&text);
    count += bearer;
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    for ch in text.chars() {
        if is_delimiter(ch) {
            let (scrubbed, replaced) = scrub_word(&word, options, false);
            out.push_str(&scrubbed);
            count += replaced;
            word.clear();
            out.push(ch);
        } else {
            word.push(ch);
        }
    }
    let (scrubbed, replaced) = scrub_word(&word, options, truncated);
    out.push_str(&scrubbed);
    count += replaced;
    (out, count)
}

/// Std panic messages quote the offending value in backticks or single quotes
/// (a bad string slice prints the whole string), and `Debug` formatting quotes
/// strings with double quotes (`Err value: Custom("…")`). That quoted value can
/// be user content. Keep the message's shape, drop what it quotes.
pub fn redact_quoted(msg: &str) -> String {
    let chars: Vec<char> = msg.chars().collect();
    let mut out = String::with_capacity(msg.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            // Debug escaping: a `\"` inside the span does not close it.
            let mut j = i + 1;
            let mut closed = false;
            while j < chars.len() {
                match chars[j] {
                    '\\' => j += 2,
                    '"' => {
                        closed = true;
                        break;
                    }
                    _ => j += 1,
                }
            }
            if closed {
                out.push('"');
                out.push_str(&format!("<redacted:{}>", j - i - 1));
                out.push('"');
                i = j + 1;
                continue;
            }
        }
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

/// Scrub every string leaf of a JSON value in place: quoted/Debug content is
/// dropped first, then the full text scrub (URLs/OAuth, bearer, credential,
/// email, IP and user-path shapes) runs over what remains.
pub fn scrub_value(value: &mut Value) {
    match value {
        Value::String(text) => {
            let scrubbed = scrub_text(&redact_quoted(text));
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

/// Scrubs one word and returns it with the number of replacements made in it. `truncated` is set
/// for the last word of an input that may have been cut short (see [`scrub_text_counted`]).
fn scrub_word(word: &str, options: &ScrubOptions, truncated: bool) -> (String, usize) {
    if word.is_empty() {
        return (String::new(), 0);
    }
    if looks_like_email(word) {
        return ("[redacted:email]".into(), 1);
    }
    if looks_like_credential(word) {
        return ("[redacted:credential]".into(), 1);
    }
    if truncated && starts_with_credential_prefix(word) {
        return ("[redacted:credential]".into(), 1);
    }
    if looks_like_ipv4(word) {
        return ("[redacted:ip]".into(), 1);
    }
    if options.redact_paths && looks_like_absolute_path(word) {
        return ("[path]".into(), 1);
    }
    mask_user_segment(word)
}

/// Prefixes of well-known API keys and tokens. Checked with a length floor by
/// [`looks_like_credential`], and at any length for a word that ends a truncated input.
const CREDENTIAL_PREFIXES: &[&str] = &[
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

fn starts_with_credential_prefix(word: &str) -> bool {
    CREDENTIAL_PREFIXES
        .iter()
        .any(|prefix| word.starts_with(prefix))
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
fn redact_bearer(text: &str) -> (String, usize) {
    const MARKER: &str = "bearer";
    // ASCII lowercasing keeps every byte offset of `text`.
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut redacted = 0usize;
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
        redacted += 1;
        cursor = token_end;
        search = token_end;
    }
    out.push_str(&text[cursor..]);
    (out, redacted)
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn looks_like_credential(word: &str) -> bool {
    if word.len() >= 12 && starts_with_credential_prefix(word) {
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
fn mask_user_segment(word: &str) -> (String, usize) {
    let mut result = word.to_string();
    let mut masked = 0usize;
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
                masked += 1;
                from = start + USER_PLACEHOLDER.len();
            } else {
                from = end.max(start);
            }
            if from >= result.len() {
                break;
            }
        }
    }
    (result, masked)
}

/// Query-string and OAuth parameter names whose values are secrets wherever they appear in text.
const SECRET_PARAM_NAMES: &[&str] = &[
    "access_token",
    "refresh_token",
    "id_token",
    "client_secret",
    "code_verifier",
    "assertion",
    "api_key",
    "apikey",
    "password",
    "secret",
    "token",
    "code",
    "state",
    "key",
];

/// Redact URLs and OAuth-style secret values in free text, for error strings that leave the
/// device. Each `scheme://` URL keeps its scheme, host and path; its userinfo is dropped, and a
/// query string becomes `?[redacted]`; a fragment is dropped. Each `name=value` pair whose name is
/// a known secret parameter (`access_token`, `code`, `client_secret`, `key`, and similar) keeps
/// the name and replaces the value with `[redacted]`. Other text is unchanged.
pub fn redact_url_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(idx) = rest.find("://") {
        out.push_str(&rest[..idx + 3]);
        let after = &rest[idx + 3..];
        let end = url_token_end(after);
        out.push_str(&redact_url_token(&after[..end]));
        rest = &after[end..];
    }
    out.push_str(rest);
    redact_secret_params(&out)
}

/// Byte length of the URL token at the start of `after` (the text following `://`). A bracketed
/// IPv6 authority is skipped whole before the terminator search, so its `]` does not end the URL.
fn url_token_end(after: &str) -> usize {
    let authority_end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let host_start = after[..authority_end].rfind('@').map_or(0, |at| at + 1);
    let mut from = 0;
    if after[host_start..].starts_with('[') {
        if let Some(close) = after[host_start..].find(']') {
            let inside = &after[host_start + 1..host_start + close];
            if !inside.chars().any(is_url_terminator) {
                from = host_start + close + 1;
            }
        }
    }
    after[from..]
        .find(is_url_terminator)
        .map_or(after.len(), |i| from + i)
}

fn is_url_terminator(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | ')' | ']' | '}' | ',')
}

/// Redact one URL token (the text after `://`): userinfo dropped, query to `?[redacted]`,
/// fragment dropped.
fn redact_url_token(token: &str) -> String {
    let before_fragment = token.split_once('#').map_or(token, |(head, _)| head);
    let (head, has_query) = match before_fragment.split_once('?') {
        Some((head, _)) => (head, true),
        None => (before_fragment, false),
    };
    let end = head.find('/').unwrap_or(head.len());
    let (authority, path) = head.split_at(end);
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let mut out = format!("{host_port}{path}");
    if has_query {
        out.push_str("?[redacted]");
    }
    out
}

/// Replace the value of each `name=value` pair whose name is in [`SECRET_PARAM_NAMES`].
fn redact_secret_params(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for eq in 0..bytes.len() {
        if bytes[eq] != b'=' {
            continue;
        }
        let mut name_start = eq;
        while name_start > 0
            && (bytes[name_start - 1].is_ascii_alphanumeric()
                || matches!(bytes[name_start - 1], b'_' | b'-'))
        {
            name_start -= 1;
        }
        if eq <= cursor || name_start < cursor {
            continue;
        }
        let name = &lower[name_start..eq];
        let boundary_ok = name_start == 0
            || matches!(
                bytes[name_start - 1],
                b'?' | b'&' | b';' | b',' | b' ' | b'\t' | b'\n' | b'"' | b'\'' | b'(' | b'{'
            );
        if !boundary_ok || !SECRET_PARAM_NAMES.contains(&name) {
            continue;
        }
        let value_start = eq + 1;
        let value_end = bytes[value_start..]
            .iter()
            .position(|&b| {
                b.is_ascii_whitespace()
                    || matches!(b, b'&' | b'"' | b'\'' | b';' | b',' | b')' | b'}')
            })
            .map_or(bytes.len(), |p| value_start + p);
        if value_end == value_start {
            continue;
        }
        out.push_str(&text[cursor..value_start]);
        out.push_str("[redacted]");
        cursor = value_end;
    }
    out.push_str(&text[cursor..]);
    out
}
