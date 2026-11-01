//! Shared secret detection (A195).
//!
//! Pure scanner used by the engine's reversible redactor (model boundary) and
//! by the storage layer's irreversible persistence redaction. It reports byte
//! spans of secrets; callers decide the replacement. Raw values are never
//! logged or retained here.

use std::borrow::Cow;

/// Prefix shared by every redaction placeholder.
pub const SECRET_PLACEHOLDER_OPEN: &str = "[[CR_SECRET_";
/// Suffix shared by every redaction placeholder.
pub const SECRET_PLACEHOLDER_CLOSE: &str = "]]";

const MIN_ASSIGNED_VALUE_LEN: usize = 8;
const NAME_KEYWORDS: &[&str] = &[
    "SECRET",
    "TOKEN",
    "PASSWORD",
    "PASSWD",
    "API_KEY",
    "APIKEY",
    "PRIVATE_KEY",
    "ACCESS_KEY",
    "CREDENTIAL",
];

/// One detected secret: byte range `start..end` of the scanned text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretSpan {
    pub start: usize,
    pub end: usize,
    pub kind: &'static str,
}

fn is_tok(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'
}

fn classify(run: &str, after_bearer: bool) -> Option<&'static str> {
    let len = run.len();
    if run.starts_with("sk-ant-") && len >= 20 {
        return Some("ANTHROPIC");
    }
    if run.starts_with("sk-") && len >= 20 {
        return Some("APIKEY");
    }
    if ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"]
        .iter()
        .any(|p| run.starts_with(p))
        && len >= 20
    {
        return Some("GITHUB");
    }
    if run.starts_with("github_pat_") && len >= 30 {
        return Some("GITHUB");
    }
    if (run.starts_with("AKIA") || run.starts_with("ASIA"))
        && len == 20
        && run
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return Some("AWS");
    }
    if run.starts_with("AIza") && len == 39 {
        return Some("GOOGLE");
    }
    if run.starts_with("xox")
        && len >= 15
        && matches!(
            run.as_bytes().get(3),
            Some(b'b' | b'a' | b'p' | b'r' | b's')
        )
        && run.as_bytes().get(4) == Some(&b'-')
    {
        return Some("SLACK");
    }
    if run.starts_with("eyJ") && len >= 30 {
        let parts: Vec<&str> = run.split('.').collect();
        if parts.len() == 3 && parts.iter().all(|p| !p.is_empty()) {
            return Some("JWT");
        }
    }
    if after_bearer && len >= 16 {
        return Some("BEARER");
    }
    None
}

fn name_is_secretish(name: &str) -> bool {
    let norm = name.to_ascii_uppercase().replace('-', "_");
    NAME_KEYWORDS.iter().any(|k| norm.contains(k))
}

fn ends_with_bearer(prefix: &str) -> bool {
    let b = prefix.as_bytes();
    b.len() >= 7 && b[b.len() - 7..].eq_ignore_ascii_case(b"bearer ")
}

/// Byte span of the value in `NAME=value`, `NAME: value`, `"name": "value"`
/// style assignments, where `name_end` is the end of the name token.
fn assigned_value_span(s: &str, name_end: usize) -> Option<(usize, usize)> {
    let b = s.as_bytes();
    let n = b.len();
    let mut i = name_end;
    if i < n && (b[i] == b'"' || b[i] == b'\'') {
        i += 1;
    }
    while i < n && b[i] == b' ' {
        i += 1;
    }
    if i >= n || (b[i] != b'=' && b[i] != b':') {
        return None;
    }
    i += 1;
    while i < n && b[i] == b' ' {
        i += 1;
    }
    if i < n && (b[i] == b'"' || b[i] == b'\'') {
        i += 1;
    }
    let vs = i;
    while i < n
        && !b[i].is_ascii_whitespace()
        && !matches!(b[i], b'"' | b'\'' | b',' | b';' | b'}' | b']')
    {
        i += 1;
    }
    if i > vs {
        Some((vs, i))
    } else {
        None
    }
}

fn pem_spans(text: &str) -> Vec<SecretSpan> {
    let mut spans = Vec::new();
    let mut base = 0usize;
    while let Some(rel) = text[base..].find("-----BEGIN ") {
        let begin = base + rel;
        let after = &text[begin..];
        let header_end = match after[11..].find("-----") {
            Some(e) => e + 11 + 5,
            None => break,
        };
        if !after[..header_end].contains("PRIVATE KEY") {
            base = begin + header_end;
            continue;
        }
        // Unterminated block: redact through end of text (fail closed).
        let block_end = match after.find("-----END ") {
            Some(e) => after[e + 9..]
                .find("-----")
                .map(|x| e + 9 + x + 5)
                .unwrap_or(after.len()),
            None => after.len(),
        };
        spans.push(SecretSpan {
            start: begin,
            end: begin + block_end,
            kind: "PRIVATEKEY",
        });
        base = begin + block_end;
    }
    spans
}

