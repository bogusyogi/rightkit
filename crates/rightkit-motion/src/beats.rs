//! Bracketed-script beat sync.
//!
//! A script marks animation cues with `[spans]`: `"We ship [three] tools and [one] rule"`.
//! `\[`, `\]` and `\\` escape the literal characters. Spans do not nest and may not be empty.
//! Spans map positionally to the caller's animations: span 0 is the first cue, and so on.
//!
//! [`snap`] aligns the script's words one to one with timed words from `rightkit-transcript`
//! and turns each span into a [`Beat`] carrying the covered words' start and end. Comparison
//! is exact after normalisation (case-folded, edge punctuation stripped, tokens that are pure
//! punctuation ignored). A word that does not match is a typed error, never a guess: fuzzy ASR
//! matching belongs to `rightkit-align`.
//!
//! A span that touches part of a word (`"[foo]bar"`) covers that whole word.

use rightkit_transcript::TimedWord;
use serde::{Deserialize, Serialize};

/// One `[span]` of a parsed script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptSpan {
    /// Zero-based position among the spans; maps to the caller's animation at that index.
    pub index: usize,
    /// The span text without brackets.
    pub text: String,
    /// First covered word (index into [`BracketedScript::words`]).
    pub word_start: usize,
    /// One past the last covered word.
    pub word_end: usize,
}

/// A parsed script: plain text with brackets removed, its words, and the spans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketedScript {
    /// Script text with brackets removed and escapes resolved.
    pub text: String,
    /// Whitespace-separated words of `text`, as written (not normalised).
    pub words: Vec<String>,
    pub spans: Vec<ScriptSpan>,
}

/// Script parse and snap errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BeatError {
    #[error("unclosed '[' at byte {0}")]
    UnclosedSpan(usize),
    #[error("unmatched ']' at byte {0}")]
    UnmatchedClose(usize),
    #[error("nested '[' at byte {0}")]
    NestedSpan(usize),
    #[error("empty span at byte {0}")]
    EmptySpan(usize),
    #[error("script has no spans")]
    NoSpans,
    #[error("script has {script} words but the timing has {timed}")]
    WordCountMismatch { script: usize, timed: usize },
    #[error("word {index}: script has {expected:?}, timing has {found:?}")]
    WordMismatch {
        index: usize,
        expected: String,
        found: String,
    },
    #[error("timed word {index} ends before it starts")]
    BadTiming { index: usize },
}

/// Parse a bracketed script.
pub fn parse_script(script: &str) -> Result<BracketedScript, BeatError> {
    let mut text = String::new();
    // (start byte in `text`, end byte in `text`, source byte of the '[')
    let mut raw_spans: Vec<(usize, usize, usize)> = Vec::new();
    let mut open: Option<(usize, usize)> = None; // (start in text, source byte)

    let mut chars = script.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => match chars.peek() {
                Some(&(_, n)) if n == '[' || n == ']' || n == '\\' => {
                    text.push(n);
                    chars.next();
                }
                _ => text.push('\\'),
            },
            '[' => {
                if open.is_some() {
                    return Err(BeatError::NestedSpan(i));
                }
                open = Some((text.len(), i));
            }
            ']' => match open.take() {
                None => return Err(BeatError::UnmatchedClose(i)),
                Some((start, src)) => {
                    if text[start..].trim().is_empty() {
                        return Err(BeatError::EmptySpan(src));
                    }
                    raw_spans.push((start, text.len(), src));
                }
            },
            _ => text.push(c),
        }
    }
    if let Some((_, src)) = open {
        return Err(BeatError::UnclosedSpan(src));
    }

    // Word byte ranges in `text`.
    let mut word_ranges: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                word_ranges.push((s, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        word_ranges.push((s, text.len()));
    }

    let words: Vec<String> = word_ranges
        .iter()
        .map(|&(s, e)| text[s..e].to_string())
        .collect();
    let spans = raw_spans
        .iter()
        .enumerate()
        .map(|(index, &(s, e, _))| {
            let covered: Vec<usize> = word_ranges
                .iter()
                .enumerate()
                .filter(|(_, &(ws, we))| ws < e && we > s)
                .map(|(wi, _)| wi)
                .collect();
            ScriptSpan {
                index,
                text: text[s..e].trim().to_string(),
                word_start: covered.first().copied().unwrap_or(0),
                word_end: covered.last().map(|l| l + 1).unwrap_or(0),
            }
        })
        .collect();

    Ok(BracketedScript { text, words, spans })
}

/// Snap tuning. Defaults add no padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SnapConfig {
    /// Start each beat this much earlier than its first word (clamped at 0).
    pub lead_ms: u64,
    /// End each beat this much later than its last word.
    pub tail_ms: u64,
}

/// One cue snapped to speech.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Beat {
    /// Span index, equal to the caller's animation index.
    pub span: usize,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Covered timed words, as a half-open range into the slice given to [`snap`].
    pub word_start: usize,
    pub word_end: usize,
}

fn normalise(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Snap each span of `script` to the timed words.
///
/// Both sides are reduced to their normalised, non-empty words and compared position by
/// position. Returned beats are in span order. Word indices refer to the original `words` slice.
pub fn snap(
    script: &BracketedScript,
    words: &[TimedWord],
    config: SnapConfig,
) -> Result<Vec<Beat>, BeatError> {
    if script.spans.is_empty() {
        return Err(BeatError::NoSpans);
    }
    for (index, w) in words.iter().enumerate() {
        if w.end_ms < w.start_ms {
            return Err(BeatError::BadTiming { index });
        }
    }

    // Script word index -> normalised text, skipping pure punctuation.
    let script_norm: Vec<(usize, String)> = script
        .words
        .iter()
        .enumerate()
        .map(|(i, w)| (i, normalise(w)))
        .filter(|(_, n)| !n.is_empty())
        .collect();
    let timed_norm: Vec<(usize, String)> = words
        .iter()
        .enumerate()
        .map(|(i, w)| (i, normalise(&w.text)))
        .filter(|(_, n)| !n.is_empty())
        .collect();

    if script_norm.len() != timed_norm.len() {
        return Err(BeatError::WordCountMismatch {
            script: script_norm.len(),
            timed: timed_norm.len(),
        });
    }
    // script word index -> timed word index
    let mut map = vec![None; script.words.len()];
    for (k, ((si, sn), (ti, tn))) in script_norm.iter().zip(timed_norm.iter()).enumerate() {
        if sn != tn {
            return Err(BeatError::WordMismatch {
                index: k,
                expected: sn.clone(),
                found: tn.clone(),
            });
        }
        map[*si] = Some(*ti);
    }

    let mut beats = Vec::with_capacity(script.spans.len());
    for span in &script.spans {
        let mapped: Vec<usize> = (span.word_start..span.word_end)
            .filter_map(|i| map.get(i).copied().flatten())
            .collect();
        let (Some(&first), Some(&last)) = (mapped.first(), mapped.last()) else {
            // A span made only of punctuation has no spoken word to snap to.
            return Err(BeatError::EmptySpan(span.index));
        };
        beats.push(Beat {
            span: span.index,
            start_ms: words[first].start_ms.saturating_sub(config.lead_ms),
            end_ms: words[last].end_ms.saturating_add(config.tail_ms),
            word_start: first,
            word_end: last + 1,
        });
    }
    Ok(beats)
}
