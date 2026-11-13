//! Free-text PII scrubbing for strings that leave the device (crash reports,
//! outbox payloads, support bundles).
//!
//! Key-based redaction (`RedactionPolicy`) protects known-sensitive fields.
//! This module protects *values*: panic messages, backtraces and error strings
//! that can carry home directories, e-mail addresses, credentials or quoted
//! user content.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

const USER_DIR_MARKERS: &[&str] = &["/Users/", "/home/", "\\Users\\", "/Documents and Settings/"];
const USER_PLACEHOLDER: &str = "<user>";

/// How many fragments each kind of redaction replaced (issue #793). Counts only: no redacted
/// value, key name or position is recorded, so the summary is safe to ship in a support bundle.
///
/// Fill one with [`scrub_text_report`], [`scrub_value_counted`] or
/// [`crate::RedactionPolicy::redact_value_counted`], and add runs together with
/// [`RedactionCounts::merge`]. A bundle's summary is the merge of every section's counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RedactionCounts {
    /// Home-directory prefixes replaced by `~`.
    pub home_dir: u64,
    /// URLs that lost userinfo, a query string or a fragment.
    pub url: u64,
    /// `name=value` pairs whose name is a known secret parameter (`token`, `code`, `key`, ...).
    pub secret_param: u64,
    /// `Bearer <credential>` tokens.
    pub bearer: u64,
    /// API keys and other credential-shaped words (`sk-...`, `ghp_...`, long opaque blobs).
    pub credential: u64,
    /// Telegram bot tokens (`<numeric id>:<secret>`).
    pub telegram_token: u64,
    /// E-mail addresses.
    pub email: u64,
    /// IPv4 addresses.
    pub ip: u64,
    /// Absolute path tokens replaced by `[path]` (only with [`ScrubOptions::redact_paths`]).
    pub path: u64,
    /// User-name segments masked inside a path (`/Users/<user>/...`).
    pub user_segment: u64,
    /// Quoted or Debug-formatted spans dropped from a message.
    pub quoted: u64,
    /// `key=value` tokens redacted by [`ScrubOptions::key_value_min_len`].
    pub key_value: u64,
    /// JSON values replaced because their key is sensitive (`password`, `api_key`, ...), counted
    /// by [`crate::RedactionPolicy::redact_value_counted`].
    pub sensitive_key: u64,
}

impl RedactionCounts {
    /// Every kind added together.
    pub fn total(&self) -> u64 {
        self.by_kind().values().sum()
    }

    /// True when nothing was redacted.
    pub fn is_empty(&self) -> bool {
        self.total() == 0
    }

    /// Adds `other`'s counts to this one.
    pub fn merge(&mut self, other: &RedactionCounts) {
        self.home_dir += other.home_dir;
        self.url += other.url;
        self.secret_param += other.secret_param;
        self.bearer += other.bearer;
        self.credential += other.credential;
        self.telegram_token += other.telegram_token;
        self.email += other.email;
        self.ip += other.ip;
        self.path += other.path;
        self.user_segment += other.user_segment;
        self.quoted += other.quoted;
        self.key_value += other.key_value;
        self.sensitive_key += other.sensitive_key;
    }

    /// Non-zero counts keyed by kind name (the field names above), sorted by name.
    pub fn by_kind(&self) -> BTreeMap<&'static str, u64> {
        [
            ("home_dir", self.home_dir),
            ("url", self.url),
            ("secret_param", self.secret_param),
            ("bearer", self.bearer),
            ("credential", self.credential),
            ("telegram_token", self.telegram_token),
            ("email", self.email),
            ("ip", self.ip),
            ("path", self.path),
            ("user_segment", self.user_segment),
            ("quoted", self.quoted),
            ("key_value", self.key_value),
            ("sensitive_key", self.sensitive_key),
        ]
        .into_iter()
        .filter(|(_, count)| *count > 0)
        .collect()
    }

    /// The kinds [`scrub_text_counted`] has always counted (not URL, secret-parameter, quoted or
    /// sensitive-key), so its result does not change.
    fn text_counted(&self) -> usize {
        (self.home_dir
            + self.bearer
            + self.credential
            + self.telegram_token
            + self.email
            + self.ip
            + self.path
            + self.user_segment
            + self.key_value) as usize
    }
}

