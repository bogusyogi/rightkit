//! Measures an [`EditingStyle`] from a finished reference edit (#93).
//!
//! Inputs are the transcript of one source and the kept source ranges (ms) of
//! the reference edit. A word is kept when it lies wholly inside a kept range.
//! Measured values carry [`Provenance::Measured`] with the transcript's source
//! BLAKE3 hash. Values this measurement cannot observe (release and attack pads,
//! min cut, filler isolation) keep their authored preset value.
//!
//! Percentiles use nearest rank over the sorted samples; the median is the
//! nearest-rank 50th percentile.

use rightkit_transcript::Transcript;
use serde::Serialize;

use crate::cuts::config::{CutConfig, Preset};
use crate::cuts::style::{EditingStyle, FillerPolicy, GapMs, Pace, Provenance, StyleValue};
use crate::cuts::text::{ends_sentence, find_fillers, normalize, W};

pub const METHOD_MAX_PAUSE: &str = "reference-kept-pause-p90-v1";
pub const METHOD_SENTENCE_PAUSE: &str = "reference-kept-sentence-pause-p90-v1";
pub const METHOD_LEAD_IN: &str = "reference-kept-lead-in-median-v1";
pub const METHOD_TAIL_OUT: &str = "reference-kept-tail-out-median-v1";
pub const METHOD_FILLERS: &str = "reference-kept-filler-policy-v1";
pub const METHOD_PACE: &str = "reference-kept-words-per-minute-v1";

/// Why a reference edit cannot be measured.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MeasureError {
    #[error("the reference edit keeps no ranges")]
    NoKeptRanges,
    #[error("the reference edit keeps no transcript words")]
    NoKeptWords,
    #[error("invalid transcript: {0}")]
    InvalidTranscript(String),
}

/// The raw distributions behind a measured style. Fields are `None` where the
/// reference edit gave no samples.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Measurement {
    pub source_hash: String,
    pub kept_words: usize,
    pub kept_ranges: usize,
    pub kept_duration_ms: i64,
    /// Pauses between consecutive kept words that sit inside one kept range.
    pub kept_gap_count: usize,
    pub median_gap_ms: Option<i64>,
    pub p90_gap_ms: Option<i64>,
    /// Kept pauses that follow a sentence-final word.
    pub sentence_gap_count: usize,
    pub p90_sentence_gap_ms: Option<i64>,
    pub median_lead_in_ms: Option<i64>,
    pub median_lead_out_ms: Option<i64>,
    /// Filler words in the whole transcript, kept or cut.
    pub filler_hits_total: usize,
    /// Filler words kept in the reference edit.
    pub filler_hits_kept: usize,
    pub filler_rate_per_100_kept_words: f64,
    pub wpm: Option<u32>,
}

/// Nearest-rank percentile of `sorted` (ascending). `None` when empty.
fn nearest_rank(sorted: &[i64], pct: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let n = sorted.len();
    let rank = ((pct * n as f64).ceil() as usize).clamp(1, n);
    Some(sorted[rank - 1])
}

fn sorted(mut v: Vec<i64>) -> Vec<i64> {
    v.sort_unstable();
    v
}

