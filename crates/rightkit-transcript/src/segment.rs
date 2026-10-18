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
        let mut word = TimedWord::new(text, w.start_ms, w.end_ms);
        word.source_ms = w.source_ms;
        out.push(word);
    }
    out
}

fn chars(s: &str) -> usize {
    s.chars().count()
}

/// Width callback & usable text width for deterministic measured line breaking.
///
/// `measure_width` receives whole candidate lines, including spaces, in rendered
/// case. Return widths in the same units as `max_width_px` (normally pixels).
/// Capture font face & size in the callback; this crate has no font dependency.
/// The callback should return identical widths for identical text. Errors pass
/// through [`wrap_lines`] & [`build_cues_with_metrics`] unchanged.
#[derive(Clone, Copy)]
pub struct LineMetrics<'a> {
    pub measure_width: &'a dyn Fn(&str) -> Result<u64, String>,
    pub max_width_px: u64,
    /// Measure uppercase text & count its expanded characters without changing
    /// transcript text or returned display lines.
    pub uppercase: bool,
}

impl std::fmt::Debug for LineMetrics<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LineMetrics")
            .field("max_width_px", &self.max_width_px)
            .field("uppercase", &self.uppercase)
            .finish_non_exhaustive()
    }
}

impl LineMetrics<'_> {
    fn width(&self, text: &str) -> Result<u64, String> {
        if self.uppercase {
            (self.measure_width)(&text.to_uppercase())
        } else {
            (self.measure_width)(text)
        }
    }
}

fn fits(text: &str, max: usize, metrics: Option<&LineMetrics<'_>>) -> Result<bool, String> {
    let count = if metrics.is_some_and(|m| m.uppercase) {
        chars(&text.to_uppercase())
    } else {
        chars(text)
    };
    Ok(count <= max
        && match metrics {
            Some(m) => m.width(text)? <= m.max_width_px,
            None => true,
        })
}

/// Wrap whitespace-separated text, preserving case & word order.
///
/// With metrics, both character count & measured width must fit. An overlong
/// word splits at Unicode scalar boundaries; an individual glyph that cannot
/// fit returns an error. Without metrics, overlong words stay on one line &
/// character-count wrapping is infallible. Empty text returns no lines.
/// Wrapping does not change cue timing or word indices.
pub fn wrap_lines(
    text: &str,
    max_chars: usize,
    metrics: Option<&LineMetrics<'_>>,
) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let joined = if cur.is_empty() {
            word.to_string()
        } else {
            format!("{cur} {word}")
        };
        if fits(&joined, max_chars, metrics)? {
            cur = joined;
            continue;
        }
        if !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
        }
        if metrics.is_none() || fits(word, max_chars, metrics)? {
            cur = word.to_string();
            continue;
        }
        // An overlong word keeps its timing, but occupies multiple display lines.
        for c in word.chars() {
            let joined = format!("{cur}{c}");
            if !fits(&joined, max_chars, metrics)? {
                if cur.is_empty() || !fits(&c.to_string(), max_chars, metrics)? {
                    return Err(format!("caption glyph {c:?} exceeds the safe line width"));
                }
                lines.push(std::mem::take(&mut cur));
            }
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    Ok(lines)
}

fn line_count(
    group: &[TimedWord],
    max: usize,
    metrics: Option<&LineMetrics<'_>>,
) -> Result<usize, String> {
    let text = group
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Ok(wrap_lines(&text, max, metrics)?.len())
}

fn text_chars(group: &[TimedWord]) -> usize {
    group
        .iter()
        .map(|w| chars(&w.text) + 1)
        .sum::<usize>()
        .saturating_sub(1)
}

/// Duration of a group, first start to last end, in milliseconds. A reversed span
/// (end before start) is zero length, as in `group_words`, and an empty group is zero.
/// Neither can underflow.
fn span(group: &[TimedWord]) -> u64 {
    match (group.first(), group.last()) {
        (Some(first), Some(last)) => last.end_ms.saturating_sub(first.start_ms),
        _ => 0,
    }
}

