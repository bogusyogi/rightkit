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
    /// Original source span (exclusive end), retained when output timing shifts.
    /// Consumers can derive stable cue ids from this span. Normalization may
    /// clamp output overlaps but leaves source spans unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ms: Option<(u64, u64)>,
}

impl TimedWord {
    pub fn new(text: impl Into<String>, start_ms: u64, end_ms: u64) -> Self {
        Self {
            text: text.into(),
            start_ms,
            end_ms,
            source_ms: None,
        }
    }

    /// From engine seconds. Negative or non-finite times clamp to 0; rounding
    /// is to the nearest millisecond (the same rule as [`words_from_seconds`]).
    pub fn from_seconds(text: impl Into<String>, start_s: f32, end_s: f32) -> Self {
        Self::new(text, seconds_to_ms(start_s), seconds_to_ms(end_s))
    }

    /// Start in seconds.
    pub fn start_secs(&self) -> f64 {
        self.start_ms as f64 / 1000.0
    }

    /// End in seconds.
    pub fn end_secs(&self) -> f64 {
        self.end_ms as f64 / 1000.0
    }
}

impl<T: Into<String>> From<(T, u64, u64)> for TimedWord {
    /// `(text, start_ms, end_ms)`.
    fn from((text, start_ms, end_ms): (T, u64, u64)) -> Self {
        Self::new(text, start_ms, end_ms)
    }
}

fn seconds_to_ms(s: f32) -> u64 {
    if s.is_finite() && s > 0.0 {
        (s as f64 * 1000.0).round() as u64
    } else {
        0
    }
}

/// Engine adapter: `(text, start_seconds, end_seconds)` -> milliseconds.
/// Negative or non-finite times are clamped to 0; rounding is to nearest ms.
/// This is the shape of `rightkit_asr::TimedWord`/`TimedToken`.
pub fn words_from_seconds<'a>(
    items: impl IntoIterator<Item = (&'a str, f32, f32)>,
) -> Vec<TimedWord> {
    items
        .into_iter()
        .map(|(t, s, e)| TimedWord::from_seconds(t, s, e))
        .collect()
}

/// Maps a source [`Transcript`] onto an output timeline (cut/edit).
///
/// `map` converts a source time (ms) to output time (ms), returning `None`
/// when that instant lies in a removed range. A word is kept when its start
/// and end both map and the mapped duration matches the source duration
/// (within [`MAP_DURATION_TOLERANCE_MS`]); otherwise it falls in (or
/// straddles) a removed range and is dropped. Each retained word records its
/// original source span in [`TimedWord::source_ms`]. An unmapped exclusive end
/// falls back to `map(end - 1) + 1` so words ending at cut boundaries survive.
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
        let mut word = TimedWord::new(text, start, end);
        word.source_ms = Some((w.start_ms, w.end_ms));
        out.push(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_span_is_optional_on_wire_and_defaults_for_all_adapters() {
        let legacy = r#"{"text":"hi","start_ms":10,"end_ms":20}"#;
        let word: TimedWord = serde_json::from_str(legacy).unwrap();
        assert_eq!(word, TimedWord::new("hi", 10, 20));
        assert_eq!(
            serde_json::to_value(&word).unwrap(),
            serde_json::from_str::<serde_json::Value>(legacy).unwrap()
        );
        assert_eq!(TimedWord::from_seconds("hi", 0.01, 0.02).source_ms, None);
        assert_eq!(TimedWord::from(("hi", 10, 20)).source_ms, None);
        assert_eq!(words_from_seconds([("hi", 0.01, 0.02)])[0].source_ms, None);
        let mut mapped = word;
        mapped.source_ms = Some((100, 110));
        let value = serde_json::to_value(&mapped).unwrap();
        assert_eq!(value["source_ms"], serde_json::json!([100, 110]));
        assert_eq!(serde_json::from_value::<TimedWord>(value).unwrap(), mapped);
    }

    #[test]
    fn mapping_records_source_and_enforces_duration_tolerance() {
        let transcript = Transcript::from_words(
            &[TimedWord::new(" shifted ", 100, 200)],
            "en",
            Some(1000),
            crate::ModelRef::default(),
            crate::EndSource::Model,
        );
        let mapped = words_from_transcript(&transcript, |t| Some(t + 500));
        assert_eq!(mapped[0].text, "shifted");
        assert_eq!((mapped[0].start_ms, mapped[0].end_ms), (600, 700));
        assert_eq!(mapped[0].source_ms, Some((100, 200)));
        let at_limit =
            words_from_transcript(&transcript, |t| Some(if t == 200 { t - 20 } else { t }));
        assert_eq!(at_limit.len(), 1);
        let beyond_limit =
            words_from_transcript(&transcript, |t| Some(if t == 200 { t - 21 } else { t }));
        assert!(beyond_limit.is_empty());
    }
}
