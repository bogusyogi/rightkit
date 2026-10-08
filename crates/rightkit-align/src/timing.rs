//! Timing confidence and boundary error.
//!
//! Timing confidence is intrinsic: it estimates how trustworthy an ASR word's
//! boundaries are, from how they were derived and how long the word is. It does
//! not need annotations. Boundary error needs annotated truth and is used by
//! fixtures and qualification gates.

use serde::{Deserialize, Serialize};

use rightkit_transcript::{EndSource, Word};

use crate::{Alignment, Outcome};

/// Parakeet emits timestamps on an 80 ms lattice.
pub const LATTICE_MS: u64 = 80;
/// Parakeet's minimum word duration. Shorter reported words are less trustworthy.
pub const MIN_WORD_MS: u64 = 80;
/// Acceptance tolerance for boundary error, per R8 (50 ms).
pub const TIMING_TOLERANCE_MS: u64 = 50;

/// Trust in one ASR word's boundaries, in 0..=1.
///
/// The base comes from how the end was derived: model-timed 1.0, VAD-bounded
/// 0.8, next-token-start 0.6, interpolated 0.3. Words shorter than
/// [`MIN_WORD_MS`] are scaled down by their duration ratio.
pub fn word_timing_confidence(word: &Word) -> f32 {
    let base = match word.end_source {
        EndSource::Model => 1.0,
        EndSource::Vad => 0.8,
        EndSource::NextTokenStart => 0.6,
        EndSource::Interpolated => 0.3,
    };
    let duration = word.end_ms.saturating_sub(word.start_ms);
    let duration_factor = if duration >= MIN_WORD_MS {
        1.0
    } else {
        duration as f32 / MIN_WORD_MS as f32
    };
    base * duration_factor
}

/// An annotated truth boundary for one script token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Boundary {
    pub token_id: usize,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundaryError {
    pub token_id: usize,
    /// Accepted start minus annotated start. `None` when the anchor was rejected.
    pub start_error_ms: Option<i64>,
    pub end_error_ms: Option<i64>,
    /// True only when the anchor was accepted and both edges are within tolerance.
    pub within_tolerance: bool,
}

/// Compares accepted anchors with annotated boundaries. A rejected anchor has no
/// error, so it can never count as within tolerance.
pub fn boundary_errors(alignment: &Alignment, truth: &[Boundary]) -> Vec<BoundaryError> {
    truth
        .iter()
        .map(|boundary| {
            let accepted =
                alignment
                    .anchor(boundary.token_id)
                    .and_then(|anchor| match anchor.outcome {
                        Outcome::Accepted { span } => Some(span),
                        Outcome::Rejected { .. } => None,
                    });
            let start_error_ms = accepted.map(|s| s.start_ms as i64 - boundary.start_ms as i64);
            let end_error_ms = accepted.map(|s| s.end_ms as i64 - boundary.end_ms as i64);
            let within_tolerance = match (start_error_ms, end_error_ms) {
                (Some(start), Some(end)) => {
                    start.unsigned_abs() <= TIMING_TOLERANCE_MS
                        && end.unsigned_abs() <= TIMING_TOLERANCE_MS
                }
                _ => false,
            };
            BoundaryError {
                token_id: boundary.token_id,
                start_error_ms,
                end_error_ms,
                within_tolerance,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(start_ms: u64, end_ms: u64, end_source: EndSource) -> Word {
        Word {
            text: "x".into(),
            start_ms,
            end_ms,
            confidence: None,
            end_source,
        }
    }

    #[test]
    fn model_timed_words_of_full_duration_are_fully_trusted() {
        assert_eq!(word_timing_confidence(&word(0, 160, EndSource::Model)), 1.0);
    }

    #[test]
    fn weaker_sources_and_short_words_reduce_trust() {
        assert!(
            (word_timing_confidence(&word(0, 160, EndSource::Interpolated)) - 0.3).abs() < 1e-6
        );
        assert!((word_timing_confidence(&word(0, 40, EndSource::Model)) - 0.5).abs() < 1e-6);
    }
}
