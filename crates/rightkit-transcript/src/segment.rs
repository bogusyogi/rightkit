//! Cue segmentation: phrase grouping, splitting, merging, line breaking.
//!
//! Donor: CutRight `cutright-captions` (itself ported from `video-media`'s
//! caption model). Deterministic: identical words + config give identical cues.

use serde::{Deserialize, Serialize};

use crate::align::ALIGNMENT_TOLERANCE_MS;
use crate::words::TimedWord;

/// Named segmentation profile (replaces CutRight's timeline `Preset`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    /// Landscape video (YouTube): 42 chars x 2 lines.
    Standard,
    /// Vertical feed (TikTok): 20 chars x 2 lines, short cues.
    Vertical,
    /// Reels / square feed: 24 chars x 2 lines.
    Compact,
}

/// Per-profile segmentation limits.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CaptionConfig {
    pub max_chars_per_line: usize,
    pub max_lines: usize,
    /// Reading-speed ceiling: characters per second of cue time.
    pub max_chars_per_second: f64,
    /// Cues shorter than this are merged with a neighbour when possible, else
    /// held on screen up to [`ALIGNMENT_TOLERANCE_MS`] into the following gap.
    pub min_cue_ms: u64,
    /// A cue never spans longer than this (it splits instead).
    pub max_cue_ms: u64,
    /// A gap between words larger than this always starts a new cue.
    pub max_gap_ms: u64,
}

impl CaptionConfig {
    pub fn for_profile(profile: Profile) -> Self {
        match profile {
            Profile::Standard => Self {
                max_chars_per_line: 42,
                max_lines: 2,
                max_chars_per_second: 21.0,
                min_cue_ms: 600,
                max_cue_ms: 7000,
                max_gap_ms: 700,
            },
            Profile::Vertical => Self {
                max_chars_per_line: 20,
                max_lines: 2,
                max_chars_per_second: 18.0,
                min_cue_ms: 600,
                max_cue_ms: 4500,
                max_gap_ms: 700,
            },
            Profile::Compact => Self {
                max_chars_per_line: 24,
                max_lines: 2,
                max_chars_per_second: 18.0,
                min_cue_ms: 600,
                max_cue_ms: 4500,
                max_gap_ms: 700,
            },
        }
    }
}

impl Default for CaptionConfig {
    fn default() -> Self {
        Self::for_profile(Profile::Standard)
    }
}

/// One caption cue on the output timeline. Cues never overlap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    /// Display lines (already broken; at most `max_lines` unless a single word
    /// is longer than a line).
    pub lines: Vec<String>,
    /// Index range `[first_word, last_word]` into the normalized word list.
    pub first_word: usize,
    pub last_word: usize,
}