/// Options for [`scrub_text_with`]. The default keeps path shape, as
/// [`scrub_text`] does.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrubOptions {
    /// Replace every absolute path token (`/…`, `~/…`, `C:\…`, `\\server\…`)
    /// with `[path]`, the way the CodeRight support bundle does. Off by default.
    pub redact_paths: bool,
    /// Replace the value of every `key=value` token whose value is at least this many characters
    /// and made only of ASCII letters, digits, `%`, `_` and `-`, with `key=<redacted>`. Tokens end
    /// at whitespace or `;`. For cookie-shaped pairs (`sid=abcdef123456`, `a=…; b=…`) that the
    /// 32-character opaque-word floor passes.
    /// `None` (the default) leaves the output unchanged.
    pub key_value_min_len: Option<usize>,
}

/// Redacts the value of each `key=value` token whose value meets `min_len` (see
/// [`ScrubOptions::key_value_min_len`]). Tokens are separated by whitespace or `;`, so a cookie
/// list (`a=…; b=…`) is covered; the key is everything before the first `=`. Separators are kept
/// as written. Returns the text and the number of values replaced.
fn redact_key_values(text: &str, min_len: usize) -> (String, usize) {
    let is_separator = |c: char| c.is_whitespace() || c == ';';
    let is_value_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '%' | '_' | '-');
    let mut out = String::with_capacity(text.len());
    let mut count = 0usize;
    let mut rest = text;
    while !rest.is_empty() {
        let lead = rest.len() - rest.trim_start_matches(is_separator).len();
        out.push_str(&rest[..lead]);
        rest = &rest[lead..];
        let end = rest.find(is_separator).unwrap_or(rest.len());
        let (token, tail) = rest.split_at(end);
        match token.split_once('=') {
            Some((key, value)) if value.len() >= min_len && value.chars().all(is_value_char) => {
                out.push_str(key);
                out.push_str("=<redacted>");
                count += 1;
            }
            _ => out.push_str(token),
        }
        rest = tail;
    }
    (out, count)
}

/// Redacts the string value of each quoted JSON pair inside free text whose key is one of `keys`:
/// `"access_token": "abc"` becomes `"access_token": "<redacted>"`. Keys and values are scanned
/// escape-aware (`\"` does not end a string). Pairs with other keys, and values that are numbers,
/// booleans or null (for example `"code": 200`), are left as written. The quotes stay, so the
/// surrounding JSON-like text stays readable.
pub fn redact_json_pairs(text: &str, keys: &[&str]) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut flushed = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'"' {
            i += 1;
            continue;
        }
        let Some(key_end) = closing_quote(bytes, i) else {
            break;
        };
        let key = &text[i + 1..key_end];
        i = key_end + 1;
        let mut j = skip_ws(bytes, i);
        if bytes.get(j) != Some(&b':') {
            continue;
        }
        j = skip_ws(bytes, j + 1);
        if bytes.get(j) != Some(&b'"') {
            i = j;
            continue;
        }
        let Some(value_end) = closing_quote(bytes, j) else {
            break;
        };
        i = value_end + 1;
        if keys.contains(&key) {
            out.push_str(&text[flushed..=j]);
            out.push_str("<redacted>");
            flushed = value_end;
        }
    }
    out.push_str(&text[flushed..]);
    out
}

