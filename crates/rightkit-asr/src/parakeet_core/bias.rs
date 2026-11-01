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

#[cfg(test)]
mod tests {
    use super::*;

    /// Same inputs and expected values as the vendored ONNX crate's
    /// `context_bias_boosts_phrase_start_and_continuation` and the iOS Swift
    /// smoke test, so all three implementations are pinned to one another.
    #[test]
    fn phrase_start_and_continuation_match_the_cross_platform_contract() {
        let bias = ContextBias::new(vec![vec![10, 11, 12]], 1.0);
        assert_eq!(bias.next_token_bonuses(&[]).get(&10).copied(), Some(1.0));
        assert_eq!(bias.next_token_bonuses(&[10]).get(&11).copied(), Some(1.0));
        assert_eq!(
            bias.next_token_bonuses(&[10, 11]).get(&12).copied(),
            Some(1.0)
        );
        assert_eq!(bias.next_token_bonuses(&[10]).get(&12).copied(), None);
    }

    /// A completed phrase does not stop biasing — it re-arms. `matching_prefix_len`
    /// caps the match at `phrase.len() - 1`, so a fully-emitted phrase finds no
    /// partial match, falls back to length 0, and bonuses the phrase START again.
    /// That is what both the macOS and vendored implementations did, and it is
    /// the behavior a user wants: saying a vocabulary term twice should bias it
    /// twice. Asserted explicitly because it is surprising enough to be
    /// "corrected" by someone reading only the function name.
    #[test]
    fn a_completed_phrase_re_arms_at_its_start() {
        let bias = ContextBias::new(vec![vec![7, 8]], 2.0);
        let after = bias.next_token_bonuses(&[7, 8]);
        assert_eq!(after.get(&7).copied(), Some(2.0), "phrase should re-arm");
        assert_eq!(
            after.get(&8).copied(),
            None,
            "mid-phrase token is not armed"
        );
    }

    /// Bonuses computed from only the trailing `history_window()` tokens are
    /// identical to bonuses from the full history, for every prefix.
    #[test]
    fn tail_window_bonuses_equal_full_history_bonuses() {
        let bias = ContextBias::new(vec![vec![1, 2, 3], vec![2, 3, 4, 5], vec![9]], 1.25);
        let hist = [1usize, 2, 3, 4, 2, 3, 4, 9, 1, 2, 9, 2, 3, 4, 5, 2, 3];
        let w = bias.history_window();
        assert_eq!(w, 3);
        for n in 0..=hist.len() {
            let full = &hist[..n];
            let tail = &full[full.len().saturating_sub(w)..];
            assert_eq!(
                bias.next_token_bonuses(full),
                bias.next_token_bonuses(tail),
                "n={n}"
            );
        }
    }

    #[test]
    fn max_score_wins_when_phrases_share_a_next_token() {
        let bias = ContextBias::new(vec![vec![4, 9], vec![4, 9]], 1.5);
        assert_eq!(bias.next_token_bonuses(&[4]).get(&9).copied(), Some(1.5));
    }

    #[test]
    fn normalization_matches_both_previous_implementations() {
        assert_eq!(normalize_phrase_for_bias("AcmeCorp!"), "acmecorp");
        assert_eq!(normalize_phrase_for_bias("it's  a   test"), "it's a test");
        assert_eq!(normalize_phrase_for_bias("!!!"), "");
    }

