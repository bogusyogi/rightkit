//! Source fingerprint matching. Port of `sourceMatchers`, `sourceFindings` and
//! `sourceNonCode` (`ownership.mjs`, `ownership-source.mjs`).
//!
//! Catalog patterns are JavaScript regular expressions evaluated with `fancy-regex`,
//! which supports the lookaround the catalog uses. Two known differences from V8: `\w`,
//! `\d` and `\b` are Unicode-aware here and ASCII-only in JavaScript, and `\s` follows
//! Rust's White_Space set. Patterns in the catalog are ASCII.

use std::collections::HashMap;

use fancy_regex::Regex;
use serde_json::{json, Map, Value};

use crate::error::{Res, ScanError};
use crate::json::{field, items, str_of, truthy};

/// A source fingerprint with its owning capability.
pub struct SourceMatcher {
    capability: String,
    package: String,
    id: String,
    tier: String,
    policy: Option<String>,
    code_only: bool,
    extensions: Option<Vec<String>>,
    /// `excludePaths`: exact files or directory prefixes this rule never matches in.
    exclude_paths: Vec<String>,
    re: Regex,
}

/// `{tier, policy?}` of a fingerprint under its capability (`findingPolicy`).
pub fn finding_policy(capability: &Value, fingerprint: &Value) -> (String, Option<String>) {
    let pick = |value: &Value| value.as_str().map(str::to_string);
    let tier = pick(field(fingerprint, "tier"))
        .or_else(|| pick(field(capability, "tier")))
        .unwrap_or_else(|| "block".into());
    let policy = truthy(field(fingerprint, "policy"))
        .then(|| str_of(field(fingerprint, "policy")).to_string());
    (tier, policy)
}

pub fn source_matchers(manifest: &Value) -> Res<Vec<SourceMatcher>> {
    let mut out = Vec::new();
    for capability in items(field(manifest, "capabilities")) {
        for fingerprint in items(field(capability, "fingerprints")) {
            if field(fingerprint, "type").as_str() != Some("source") {
                continue;
            }
            let pattern = str_of(field(fingerprint, "pattern"));
            let flags: String = str_of(field(fingerprint, "flags"))
                .chars()
                .filter(|c| matches!(c, 'i' | 'm' | 's'))
                .collect();
            let prefix = if flags.is_empty() {
                String::new()
            } else {
                format!("(?{flags})")
            };
            let re = Regex::new(&format!("{prefix}{pattern}")).map_err(|error| {
                ScanError::new(format!("Invalid regular expression: /{pattern}/: {error}"))
            })?;
            let (tier, policy) = finding_policy(capability, fingerprint);
            out.push(SourceMatcher {
                capability: str_of(field(capability, "capability")).into(),
                package: str_of(field(capability, "package")).into(),
                id: str_of(field(fingerprint, "id")).into(),
                tier,
                policy,
                code_only: truthy(field(fingerprint, "codeOnly")),
                extensions: field(fingerprint, "extensions")
                    .as_array()
                    .map(|list| list.iter().map(|e| str_of(e).to_string()).collect()),
                exclude_paths: field(fingerprint, "excludePaths")
                    .as_array()
                    .map(|list| list.iter().map(|e| str_of(e).to_string()).collect())
                    .unwrap_or_default(),
                re,
            });
        }
    }
    Ok(out)
}

/// `path.posix.extname(rel).toLowerCase()`.
pub fn extname(rel: &str) -> String {
    let base = rel.rsplit('/').next().unwrap_or("");
    match base.rfind('.') {
        Some(0) | None => String::new(),
        Some(at) => base[at..].to_lowercase(),
    }
}

fn comment_only(line: &str) -> bool {
    let t = line.trim();
    t.starts_with("//")
        || t.starts_with('#')
        || t.starts_with('*')
        || t.starts_with("/*")
        || t.starts_with("--")
}

/// `text.split(/\r?\n/)`.
fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