pub(crate) fn measure(
    transcript: &Transcript,
    kept_ms: &[(i64, i64)],
    base: Preset,
) -> Result<(EditingStyle, Measurement), MeasureError> {
    transcript
        .validate()
        .map_err(|e| MeasureError::InvalidTranscript(e.to_string()))?;

    let mut ranges: Vec<(i64, i64)> = kept_ms.iter().copied().filter(|(s, e)| e > s).collect();
    ranges.sort_unstable();
    if ranges.is_empty() {
        return Err(MeasureError::NoKeptRanges);
    }

    let cfg = CutConfig::for_preset(base);
    let words = &transcript.words;

    // Range index for each word wholly inside a kept range; `None` means cut.
    let owner: Vec<Option<usize>> = words
        .iter()
        .map(|w| {
            ranges
                .iter()
                .position(|&(s, e)| (w.start_ms as i64) >= s && (w.end_ms as i64) <= e)
        })
        .collect();
    let kept_words = owner.iter().filter(|o| o.is_some()).count();
    if kept_words == 0 {
        return Err(MeasureError::NoKeptWords);
    }

    // Pauses between consecutive kept words inside the same kept range.
    let mut gaps = Vec::new();
    let mut sentence_gaps = Vec::new();
    for i in 0..words.len().saturating_sub(1) {
        if let (Some(a), Some(b)) = (owner[i], owner[i + 1]) {
            if a == b {
                let gap = ((words[i + 1].start_ms as i64) - (words[i].end_ms as i64)).max(0);
                gaps.push(gap);
                if ends_sentence(&words[i].text) {
                    sentence_gaps.push(gap);
                }
            }
        }
    }

    // Lead-in and lead-out: silence between each kept range edge and the first
    // or last kept word inside it.
    let mut bounds: Vec<Option<(i64, i64)>> = vec![None; ranges.len()];
    for (w, o) in words.iter().zip(&owner) {
        if let Some(r) = *o {
            bounds[r] = Some(match bounds[r] {
                None => ((w.start_ms as i64), (w.end_ms as i64)),
                Some((s, e)) => (s.min(w.start_ms as i64), e.max(w.end_ms as i64)),
            });
        }
    }
    let mut lead_in = Vec::new();
    let mut lead_out = Vec::new();
    for (r, b) in bounds.iter().enumerate() {
        if let Some((first, last)) = *b {
            lead_in.push((first - ranges[r].0).max(0));
            lead_out.push((ranges[r].1 - last).max(0));
        }
    }

    // Fillers, counted against the cut mask of the reference edit.
    let norm_words: Vec<W> = words
        .iter()
        .map(|w| W {
            start: (w.start_ms as i64),
            end: (w.end_ms as i64),
            text: w.text.clone(),
            norm: normalize(&w.text),
        })
        .collect();
    let none_removed = vec![false; words.len()];
    let cut_mask: Vec<bool> = owner.iter().map(|o| o.is_none()).collect();
    let filler_total = find_fillers(&norm_words, &none_removed, &cfg).len();
    let filler_kept = find_fillers(&norm_words, &cut_mask, &cfg).len();

    let kept_duration_ms: i64 = ranges.iter().map(|(s, e)| e - s).sum();
    let wpm = if kept_duration_ms > 0 {
        let num = kept_words as i64 * 60_000;
        Some(((num + kept_duration_ms / 2) / kept_duration_ms) as u32)
    } else {
        None
    };

    let gaps_sorted = sorted(gaps.clone());
    let sentence_sorted = sorted(sentence_gaps.clone());
    let lead_in_sorted = sorted(lead_in.clone());
    let lead_out_sorted = sorted(lead_out.clone());

    let measurement = Measurement {
        source_hash: transcript.source_blake3.clone(),
        kept_words,
        kept_ranges: ranges.len(),
        kept_duration_ms,
        kept_gap_count: gaps.len(),
        median_gap_ms: nearest_rank(&gaps_sorted, 0.5),
        p90_gap_ms: nearest_rank(&gaps_sorted, 0.9),
        sentence_gap_count: sentence_gaps.len(),
        p90_sentence_gap_ms: nearest_rank(&sentence_sorted, 0.9),
        median_lead_in_ms: nearest_rank(&lead_in_sorted, 0.5),
        median_lead_out_ms: nearest_rank(&lead_out_sorted, 0.5),
        filler_hits_total: filler_total,
        filler_hits_kept: filler_kept,
        filler_rate_per_100_kept_words: filler_kept as f64 * 100.0 / kept_words as f64,
        wpm,
    };

    let mut style = EditingStyle::from_config(&cfg);
    let measured = |method: &str| Provenance::Measured {
        method: method.to_string(),
        source_hash: transcript.source_blake3.clone(),
    };
    if let Some(v) = measurement.p90_gap_ms {
        style.max_pause = StyleValue {
            value: GapMs(v),
            provenance: measured(METHOD_MAX_PAUSE),
        };
    }
    if let Some(v) = measurement.p90_sentence_gap_ms {
        style.sentence_pause = StyleValue {
            value: GapMs(v),
            provenance: measured(METHOD_SENTENCE_PAUSE),
        };
    }
    if let Some(v) = measurement.median_lead_in_ms {
        style.lead_in = StyleValue {
            value: GapMs(v),
            provenance: measured(METHOD_LEAD_IN),
        };
    }
    if let Some(v) = measurement.median_lead_out_ms {
        style.tail_out = StyleValue {
            value: GapMs(v),
            provenance: measured(METHOD_TAIL_OUT),
        };
    }
    if filler_total > 0 {
        style.fillers = StyleValue {
            value: if filler_kept > 0 {
                FillerPolicy::Keep
            } else {
                FillerPolicy::Remove
            },
            provenance: measured(METHOD_FILLERS),
        };
    }
    if let Some(v) = wpm {
        style.pace = Some(StyleValue {
            value: Pace(v),
            provenance: measured(METHOD_PACE),
        });
    }
    Ok((style, measurement))
}
