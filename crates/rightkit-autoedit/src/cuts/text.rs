//! Token normalisation, filler matching and retake (repeated take) detection.

use crate::cuts::config::{CutConfig, RetakeConfig};

/// Lowercase, fold curly apostrophes, trim non-alphanumerics from both ends.
pub(crate) fn normalize(raw: &str) -> String {
    let lower = raw.to_lowercase().replace(['\u{2019}', '\u{2018}'], "'");
    lower
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_string()
}

pub(crate) fn ends_sentence(raw: &str) -> bool {
    raw.trim_end_matches(['"', '\'', ')', ']', '\u{201d}', '\u{2019}'])
        .ends_with(['.', '?', '!', '\u{2026}'])
}

fn ends_phrase(raw: &str) -> bool {
    ends_sentence(raw)
        || raw
            .trim_end_matches(['"', '\'', ')', ']'])
            .ends_with([',', ';', ':', '\u{2014}', '-'])
}

/// A word as the planner sees it: refined end, normalised token.
#[derive(Debug, Clone)]
pub(crate) struct W {
    pub start: i64,
    pub end: i64,
    pub text: String,
    pub norm: String,
}

pub(crate) struct FillerHit {
    pub first: usize,
    /// Exclusive.
    pub end: usize,
    pub context: bool,
    pub pause_before: i64,
    pub pause_after: i64,
}

struct Pattern {
    tokens: Vec<String>,
    context: bool,
}

fn patterns(cfg: &CutConfig) -> Vec<Pattern> {
    let mut out: Vec<Pattern> = Vec::new();
    let mut add = |list: &[String], context: bool| {
        for entry in list {
            let tokens: Vec<String> = entry
                .split_whitespace()
                .map(normalize)
                .filter(|t| !t.is_empty())
                .collect();
            if !tokens.is_empty() {
                out.push(Pattern { tokens, context });
            }
        }
    };
    add(&cfg.fillers, false);
    add(&cfg.context_fillers, true);
    // Longest first so "you know" wins over a hypothetical "you"; stable otherwise.
    out.sort_by_key(|p| std::cmp::Reverse(p.tokens.len()));
    out
}

/// Finds filler hits in non-removed words. `removed[i]` words are skipped and
/// never form part of a match. Hits are non-overlapping, in order.
pub(crate) fn find_fillers(words: &[W], removed: &[bool], cfg: &CutConfig) -> Vec<FillerHit> {
    let pats = patterns(cfg);
    let mut hits = Vec::new();
    let mut i = 0;
    while i < words.len() {
        if removed[i] {
            i += 1;
            continue;
        }
        let mut matched = None;
        for p in &pats {
            let n = p.tokens.len();
            if i + n > words.len() {
                continue;
            }
            if (i..i + n).any(|k| removed[k]) {
                continue;
            }
            if !(0..n).all(|k| words[i + k].norm == p.tokens[k]) {
                continue;
            }
            let pause_before = if i == 0 {
                i64::MAX
            } else {
                words[i].start - words[i - 1].end
            };
            let pause_after = if i + n >= words.len() {
                i64::MAX
            } else {
                words[i + n].start - words[i + n - 1].end
            };
            if p.context
                && (pause_before < cfg.filler_isolation_ms || pause_after < cfg.filler_isolation_ms)
            {
                continue;
            }
            matched = Some(FillerHit {
                first: i,
                end: i + n,
                context: p.context,
                pause_before,
                pause_after,
            });
            break;
        }
        match matched {
            Some(h) => {
                i = h.end;
                hits.push(h);
            }
            None => i += 1,
        }
    }
    hits
}

pub(crate) struct RetakeHit {
    /// First word index removed (start of the abandoned attempt).
    pub from: usize,
    /// Exclusive: first word of the restart, which is kept.
    pub to: usize,
    pub similarity: f32,
    /// Word range (exclusive end) of the later take the attempt was matched to.
    pub kept_from: usize,
    pub kept_end: usize,
}

fn lcs(a: &[&str], b: &[&str]) -> usize {
    let mut prev = vec![0u16; b.len() + 1];
    let mut cur = vec![0u16; b.len() + 1];
    for x in a {
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = if x == y {
                prev[j] + 1
            } else {
                prev[j + 1].max(cur[j])
            };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()] as usize
}

/// Detects earlier attempts that a later take repeats; the LAST take is kept.
/// `eligible[i]` is false for words already removed (fillers, manual): they are
/// ignored when comparing but still removed if they sit inside a retake span.
pub(crate) fn find_retakes(words: &[W], eligible: &[bool], cfg: &RetakeConfig) -> Vec<RetakeHit> {
    if !cfg.enabled || cfg.min_words == 0 {
        return Vec::new();
    }
    let toks: Vec<usize> = (0..words.len())
        .filter(|&i| eligible[i] && !words[i].norm.is_empty())
        .collect();
    let norm = |t: usize| words[toks[t]].norm.as_str();
    // Boundary after token t: punctuation, a pause, or the end of speech.
    let boundary = |t: usize| -> bool {
        if t + 1 >= toks.len() {
            return true;
        }
        let (a, b) = (&words[toks[t]], &words[toks[t + 1]]);
        ends_phrase(&a.text) || b.start - a.end >= cfg.boundary_pause_ms
    };
    let mut hits = Vec::new();
    let mut p = 0;
    while p < toks.len() {
        let mut found = None;
        'n: for n in (cfg.min_words..=cfg.max_words.min(toks.len() - p)).rev() {
            if !boundary(p + n - 1) {
                continue;
            }
            for g in 0..=cfg.max_between_words {
                let q = p + n + g;
                if q >= toks.len() {
                    break;
                }
                if norm(q) != norm(p) {
                    continue;
                }
                let attempt_end = words[toks[p + n - 1]].end;
                if words[toks[q]].start - attempt_end > cfg.max_between_ms {
                    break;
                }
                let m = n.min(toks.len() - q);
                if m < cfg.min_words {
                    continue;
                }
                let a: Vec<&str> = (p..p + n).map(norm).collect();
                let b: Vec<&str> = (q..q + m).map(norm).collect();
                let sim = 2.0 * lcs(&a, &b) as f32 / (n + m) as f32;
                if sim >= cfg.threshold {
                    found = Some((n, q, m, sim));
                    break 'n;
                }
            }
        }
        match found {
            Some((_n, q, m, sim)) => {
                hits.push(RetakeHit {
                    from: toks[p],
                    to: toks[q],
                    similarity: sim,
                    kept_from: toks[q],
                    kept_end: toks[q + m - 1] + 1,
                });
                p = q;
            }
            None => p += 1,
        }
    }
    hits
}