/// Replaces the value of each `key=value` pair in free text whose key is one of `keys` with a
/// stable token, `key=tok_<24 hex>` (rightkit#520). The token is the first 12 bytes of
/// HMAC-SHA256 under `salt` over `key`, a NUL and the value, so the same account or label always
/// maps to the same token (log lines stay correlatable) while the value cannot be recovered or
/// guessed without the salt, and the same value under another key gets another token.
///
/// `salt` must be private to the installation, at least 16 bytes, and kept in the OS credential
/// store (for example `rightkit_credentials::Store::get_or_create_key`); a public or constant salt
/// makes the tokens a dictionary away from the values. A salt shorter than 16 bytes is refused by
/// returning the input with every matching value redacted as `key=<redacted>`, never hashed.
///
/// A value is a double-quoted string (escape-aware, the quotes are dropped) or the run of
/// characters up to whitespace, `,`, `;` or `&`. A key matches only at the start of the text or
/// after a character that cannot be part of a name; an empty value is left as written. Other
/// text is copied unchanged.
#[cfg(feature = "token-hash")]
pub fn redact_key_tokens(text: &str, keys: &[&str], salt: &[u8]) -> String {
    let weak = salt.len() < 16;
    let bytes = text.as_bytes();
    let is_name = |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.');
    let mut out = String::with_capacity(text.len());
    let mut flushed = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let at_boundary = text.is_char_boundary(i) && (i == 0 || !is_name(bytes[i - 1]));
        let hit = if at_boundary {
            keys.iter()
                .copied()
                .filter(|k| !k.is_empty())
                .find(|k| text[i..].starts_with(*k) && bytes.get(i + k.len()) == Some(&b'='))
        } else {
            None
        };
        let Some(key) = hit else {
            i += 1;
            continue;
        };
        let value_start = i + key.len() + 1;
        let (value, value_end) = if bytes.get(value_start) == Some(&b'"') {
            match closing_quote(bytes, value_start) {
                Some(end) => (&text[value_start + 1..end], end + 1),
                None => (&text[value_start + 1..], bytes.len()),
            }
        } else {
            let end = text[value_start..]
                .find(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '&'))
                .map_or(bytes.len(), |n| value_start + n);
            (&text[value_start..end], end)
        };
        if value.is_empty() {
            i = value_start;
            continue;
        }
        out.push_str(&text[flushed..value_start]);
        if weak {
            out.push_str("<redacted>");
        } else {
            let mut msg = Vec::with_capacity(key.len() + 1 + value.len());
            msg.extend_from_slice(key.as_bytes());
            msg.push(0);
            msg.extend_from_slice(value.as_bytes());
            let mac = rightkit_crypto::hmac_sha256(salt, &msg);
            out.push_str("tok_");
            out.push_str(&rightkit_crypto::hex_encode(&mac[..12]));
        }
        flushed = value_end;
        i = value_end;
    }
    out.push_str(&text[flushed..]);
    out
}

/// Index of the quote that closes the string opened at `open`, skipping escaped characters.
fn closing_quote(bytes: &[u8], open: usize) -> Option<usize> {
    let mut k = open + 1;
    while k < bytes.len() {
        match bytes[k] {
            b'\\' => k += 2,
            b'"' => return Some(k),
            _ => k += 1,
        }
    }
    None
}

fn skip_ws(bytes: &[u8], mut k: usize) -> usize {
    while bytes.get(k).is_some_and(u8::is_ascii_whitespace) {
        k += 1;
    }
    k
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
    scrub_report_inner(input, options, false).0
}

/// [`scrub_text_with`] that also returns what it replaced, by kind (issue #793). The text is
/// identical to `scrub_text_with`'s.
pub fn scrub_text_report(input: &str, options: &ScrubOptions) -> (String, RedactionCounts) {
    scrub_report_inner(input, options, false)
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
    let (text, counts) = scrub_report_inner(input, options, truncated);
    (text, counts.text_counted())
}

/// The one scrub pass: the text and what was replaced, by kind.
fn scrub_report_inner(
    input: &str,
    options: &ScrubOptions,
    truncated: bool,
) -> (String, RedactionCounts) {
    let mut counts = RedactionCounts::default();
    let mut text = input.to_string();
    for home in home_dirs() {
        let found = text.matches(&home).count();
        if found > 0 {
            counts.home_dir += found as u64;
            text = text.replace(&home, "~");
        }
    }
    // URLs/OAuth params first, while `://`, `?`, `&` and `=` structure is intact:
    // the word pass below splits on those delimiters and would lose it.
    let (text, urls, params) = redact_url_text_counted(&text);
    counts.url += urls;
    counts.secret_param += params;
    let (text, bearer) = redact_bearer(&text);
    counts.bearer += bearer as u64;
    let text = match options.key_value_min_len {
        Some(min_len) => {
            let (text, pairs) = redact_key_values(&text, min_len);
            counts.key_value += pairs as u64;
            text
        }
        None => text,
    };
    let mut out = String::with_capacity(text.len());
    let mut word = String::new();
    for ch in text.chars() {
        if is_delimiter(ch) {
            out.push_str(&scrub_word(&word, options, false, &mut counts));
            word.clear();
            out.push(ch);
        } else {
            word.push(ch);
        }
    }
    out.push_str(&scrub_word(&word, options, truncated, &mut counts));
    (out, counts)
}

