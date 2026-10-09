//! Word timing input: [`TimedWord`] in OUTPUT time, plus engine adapters.

use crate::contract::Transcript;
use serde::{Deserialize, Serialize};

/// A mapped word may differ from its source duration by at most this much; a
/// larger difference means a removed range cut through the word, so it is dropped.
pub const MAP_DURATION_TOLERANCE_MS: u64 = 20;

/// One spoken word (`end_ms` exclusive).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimedWord {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

impl TimedWord {
    pub fn new(text: impl Into<String>, start_ms: u64, end_ms: u64) -> Self {
        Self {
            text: text.into(),
            start_ms,
            end_ms,
        }
    }
}

/// Engine adapter: `(text, start_seconds, end_seconds)` -> milliseconds.
/// Negative or non-finite times are clamped to 0; rounding is to nearest ms.
/// This is the shape of `rightkit_asr::TimedWord`/`TimedToken`.
pub fn words_from_seconds<'a>(
    items: impl IntoIterator<Item = (&'a str, f32, f32)>,
) -> Vec<TimedWord> {
    let ms = |s: f32| {
        if s.is_finite() && s > 0.0 {
            (s as f64 * 1000.0).round() as u64
        } else {
            0
        }
    };
    items
        .into_iter()
        .map(|(t, s, e)| TimedWord::new(t, ms(s), ms(e)))
        .collect()
}

/// Maps a source [`Transcript`] onto an output timeline (cut/edit).
///
/// `map` converts a source time (ms) to output time (ms), returning `None`
/// when that instant lies in a removed range. A word is kept when its start
/// and end both map and the mapped duration matches the source duration
/// (within [`MAP_DURATION_TOLERANCE_MS`]); otherwise it falls in (or
/// straddles) a removed range and is dropped.
pub fn words_from_transcript<F>(transcript: &Transcript, map: F) -> Vec<TimedWord>
where
    F: Fn(u64) -> Option<u64>,
{
    let mut out = Vec::new();
    for w in &transcript.words {
        let text = w.text.trim();
        if text.is_empty() || w.end_ms <= w.start_ms {
            continue;
        }
        let Some(start) = map(w.start_ms) else {
            continue;
        };
        let Some(end) = map(w.end_ms).or_else(|| map(w.end_ms - 1).map(|e| e + 1)) else {
            continue;
        };
        if end <= start {
            continue;
        }
        if (end - start).abs_diff(w.end_ms - w.start_ms) > MAP_DURATION_TOLERANCE_MS {
            continue;
        }
        out.push(TimedWord::new(text, start, end));
    }
    out
}
