//! Measured audio as a typed ranking input.
//!
//! This crate measures nothing. The caller passes loudness samples (LUFS) and laughter
//! spans that were measured elsewhere, and [`window_audio`] / [`audio_adjustment`] turn
//! them into a bounded, explained change to a window's composite. Laughter is never
//! detected here, and no model runs. With no `AudioFeatures` supplied, ranking is
//! unchanged.

use serde::{Deserialize, Serialize};

use crate::config::AudioRules;
use crate::error::ShortsError;
use crate::score::q6;

/// Integrated or short-term loudness measured over `source_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LoudnessSample {
    pub source_ms: u64,
    pub lufs: f64,
}

/// A laughter span measured upstream, with the detector's confidence in 0..=1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LaughterSpan {
    pub start_ms: u64,
    pub end_ms: u64,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioFeatures {
    pub loudness: Vec<LoudnessSample>,
    pub laughter: Vec<LaughterSpan>,
}

impl AudioFeatures {
    pub fn validate(&self) -> Result<(), ShortsError> {
        for s in &self.loudness {
            if !s.lufs.is_finite() || !(-70.0..=0.0).contains(&s.lufs) {
                return Err(ShortsError::Invalid(format!(
                    "loudness at {}ms is {} LUFS, outside -70..=0",
                    s.source_ms, s.lufs
                )));
            }
        }
        for l in &self.laughter {
            if l.end_ms <= l.start_ms {
                return Err(ShortsError::Invalid(format!(
                    "laughter span {}..{}ms ends before it starts",
                    l.start_ms, l.end_ms
                )));
            }
            if !(0.0..=1.0).contains(&l.confidence) {
                return Err(ShortsError::Invalid(format!(
                    "laughter confidence {} is outside 0..=1",
                    l.confidence
                )));
            }
        }
        Ok(())
    }
}

/// What the measured audio says about one window `[start_ms, end_ms)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WindowAudio {
    pub loudness_samples: usize,
    /// Mean of the samples inside the window; `None` when there are none.
    pub mean_lufs: Option<f64>,
    /// Share of the window covered by laughter, each span weighted by its confidence,
    /// clamped to 0..=1 (overlapping spans cannot count twice).
    pub laughter_score: f64,
}

pub fn window_audio(features: &AudioFeatures, start_ms: u64, end_ms: u64) -> WindowAudio {
    let inside: Vec<f64> = features
        .loudness
        .iter()
        .filter(|s| s.source_ms >= start_ms && s.source_ms < end_ms)
        .map(|s| s.lufs)
        .collect();
    let mean_lufs = if inside.is_empty() {
        None
    } else {
        Some(inside.iter().sum::<f64>() / inside.len() as f64)
    };
    let span = end_ms.saturating_sub(start_ms).max(1) as f64;
    let laughter_ms: f64 = features
        .laughter
        .iter()
        .map(|l| {
            let overlap = l
                .end_ms
                .min(end_ms)
                .saturating_sub(l.start_ms.max(start_ms));
            overlap as f64 * l.confidence
        })
        .sum();
    WindowAudio {
        loudness_samples: inside.len(),
        mean_lufs,
        laughter_score: (laughter_ms / span).clamp(0.0, 1.0),
    }
}

/// A signed change to a window's composite, with the reason in words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioAdjustment {
    pub delta: f64,
    pub rationale: String,
}

/// Laughter adds to the window, and loudness away from the target subtracts from it.
/// The term is `laughter_weight * laughter_score - loudness_penalty`, clamped to -1..=1
/// and scaled by `max_adjustment`, so the largest possible change is `max_adjustment`.
/// The loudness penalty is zero within `loudness_tolerance_lu` of the target and reaches
/// full strength at twice that distance.
pub fn audio_adjustment(window: &WindowAudio, rules: &AudioRules) -> AudioAdjustment {
    if window.loudness_samples == 0 && window.laughter_score == 0.0 {
        return AudioAdjustment {
            delta: 0.0,
            rationale: "no measured audio in this window; no audio adjustment".into(),
        };
    }
    let penalty = match window.mean_lufs {
        Some(mean) => {
            let deviation = (mean - rules.loudness_target_lufs).abs();
            ((deviation - rules.loudness_tolerance_lu) / rules.loudness_tolerance_lu)
                .clamp(0.0, 1.0)
        }
        None => 0.0,
    };
    let term = (rules.laughter_weight * window.laughter_score - penalty).clamp(-1.0, 1.0);
    let delta = q6(rules.max_adjustment * term);
    let loudness = window
        .mean_lufs
        .map_or("loudness unmeasured".to_string(), |m| {
            format!("mean loudness {m:.1} LUFS")
        });
    AudioAdjustment {
        delta,
        rationale: format!(
            "measured audio: laughter {:.2}, {loudness} -> adjustment {delta:+.4}",
            window.laughter_score
        ),
    }
}