fn excluded(matcher: &SourceMatcher, rel: &str) -> bool {
    matcher.exclude_paths.iter().any(|p| {
        rel == p
            || rel.starts_with(&if p.ends_with('/') {
                p.clone()
            } else {
                format!("{p}/")
            })
    })
}

pub fn source_findings(
    rel: &str,
    bytes: &[u8],
    matchers: &[SourceMatcher],
) -> Vec<Map<String, Value>> {
    let ext = extname(rel);
    let text = String::from_utf8_lossy(bytes);
    let text: &str = &text;
    let lines = split_lines(text);
    let mut findings: Vec<Map<String, Value>> = Vec::new();
    let mut ordinals: HashMap<String, u64> = HashMap::new();
    let mut push = |m: &SourceMatcher, line: usize, line_text: &str| {
        let identity = format!("{}\0{}", m.id, line_text);
        let ordinal = *ordinals.get(&identity).unwrap_or(&0);
        ordinals.insert(identity, ordinal + 1);
        let mut finding = Map::new();
        finding.insert("fingerprint".into(), json!(m.id));
        finding.insert("capability".into(), json!(m.capability));
        finding.insert("package".into(), json!(m.package));
        finding.insert("tier".into(), json!(m.tier));
        if let Some(policy) = &m.policy {
            finding.insert("policy".into(), json!(policy));
        }
        finding.insert("kind".into(), json!("source"));
        finding.insert("line".into(), json!(line));
        finding.insert("lineText".into(), json!(line_text));
        finding.insert("ordinal".into(), json!(ordinal));
        finding.insert(
            "text".into(),
            json!(line_text.trim().chars().take(160).collect::<String>()),
        );
        findings.push(finding);
    };
    let applicable: Vec<&SourceMatcher> = matchers
        .iter()
        .filter(|m| m.extensions.as_ref().is_none_or(|list| list.contains(&ext)))
        .filter(|m| !excluded(m, rel))
        .collect();
    let line_matchers: Vec<&&SourceMatcher> = applicable.iter().filter(|m| !m.code_only).collect();
    if !line_matchers.is_empty() {
        for (index, line) in lines.iter().enumerate() {
            if comment_only(line) {
                continue;
            }
            for m in &line_matchers {
                if m.re.is_match(line).unwrap_or(false) {
                    push(m, index + 1, line);
                }
            }
        }
    }
    let code_matchers: Vec<&&SourceMatcher> = applicable.iter().filter(|m| m.code_only).collect();
    if !code_matchers.is_empty() {
        let non_code = source_non_code(text, ext == ".rs");
        for m in code_matchers {
            let mut reported = std::collections::HashSet::new();
            let (mut line, mut offset) = (1usize, 0usize);
            for found in m.re.find_iter(text) {
                let Ok(found) = found else { break };
                if found.as_str().is_empty() || non_code[found.start()] {
                    continue;
                }
                while offset < found.start() {
                    if text.as_bytes()[offset] == b'\n' {
                        line += 1;
                    }
                    offset += 1;
                }
                if !reported.insert(line) {
                    continue;
                }
                push(m, line, lines.get(line - 1).copied().unwrap_or(""));
            }
        }
    }
    findings
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Rust character literal at `rest` (which starts with `'`), returning its byte length.
fn rust_char_literal(rest: &str) -> Option<usize> {
    let body = &rest[1..];
    let mut chars = body.chars();
    let first = chars.next()?;
    if first == '\\' {
        let after = &body[1..];
        // \u{hex+}, \xHH, or any character except CR and LF, each followed by `'`.
        if let Some(inner) = after.strip_prefix("u{") {
            if let Some(close) = inner.find('}') {
                if close > 0
                    && inner[..close].bytes().all(|b| b.is_ascii_hexdigit())
                    && inner[close + 1..].starts_with('\'')
                {
                    return Some(1 + 1 + 2 + close + 1 + 1);
                }
            }
        }
        if let Some(hex) = after.strip_prefix('x') {
            if hex.len() >= 2
                && hex.as_bytes()[..2].iter().all(u8::is_ascii_hexdigit)
                && hex[2..].starts_with('\'')
            {
                return Some(1 + 1 + 1 + 2 + 1);
            }
        }
        let escaped = after.chars().next()?;
        if escaped != '\r' && escaped != '\n' && after[escaped.len_utf8()..].starts_with('\'') {
            return Some(1 + 1 + escaped.len_utf8() + 1);
        }
        return None;
    }
    if first != '\'' && first != '\r' && first != '\n' && body[first.len_utf8()..].starts_with('\'')
    {
        return Some(1 + first.len_utf8() + 1);
    }
    None
}

/// Marks bytes inside comments and literals. Code-only patterns must start outside
/// them; offsets and newlines stay intact for multiline call reporting.
pub fn source_non_code(text: &str, rust: bool) -> Vec<bool> {
    let bytes = text.as_bytes();
    let mut non_code = vec![false; bytes.len()];
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i..].starts_with(b"//") || bytes[i..].starts_with(b"/*") {
            if bytes.get(i + 1) == Some(&b'/') {
                i = bytes[i + 2..]
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(bytes.len(), |p| p + i + 2);
            } else {
                i += 2;
                let mut depth = 1;
                while i < bytes.len() && depth > 0 {
                    if rust && bytes[i..].starts_with(b"/*") {
                        depth += 1;
                        i += 2;
                    } else if bytes[i..].starts_with(b"*/") {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            non_code[start..i.min(bytes.len())].fill(true);
            continue;
        }
        let raw = if rust && (i == 0 || !is_word(bytes[i - 1])) {
            if matches!(bytes[i], b'b' | b'c' | b'r') {
                raw_string_open(&text[i..])
            } else {
                None
            }
        } else {
            None
        };
        if let Some((open_len, hashes)) = raw {
            let closing = format!("\"{}", "#".repeat(hashes));
            i = text[i + open_len..]
                .find(&closing)
                .map_or(bytes.len(), |p| i + open_len + p + closing.len());
        } else if bytes[i] == b'"'
            || (!rust && (bytes[i] == b'\'' || bytes[i] == b'`'))
            || (rust && bytes[i] == b'\'' && rust_char_literal(&text[i..]).is_some())
        {
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' {
                    i = (i + 2).min(bytes.len());
                } else {
                    let current = bytes[i];
                    i += 1;
                    if current == quote {
                        break;
                    }
                }
            }
        } else {
            i += 1;
            continue;
        }
        non_code[start..i.min(bytes.len())].fill(true);
    }
    non_code
}

