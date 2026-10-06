//! Loudness / true-peak / clipping gate. Ported from
//! `video-project::audio_profile::AudioProfile::evaluate_loudness_gate`.

use serde::{Deserialize, Serialize};

use crate::measure::LevelMeasurement;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AudioGate {
    pub target_integrated_lufs: f64,
    pub loudness_tolerance_lu: f64,
    pub target_true_peak_dbtp: f64,
    pub true_peak_tolerance_db: f64,
}

impl Default for AudioGate {
    fn default() -> Self {
        Self {
            target_integrated_lufs: -14.0,
            loudness_tolerance_lu: 1.0,
            target_true_peak_dbtp: -1.0,
            true_peak_tolerance_db: 0.5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoudnessGateResult {
    pub passed: bool,
    pub integrated_lufs: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    pub clipped_samples: u64,
    pub failures: Vec<String>,
}

impl AudioGate {
    pub fn evaluate(&self, m: &LevelMeasurement) -> LoudnessGateResult {
        let mut failures = Vec::new();
        match m.integrated_lufs {
            Some(l) if (l - self.target_integrated_lufs).abs() <= self.loudness_tolerance_lu => {}
            Some(l) => failures.push(format!(
                "integrated loudness {l:.2} LUFS is outside {:.2} +/- {:.2} LU",
                self.target_integrated_lufs, self.loudness_tolerance_lu
            )),
            None => failures.push("integrated loudness could not be measured".into()),
        }
        match m.true_peak_dbtp {
            Some(tp) if tp <= self.target_true_peak_dbtp + self.true_peak_tolerance_db => {}
            Some(tp) => failures.push(format!(
                "true peak {tp:.2} dBTP exceeds the {:.2} dBTP ceiling (+{:.2} dB tolerance)",
                self.target_true_peak_dbtp, self.true_peak_tolerance_db
            )),
            None => failures.push("true peak could not be measured".into()),
        }
        if m.clipped_samples > 0 {
            failures.push(format!(
                "{} clipped samples detected; none are permitted",
                m.clipped_samples
            ));
        }
        LoudnessGateResult {
            passed: failures.is_empty(),
            integrated_lufs: m.integrated_lufs,
            true_peak_dbtp: m.true_peak_dbtp,
            clipped_samples: m.clipped_samples,
            failures,
        }
    }
}
