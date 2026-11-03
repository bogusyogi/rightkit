//! Content filter strategies for article/website extraction (per 2026-06-25 plan).
//!
//! Goals:
//! - boilerplate ratio ≤16%
//! - +≥20% main-content chars
//! - <60ms p95 overhead
//! - key paragraphs preserved
//! - additive .fit.md only (raw .txt/.md unchanged)

use regex::Regex;
use std::collections::HashSet;

/// Okapi BM25 free parameters (the usual defaults).
const BM25_K1: f64 = 1.2;
const BM25_B: f64 = 0.75;

/// Returns `(pruned, fit)`: the text with boilerplate blocks pruned, and the BM25 "fit"
/// selection of the pruned text. With `enabled` false both are the input unchanged.
pub fn apply_article_filters(input: &str, enabled: bool) -> (String, String) {
    if enabled {
        let pruned = prune(input);
        let fit = fit_select(&pruned);
        (pruned, fit)
    } else {
        let t = input.to_string();
        (t.clone(), t)
    }
}

fn prune(input: &str) -> String {
    // Basic pruning: drop obvious boilerplate lines/phrases, very short lines,
    // high "nav density" blocks. Keep structure.
    static NOISE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?i)\b(subscribe|sign up|newsletter|cookie|privacy|terms|advertis|related|you may also|share|follow us|login|register|read more|table of contents|skip to)\b").unwrap()
    });

    let lines: Vec<&str> = input.lines().collect();
    let mut kept = Vec::new();

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.len() < 15 {
            continue;
        } // short line filter
        if NOISE.is_match(trimmed) {
            continue;
        }

        // crude nav check: many | or > or very repetitive short tokens
        let pipes = trimmed.matches('|').count();
        if pipes > 3 {
            continue;
        }

        kept.push(trimmed);
    }

    kept.join("\n\n")
}

/// Lowercased alphanumeric core of a whitespace-delimited word.
fn clean_term(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

/// Okapi BM25 score of every block, with each block (paragraph) as a document.
///
/// For a query term q, with N = number of blocks, n(q) = blocks containing q,
/// f = count of q in the block, |d| = cleaned-word count of the block and
/// avgdl = mean |d| over all blocks:
///
///   idf(q)   = ln(1 + (N - n(q) + 0.5) / (n(q) + 0.5))
///   score(d) = sum over q of idf(q) * f * (k1 + 1) / (f + k1 * (1 - b + b * |d| / avgdl))
///
/// with k1 = 1.2 and b = 0.75. Query terms are used once each (not weighted by
/// how often they appear in the query).
fn bm25_scores(blocks: &[&str], query: &[String]) -> Vec<f64> {
    let docs: Vec<Vec<String>> = blocks
        .iter()
        .map(|b| {
            b.split_whitespace()
                .map(clean_term)
                .filter(|t| !t.is_empty())
                .collect()
        })
        .collect();
    if docs.is_empty() {
        return Vec::new();
    }
    let n_docs = docs.len() as f64;
    let avgdl = (docs.iter().map(Vec::len).sum::<usize>() as f64 / n_docs).max(1.0);

    let mut scores = vec![0.0f64; docs.len()];
    for term in query {
        let n = docs.iter().filter(|d| d.iter().any(|t| t == term)).count() as f64;
        let idf = (1.0 + (n_docs - n + 0.5) / (n + 0.5)).ln();
        for (score, doc) in scores.iter_mut().zip(&docs) {
            let f = doc.iter().filter(|t| *t == term).count() as f64;
            if f > 0.0 {
                let norm = BM25_K1 * (1.0 - BM25_B + BM25_B * doc.len() as f64 / avgdl);
                *score += idf * f * (BM25_K1 + 1.0) / (f + norm);
            }
        }
    }
    scores
}

fn fit_select(input: &str) -> String {
    // Okapi BM25 over the article's blocks (paragraphs split on blank lines).
    // Query terms: the salient words (longer than 3 characters) of the head of
    // the doc, the first three blocks, as before.
    let blocks: Vec<&str> = input
        .split("\n\n")
        .filter(|b| !b.trim().is_empty())
        .collect();
    if blocks.len() <= 2 {
        return input.to_string();
    }

    let mut query_set: HashSet<String> = HashSet::new();
    for w in blocks.iter().take(3).flat_map(|b| b.split_whitespace()) {
        let clean = clean_term(w);
        if clean.len() > 3 {
            query_set.insert(clean);
        }
    }
    // Sorted so the score sums run in a fixed order.
    let mut query: Vec<String> = query_set.into_iter().collect();
    query.sort();

    let scores = bm25_scores(&blocks, &query);

    // keep top ~65% by score, preserve relative order
    let mut with_idx: Vec<(usize, f64)> = scores.into_iter().enumerate().collect();
    with_idx.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let keep_n = ((with_idx.len() as f32) * 0.65).ceil() as usize;
    let mut kept_idx: Vec<usize> = with_idx.into_iter().take(keep_n).map(|(i, _)| i).collect();
    kept_idx.sort();

    kept_idx
        .iter()
        .map(|&i| blocks[i])
        .collect::<Vec<_>>()
        .join("\n\n")
}
