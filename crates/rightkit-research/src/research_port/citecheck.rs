//! Port of `tools/research-core/citecheck.py`: citation-to-sentence binding and
//! source-support verification for a Markdown draft.
//!
//! Every `[+id]` marker must name an evidence row, every factual sentence must
//! carry a citation, and each cited sentence is graded against the opened
//! source text by number presence and content-word overlap.
//!
//! Deliberate divergences from the Python:
//! - Sentence splitting replaces the `(?<=[.!?])\s+(?=[A-Z\[])` lookaround,
//!   which the regex crate does not support, with an equivalent scan.
//! - Lines are split with `str::lines`, not Python's wider `splitlines` set.
//! - A missing evidence `id` is an error (Python raises KeyError).

use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use super::pyval::{py_list_repr, py_str, py_str_or};

const STOP_WORDS: [&str; 12] = [
    "the", "and", "that", "with", "from", "this", "have", "were", "been", "into", "than", "for",
];

/// Compiles one of the constant patterns below. Every pattern is exercised by
/// the unit tests in this module, so a failure here is a programming error.
fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).expect("citecheck pattern is a valid constant")
}

fn cite_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(r"\[\+\s*([A-Za-z0-9_.:\-]+)\s*\]"))
}

fn number_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(r"[-+]?\d[\d,]*(?:\.\d+)?%?"))
}

fn word_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(r"[A-Za-z0-9]{3,}"))
}

fn status_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(r"(?i)\[(?:confidence|status):[^\]]+\]"))
}

fn leading_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(r"^\[[A-Z][A-Z\-]*\]\s*"))
}

fn bullet_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(r"^[-*]\s+"))
}

fn factual_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        compile(
            r"(?i)\b(is|are|was|were|has|have|causes?|reduces?|increases?|requires?|prohibits?)\b",
        )
    })
}

fn merge_full_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| compile(r"(?i)^(?:\[+?[^\]]+\]|\[(?:confidence|status):[^\]]+\]\s*)+$"))
}

/// Python `re.split(r'(?<=[.!?])\s+(?=[A-Z\[])', text)`, written as a scan:
/// a maximal whitespace run splits the text when the character before it is
/// `.`, `!` or `?` and the character after it is an ASCII capital or `[`.
fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut parts: Vec<String> = Vec::new();
    let mut start = 0usize;
    let mut index = 0usize;
    while index < chars.len() {
        let (byte_index, ch) = chars[index];
        if ch.is_whitespace() && index > 0 && matches!(chars[index - 1].1, '.' | '!' | '?') {
            let mut end = index;
            while end < chars.len() && chars[end].1.is_whitespace() {
                end += 1;
            }
            if end < chars.len() && (chars[end].1.is_ascii_uppercase() || chars[end].1 == '[') {
                parts.push(text[start..byte_index].to_string());
                start = chars[end].0;
            }
            index = end;
            continue;
        }
        index += 1;
    }
    parts.push(text[start..].to_string());
    parts
}

/// `_sentences`: `(index, text)` rows for every sentence outside the
/// Sources and References sections.
fn sentences(markdown: &str) -> Vec<(usize, String)> {
    let mut rows: Vec<(usize, String)> = Vec::new();
    let mut section = String::new();
    let mut next_index = 0usize;
    for raw_line in markdown.lines() {
        let line = raw_line.trim();
        if line.starts_with('#') {
            section = line.trim_start_matches('#').trim().to_lowercase();
            continue;
        }
        if line.is_empty()
            || section == "sources"
            || section == "references"
            || line.starts_with("_as of")
        {
            continue;
        }
        let is_bullet = bullet_re().is_match(line);
        let text = bullet_re().replace(line, "").into_owned();
        let chunks: Vec<String> = if is_bullet {
            vec![text]
        } else {
            let mut merged: Vec<String> = Vec::new();
            for chunk in split_sentences(&text) {
                let stripped = chunk.trim().to_string();
                if !merged.is_empty() && merge_full_re().is_match(&stripped) {
                    let last = merged.len() - 1;
                    merged[last] = format!("{} {}", merged[last], stripped);
                } else {
                    merged.push(chunk);
                }
            }
            merged
        };
        for chunk in chunks {
            let stripped = chunk.trim().to_string();
            if !stripped.is_empty() {
                rows.push((next_index, stripped));
                next_index += 1;
            }
        }
    }
    rows
}

/// `_norm_words`: lowercase `[A-Za-z0-9]{3,}` tokens without stop words.
fn normalised_words(text: &str) -> Vec<String> {
    word_re()
        .find_iter(text)
        .map(|found| found.as_str().to_lowercase())
        .filter(|word| !STOP_WORDS.contains(&word.as_str()))
        .collect()
}

