//! L11: query compiler. `rightkit-search` owns the lexer, parser and term
//! data (`Query`, `OpTerm`); this module owns the Gmail-style operator vocabulary
//! (`syntax`) and compiles the parsed `Query` into the flat `SearchTerms`
//! accumulator the SQL builder (`search.rs`/`candidates.rs`) already consumes.
//! `parse_search_checked` is the typed-error entry point (unsupported
//! operators, unbalanced groups); `parse_search` wraps it for the existing
//! infallible call sites, silently dropping a malformed query to empty terms.

pub use super::search_terms::SearchTerms;
use rightkit_search::ast::{ParseError, Syntax};
pub use rightkit_search::snippet::{contains_any_term, excerpt_around};
use rightkit_search::terms::{OpTerm, Query};
use rightkit_search::values::{now_ms, parse_date_ms, parse_relative_ms, parse_size_bytes};

/// the Gmail-style operator vocabulary (lowercase, no colon).
const OPERATORS: [&str; 18] = [
    "from",
    "to",
    "cc",
    "bcc",
    "subject",
    "filename",
    "has",
    "is",
    "after",
    "before",
    "newer",
    "older",
    "in",
    "label",
    "size",
    "larger",
    "smaller",
    "rfc822msgid",
];

pub fn syntax() -> Syntax {
    Syntax::new(OPERATORS)
}

/// Strip the characters the free-text builder never keeps.
fn clean(value: &str) -> String {
    value.chars().filter(|c| !"\"*()".contains(*c)).collect()
}

/// Apply one operator term onto the accumulator. `negated` routes a negated
/// term to its exclusion field instead of its positive field; some operators
/// (has:/is:/after:/before:/newer:/older:/size:/larger:/smaller:/rfc822msgid:)
/// have no separate negated representation and are applied as written, same
/// as the old parser. Last-wins for repeated operators; a value that does not
/// parse is dropped.
fn apply_op(s: &mut SearchTerms, t: &OpTerm, now_ms: i64) {
    let value = t.value.trim();
    if value.is_empty() {
        return;
    }
    match t.op.as_str() {
        "from" => {
            if t.negated {
                s.negated_from.push(value.to_string());
            } else {
                s.from = Some(value.to_string());
            }
        }
        "to" => {
            if t.negated {
                s.negated_to.push(value.to_string());
            } else {
                s.to = Some(value.to_string());
            }
        }
        "cc" => {
            if t.negated {
                s.cc_negated.push(value.to_string());
            } else {
                s.cc = Some(value.to_string());
            }
        }
        "bcc" => {
            if t.negated {
                s.bcc_negated.push(value.to_string());
            } else {
                s.bcc = Some(value.to_string());
            }
        }
        "subject" => s.subject = Some(value.to_string()),
        "filename" => s.filename = Some(value.to_string()),
        "label" => {
            s.label = Some(value.to_string());
            s.label_negated = t.negated;
        }
        "in" => s.in_folder = Some(value.to_ascii_uppercase()),
        "after" => {
            if let Some(ms) = parse_date_ms(value) {
                s.after_ms = Some(ms);
            }
        }
        "before" => {
            if let Some(ms) = parse_date_ms(value) {
                s.before_ms = Some(ms);
            }
        }
        "newer" => {
            if let Some(ms) = parse_relative_ms(value, now_ms) {
                s.newer_ms = Some(ms);
            }
        }
        "older" => {
            if let Some(ms) = parse_relative_ms(value, now_ms) {
                s.older_ms = Some(ms);
            }
        }
        "size" => {
            if let Some(bytes) = parse_size_bytes(value) {
                // Gmail's bare `size:` behaves as an exact-or-larger
                // convenience alias for `larger:` (see field doc comment).
                s.size_min = Some(bytes);
            }
        }
        "larger" => {
            if let Some(bytes) = parse_size_bytes(value) {
                s.size_min = Some(bytes);
            }
        }
        "smaller" => {
            if let Some(bytes) = parse_size_bytes(value) {
                s.size_max = Some(bytes);
            }
        }
        "rfc822msgid" => {
            s.rfc822_message_id = Some(value.trim_matches(['<', '>']).to_string());
        }
        "has" => {
            let lower = value.to_ascii_lowercase();
            if lower == "attachment" || lower == "attachments" {
                s.has_attachment = true;
            }
        }
        "is" => match value.to_ascii_lowercase().as_str() {
            "unread" => s.unread = true,
            "read" => s.is_read = Some(true),
            "starred" => s.starred = true,
            "important" => s.important = true,
            _ => {}
        },
        _ => {}
    }
}

impl SearchTerms {
    /// Compile a parsed `rightkit_search` query into the flat accumulator.
    /// Operator-containing OR groups are flattened to AND: their operator
    /// terms apply as top-level filters, and their bare-text terms form one
    /// text disjunction (or, for negated bare text, exclusions).
    pub fn from_query(q: &Query, now_ms: i64) -> Self {
        let mut s = SearchTerms::default();
        for t in &q.ops {
            apply_op(&mut s, t, now_ms);
        }
        s.text.extend(q.text.iter().cloned());
        s.phrases.extend(q.phrases.iter().cloned());
        s.negated_text.extend(q.negated_text.iter().cloned());
        s.or_groups.extend(q.or_groups.iter().cloned());
        for group in &q.op_or_groups {
            let mut text_or: Vec<String> = Vec::new();
            for t in group {
                if !t.op.is_empty() {
                    apply_op(&mut s, t, now_ms);
                } else if t.negated {
                    let value = if t.is_phrase {
                        t.value.trim().to_string()
                    } else {
                        clean(&t.value)
                    };
                    if !value.is_empty() {
                        s.negated_text.push(value);
                    }
                } else {
                    let cleaned = clean(&t.value);
                    if !cleaned.is_empty() {
                        text_or.push(cleaned);
                    }
                }
            }
            if !text_or.is_empty() {
                s.or_groups.push(text_or);
            }
        }
        s
    }
}

/// Parse a search string into operators and free-text terms, returning a
/// typed `ParseError` when the query contains an unsupported operator or an
/// unbalanced group instead of silently mistreating it as free text.
pub fn parse_search_checked(q: &str) -> Result<SearchTerms, ParseError> {
    let query = Query::parse(q, &syntax())?;
    Ok(SearchTerms::from_query(&query, now_ms()))
}

/// Parse a search string into operators and free-text terms. Supports Gmail
/// parity v1 (from:/to:/cc:/bcc:/subject:/filename:, has:attachment, is:*,
/// after:/before:/newer:/older:, in:*, label:foo, negation, OR/brace
/// grouping), size:/larger:/smaller:/rfc822msgid:, quoted-phrase FTS terms,
/// and parenthesis grouping. Malformed input (unsupported operator,
/// unbalanced group) is treated as an empty match rather than propagating an
/// error — existing callers of this function are infallible; use
/// `parse_search_checked` where the error should reach the user.
pub fn parse_search(q: &str) -> SearchTerms {
    parse_search_checked(q).unwrap_or_default()
}