/// Std panic messages quote the offending value in backticks or single quotes
/// (a bad string slice prints the whole string), and `Debug` formatting quotes
/// strings with double quotes (`Err value: Custom("…")`). That quoted value can
/// be user content. Keep the message's shape, drop what it quotes.
pub fn redact_quoted(msg: &str) -> String {
    redact_quoted_counted(msg).0
}

/// [`redact_quoted`] that also returns how many quoted spans it dropped.
fn redact_quoted_counted(msg: &str) -> (String, u64) {
    let mut dropped = 0u64;
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
                dropped += 1;
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
                    dropped += 1;
                    i += len + 2;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    (out, dropped)
}

/// Scrub every string leaf of a JSON value in place: quoted/Debug content is
/// dropped first, then the full text scrub (URLs/OAuth, bearer, credential,
/// email, IP and user-path shapes) runs over what remains.
pub fn scrub_value(value: &mut Value) {
    scrub_value_counted(value);
}

/// [`scrub_value`] that also returns what it replaced, by kind (issue #793): the redaction
/// summary of a support-bundle section. Counts only; nothing about the redacted values is kept.
pub fn scrub_value_counted(value: &mut Value) -> RedactionCounts {
    let mut counts = RedactionCounts::default();
    match value {
        Value::String(text) => {
            let (quoted_out, quoted) = redact_quoted_counted(text);
            let (scrubbed, found) =
                scrub_report_inner(&quoted_out, &ScrubOptions::default(), false);
            counts = found;
            counts.quoted += quoted;
            if scrubbed != *text {
                *text = scrubbed;
            }
        }
        Value::Array(items) => {
            for item in items {
                counts.merge(&scrub_value_counted(item));
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                counts.merge(&scrub_value_counted(item));
            }
        }
        _ => {}
    }
    counts
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

/// Scrubs one word, adding what it replaced to `counts`. `truncated` is set for the last word of
/// an input that may have been cut short (see [`scrub_text_counted`]).
fn scrub_word(
    word: &str,
    options: &ScrubOptions,
    truncated: bool,
    counts: &mut RedactionCounts,
) -> String {
    if word.is_empty() {
        return String::new();
    }
    if looks_like_email(word) {
        counts.email += 1;
        return "[redacted:email]".into();
    }
    if looks_like_credential(word) {
        if looks_like_telegram_token(word) {
            counts.telegram_token += 1;
        } else {
            counts.credential += 1;
        }
        return "[redacted:credential]".into();
    }
    if truncated && starts_with_credential_prefix(word) {
        counts.credential += 1;
        return "[redacted:credential]".into();
    }
    if looks_like_ipv4(word) {
        counts.ip += 1;
        return "[redacted:ip]".into();
    }
    if options.redact_paths && looks_like_absolute_path(word) {
        counts.path += 1;
        return "[path]".into();
    }
    let (masked, segments) = mask_user_segment(word);
    counts.user_segment += segments as u64;
    masked
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
    redact_url_text_counted(text).0
}

/// [`redact_url_text`] that also returns how many URLs lost something and how many secret
/// parameter values were replaced.
fn redact_url_text_counted(text: &str) -> (String, u64, u64) {
    let mut out = String::with_capacity(text.len());
    let mut urls = 0u64;
    let mut rest = text;
    while let Some(idx) = rest.find("://") {
        out.push_str(&rest[..idx + 3]);
        let after = &rest[idx + 3..];
        let end = url_token_end(after);
        let token = &after[..end];
        let redacted = redact_url_token(token);
        if redacted != token {
            urls += 1;
        }
        out.push_str(&redacted);
        rest = &after[end..];
    }
    out.push_str(rest);
    let (out, params) = redact_secret_params(&out);
    (out, urls, params)
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

/// Replace the value of each `name=value` pair whose name is in [`SECRET_PARAM_NAMES`]. Returns
/// the text and the number of values replaced.
fn redact_secret_params(text: &str) -> (String, u64) {
    let mut replaced = 0u64;
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
        replaced += 1;
        cursor = value_end;
    }
    out.push_str(&text[cursor..]);
    (out, replaced)
}