fn is_all_digits(word: &str) -> bool {
    !word.is_empty() && word.chars().all(|ch| ch.is_ascii_digit())
}

/// `_support_blob`: the evidence text fields joined by spaces.
fn support_blob(evidence: &Value) -> String {
    [
        "quote_or_paraphrase",
        "evidence_text",
        "body_plain",
        "title",
        "locator",
    ]
    .iter()
    .map(|key| py_str_or(evidence.get(*key), ""))
    .collect::<Vec<String>>()
    .join(" ")
}

/// `_verdict`: `(verdict, reason)` for one sentence against one evidence row.
fn grade(sentence: &str, evidence: &Value) -> (String, String) {
    let without_cites = cite_re().replace_all(sentence, "").into_owned();
    let without_status = status_tag_re().replace_all(&without_cites, "").into_owned();
    let clean = leading_tag_re().replace(&without_status, "").into_owned();

    let numbers: Vec<String> = number_re()
        .find_iter(&clean)
        .map(|found| found.as_str().replace(',', ""))
        .collect();
    let blob = support_blob(evidence);
    let blob_numbers = blob.replace(',', "");
    if !numbers.is_empty()
        && !numbers
            .iter()
            .all(|number| blob_numbers.contains(number.as_str()))
    {
        return (
            "unsupported".to_string(),
            format!(
                "sentence numbers {} are not all present in evidence",
                py_list_repr(&numbers)
            ),
        );
    }

    let words = normalised_words(&clean);
    if words.is_empty() {
        return (
            "supported".to_string(),
            "citation-only or metadata sentence".to_string(),
        );
    }
    let evidence_words: BTreeSet<String> = normalised_words(&blob).into_iter().collect();
    let content: BTreeSet<String> = words
        .into_iter()
        .filter(|word| !is_all_digits(word))
        .collect();
    let shared = content.intersection(&evidence_words).count() as f64;
    let overlap = shared / (content.len().max(1) as f64);
    let reason = format!("lexical overlap={overlap:.2}");

    if overlap >= 0.55 || (!numbers.is_empty() && overlap >= 0.25) {
        ("supported".to_string(), reason)
    } else if overlap >= 0.30 {
        ("partially-supported".to_string(), reason)
    } else {
        ("unsupported".to_string(), reason)
    }
}

/// `check(markdown, evidence)`: the citation-binding report.
pub fn check(markdown: &str, evidence: &[Value]) -> Result<Value, String> {
    let mut by_id: BTreeMap<String, &Value> = BTreeMap::new();
    for row in evidence {
        match row.get("id") {
            Some(id) => {
                by_id.insert(py_str(Some(id)), row);
            }
            None => return Err("evidence row missing id".to_string()),
        }
    }

    let mut pairs: Vec<Value> = Vec::new();
    let mut unbound: Vec<Value> = Vec::new();
    let mut dangling: Vec<Value> = Vec::new();
    let mut unsupported: Vec<Value> = Vec::new();

    for (index, text) in sentences(markdown) {
        let ids: Vec<String> = cite_re()
            .captures_iter(&text)
            .map(|captures| captures[1].to_string())
            .collect();
        let factual = number_re().is_match(&text) || factual_re().is_match(&text);
        if ids.is_empty() {
            if factual {
                unbound.push(json!({"index": index, "text": text}));
            }
            continue;
        }
        for eid in &ids {
            match by_id.get(eid) {
                None => {
                    let item = json!({
                        "index": index,
                        "text": text,
                        "evidence_id": eid,
                        "verdict": "dangling",
                    });
                    pairs.push(item.clone());
                    dangling.push(item);
                }
                Some(source) => {
                    let (verdict, reason) = grade(&text, source);
                    let item = json!({
                        "index": index,
                        "text": text,
                        "evidence_id": eid,
                        "verdict": verdict,
                        "reason": reason,
                    });
                    pairs.push(item.clone());
                    if verdict == "unsupported" || verdict == "partially-supported" {
                        unsupported.push(item);
                    }
                }
            }
        }
    }

    let ok = unbound.is_empty() && dangling.is_empty() && unsupported.is_empty();
    Ok(json!({
        "ok": ok,
        "summary": {
            "pairs": pairs.len(),
            "unbound": unbound.len(),
            "dangling": dangling.len(),
            "unsupported_or_partial": unsupported.len(),
        },
        "pairs": pairs,
        "unbound": unbound,
        "dangling": dangling,
        "unsupported": unsupported,
    }))
}
