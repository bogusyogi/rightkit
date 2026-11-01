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

#[cfg(test)]
mod redaction_tests {
    use super::*;

    #[test]
    fn query_strings_and_oauth_values_never_survive() {
        let url = "https://x.test/cb?code=abc123&state=zzz&access_token=tok";
        let out = redact_text(url);
        assert_eq!(out, "https://x.test/cb?[redacted]");
        let out = redact_text("POST failed for https://api.test/v1/me?access_token=SECRET extra");
        assert!(
            !out.contains("SECRET") && out.ends_with("?[redacted] extra"),
            "{out}"
        );
        let out = redact_text("body code=AUTHCODE&state=ST refresh_token=RT1; ok");
        assert!(
            !out.contains("AUTHCODE") && !out.contains("ST&") && !out.contains("RT1"),
            "{out}"
        );
        assert!(out.ends_with("; ok") || out.ends_with(" ok"), "{out}");
        let out = redact_text(
            r#"{"access_token": "AT", "refresh_token":"RT", "code":"C9", "keep":"yes"}"#,
        );
        assert!(
            !out.contains("\"AT\"") && !out.contains("RT\"") && !out.contains("C9"),
            "{out}"
        );
        assert!(out.contains("\"keep\":\"yes\""), "{out}");
    }

    #[test]
    fn any_json_whitespace_around_the_colon_is_redacted() {
        for input in [
            "{\"access_token\"\n:\t\"SECRET\"}",
            "{\"access_token\"\r\n :\r\n \"SECRET\"}",
            "{\"refresh_token\"\t:\n\t\"SECRET\", \"keep\": \"yes\"}",
            "{\"keep\":\"a\\\"b\",\"client_secret\"\n\t:\n\"SECRET\"}",
        ] {
            let out = redact_text(input);
            assert!(!out.contains("SECRET"), "{input:?} -> {out:?}");
            assert!(out.contains(REDACTED), "{input:?} -> {out:?}");
        }
        let out = redact_text("{\"access_token\"\n:\t\"SECRET\", \"keep\" :\n \"yes\"}");
        assert!(out.contains("\"keep\" :\n \"yes\""), "{out:?}");
    }

    #[test]
    fn an_escaped_quote_inside_a_token_does_not_leak_its_tail() {
        let out = redact_text(r#"{"access_token": "AB\"CDEF\\", "keep": "yes", "code": "X"}"#);
        assert!(!out.contains("CDEF") && !out.contains("AB"), "{out}");
        assert!(out.contains("\"keep\": \"yes\""), "{out}");
        // An unterminated value is redacted to the end, never echoed.
        let out = redact_text(r#"{"client_secret":"AB\"CD"#);
        assert!(!out.contains("CD"), "{out}");
    }

    #[test]
    fn an_escaped_quote_in_an_earlier_string_does_not_hide_a_later_secret() {
        let out = redact_text(r#"{"keep":"a\"b","access_token":"SECRET"}"#);
        assert!(!out.contains("SECRET"), "{out}");
        assert!(out.contains(r#""keep":"a\"b""#), "{out}");
        // An escaped quote inside a KEY does not end it either.
        let out = redact_text(r#"{"k\"x":"v","client_secret":"S2"}"#);
        assert!(!out.contains("S2"), "{out}");
    }

    #[test]
    fn ordinary_text_is_unchanged() {
        for s in [
            "why?",
            "what? now",
            "unicode=1 decode=2",
            "status code is 5",
            "a ? b",
            "/l/lnk_1",
        ] {
            assert_eq!(redact_text(s), s, "{s}");
        }
    }
}
