//! JSON-lines driver for offline polish comparisons: apply a host's polish function to the `text`
//! field of each `{"id": "...", "text": "..."}` line.
//!
//! The host passes the polish function (for example
//! [`deterministic_polish`](crate::text_pipeline::deterministic_polish) or a closure over
//! [`deterministic_polish_with_brand`](crate::text_pipeline::deterministic_polish_with_brand) and
//! its brand table), so the harness scores what the product emits. The field reader is the small
//! flat-object scanner the original harness used (no JSON dependency): it finds the first
//! `"key"` on the line and reads the next string value. Nested objects and values escaped as
//! `\\"` at a string end are not handled.

use std::io::{BufRead, Write};

/// Decode the JSON escapes `\n \t \r \" \\ \uXXXX` of an already-delimited string body; any other
/// escaped character stands for itself, an invalid `\u` is dropped and a trailing lone `\` ends
/// the string.
pub fn json_unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('u') => {
                let hex: String = chars.by_ref().take(4).collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

/// Escape `s` for a JSON string body: `"`, `\`, `\n`, `\r`, `\t`, and other control characters as
/// `\u00XX`.
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Pull one string field out of a flat JSON object without a JSON dependency. `None` when the key
/// or its string value is absent.
pub fn json_string_field(line: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let start = line.find(&pat)? + pat.len();
    let rest = &line[start..];
    let colon = rest.find(':')? + 1;
    let rest = &rest[colon..];
    let open = rest.find('"')? + 1;
    let bytes = rest.as_bytes();
    let mut i = open;
    while i < bytes.len() {
        if bytes[i] == b'"' && bytes[i - 1] != b'\\' {
            return Some(json_unescape(&rest[open..i]));
        }
        i += 1;
    }
    None
}

/// Polish one `{"id": ..., "text": ...}` line: the output line (without a newline) has the same
/// shape with `text` replaced by `polish(text)`. A missing `id` or `text` reads as empty.
pub fn polish_jsonl_line(line: &str, polish: impl Fn(&str) -> String) -> String {
    let id = json_string_field(line, "id").unwrap_or_default();
    let text = json_string_field(line, "text").unwrap_or_default();
    let polished = polish(&text);
    format!(
        "{{\"id\":\"{}\",\"text\":\"{}\"}}",
        json_escape(&id),
        json_escape(&polished)
    )
}

/// Run [`polish_jsonl_line`] over every non-blank line of `input`, writing one output line each.
/// A line that cannot be read and a write that fails are skipped, as the original harness did.
pub fn polish_jsonl(input: impl BufRead, mut output: impl Write, polish: impl Fn(&str) -> String) {
    for line in input.lines() {
        let Ok(line) = line else { continue };
        if line.trim().is_empty() {
            continue;
        }
        let _ = writeln!(output, "{}", polish_jsonl_line(&line, &polish));
    }
}

/// Apply `polish` to each element of a JSON array of strings and return the polished array as JSON
/// text. The host passes the array text (for example read from stdin); an error is the parse or
/// serialise error's text. Needs the `store` feature (it enables `serde_json`).
#[cfg(feature = "store")]
pub fn polish_json_array(input: &str, polish: impl Fn(&str) -> String) -> Result<String, String> {
    let values: Vec<String> = serde_json::from_str(input).map_err(|e| e.to_string())?;
    let polished: Vec<String> = values.iter().map(|value| polish(value)).collect();
    serde_json::to_string(&polished).map_err(|e| e.to_string())
}
