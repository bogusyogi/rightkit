//! Timing confidence and boundary error.
//!
//! Timing confidence is intrinsic: it estimates how trustworthy an ASR word's
//! boundaries are, from how they were derived and the lattice they sit on. It
//! does not need annotations. Boundary error needs annotated truth and is used
//! by fixtures and qualification gates.
//!
//! # Formula
//!
//! Per ASR word, the boundary trust `b` is:
//!
//! - Model-timed, lattice declared (`AlignOptions::lattice_ms = Some(L)`):
//!   `b = clamp(tolerance_ms / L, 0, 1)`. A boundary reported on an `L`-ms
//!   lattice is known only to a cell `L` wide. If the true boundary is equally
//!   likely anywhere in that cell, it lies within `tolerance_ms` of the report
//!   with probability `tolerance_ms / L`. Parakeet (`L = 80`) with the 50 ms
//!   tolerance gives `0.625`. This is the bound that the lattice imposes. It
//!   holds whatever the engine's word confidence is, because word confidence
//!   says nothing about where a boundary lies.
//! - Model-timed, no lattice (`None`): `b = 1.0`. The caller asserts the
//!   boundaries are continuous and not quantised.
//! - Model-timed end clamped to the media boundary (`EndSource::SourceBoundary`):
//!   the same bound as model-timed, because clamping does not change how
//!   precisely the model placed the end.
//! - VAD-bounded `0.8`, next-token-start `0.6`, interpolated `0.3`.
//! - Words shorter than [`MIN_WORD_MS`] are scaled by `duration / MIN_WORD_MS`.
//!
//! A token's `timing_confidence` is the minimum `b` over the ASR words it
//! matched, multiplied by the token's lexical confidence.

use serde::{Deserialize, Serialize};

use rightkit_transcript::{EndSource, Word};

use crate::{AlignOptions, Alignment};

/// Parakeet emits timestamps on an 80 ms lattice.
pub const LATTICE_MS: u64 = 80;
/// Parakeet's minimum word duration. Shorter reported words are less trustworthy.
pub const MIN_WORD_MS: u64 = 80;
/// Acceptance tolerance for boundary error, per R8 (50 ms).
pub const TIMING_TOLERANCE_MS: u64 = 50;

/// Boundary trust for a model-timed word on a lattice with frame step `lattice_ms`:
/// `clamp(tolerance_ms / lattice_ms, 0, 1)`. See the module docs for the derivation.
pub fn lattice_boundary_confidence(tolerance_ms: u64, lattice_ms: u32) -> f32 {
    (tolerance_ms as f32 / lattice_ms as f32).clamp(0.0, 1.0)
}

/// Trust in one ASR word's boundaries, in 0..=1. See the module docs.
pub fn word_timing_confidence(word: &Word, options: &AlignOptions) -> f32 {
    let base = match word.end_source {
        // A SourceBoundary end is the model's own end, clamped to the media
        // duration (encoder padding). Clamping moves it onto the boundary but
        // does not change how precisely the model placed it, so it gets the
        // model-timed bound.
        EndSource::Model | EndSource::SourceBoundary => match options.lattice_ms {
            Some(lattice) => lattice_boundary_confidence(options.tolerance_ms, lattice),
            None => 1.0,
        },
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

/// An annotated truth boundary for one script token, keyed by its `word_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Boundary {
    pub word_id: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundaryError {
    pub word_id: String,
    /// Accepted start minus annotated start. `None` when the anchor was rejected.
    pub start_error_ms: Option<i64>,
    pub end_error_ms: Option<i64>,
    /// True only when the anchor was accepted and both edges are within the
    /// alignment's `timing_tolerance_ms`.
    pub within_tolerance: bool,
}

/// Compares accepted anchors with annotated boundaries, using the tolerance the
/// alignment was produced with. A rejected anchor has no error, so it can
/// never count as within tolerance.
pub fn boundary_errors(alignment: &Alignment, truth: &[Boundary]) -> Vec<BoundaryError> {
    let tolerance = alignment.timing_tolerance_ms;
    truth
        .iter()
        .map(|boundary| {
            let accepted = alignment.accepted_span(&boundary.word_id);
            let start_error_ms = accepted.map(|s| s.start_ms as i64 - boundary.start_ms as i64);
            let end_error_ms = accepted.map(|s| s.end_ms as i64 - boundary.end_ms as i64);
            let within_tolerance = match (start_error_ms, end_error_ms) {
                (Some(start), Some(end)) => {
                    start.unsigned_abs() <= tolerance && end.unsigned_abs() <= tolerance
                }
                _ => false,
            };
            BoundaryError {
                word_id: boundary.word_id.clone(),
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

    fn lattice() -> AlignOptions {
        AlignOptions::default()
    }

    #[test]
    fn lattice_timed_words_are_bounded_by_tolerance_over_lattice() {
        let confidence = word_timing_confidence(&word(0, 160, EndSource::Model), &lattice());
        assert!((confidence - 0.625).abs() < 1e-6, "{confidence}");
    }

    #[test]
    fn continuous_model_timed_words_without_a_lattice_are_fully_trusted() {
        let options = AlignOptions {
            lattice_ms: None,
            ..AlignOptions::default()
        };
        assert_eq!(
            word_timing_confidence(&word(0, 160, EndSource::Model), &options),
            1.0
        );
    }

    #[test]
    fn the_lattice_bound_clamps_to_one_when_tolerance_covers_a_cell() {
        assert_eq!(lattice_boundary_confidence(100, 80), 1.0);
        assert!((lattice_boundary_confidence(20, 80) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn a_source_boundary_end_gets_the_model_bound() {
        let clamped = word(0, 160, EndSource::SourceBoundary);
        assert_eq!(
            word_timing_confidence(&clamped, &lattice()),
            word_timing_confidence(&word(0, 160, EndSource::Model), &lattice())
        );
    }

    #[test]
    fn weaker_sources_and_short_words_reduce_trust() {
        assert!(
            (word_timing_confidence(&word(0, 160, EndSource::Interpolated), &lattice()) - 0.3)
                .abs()
                < 1e-6
        );
        assert!(
            (word_timing_confidence(&word(0, 40, EndSource::Model), &lattice()) - 0.3125).abs()
                < 1e-6
        );
    }
}