impl Cue {
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

fn sentence_end(t: &str) -> bool {
    t.ends_with(['.', '?', '!', '\u{2026}'])
}

fn clause_end(t: &str) -> bool {
    t.ends_with([',', ';', ':', '\u{2014}', '\u{2013}'])
}

/// Trims, drops empty/zero-length words, collapses inner whitespace and clamps
/// overlaps so each word ends no later than the next starts.
pub fn normalize_words(words: &[TimedWord]) -> Vec<TimedWord> {
    let mut out: Vec<TimedWord> = Vec::with_capacity(words.len());
    for w in words {
        let text = w.text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() || w.end_ms <= w.start_ms {
            continue;
        }
        if let Some(prev) = out.last_mut() {
            if prev.end_ms > w.start_ms {
                prev.end_ms = w.start_ms;
                if prev.end_ms <= prev.start_ms {
                    out.pop();
                }
            }
        }
        out.push(TimedWord::new(text, w.start_ms, w.end_ms));
    }
    out
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

fn greedy_wrap(words: &[&str], max: usize) -> Vec<Vec<usize>> {
    // Returns word-index ranges per line as index lists (start..end pairs flattened).
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut cur: Vec<usize> = Vec::new();
    let mut len = 0usize;
    for (i, w) in words.iter().enumerate() {
        let wl = chars(w);
        if cur.is_empty() {
            cur.push(i);
            len = wl;
        } else if len + 1 + wl <= max {
            cur.push(i);
            len += 1 + wl;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur.push(i);
            len = wl;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

fn line_count(group: &[TimedWord], max: usize) -> usize {
    let w: Vec<&str> = group.iter().map(|w| w.text.as_str()).collect();
    greedy_wrap(&w, max).len()
}

fn text_chars(group: &[TimedWord]) -> usize {
    group
        .iter()
        .map(|w| chars(&w.text) + 1)
        .sum::<usize>()
        .saturating_sub(1)
}

fn span(group: &[TimedWord]) -> u64 {
    group.last().unwrap().end_ms - group.first().unwrap().start_ms
}

fn needs_split(group: &[TimedWord], cfg: &CaptionConfig) -> bool {
    if group.len() <= 1 {
        return false;
    }
    let dur = span(group);
    if dur > cfg.max_cue_ms {
        return true;
    }
    if line_count(group, cfg.max_chars_per_line) > cfg.max_lines {
        return true;
    }
    // Judge reading speed against the time the cue will actually be on screen.
    let shown = dur.max(cfg.min_cue_ms) as f64 / 1000.0;
    text_chars(group) as f64 > cfg.max_chars_per_second * shown
}

/// Split index (start of the right half): prefers sentence then clause ends
/// near the middle, then the largest silence, falling back to the character
/// midpoint.
fn split_index(group: &[TimedWord]) -> usize {
    let total = text_chars(group) as i64;
    let mut best: Option<(i64, i64, usize)> = None; // (punct rank, distance, idx)
    let mut run = 0i64;
    for (i, w) in group.iter().enumerate().take(group.len() - 1) {
        run += chars(&w.text) as i64 + 1;
        let dist = (run - total / 2).abs();
        // Punctuation only counts when it leaves a reasonable remainder.
        let balanced = run * 4 >= total && (total - run) * 4 >= total;
        let rank = if balanced && sentence_end(&w.text) {
            0
        } else if balanced && clause_end(&w.text) {
            1
        } else {
            2
        };
        if best.is_none_or(|(r, d, _)| (rank, dist) < (r, d)) {
            best = Some((rank, dist, i + 1));
        }
    }
    best.map(|b| b.2).unwrap_or(1).clamp(1, group.len() - 1)
}

fn split_rec(group: Vec<TimedWord>, cfg: &CaptionConfig, out: &mut Vec<Vec<TimedWord>>) {
    if group.is_empty() {
        return;
    }
    if !needs_split(&group, cfg) {
        out.push(group);
        return;
    }
    let at = split_index(&group);
    let mut left = group;
    let right = left.split_off(at);
    split_rec(left, cfg, out);
    split_rec(right, cfg, out);
}

fn phrase_groups(words: &[TimedWord], cfg: &CaptionConfig) -> Vec<Vec<TimedWord>> {
    let mut groups: Vec<Vec<TimedWord>> = Vec::new();
    for w in words {
        let new = match groups.last().and_then(|g| g.last()) {
            None => true,
            Some(last) => w.start_ms - last.end_ms > cfg.max_gap_ms || sentence_end(&last.text),
        };
        if new {
            groups.push(vec![w.clone()]);
        } else {
            groups.last_mut().unwrap().push(w.clone());
        }
    }
    groups
}

/// Merges a too-short cue into its neighbour when the pair still fits the
/// limits, the gap is small and no sentence ends between them.
fn merge_short(groups: Vec<Vec<TimedWord>>, cfg: &CaptionConfig) -> Vec<Vec<TimedWord>> {
    let mut out: Vec<Vec<TimedWord>> = Vec::new();
    let mut iter = groups.into_iter().peekable();
    while let Some(mut cur) = iter.next() {
        while span(&cur) < cfg.min_cue_ms {
            let Some(next) = iter.peek() else { break };
            let gap = next[0].start_ms - cur.last().unwrap().end_ms;
            if gap > cfg.max_gap_ms || sentence_end(&cur.last().unwrap().text) {
                break;
            }
            let mut joined = cur.clone();
            joined.extend(next.iter().cloned());
            if needs_split(&joined, cfg) {
                break;
            }
            cur = joined;
            iter.next();
        }
        // Also fold a short leftover into the previous cue when it fits.
        if span(&cur) < cfg.min_cue_ms {
            if let Some(prev) = out.last_mut() {
                let gap = cur[0].start_ms - prev.last().unwrap().end_ms;
                if gap <= cfg.max_gap_ms && !sentence_end(&prev.last().unwrap().text) {
                    let mut joined = prev.clone();
                    joined.extend(cur.iter().cloned());
                    if !needs_split(&joined, cfg) {
                        *prev = joined;
                        continue;
                    }
                }
            }
        }
        out.push(cur);
    }
    out
}

/// Breaks a cue's words into display lines: greedy wrap, then for two lines the
/// balanced break that prefers a clause boundary.
fn break_lines(group: &[TimedWord], cfg: &CaptionConfig) -> Vec<String> {
    let w: Vec<&str> = group.iter().map(|w| w.text.as_str()).collect();
    let greedy = greedy_wrap(&w, cfg.max_chars_per_line);
    let join = |r: &[usize]| r.iter().map(|&i| w[i]).collect::<Vec<_>>().join(" ");
    if greedy.len() != 2 || cfg.max_lines < 2 {
        return greedy.iter().map(|l| join(l)).collect();
    }
    let all: Vec<usize> = (0..w.len()).collect();
    let total: usize = all.iter().map(|&i| chars(w[i]) + 1).sum::<usize>() - 1;
    let mut best: Option<((u8, usize), usize)> = None;
    for k in 1..w.len() {
        let l = join(&all[..k]);
        let r = join(&all[k..]);
        if chars(&l) > cfg.max_chars_per_line || chars(&r) > cfg.max_chars_per_line {
            continue;
        }
        let diff = chars(&l).abs_diff(chars(&r));
        let short = chars(&l).min(chars(&r));
        let punct = (sentence_end(w[k - 1]) || clause_end(w[k - 1])) && short * 4 >= total;
        let key = (u8::from(!punct), diff);
        if best.is_none_or(|(b, _)| key < b) {
            best = Some((key, k));
        }
    }
    match best {
        Some((_, k)) => vec![join(&all[..k]), join(&all[k..])],
        None => greedy.iter().map(|l| join(l)).collect(),
    }
}

/// Builds non-overlapping cues from output-time words.
///
/// Every cue starts at a word start and ends at a word end, except that a cue
/// shorter than `min_cue_ms` is held on screen up to [`ALIGNMENT_TOLERANCE_MS`]
/// past its last word (never into the next cue).
pub fn build_cues(words: &[TimedWord], cfg: &CaptionConfig) -> Vec<Cue> {
    let words = normalize_words(words);
    let mut groups = Vec::new();
    for phrase in phrase_groups(&words, cfg) {
        split_rec(phrase, cfg, &mut groups);
    }
    let groups = merge_short(groups, cfg);

    // Word index of the start of each group.
    let mut cues: Vec<Cue> = Vec::with_capacity(groups.len());
    let mut idx = 0usize;
    for g in &groups {
        let first = idx;
        idx += g.len();
        cues.push(Cue {
            start_ms: g[0].start_ms,
            end_ms: g.last().unwrap().end_ms,
            lines: break_lines(g, cfg),
            first_word: first,
            last_word: idx - 1,
        });
    }
    for i in 0..cues.len() {
        let next_start = cues.get(i + 1).map(|c| c.start_ms);
        let c = &mut cues[i];
        if c.end_ms - c.start_ms < cfg.min_cue_ms {
            let mut end = (c.start_ms + cfg.min_cue_ms).min(c.end_ms + ALIGNMENT_TOLERANCE_MS);
            if let Some(n) = next_start {
                end = end.min(n);
            }
            c.end_ms = end.max(c.end_ms);
        }
    }
    cues
}