fn needs_split(
    group: &[TimedWord],
    cfg: &CaptionConfig,
    metrics: Option<&LineMetrics<'_>>,
) -> Result<bool, String> {
    if group.len() <= 1 {
        return Ok(false);
    }
    let dur = span(group);
    if dur > cfg.max_cue_ms {
        return Ok(true);
    }
    if line_count(group, cfg.max_chars_per_line, metrics)? > cfg.max_lines {
        return Ok(true);
    }
    // Judge reading speed against the time the cue will actually be on screen.
    let shown = dur.max(cfg.min_cue_ms) as f64 / 1000.0;
    Ok(text_chars(group) as f64 > cfg.max_chars_per_second * shown)
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

fn split_rec(
    group: Vec<TimedWord>,
    cfg: &CaptionConfig,
    metrics: Option<&LineMetrics<'_>>,
    out: &mut Vec<Vec<TimedWord>>,
) -> Result<(), String> {
    if group.is_empty() {
        return Ok(());
    }
    if !needs_split(&group, cfg, metrics)? {
        out.push(group);
        return Ok(());
    }
    let at = split_index(&group);
    let mut left = group;
    let right = left.split_off(at);
    split_rec(left, cfg, metrics, out)?;
    split_rec(right, cfg, metrics, out)
}

fn phrase_groups(words: &[TimedWord], cfg: &CaptionConfig) -> Vec<Vec<TimedWord>> {
    let mut groups: Vec<Vec<TimedWord>> = Vec::new();
    for w in words {
        let new = match groups.last().and_then(|g| g.last()) {
            None => true,
            // Mapped words can overlap at cut points: an overlap is no gap.
            Some(last) => {
                w.start_ms.saturating_sub(last.end_ms) > cfg.max_gap_ms || sentence_end(&last.text)
            }
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
fn merge_short(
    groups: Vec<Vec<TimedWord>>,
    cfg: &CaptionConfig,
    metrics: Option<&LineMetrics<'_>>,
) -> Result<Vec<Vec<TimedWord>>, String> {
    let mut out: Vec<Vec<TimedWord>> = Vec::new();
    let mut iter = groups.into_iter().peekable();
    while let Some(mut cur) = iter.next() {
        while span(&cur) < cfg.min_cue_ms {
            let Some(next) = iter.peek() else { break };
            let gap = next[0].start_ms.saturating_sub(cur.last().unwrap().end_ms);
            if gap > cfg.max_gap_ms || sentence_end(&cur.last().unwrap().text) {
                break;
            }
            let mut joined = cur.clone();
            joined.extend(next.iter().cloned());
            if needs_split(&joined, cfg, metrics)? {
                break;
            }
            cur = joined;
            iter.next();
        }
        // Also fold a short leftover into the previous cue when it fits.
        if span(&cur) < cfg.min_cue_ms {
            if let Some(prev) = out.last_mut() {
                let gap = cur[0].start_ms.saturating_sub(prev.last().unwrap().end_ms);
                if gap <= cfg.max_gap_ms && !sentence_end(&prev.last().unwrap().text) {
                    let mut joined = prev.clone();
                    joined.extend(cur.iter().cloned());
                    if !needs_split(&joined, cfg, metrics)? {
                        *prev = joined;
                        continue;
                    }
                }
            }
        }
        out.push(cur);
    }
    Ok(out)
}

/// Breaks a cue's words into display lines: greedy wrap, then for two lines the
/// balanced break that prefers a clause boundary.
fn break_lines(
    group: &[TimedWord],
    cfg: &CaptionConfig,
    metrics: Option<&LineMetrics<'_>>,
) -> Result<Vec<String>, String> {
    let w: Vec<&str> = group.iter().map(|w| w.text.as_str()).collect();
    let greedy = wrap_lines(&w.join(" "), cfg.max_chars_per_line, metrics)?;
    let join = |r: &[usize]| r.iter().map(|&i| w[i]).collect::<Vec<_>>().join(" ");
    if greedy.len() != 2 || cfg.max_lines < 2 {
        return Ok(greedy);
    }
    let all: Vec<usize> = (0..w.len()).collect();
    let mut best: Option<((u8, u64), usize)> = None;
    for k in 1..w.len() {
        let l = join(&all[..k]);
        let r = join(&all[k..]);
        if !fits(&l, cfg.max_chars_per_line, metrics)?
            || !fits(&r, cfg.max_chars_per_line, metrics)?
        {
            continue;
        }
        let size = |s: &str| match metrics {
            Some(m) => m.width(s),
            None => Ok(chars(s) as u64),
        };
        let (left, right) = (size(&l)?, size(&r)?);
        let diff = left.abs_diff(right);
        let short = left.min(right);
        let total = left + right + u64::from(metrics.is_none());
        let punct = (sentence_end(w[k - 1]) || clause_end(w[k - 1])) && short * 4 >= total;
        let key = (u8::from(!punct), diff);
        if best.is_none_or(|(b, _)| key < b) {
            best = Some((key, k));
        }
    }
    Ok(match best {
        Some((_, k)) => vec![join(&all[..k]), join(&all[k..])],
        None => greedy,
    })
}

/// Rules for [`group_words`], the lightweight cue grouper for engines whose
/// words are already clean (no line breaking, no reading-speed limits). Use
/// [`build_cues`] for caption-grade segmentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupRules {
    /// A cue closes once it holds this many words.
    pub max_words: usize,
    /// A cue closes once its span reaches this length.
    pub max_span_ms: u64,
    /// A gap larger than this between words starts a new cue.
    pub max_gap_ms: u64,
    /// Sentence-final punctuation (`.`, `?`, `!`) closes a cue holding at
    /// least this many words.
    pub sentence_min_words: usize,
    /// Cues shorter than this are widened to it (zero-width cues never export).
    pub min_cue_ms: u64,
}

impl Default for GroupRules {
    /// The engine-side defaults `rightkit-asr` has always used: 12 words, 7 s,
    /// 1 s gap, sentence close from 3 words, 80 ms floor (one encoder frame).
    fn default() -> Self {
        Self {
            max_words: 12,
            max_span_ms: 7_000,
            max_gap_ms: 1_000,
            sentence_min_words: 3,
            min_cue_ms: 80,
        }
    }
}

fn single_line_cue(
    text: &str,
    start_ms: u64,
    end_ms: u64,
    first: usize,
    last: usize,
    min_cue_ms: u64,
) -> Cue {
    Cue {
        start_ms,
        end_ms: end_ms.max(start_ms + min_cue_ms),
        lines: vec![text.trim().to_string()],
        first_word: first,
        last_word: last,
    }
}

/// Groups words (in order, not re-normalised) into single-line cues by
/// [`GroupRules`]. `first_word`/`last_word` index the input slice. A cue closes
/// on sentence-final punctuation (after `sentence_min_words` words), at
/// `max_words`, at `max_span_ms`, or on a gap over `max_gap_ms`.
pub fn group_words(words: &[TimedWord], rules: &GroupRules) -> Vec<Cue> {
    struct Open {
        text: String,
        start: u64,
        end: u64,
        first: usize,
        last: usize,
        count: usize,
    }
    let close =
        |o: &Open| single_line_cue(&o.text, o.start, o.end, o.first, o.last, rules.min_cue_ms);
    let open = |i: usize, w: &TimedWord| Open {
        text: w.text.clone(),
        start: w.start_ms,
        end: w.end_ms,
        first: i,
        last: i,
        count: 1,
    };
    let mut cues = Vec::new();
    let mut current: Option<Open> = None;
    for (i, word) in words.iter().enumerate() {
        match current.as_mut() {
            Some(c) => {
                let gap_over = word.start_ms > c.end && word.start_ms - c.end > rules.max_gap_ms;
                // An inverted word (end before start) is a zero-length span, not an underflow.
                let span = c.end.saturating_sub(c.start);
                if gap_over || c.count >= rules.max_words || span >= rules.max_span_ms {
                    cues.push(close(c));
                    current = Some(open(i, word));
                } else {
                    c.text.push(' ');
                    c.text.push_str(&word.text);
                    c.end = word.end_ms;
                    c.last = i;
                    c.count += 1;
                }
            }
            None => current = Some(open(i, word)),
        }
        if current.as_ref().is_some_and(|c| {
            c.text.ends_with(['.', '?', '!']) && c.count >= rules.sentence_min_words
        }) {
            cues.push(close(&current.take().expect("cue present")));
        }
    }
    if let Some(c) = current {
        cues.push(close(&c));
    }
    cues
}

/// One single-line cue per span, in order (`first_word == last_word ==` the
/// span's index), widened to `min_cue_ms`. For callers that already hold cues
/// as [`TimedWord`] spans (text = cue text) and only need export.
pub fn cues_from_spans(spans: &[TimedWord], min_cue_ms: u64) -> Vec<Cue> {
    spans
        .iter()
        .enumerate()
        .map(|(i, s)| single_line_cue(&s.text, s.start_ms, s.end_ms, i, i, min_cue_ms))
        .collect()
}

/// Builds non-overlapping cues from output-time words.
///
/// Every cue starts at a word start and ends at a word end, except that a cue
/// shorter than `min_cue_ms` is held on screen up to [`ALIGNMENT_TOLERANCE_MS`]
/// past its last word (never into the next cue).
pub fn build_cues(words: &[TimedWord], cfg: &CaptionConfig) -> Vec<Cue> {
    build_cues_with_metrics(words, cfg, None).expect("character-count layout is infallible")
}

/// Builds cues using caller-measured widths, or character limits without metrics.
///
/// Measured limits apply to splitting, short-cue merging & line balancing.
/// Character limits remain an upper bound. Returned text keeps input case;
/// indices refer to [`normalize_words`] output & timings stay on word boundaries
/// within [`ALIGNMENT_TOLERANCE_MS`]. A single word may occupy more than
/// `max_lines` when it must split to fit. Measurement & overwide-glyph errors
/// propagate without returning partial cues.
pub fn build_cues_with_metrics(
    words: &[TimedWord],
    cfg: &CaptionConfig,
    metrics: Option<&LineMetrics<'_>>,
) -> Result<Vec<Cue>, String> {
    let words = normalize_words(words);
    let mut groups = Vec::new();
    for phrase in phrase_groups(&words, cfg) {
        split_rec(phrase, cfg, metrics, &mut groups)?;
    }
    let groups = merge_short(groups, cfg, metrics)?;

    // Word index of the start of each group.
    let mut cues: Vec<Cue> = Vec::with_capacity(groups.len());
    let mut idx = 0usize;
    for g in &groups {
        let first = idx;
        idx += g.len();
        cues.push(Cue {
            start_ms: g[0].start_ms,
            end_ms: g.last().unwrap().end_ms,
            lines: break_lines(g, cfg, metrics)?,
            first_word: first,
            last_word: idx - 1,
        });
    }
    for i in 0..cues.len() {
        let next_start = cues.get(i + 1).map(|c| c.start_ms);
        let c = &mut cues[i];
        if c.end_ms.saturating_sub(c.start_ms) < cfg.min_cue_ms {
            let mut end = (c.start_ms + cfg.min_cue_ms).min(c.end_ms + ALIGNMENT_TOLERANCE_MS);
            if let Some(n) = next_start {
                end = end.min(n);
            }
            c.end_ms = end.max(c.end_ms);
        }
    }
    Ok(cues)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_words_treats_an_inverted_word_as_zero_length() {
        // Not normalised input: the first word ends before it starts.
        let words = [TimedWord::new("a", 500, 100), TimedWord::new("b", 600, 700)];
        let cues = group_words(&words, &GroupRules::default());
        assert_eq!(cues.len(), 1, "inverted span must not force a split");
        assert_eq!(cues[0].start_ms, 500);
        assert!(cues[0].end_ms >= cues[0].start_ms);
        assert_eq!(cues[0].lines, vec!["a b".to_string()]);

        let lone = group_words(&[TimedWord::new("x", 900, 10)], &GroupRules::default());
        assert_eq!(lone.len(), 1);
        assert_eq!(lone[0].start_ms, 900);
        assert_eq!(lone[0].end_ms, 900 + GroupRules::default().min_cue_ms);
    }

    #[test]
    fn span_of_a_reversed_or_empty_group_is_zero_and_never_underflows() {
        // Last word ends before the first word starts: zero, not a u64 underflow.
        let reversed = [TimedWord::new("a", 500, 100), TimedWord::new("b", 600, 200)];
        assert_eq!(span(&reversed), 0);
        // Ordinary groups still measure first start to last end.
        let ordered = [TimedWord::new("a", 100, 300), TimedWord::new("b", 320, 900)];
        assert_eq!(span(&ordered), 800);
        assert_eq!(span(&[]), 0);
    }
}

#[cfg(test)]
mod measured_tests {
    use super::*;

    fn count(text: &str) -> Result<u64, String> {
        Ok(text.chars().count() as u64)
    }

    #[test]
    fn wraps_whitespace_and_preserves_overlong_words_without_metrics() {
        assert_eq!(
            wrap_lines("  one\n two\tthree ", 7, None).unwrap(),
            ["one two", "three"]
        );
        assert_eq!(
            wrap_lines("superlong x", 3, None).unwrap(),
            ["superlong", "x"]
        );
        assert!(wrap_lines(" \t\n", 0, None).unwrap().is_empty());
    }

    #[test]
    fn measured_long_word_splits_unicode_without_losing_text() {
        let metrics = LineMetrics {
            measure_width: &count,
            max_width_px: 2,
            uppercase: false,
        };
        assert_eq!(
            wrap_lines("é😀界abc", 42, Some(&metrics)).unwrap(),
            ["é😀", "界a", "bc"]
        );
        assert_eq!(
            wrap_lines("abcde", 1, Some(&metrics)).unwrap(),
            ["a", "b", "c", "d", "e"]
        );
    }

    #[test]
    fn uppercase_expansion_counts_rendered_characters_and_preserves_case() {
        let measured = |text: &str| {
            assert_eq!(text, text.to_uppercase());
            count(text)
        };
        let metrics = LineMetrics {
            measure_width: &measured,
            max_width_px: 100,
            uppercase: true,
        };
        assert_eq!(
            wrap_lines("ßa b", 2, Some(&metrics)).unwrap(),
            ["ß", "a", "b"]
        );
    }

    #[test]
    fn callback_measures_complete_candidate_lines_including_spaces() {
        // A whole-line measurement can model kerning/shaping, not just advances.
        let measure = |text: &str| {
            Ok(if text == "ab cd" {
                4
            } else {
                text.chars().count() as u64
            })
        };
        let metrics = LineMetrics {
            measure_width: &measure,
            max_width_px: 4,
            uppercase: false,
        };
        assert_eq!(wrap_lines("ab cd", 10, Some(&metrics)).unwrap(), ["ab cd"]);
    }

    #[test]
    fn errors_propagate_and_impossible_glyph_is_reported() {
        let fail = |_: &str| Err("measurement failed".to_string());
        let metrics = LineMetrics {
            measure_width: &fail,
            max_width_px: 100,
            uppercase: false,
        };
        assert_eq!(
            wrap_lines("x", 42, Some(&metrics)),
            Err("measurement failed".into())
        );
        assert_eq!(
            build_cues_with_metrics(
                &[TimedWord::new("x", 0, 800)],
                &CaptionConfig::default(),
                Some(&metrics)
            ),
            Err("measurement failed".into())
        );
        let metrics = LineMetrics {
            measure_width: &count,
            max_width_px: 0,
            uppercase: false,
        };
        assert_eq!(
            wrap_lines("界", 42, Some(&metrics)),
            Err("caption glyph '界' exceeds the safe line width".into())
        );
    }

    #[test]
    fn measured_balance_prefers_clause_boundary() {
        let words: Vec<_> = "aa bb, cc dd"
            .split(' ')
            .enumerate()
            .map(|(i, t)| TimedWord::new(t, i as u64 * 800, (i as u64 + 1) * 800))
            .collect();
        let metrics = LineMetrics {
            measure_width: &count,
            max_width_px: 8,
            uppercase: false,
        };
        assert_eq!(
            break_lines(&words, &CaptionConfig::default(), Some(&metrics)).unwrap(),
            ["aa bb,", "cc dd"]
        );
    }

    #[test]
    fn short_merge_respects_measured_line_limit_in_both_directions() {
        let cfg = CaptionConfig {
            max_lines: 1,
            max_chars_per_second: 1000.0,
            ..CaptionConfig::default()
        };
        let groups = vec![
            vec![TimedWord::new("wide", 0, 200)],
            vec![TimedWord::new("wide", 200, 400)],
        ];
        assert_eq!(merge_short(groups.clone(), &cfg, None).unwrap().len(), 1);
        let metrics = LineMetrics {
            measure_width: &count,
            max_width_px: 4,
            uppercase: false,
        };
        assert_eq!(merge_short(groups, &cfg, Some(&metrics)).unwrap().len(), 2);
    }

    #[test]
    fn normalization_keeps_source_spans_when_clamping_output() {
        let mut words = vec![
            TimedWord::new(" one\t word ", 0, 500),
            TimedWord::new("two", 400, 800),
        ];
        words[0].source_ms = Some((1000, 1500));
        words[1].source_ms = Some((1400, 1800));
        let normalized = normalize_words(&words);
        assert_eq!(normalized[0].text, "one word");
        assert_eq!(normalized[0].end_ms, 400);
        assert_eq!(normalized[0].source_ms, Some((1000, 1500)));
        assert_eq!(normalized[1].source_ms, Some((1400, 1800)));
    }
}
