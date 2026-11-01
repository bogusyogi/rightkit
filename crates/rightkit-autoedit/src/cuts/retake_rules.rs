//! Deterministic retake rules over transcript words (issue #96).
//!
//! Pure functions over [`Word`]s: no model call, no audio. A rule returns typed
//! [`RuleHit`]s; a word that no hit marks for removal is kept. [`Verdict::Escalate`]
//! marks a case for a later judge and never removes words.
//!
//! Deferred (not implemented here): moving an alternate hook to the start. It needs
//! a reorder action that the timeline and realise layers do not have, and choosing
//! which hook is the better one is a judgement the transcript does not carry.

use std::ops::Range;

use rightkit_transcript::Word;

use crate::cuts::text::{ends_sentence, normalize};

/// Thresholds for the retake rules. Defaults follow the retake policy in the review.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleConfig {
    /// Silence (ms) that ends an abandoned attempt when there is no punctuation.
    pub boundary_pause_ms: i64,
    /// Longest abandoned attempt (words) a self-talk cue may remove.
    pub max_attempt_words: usize,
    /// Pause (ms) needed on each side before a soft filler may be cut.
    pub isolation_ms: i64,
    /// Lowest word confidence at which a soft filler may be cut.
    pub soft_filler_min_confidence: f32,
}