/// `^(?:br|cr|r)(#*)"`: returns the opener length and hash count.
fn raw_string_open(rest: &str) -> Option<(usize, usize)> {
    let after = ["br", "cr", "r"]
        .iter()
        .find_map(|p| rest.strip_prefix(p).map(|r| (p.len(), r)))?;
    let hashes = after.1.bytes().take_while(|b| *b == b'#').count();
    after.1[hashes..]
        .starts_with('"')
        .then_some((after.0 + hashes + 1, hashes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_code_marks_comments_and_strings() {
        let text = "a(); // b()\nc(\"d()\"); /* e() */ f();";
        let mask = source_non_code(text, true);
        let at = |needle: &str| mask[text.find(needle).unwrap()];
        assert!(!at("a()") && at("b()") && !at("c(") && at("d()") && at("e()") && !at("f()"));
    }

    #[test]
    fn rust_lifetimes_are_not_char_literals() {
        let text = "fn f<'a>(x: &'a str) { g() } 'x' h()";
        let mask = source_non_code(text, true);
        assert!(!mask[text.find("g()").unwrap()]);
        assert!(mask[text.find("'x'").unwrap()]);
    }

    #[test]
    fn extname_follows_node() {
        assert_eq!(extname("src/MAIN.RS"), ".rs");
        assert_eq!(extname(".gitignore"), "");
        assert_eq!(extname("a/b"), "");
    }
}