    #[test]
    fn encoding_uses_longest_prefix_and_rejects_unrepresentable_text() {
        let pieces: Vec<String> = ["<blk>", "\u{2581}he", "ard", "\u{2581}x", "right"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(encode_phrase_greedy(&pieces, "heard"), Some(vec![1, 2]));
        // 'q' appears in no piece, so the phrase cannot be represented at all.
        assert_eq!(encode_phrase_greedy(&pieces, "q"), None);
        assert_eq!(encode_phrase_greedy(&pieces, "!!!"), None);
    }

    /// SentencePiece-like vocabulary: specials, duplicates, every single
    /// character with and without `▁`, then pseudo-random 2–8 letter pieces.
    fn synthetic_vocab(seed: u64, size: usize) -> Vec<String> {
        let mut x = seed;
        let mut next = move || {
            x = x
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            x >> 33
        };
        let mut pieces: Vec<String> = vec!["<unk>".into(), "<pad>".into(), String::new()];
        for c in "abcdefghijklmnopqrstuvwxyz0123456789'".chars() {
            pieces.push(c.to_string());
            pieces.push(format!("\u{2581}{c}"));
        }
        // Duplicate pieces: the lowest id must keep winning ties.
        pieces.push("\u{2581}a".into());
        pieces.push("e".into());
        // Multi-byte pieces, including a lone `▁`: prefix lengths must respect
        // char boundaries.
        pieces.push("\u{2581}".into());
        pieces.push("\u{2581}\u{e9}t".into());
        pieces.push("\u{e9}".into());
        let letters: Vec<char> = "etaoinshrdlucmfwypvbgkjqxz".chars().collect();
        while pieces.len() < size {
            let len = 2 + (next() % 7) as usize;
            let mut s = String::new();
            if next() % 100 < 55 {
                s.push('\u{2581}');
            }
            for _ in 0..len {
                let i = ((next() % 1000) as f64 / 1000.0).powf(2.0) * letters.len() as f64;
                s.push(letters[(i as usize).min(25)]);
            }
            pieces.push(s);
        }
        pieces.push("<blk>".into());
        pieces
    }

    fn synthetic_terms(n: usize) -> Vec<String> {
        let words = [
            "heard right",
            "AcmeCorp!",
            "zephyr stop",
            "it's",
            "kubernetes",
            "Ünïcode café",
            "!!!",
            "",
            "postgres 16",
            "o'brien",
            "q",
            "the quick brown fox",
        ];
        (0..n)
            .map(|i| {
                let mut x = (i as u64).wrapping_mul(2654435761);
                let mut t = String::new();
                for _ in 0..(3 + (x % 7) as usize) {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                    t.push((b'a' + ((x >> 40) % 26) as u8) as char);
                }
                match i % 3 {
                    0 => format!("{} {t}", words[i % words.len()]),
                    1 => words[i % words.len()].to_string(),
                    _ => t,
                }
            })
            .collect()
    }

    /// ENG-02 identity: the indexed lookup segments every phrase exactly as
    /// the full-vocabulary scan does, representable or not.
    #[test]
    fn indexed_encoding_equals_the_full_scan() {
        for seed in [1u64, 42, 9001] {
            let pieces = synthetic_vocab(seed, 4_096);
            let index = VocabIndex::new(&pieces);
            for term in synthetic_terms(600) {
                assert_eq!(
                    index.encode_phrase_greedy(&term),
                    encode_phrase_greedy_scan(&pieces, &term),
                    "seed={seed} term={term:?}"
                );
            }
        }
        // Ties between duplicate pieces still go to the lowest id.
        let pieces: Vec<String> = ["\u{2581}ab", "c", "\u{2581}ab", "c"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            VocabIndex::new(&pieces).encode_phrase_greedy("abc"),
            Some(vec![0, 1])
        );
    }

    /// ENG-02 identity: a cached build (miss, hit, alternating score, changed
    /// terms, changed vocabulary) equals a fresh uncached build, and so yields
    /// the same bonuses for every history — the bias is the decoder's whole
    /// view of custom vocabulary, so equal `ContextBias` means equal decodes.
    #[test]
    fn cached_bias_equals_a_fresh_uncached_build() {
        let mut cache = ContextBiasCache::new();
        let vocab_a = synthetic_vocab(7, 4_096);
        let vocab_b = synthetic_vocab(8, 4_096);
        let base = synthetic_terms(76);
        let mut with_screen = base.clone();
        with_screen.extend(synthetic_terms(126).into_iter().skip(76));
        let cases: Vec<(&Vec<String>, &Vec<String>, f32)> = vec![
            (&vocab_a, &with_screen, 1.0),
            (&vocab_a, &base, 5.0),
            (&vocab_a, &with_screen, 1.0), // hit after a probe build
            (&vocab_a, &base, 5.0),        // hit
            (&vocab_a, &base, 1.0),        // same terms, other score
            (&vocab_b, &with_screen, 1.0), // vocabulary changed: rebuild
            (&vocab_a, &with_screen, 1.0), // and back
            (&vocab_a, &with_screen, 0.0), // installs nothing
            (&vocab_a, &with_screen, f32::NAN),
        ];
        for (n, (pieces, terms, score)) in cases.into_iter().enumerate() {
            let fresh = ContextBias::from_phrases_uncached(pieces, terms.iter(), score);
            let cached = cache.get_or_build(pieces, terms, score);
            assert_eq!(cached, fresh, "case {n}");
            for start in 0..fresh.phrase_token_ids.len().min(40) {
                let history: Vec<usize> = fresh.phrase_token_ids[start..]
                    .iter()
                    .flatten()
                    .copied()
                    .take(9)
                    .collect();
                for cut in 0..=history.len() {
                    assert_eq!(
                        cached.next_token_bonuses(&history[..cut]),
                        fresh.next_token_bonuses(&history[..cut]),
                        "case {n} start {start} cut {cut}"
                    );
                }
            }
        }
        assert!(cache.entries.len() <= BIAS_CACHE_ENTRIES);
    }

    /// A vocabulary edit changes the key: the next build reflects it at once.
    #[test]
    fn a_vocabulary_edit_is_never_served_stale() {
        let mut cache = ContextBiasCache::new();
        let pieces = synthetic_vocab(3, 2_048);
        let before = vec!["zephyr stop".to_string()];
        let after = vec!["zephyr stop".to_string(), "kubernetes".to_string()];
        assert_eq!(cache.get_or_build(&pieces, &before, 1.0).len(), 1);
        assert_eq!(cache.get_or_build(&pieces, &after, 1.0).len(), 2);
        assert_eq!(cache.get_or_build(&pieces, &before, 1.0).len(), 1);
    }

    /// The public entry points (process-wide cache) agree with the uncached ones.
    #[test]
    fn public_entry_points_match_the_uncached_reference() {
        let pieces = synthetic_vocab(11, 2_048);
        let terms = synthetic_terms(50);
        for _ in 0..2 {
            assert_eq!(
                ContextBias::from_phrases(&pieces, terms.iter(), 1.0),
                ContextBias::from_phrases_uncached(&pieces, terms.iter(), 1.0)
            );
        }
        for term in &terms {
            assert_eq!(
                encode_phrase_greedy(&pieces, term),
                encode_phrase_greedy_scan(&pieces, term)
            );
        }
    }

    #[test]
    fn a_non_positive_score_installs_nothing() {
        let pieces: Vec<String> = vec!["\u{2581}a".to_string()];
        assert!(ContextBias::from_phrases(&pieces, ["a"], 0.0).is_empty());
        assert!(ContextBias::from_phrases(&pieces, ["a"], f32::NAN).is_empty());
    }
}