impl Default for RuleConfig {
    fn default() -> Self {
        Self {
            boundary_pause_ms: 200,
            max_attempt_words: 12,
            isolation_ms: 120,
            soft_filler_min_confidence: 0.7,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetakeRule {
    SelfTalk,
    SoftFiller,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Remove the words in the hit's range.
    Cut,
    /// Keep the words; the rule looked and decided against removal.
    Keep,
    /// Keep the words and mark the case for a later judge. No model is called here.
    Escalate,
}

/// Whether a take may be cut without review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Auto,
    Suggest,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuleHit {
    pub rule: RetakeRule,
    /// First word index.
    pub first: usize,
    /// Exclusive end word index.
    pub end: usize,
    pub verdict: Verdict,
    pub reason: String,
}

/// Cues that end an attempt on their own: the speaker is starting again.
const STRONG_CUES: &[&str] = &[
    "scratch that",
    "let me start again",
    "start over",
    "no wait",
    "wait no",
    "actually no",
    "let me try that again",
    "let me say that again",
    "let me redo that",
    "oops",
];

/// Cues that also have ordinary meanings ("I mean it"). Marked for escalation only.
const AMBIGUOUS_CUES: &[&str] = &["i mean", "wait", "sorry"];

/// Soft fillers. Cut only when isolated by pauses and confidently transcribed.
const SOFT_FILLERS: &[&str] = &["like", "you know", "basically", "sort of", "kind of"];

fn norms(words: &[Word]) -> Vec<String> {
    words.iter().map(|w| normalize(&w.text)).collect()
}

/// Length in words if `phrase` (whitespace-separated, normalised) starts at `i`.
fn phrase_len_at(norms: &[String], i: usize, phrase: &str) -> Option<usize> {
    let toks: Vec<&str> = phrase.split_whitespace().collect();
    let n = toks.len();
    if n == 0 || i + n > norms.len() {
        return None;
    }
    if (0..n).all(|k| norms[i + k] == toks[k]) {
        Some(n)
    } else {
        None
    }
}

/// Longest cue starting at `i`: (length, cue, strong).
fn match_cue(norms: &[String], i: usize) -> Option<(usize, &'static str, bool)> {
    let mut best: Option<(usize, &'static str, bool)> = None;
    for (list, strong) in [(STRONG_CUES, true), (AMBIGUOUS_CUES, false)] {
        for &cue in list {
            if let Some(len) = phrase_len_at(norms, i, cue) {
                let longer = match best {
                    Some((b, _, _)) => len > b,
                    None => true,
                };
                if longer {
                    best = Some((len, cue, strong));
                }
            }
        }
    }
    best
}

/// Silence or sentence end after word `k` (the boundary between `k` and `k + 1`).
fn is_boundary(words: &[Word], k: usize, cfg: &RuleConfig) -> bool {
    if ends_sentence(&words[k].text) {
        return true;
    }
    match words.get(k + 1) {
        Some(next) => next.start_ms as i64 - words[k].end_ms as i64 >= cfg.boundary_pause_ms,
        None => true,
    }
}

/// First word of the attempt that ends at cue `cue`. The attempt is the words after
/// the last boundary strictly inside the attempt, capped at `max_attempt_words`.
fn attempt_start(words: &[Word], cue: usize, cfg: &RuleConfig) -> usize {
    let mut s = cue;
    while s > 0 && cue - s < cfg.max_attempt_words {
        let k = s - 1;
        // The boundary right before the cue ends the attempt; it is not inside it.
        if s < cue && is_boundary(words, k, cfg) {
            break;
        }
        s = k;
    }
    s
}

/// Self-talk: a strong restart cue removes the abandoned attempt before it and the
/// cue itself; the restart after the cue is kept. Ambiguous cues and cues with no
/// attempt or no restart are escalated and kept.
pub fn self_talk(words: &[Word], cfg: &RuleConfig) -> Vec<RuleHit> {
    let n = norms(words);
    let mut hits = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let Some((len, cue, strong)) = match_cue(&n, i) else {
            i += 1;
            continue;
        };
        let end = i + len;
        if !strong {
            hits.push(RuleHit {
                rule: RetakeRule::SelfTalk,
                first: i,
                end,
                verdict: Verdict::Escalate,
                reason: format!("ambiguous cue \"{cue}\": kept, marked for escalation"),
            });
        } else {
            let start = attempt_start(words, i, cfg);
            if start < i && end < words.len() {
                hits.push(RuleHit {
                    rule: RetakeRule::SelfTalk,
                    first: start,
                    end,
                    verdict: Verdict::Cut,
                    reason: format!("abandoned attempt and \"{cue}\" cue removed; restart kept"),
                });
            } else {
                hits.push(RuleHit {
                    rule: RetakeRule::SelfTalk,
                    first: i,
                    end,
                    verdict: Verdict::Escalate,
                    reason: format!(
                        "\"{cue}\" cue without both an attempt and a restart: kept, marked for escalation"
                    ),
                });
            }
        }
        i = end;
    }
    hits
}

/// Soft-filler keep gate. A soft filler is cut only when pauses on both sides reach
/// `isolation_ms` and every word has confidence at or above the minimum. Isolated but
/// low-confidence fillers are kept and escalated. Fillers with no confidence are kept.
/// Words claimed by a self-talk hit are skipped.
pub fn soft_filler_gate(words: &[Word], cfg: &RuleConfig) -> Vec<RuleHit> {
    let n = norms(words);
    let mut claimed = vec![false; words.len()];
    for h in self_talk(words, cfg) {
        for slot in claimed.iter_mut().take(h.end).skip(h.first) {
            *slot = true;
        }
    }
    let mut hits = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let mut best: Option<(usize, &'static str)> = None;
        for &p in SOFT_FILLERS {
            if let Some(len) = phrase_len_at(&n, i, p) {
                let longer = match best {
                    Some((b, _)) => len > b,
                    None => true,
                };
                if longer {
                    best = Some((len, p));
                }
            }
        }
        let Some((len, phrase)) = best else {
            i += 1;
            continue;
        };
        let end = i + len;
        if claimed[i..end].iter().any(|&c| c) {
            i += 1;
            continue;
        }
        let pause_before = if i == 0 {
            i64::MAX
        } else {
            words[i].start_ms as i64 - words[i - 1].end_ms as i64
        };
        let pause_after = if end >= words.len() {
            i64::MAX
        } else {
            words[end].start_ms as i64 - words[end - 1].end_ms as i64
        };
        let isolated = pause_before >= cfg.isolation_ms && pause_after >= cfg.isolation_ms;
        let (verdict, reason) = if !isolated {
            (
                Verdict::Keep,
                format!("\"{phrase}\" not isolated by pauses: kept"),
            )
        } else {
            let min_conf = words[i..end]
                .iter()
                .map(|w| w.confidence)
                .try_fold(f32::MAX, |acc, c| c.map(|c| acc.min(c)));
            match min_conf {
                None => (
                    Verdict::Keep,
                    format!("\"{phrase}\" has no word confidence: kept"),
                ),
                Some(c) if c < cfg.soft_filler_min_confidence => (
                    Verdict::Escalate,
                    format!(
                        "\"{phrase}\" confidence {c:.2} below {:.2}: kept, marked for escalation",
                        cfg.soft_filler_min_confidence
                    ),
                ),
                Some(c) => (
                    Verdict::Cut,
                    format!("isolated \"{phrase}\" at confidence {c:.2}"),
                ),
            }
        };
        hits.push(RuleHit {
            rule: RetakeRule::SoftFiller,
            first: i,
            end,
            verdict,
            reason,
        });
        i = end;
    }
    hits
}

/// Words marked for removal by `Cut` hits. Words no hit names are kept.
pub fn cut_mask(len: usize, hits: &[RuleHit]) -> Vec<bool> {
    let mut mask = vec![false; len];
    for h in hits.iter().filter(|h| h.verdict == Verdict::Cut) {
        for slot in mask.iter_mut().take(h.end.min(len)).skip(h.first) {
            *slot = true;
        }
    }
    mask
}

/// Hits marked for a later judge. Nothing here calls a model.
pub fn escalations(hits: &[RuleHit]) -> Vec<&RuleHit> {
    hits.iter()
        .filter(|h| h.verdict == Verdict::Escalate)
        .collect()
}

/// A take stops mid-phrase when its last word does not end a sentence. An empty or
/// out-of-range take counts as broken.
pub fn is_broken_take(words: &[Word], take: Range<usize>) -> bool {
    if take.start >= take.end || take.end > words.len() {
        return true;
    }
    !ends_sentence(&words[take.end - 1].text)
}

/// Broken later takes are suggested for review, never cut automatically.
pub fn take_tier(words: &[Word], take: Range<usize>) -> Tier {
    if is_broken_take(words, take) {
        Tier::Suggest
    } else {
        Tier::Auto
    }
}