/// Find every secret in `text`, as non-overlapping spans sorted by start.
pub fn find_secrets(text: &str) -> Vec<SecretSpan> {
    let pem = pem_spans(text);
    let in_pem = |i: usize| pem.iter().any(|s| i >= s.start && i < s.end);
    let bytes = text.as_bytes();
    let n = bytes.len();
    let mut found: Vec<SecretSpan> = Vec::new();
    let mut i = 0usize;
    while i < n {
        if !is_tok(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && is_tok(bytes[i]) {
            i += 1;
        }
        let mut end = i;
        while end > start && bytes[end - 1] == b'.' {
            end -= 1;
        }
        if end == start || in_pem(start) {
            continue;
        }
        let run = &text[start..end];
        if let Some(kind) = classify(run, ends_with_bearer(&text[..start])) {
            found.push(SecretSpan { start, end, kind });
            continue;
        }
        if name_is_secretish(run) {
            if let Some((vs, ve)) = assigned_value_span(text, end) {
                let value = &text[vs..ve];
                if value.len() >= MIN_ASSIGNED_VALUE_LEN
                    && !value.starts_with(SECRET_PLACEHOLDER_OPEN)
                {
                    found.push(SecretSpan {
                        start: vs,
                        end: ve,
                        kind: "ASSIGNED",
                    });
                    i = ve;
                }
            }
        }
    }
    found.extend(pem);
    found.sort_by_key(|s| s.start);
    found
}

/// Irreversibly replace every secret with `[[CR_SECRET_<KIND>_REDACTED]]`.
/// Returns the input unchanged (borrowed) when nothing matched.
pub fn redact_irreversible(text: &str) -> Cow<'_, str> {
    let spans = find_secrets(text);
    if spans.is_empty() {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0usize;
    for s in spans {
        out.push_str(&text[copied..s.start]);
        out.push_str(SECRET_PLACEHOLDER_OPEN);
        out.push_str(s.kind);
        out.push_str("_REDACTED");
        out.push_str(SECRET_PLACEHOLDER_CLOSE);
        copied = s.end;
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

fn walk(v: &mut serde_json::Value, changed: &mut bool) {
    match v {
        serde_json::Value::String(s) => {
            if let Cow::Owned(r) = redact_irreversible(s) {
                *s = r;
                *changed = true;
            }
        }
        serde_json::Value::Array(a) => a.iter_mut().for_each(|x| walk(x, changed)),
        serde_json::Value::Object(o) => o.iter_mut().for_each(|(_, x)| walk(x, changed)),
        _ => {}
    }
}

/// Redact every string inside a serialized JSON payload (so escaped quotes
/// and newline escapes cannot hide a secret), re-serializing only when
/// something changed. Non-JSON input is redacted as plain text.
pub fn redact_json_payload(payload: &str) -> Cow<'_, str> {
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(mut v) => {
            let mut changed = false;
            walk(&mut v, &mut changed);
            if !changed {
                return Cow::Borrowed(payload);
            }
            match serde_json::to_string(&v) {
                Ok(s) => Cow::Owned(s),
                Err(_) => redact_irreversible(payload),
            }
        }
        Err(_) => redact_irreversible(payload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_sk() -> String {
        format!("sk-{}", "FAKE".repeat(8))
    }

    #[test]
    fn irreversible_redaction_removes_value_and_is_idempotent() {
        let t = format!("a {} b", fake_sk());
        let r = redact_irreversible(&t).into_owned();
        assert!(!r.contains("FAKE"));
        assert!(r.contains("[[CR_SECRET_APIKEY_REDACTED]]"));
        assert_eq!(redact_irreversible(&r), r);
    }

    #[test]
    fn clean_text_is_borrowed() {
        assert!(matches!(
            redact_irreversible("nothing here"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn json_payload_secrets_in_nested_escaped_strings_are_redacted() {
        let inner = "{\"api_key\": \"fake-value-12345\"}";
        let payload = serde_json::json!({"content": inner, "n": 1}).to_string();
        let out = redact_json_payload(&payload);
        assert!(!out.contains("fake-value-12345"));
        assert!(serde_json::from_str::<serde_json::Value>(&out).is_ok());
    }

    #[test]
    fn json_payload_without_secrets_is_byte_identical() {
        let p = r#"{"a":"hello","b":[1,2]}"#;
        assert!(matches!(redact_json_payload(p), Cow::Borrowed(_)));
    }

    #[test]
    fn pem_block_is_one_span() {
        // Built at run time so the source holds no PEM header (secret scans read sources).
        let t = format!("x -----BEGIN {k}-----\nFAKE\n-----END {k}----- y", k = "PRIVATE KEY");
        let s = find_secrets(&t);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].kind, "PRIVATEKEY");
    }
}
