//! Prompt-text-shape extractors shared by [`crate::agent_router::signal::TaskSignalBuilder`].
//!
//! `orchestration::dispatch::DispatchRouter` computes an equivalent set of
//! signals (word count, path references, sentence count, question shape,
//! broad-scope phrasing) but its helpers are private free functions with no
//! public surface, and the lane fence forbids editing `dispatch.rs`. Per ADR
//! §D1 ("reused from `DispatchRouter`'s extractors — shared helpers, not a
//! second implementation"), this module is the shared extraction point:
//! logic is written once here, and a future lane can refactor `dispatch.rs`
//! to delegate to it without behavior change. Until then this is
//! intentionally the SAME algorithm, not a divergent reinterpretation.

use crate::agent_router::signal::PromptShape;

const BROAD_SCOPE_PHRASES: [&str; 12] = [
    "across the repo",
    "across the codebase",
    "across the project",
    "repo-wide",
    "entire repo",
    "entire codebase",
    "whole repo",
    "all call sites",
    "all usages",
    "all files",
    "everything",
    "every command",
];

const SENSITIVE_KEYWORDS: [&str; 11] = [
    "auth",
    "money",
    "migrate",
    "migration",
    "delete",
    "secret",
    "token",
    "pii",
    "ssh",
    "password",
    "credential",
];

const COMMON_EXTENSIONS: [&str; 18] = [
    ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java", ".cs", ".md", ".toml", ".json",
    ".yaml", ".yml", ".css", ".scss", ".html", ".sql",
];

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

fn sentence_count(text: &str) -> usize {
    text.matches('.').count() + text.matches('?').count() + text.matches('!').count()
}

fn contains_any(text: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| text.contains(needle))
}

fn is_question(text: &str) -> bool {
    text.contains('?')
        || text.starts_with("what ")
        || text.starts_with("why ")
        || text.starts_with("how ")
        || text.starts_with("where ")
        || text.starts_with("when ")
        || text.starts_with("who ")
        || text.starts_with("explain ")
}

/// Count whitespace-separated tokens that look like file/path references
/// (contain a path separator, or end in a recognized source-file extension).
pub fn path_reference_count(text: &str) -> usize {
    text.split_whitespace()
        .map(|token| {
            token.trim_matches(|c: char| {
                matches!(c, ',' | ';' | ':' | '"' | '\'' | '(' | ')' | '[' | ']')
            })
        })
        .filter(|token| {
            token.contains('/')
                || token.contains('\\')
                || COMMON_EXTENSIONS.iter().any(|ext| token.ends_with(ext))
        })
        .count()
}

/// True if the text contains any sensitive-surface keyword (auth, money,
/// migrate, delete, secret, token, pii, ssh, ...) per ADR §D1's floor.
pub fn contains_sensitive_keyword(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    contains_any(&lower, &SENSITIVE_KEYWORDS)
}

/// Derive a [`PromptShape`] from raw task text.
pub fn analyze(text: &str) -> PromptShape {
    let trimmed = text.trim();
    let lower = trimmed.to_ascii_lowercase();
    PromptShape {
        word_count: word_count(&lower),
        sentence_count: sentence_count(trimmed),
        is_question: is_question(&lower),
        broad_scope: contains_any(&lower, &BROAD_SCOPE_PHRASES),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyze_detects_question() {
        let shape = analyze("What does this function do?");
        assert!(shape.is_question);
    }

    #[test]
    fn analyze_detects_broad_scope() {
        let shape = analyze("Refactor this across the codebase please.");
        assert!(shape.broad_scope);
    }

    #[test]
    fn analyze_word_and_sentence_counts() {
        let shape = analyze("Fix this. Then verify it.");
        assert_eq!(shape.word_count, 5);
        assert_eq!(shape.sentence_count, 2);
    }

    #[test]
    fn path_reference_count_counts_extensions_and_separators() {
        assert_eq!(path_reference_count("edit src/main.rs and lib.rs"), 2);
        assert_eq!(path_reference_count("no paths here"), 0);
    }

    #[test]
    fn sensitive_keyword_detection() {
        assert!(contains_sensitive_keyword("rotate the auth secret"));
        assert!(contains_sensitive_keyword("run the DB migration"));
        assert!(!contains_sensitive_keyword("format this file nicely"));
    }
}
