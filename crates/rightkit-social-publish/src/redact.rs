//! Secret redaction for platform error text: URL query strings, userinfo and `name=value` secrets
//! (through rightkit-logs), then quoted JSON `"name": "value"` pairs for the OAuth names.

const REDACTED: &str = "[redacted]";

/// Text with every URL query string, userinfo and `name=value` secret (OAuth `code`, `state`, tokens, `key` and
/// similar) removed (rightkit-logs `redact_url_text`), then every JSON `"name":"value"` pair for the OAuth names.
/// Numeric values such as `"code": 200` stay: a platform error code the classifier must read. Logs and error text
/// pass through it.
pub fn redact_text(text: &str) -> String {
    redact_json_pairs(&rightkit_logs::redact_url_text(text))
}

// RIGHTKIT-GAP (bogusyogi/rightkit#1086): rightkit-logs 0.3.5 `redact_url_text` (scrub.rs:446) redacts `name=value` only; a quoted JSON pair
// `"access_token": "x"` in a platform error body passes through it (and through `scrub_text`) unchanged.
/// Byte index of the quote that closes a JSON string whose content starts at `from`: a backslash escapes the
/// next character, so `\"` inside a token does not end it (stopping there left the token's tail in the output).
fn closing_quote(text: &str, from: usize) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in text[from..].char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return Some(from + i),
            _ => {}
        }
    }
    None
}

fn redact_json_pairs(text: &str) -> String {
    const NAMES: &[&str] = &[
        "code",
        "state",
        "access_token",
        "refresh_token",
        "id_token",
        "client_secret",
    ];
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut pos = 0;
    while let Some(q) = text[pos..].find('"').map(|i| i + pos) {
        // Escape-aware for keys and values alike: a `\"` inside any string must not end it.
        let Some(close) = closing_quote(text, q + 1) else {
            break;
        };
        let key = &lower[q + 1..close];
        // Any JSON whitespace (space, tab, CR, LF) may sit around the colon.
        let ws = |c: char| matches!(c, ' ' | '\t' | '\r' | '\n');
        let rest = text[close + 1..].trim_start_matches(ws);
        let after_colon = rest.strip_prefix(':').map(|r| r.trim_start_matches(ws));
        match after_colon {
            Some(value) if NAMES.contains(&key) && value.starts_with('"') => {
                let value_start = text.len() - value.len() + 1;
                let value_end = closing_quote(text, value_start).unwrap_or(text.len());
                out.push_str(&text[pos..value_start]);
                out.push_str(REDACTED);
                // Step past the value's closing quote too, or the scan would read it as the next key's opening quote.
                pos = (value_end + 1).min(text.len());
                out.push_str(&text[value_end..pos]);
            }
            _ => {
                out.push_str(&text[pos..close + 1]);
                pos = close + 1;
            }
        }
    }
    out.push_str(&text[pos..]);
    out
}
