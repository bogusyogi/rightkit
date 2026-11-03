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

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

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
///
/// Uses the process-wide [`VocabIndex`] of `pieces` (perf audit ENG-02), which
/// returns exactly what [`encode_phrase_greedy_scan`] returns; `RIGHTKIT_ASR_BIAS_CACHE=0`
/// (test mode only) restores the full-vocabulary scan.
pub fn encode_phrase_greedy(pieces: &[String], phrase: &str) -> Option<Vec<usize>> {
    if !bias_cache_enabled() {
        return encode_phrase_greedy_scan(pieces, phrase);
    }
    let mut cache = match BIAS_CACHE.lock() {
        Ok(cache) => cache,
        Err(_) => return encode_phrase_greedy_scan(pieces, phrase),
    };
    cache.index_for(pieces).encode_phrase_greedy(phrase)
}

/// The phrase as the `▁`-joined piece text segmentation consumes, or `None` if
/// normalisation leaves nothing.
fn phrase_piece_text(phrase: &str) -> Option<String> {
    let normalized = normalize_phrase_for_bias(phrase);
    if normalized.is_empty() {
        return None;
    }
    let mut piece_text = String::new();
    for word in normalized.split_whitespace() {
        piece_text.push('\u{2581}');
        piece_text.push_str(word);
    }
    Some(piece_text)
}

