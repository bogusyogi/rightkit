//! Match highlighting: FTS5 `snippet()` SQL helper, marker splitting, and a
//! plain-text excerpt for non-FTS content. From MailRight `snippet.rs` /
//! `query.rs::excerpt_around`.

use crate::fold::{fold, folded_len, FoldOptions};

/// Unicode SOH/STX wrap matches; they never occur in real text, so UIs split on them without an HTML parser.
pub const MATCH_START: char = '\u{1}';
pub const MATCH_END: char = '\u{2}';

/// SQL expression selecting an FTS5 snippet over all columns of `fts_table`.
/// The enclosing query must `MATCH` the same table.
pub fn fts_snippet_expr(fts_table: &str, max_tokens: u32) -> String {
    format!("snippet({fts_table}, -1, '{MATCH_START}', '{MATCH_END}', '...', {max_tokens})")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub matched: bool,
}

/// Split a marked snippet into alternating plain/matched spans.
pub fn split_highlights(snippet: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut cur = String::new();
    let mut matched = false;
    for c in snippet.chars() {
        let boundary = (c == MATCH_START && !matched) || (c == MATCH_END && matched);
        if boundary {
            if !cur.is_empty() {
                spans.push(Span {
                    text: std::mem::take(&mut cur),
                    matched,
                });
            }
            matched = c == MATCH_START;
        } else if c != MATCH_START && c != MATCH_END {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        spans.push(Span { text: cur, matched });
    }
    spans
}

fn alnum_terms(terms: &[String]) -> Vec<String> {
    terms
        .iter()
        .map(|t| fold(t))
        .filter(|t| t.chars().any(char::is_alphanumeric))
        .collect()
}

/// Window of at most `max_chars` characters around the first term hit, with `...` on truncated sides.
pub fn excerpt_around(text: &str, terms: &[String], max_chars: usize) -> String {
    let terms = alnum_terms(terms);
    let lower = fold(text);
    // Folding can change byte offsets; search the folded text, then map back to the source
    // character whose folded bytes contain the hit.
    let hit_chars = terms
        .iter()
        .filter_map(|t| lower.find(t.as_str()))
        .min()
        .map(|b| {
            let mut acc = 0;
            for (ci, c) in text.chars().enumerate() {
                acc += folded_len(c, FoldOptions::default());
                if acc > b {
                    return ci;
                }
            }
            text.chars().count()
        })
        .unwrap_or(0);
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.trim().to_string();
    }
    let start = hit_chars.min(chars.len() - 1).saturating_sub(max_chars / 2);
    let end = (start + max_chars).min(chars.len());
    let mut out: String = chars[start..end]
        .iter()
        .collect::<String>()
        .trim()
        .to_string();
    if start > 0 {
        out.insert_str(0, "...");
    }
    if end < chars.len() {
        out.push_str("...");
    }
    out
}

pub fn contains_any_term(text: &str, terms: &[String]) -> bool {
    let lower = fold(text);
    alnum_terms(terms)
        .iter()
        .any(|t| lower.contains(t.as_str()))
}
