//! Phrase-aware context bias: the custom-vocabulary contract.
//!
//! FIRST-PARTY AND PLATFORM-NEUTRAL. Until now macOS and the vendored ONNX crate
//! each carried their own copy of `ContextBias`, `matching_prefix_len`,
//! `next_token_bonuses`, `normalize_phrase_for_bias` and `encode_phrase_greedy`,
//! kept byte-identical by hand and by comment. That is divergence #5 of the five
//! found in the 2026-09 decoder audit, and the only one that was being actively
//! maintained in two places rather than merely differing.
//!
//! What custom vocabulary MEANS is a product decision, so it lives here. How each
//! platform executes a model is not, and stays platform-side.

use std::collections::HashMap;

pub trait BiasScorer {
    fn bonus(&self, prefix: &[usize], next_token: usize) -> f32;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct NoBias;

impl BiasScorer for NoBias {
    fn bonus(&self, _prefix: &[usize], _next_token: usize) -> f32 {
        0.0
    }
}

/// Lowercase ASCII, keep alphanumerics and the apostrophe, turn everything else
/// into a space, collapse runs. Both previous implementations agreed on this and
/// each carried a comment saying it had to match the other; now there is one.
pub fn normalize_phrase_for_bias(phrase: &str) -> String {
    phrase
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Encode a human phrase into vocabulary token ids by longest-prefix segmentation.
///
/// The vocabulary is the model's fixed output units, so biasing cannot add new
/// tokens — it can only raise existing token paths. `▁` is the word-boundary
/// marker; pieces that are empty or angle-bracketed specials are never matched.
/// Returns `None` if any part of the phrase cannot be represented.
pub fn encode_phrase_greedy(pieces: &[String], phrase: &str) -> Option<Vec<usize>> {
    let normalized = normalize_phrase_for_bias(phrase);
    if normalized.is_empty() {
        return None;
    }
    let mut piece_text = String::new();
    for word in normalized.split_whitespace() {
        piece_text.push('\u{2581}');
        piece_text.push_str(word);
    }

    let mut remaining = piece_text.as_str();
    let mut ids = Vec::new();
    while !remaining.is_empty() {
        let mut best: Option<(usize, usize)> = None;
        for (id, token) in pieces.iter().enumerate() {
            if token.is_empty() || token.starts_with('<') {
                continue;
            }
            if remaining.starts_with(token.as_str()) {
                let len = token.len();
                if best.map(|(_, best_len)| len > best_len).unwrap_or(true) {
                    best = Some((id, len));
                }
            }
        }
        let (id, len) = best?;
        ids.push(id);
        remaining = &remaining[len..];
    }
    Some(ids)
}

/// Longest suffix of `emitted` that is a prefix of `phrase`, capped so it can
/// never equal the whole phrase (a fully-matched phrase has nothing left to bias).
pub fn matching_prefix_len(emitted: &[usize], phrase: &[usize]) -> usize {
    let max = emitted.len().min(phrase.len().saturating_sub(1));
    for len in (0..=max).rev() {
        if len == 0 || emitted[emitted.len() - len..] == phrase[..len] {
            return len;
        }
    }
    0
}

/// Installed bias phrases as token-id sequences, plus a flat per-token score.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContextBias {
    pub phrase_token_ids: Vec<Vec<usize>>,
    pub score: f32,
}

impl ContextBias {
    pub fn new(phrase_token_ids: Vec<Vec<usize>>, score: f32) -> Self {
        Self {
            phrase_token_ids: phrase_token_ids
                .into_iter()
                .filter(|phrase| !phrase.is_empty())
                .collect(),
            score,
        }
    }

    /// Encode phrases against a vocabulary. Unrepresentable phrases are dropped.
    /// A non-finite or non-positive score installs nothing.
    pub fn from_phrases<I, S>(pieces: &[String], phrases: I, score: f32) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if !(score.is_finite() && score > 0.0) {
            return Self::default();
        }
        Self::new(
            phrases
                .into_iter()
                .filter_map(|p| encode_phrase_greedy(pieces, p.as_ref()))
                .collect(),
            score,
        )
    }

    pub fn is_empty(&self) -> bool {
        self.phrase_token_ids.is_empty() || self.score == 0.0
    }

    pub fn len(&self) -> usize {
        self.phrase_token_ids.len()
    }

    /// For each phrase, if the emitted suffix matches a prefix of it, the next
    /// expected token earns `score`, taking the MAX across phrases.
    ///
    /// `emitted` is per-hypothesis, which is what makes this phrase-aware rather
    /// than a per-token nudge: a hypothesis part-way through a multi-token phrase
    /// keeps earning bonuses as it extends the match, so beam width > 1 can keep
    /// a two-token phrase alive against a locally-better single token.
    pub fn next_token_bonuses(&self, emitted: &[usize]) -> HashMap<usize, f32> {
        let mut bonuses = HashMap::new();
        if self.is_empty() {
            return bonuses;
        }
        for phrase in &self.phrase_token_ids {
            let matched = matching_prefix_len(emitted, phrase);
            if matched < phrase.len() {
                bonuses
                    .entry(phrase[matched])
                    .and_modify(|s: &mut f32| *s = s.max(self.score))
                    .or_insert(self.score);
            }
        }
        bonuses
    }
}

impl BiasScorer for ContextBias {
    fn bonus(&self, prefix: &[usize], next_token: usize) -> f32 {
        self.next_token_bonuses(prefix)
            .get(&next_token)
            .copied()
            .unwrap_or(0.0)
    }
}