/// Reference segmentation: scans the WHOLE vocabulary for every greedy step,
/// O(pieces-per-phrase × |vocab|). Kept as the identity oracle for
/// [`VocabIndex`] and as the `RIGHTKIT_ASR_BIAS_CACHE=0` path.
pub fn encode_phrase_greedy_scan(pieces: &[String], phrase: &str) -> Option<Vec<usize>> {
    let piece_text = phrase_piece_text(phrase)?;
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

/// The model vocabulary as a piece-text → id map, so a greedy step looks up
/// each candidate prefix length once instead of testing every piece.
///
/// Identity with [`encode_phrase_greedy_scan`]: the scan picks the LONGEST piece
/// that prefixes the remaining text, and among equally long matches (which are
/// the same string) the lowest id, skipping empty and `<`-special pieces. The map
/// skips the same pieces and keeps the lowest id per text; the lookup tries
/// prefix lengths from the longest piece length down, so its first hit is that
/// same piece.
#[derive(Clone, Debug)]
pub struct VocabIndex {
    pieces: Vec<String>,
    by_text: HashMap<String, usize>,
    max_piece_len: usize,
}

impl VocabIndex {
    pub fn new(pieces: &[String]) -> Self {
        let mut by_text = HashMap::with_capacity(pieces.len());
        let mut max_piece_len = 0;
        for (id, token) in pieces.iter().enumerate() {
            if token.is_empty() || token.starts_with('<') {
                continue;
            }
            by_text.entry(token.clone()).or_insert(id);
            max_piece_len = max_piece_len.max(token.len());
        }
        Self {
            pieces: pieces.to_vec(),
            by_text,
            max_piece_len,
        }
    }

    /// True when this index was built from exactly these pieces (content, not
    /// address: a reloaded model may reuse an allocation).
    pub fn is_for(&self, pieces: &[String]) -> bool {
        self.pieces.as_slice() == pieces
    }

    pub fn encode_phrase_greedy(&self, phrase: &str) -> Option<Vec<usize>> {
        let piece_text = phrase_piece_text(phrase)?;
        let mut remaining = piece_text.as_str();
        let mut ids = Vec::new();
        while !remaining.is_empty() {
            let (id, len) = (1..=self.max_piece_len.min(remaining.len()))
                .rev()
                .filter(|len| remaining.is_char_boundary(*len))
                .find_map(|len| self.by_text.get(&remaining[..len]).map(|id| (*id, len)))?;
            ids.push(id);
            remaining = &remaining[len..];
        }
        Some(ids)
    }
}

/// Recently built biases, keyed by full content: the vocabulary (via the
/// index), the ordered phrase list and the score's bits. Content keys mean no
/// invalidation can be missed: a vocabulary edit, a new screen harvest or a
/// score change is simply a different key, and a different model vocabulary
/// drops every entry. Several entries because final (base + screen terms,
/// score 1) and probe (base terms, score 5) alternate on one runtime.
#[derive(Debug)]
pub struct ContextBiasCache {
    index: Option<VocabIndex>,
    entries: VecDeque<BiasCacheEntry>,
}

#[derive(Debug)]
struct BiasCacheEntry {
    phrases: Vec<String>,
    score_bits: u32,
    bias: ContextBias,
}

const BIAS_CACHE_ENTRIES: usize = 4;

static BIAS_CACHE: Mutex<ContextBiasCache> = Mutex::new(ContextBiasCache::new());

/// `RIGHTKIT_ASR_BIAS_CACHE=0` (test mode only) restores the per-decode full rebuild
/// with the vocabulary scan, for A/B. Release builds always cache.
fn bias_cache_enabled() -> bool {
    dev_var!("RIGHTKIT_ASR_BIAS_CACHE")
        .map(|value| value.trim() != "0")
        .unwrap_or(true)
}

impl Default for ContextBiasCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextBiasCache {
    pub const fn new() -> Self {
        Self {
            index: None,
            entries: VecDeque::new(),
        }
    }

    /// The index for `pieces`, rebuilt (and every cached bias dropped) when
    /// the vocabulary content differs from the one cached.
    pub fn index_for(&mut self, pieces: &[String]) -> &VocabIndex {
        if !self
            .index
            .as_ref()
            .is_some_and(|index| index.is_for(pieces))
        {
            self.entries.clear();
            self.index = Some(VocabIndex::new(pieces));
        }
        self.index.as_ref().expect("index installed above")
    }

    /// Same result as `ContextBias::from_phrases_uncached(pieces, phrases, score)`.
    pub fn get_or_build(
        &mut self,
        pieces: &[String],
        phrases: &[String],
        score: f32,
    ) -> ContextBias {
        if !(score.is_finite() && score > 0.0) {
            return ContextBias::default();
        }
        let score_bits = score.to_bits();
        self.index_for(pieces);
        if let Some(pos) = self
            .entries
            .iter()
            .position(|e| e.score_bits == score_bits && e.phrases.as_slice() == phrases)
        {
            let entry = self.entries.remove(pos).expect("position is in range");
            let bias = entry.bias.clone();
            self.entries.push_front(entry);
            return bias;
        }
        let index = self.index.as_ref().expect("index installed above");
        let bias = ContextBias::new(
            phrases
                .iter()
                .filter_map(|p| index.encode_phrase_greedy(p))
                .collect(),
            score,
        );
        self.entries.truncate(BIAS_CACHE_ENTRIES - 1);
        self.entries.push_front(BiasCacheEntry {
            phrases: phrases.to_vec(),
            score_bits,
            bias: bias.clone(),
        });
        bias
    }
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
    ///
    /// Served from the process-wide [`ContextBiasCache`] (perf audit ENG-02):
    /// every decode used to rebuild this table from scratch. The result equals
    /// [`ContextBias::from_phrases_uncached`]; `RIGHTKIT_ASR_BIAS_CACHE=0` (test mode
    /// only) restores the uncached rebuild for A/B.
    pub fn from_phrases<I, S>(pieces: &[String], phrases: I, score: f32) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        if !(score.is_finite() && score > 0.0) {
            return Self::default();
        }
        if !bias_cache_enabled() {
            return Self::from_phrases_uncached(pieces, phrases, score);
        }
        let phrases: Vec<String> = phrases.into_iter().map(|p| p.as_ref().to_owned()).collect();
        match BIAS_CACHE.lock() {
            Ok(mut cache) => cache.get_or_build(pieces, &phrases, score),
            Err(_) => Self::from_phrases_uncached(pieces, phrases, score),
        }
    }

    /// The pre-cache builder: full-vocabulary scan per greedy step, no reuse.
    pub fn from_phrases_uncached<I, S>(pieces: &[String], phrases: I, score: f32) -> Self
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
                .filter_map(|p| encode_phrase_greedy_scan(pieces, p.as_ref()))
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
    /// Longest history suffix that can influence `next_token_bonuses`: a match
    /// is capped at `phrase.len() - 1`, so older tokens never matter. Callers
    /// pass only this many trailing emitted tokens instead of the whole history.
    pub fn history_window(&self) -> usize {
        self.phrase_token_ids
            .iter()
            .map(|p| p.len().saturating_sub(1))
            .max()
            .unwrap_or(0)
    }

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
