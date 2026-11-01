//! The versioned scoring profile. Every weight, threshold and cue list lives in
//! `data/shorts-profile.json` (embedded as the default); a project may persist its own
//! copy and load it with [`ShortsProfile::from_json`].

use serde::{Deserialize, Serialize};

use crate::error::ShortsError;

pub const PROFILE_SCHEMA_VERSION: u32 = 1;
const DEFAULT_JSON: &str = include_str!("../data/shorts-profile.json");

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlatformWindow {
    pub id: String,
    pub label: String,
    pub aspect: String,
    pub min_duration_ms: i64,
    pub ideal_min_ms: i64,
    pub ideal_max_ms: i64,
    pub max_duration_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScoreWeights {
    pub hook: f64,
    pub payoff: f64,
    pub proof: f64,
    pub value: f64,
    pub duration_fit: f64,
    pub visual_support: f64,
    pub platform_fit: f64,
    pub brand_fit: f64,
}

/// Per-signal contributions inside stages 3a-3d (the old scorer's inline constants).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SignalWeights {
    pub hook_question: f64,
    pub hook_cue: f64,
    pub hook_labeled: f64,
    pub payoff_terminal: f64,
    pub payoff_cue: f64,
    pub payoff_no_trailing_conjunction: f64,
    pub proof_digit: f64,
    pub proof_cue: f64,
    pub value_unique_ratio: f64,
    pub value_confidence: f64,
    pub value_pacing: f64,
    pub value_filler: f64,
}

/// Ideal conversational speaking-rate band (words per second); the pacing score
/// decays linearly outside it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pacing {
    pub ideal_min_wps: f64,
    pub ideal_max_wps: f64,
    pub decay_span_wps: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WindowRules {
    /// A pause at least this long ends a sentence when there is no punctuation.
    pub pause_boundary_ms: i64,
    /// A window never spans a pause longer than this.
    pub max_internal_pause_ms: i64,
    /// Silence added around the word-aligned range, never past halfway to the
    /// neighbouring word.
    pub pad_ms: i64,
    /// Hard cap on generated windows (guards pathological transcripts).
    pub max_windows: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DedupeRules {
    /// Token-Jaccard similarity at/above which two proposals are near-duplicates.
    pub diversity_similarity_threshold: f64,
    /// A proposal overlapping a better one by more than this fraction of the
    /// shorter duration is suppressed.
    pub max_overlap_fraction: f64,
}

/// LLM moment-ranking limits and how its score blends with the heuristic composite.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LlmRules {
    /// Sentences sent per request; longer transcripts go in consecutive chunks.
    pub max_sentences_per_request: usize,
    /// Moments requested per chunk.
    pub max_moments_per_request: usize,
    /// Weight of the model score in the blended composite (0 = heuristic only).
    pub blend: f64,
    pub max_tokens: u32,
    /// Estimated prompt tokens allowed per LLM request (fixed prompt plus sentence
    /// lines). Requests are packed under it; see [`crate::budget`].
    #[serde(default = "default_prompt_token_budget")]
    pub prompt_token_budget: u32,
}

fn default_prompt_token_budget() -> u32 {
    12_000
}

/// How measured audio (loudness, laughter) moves a window's composite. Bounded by
/// `max_adjustment`; see [`crate::audio`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AudioRules {
    /// Largest composite change the audio can make. 0 disables the hook.
    pub max_adjustment: f64,
    /// Weight of measured laughter coverage (0..=1) in the audio term.
    pub laughter_weight: f64,
    /// Loudness, in LUFS, that a window is judged against.
    pub loudness_target_lufs: f64,
    /// Distance from the target that carries no penalty, in LU.
    pub loudness_tolerance_lu: f64,
}

impl Default for AudioRules {
    fn default() -> Self {
        Self {
            max_adjustment: 0.05,
            laughter_weight: 0.5,
            loudness_target_lufs: -16.0,
            loudness_tolerance_lu: 6.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TitleRules {
    pub max_chars: usize,
}

/// Stage 5 (visual support) sub-signal weights and thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VisualSupportProfile {
    pub confidence_weight: f64,
    pub coverage_weight: f64,
    pub variety_weight: f64,
    pub motion_weight: f64,
    pub min_points_for_high_confidence: usize,
    pub variety_target_shot_interval_ms: i64,
    pub motion_target_avg_step: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShortsProfile {
    pub schema_version: u32,
    /// Recorded on every proposal set so a reprocessed set is never mistaken for
    /// one built under different weights.
    pub profile_version: u32,
    pub primary_platform: String,
    pub platforms: Vec<PlatformWindow>,
    pub weights: ScoreWeights,
    pub signal_weights: SignalWeights,
    pub pacing: Pacing,
    pub windows: WindowRules,
    pub dedupe: DedupeRules,
    pub title: TitleRules,
    pub llm: LlmRules,
    /// Measured-audio hook; absent from older profile files, which get the defaults.
    #[serde(default)]
    pub audio: AudioRules,
    /// Used when the recogniser reports no per-word confidence.
    pub assumed_word_confidence: f64,
    pub max_reported_rejections: usize,
    pub fillers: Vec<String>,
    pub dangling_openers: Vec<String>,
    pub hook_cues: Vec<String>,
    pub payoff_cues: Vec<String>,
    pub proof_cues: Vec<String>,
    /// Per-project; empty means brand fit cannot be verified (scored neutral, Low).
    pub brand_banned_terms: Vec<String>,
    pub visual_support: VisualSupportProfile,
}

impl Default for ShortsProfile {
    fn default() -> Self {
        Self::from_json(DEFAULT_JSON.as_bytes()).expect("embedded shorts profile is valid")
    }
}

impl ShortsProfile {
    pub fn from_json(bytes: &[u8]) -> Result<Self, ShortsError> {
        let p: ShortsProfile =
            serde_json::from_slice(bytes).map_err(|e| ShortsError::Profile(e.to_string()))?;
        p.validate()?;
        Ok(p)
    }

    pub fn to_json(&self) -> Vec<u8> {
        let mut v = serde_json::to_vec_pretty(self).expect("profile serializes");
        v.push(b'\n');
        v
    }

    pub fn platform(&self, id: &str) -> Result<&PlatformWindow, ShortsError> {
        self.platforms
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| ShortsError::UnknownPlatform(id.to_string()))
    }

    pub fn primary(&self) -> &PlatformWindow {
        self.platform(&self.primary_platform)
            .expect("validated primary platform")
    }

    pub fn validate(&self) -> Result<(), ShortsError> {
        let bad = |m: &str| Err(ShortsError::Profile(m.to_string()));
        if self.schema_version != PROFILE_SCHEMA_VERSION {
            return bad("unsupported schema_version");
        }
        if self.platforms.is_empty() || self.platform(&self.primary_platform).is_err() {
            return bad("primary_platform is not in platforms");
        }
        for p in &self.platforms {
            if !(0 < p.min_duration_ms
                && p.min_duration_ms <= p.ideal_min_ms
                && p.ideal_min_ms <= p.ideal_max_ms
                && p.ideal_max_ms <= p.max_duration_ms)
            {
                return bad(
                    "platform windows must satisfy 0 < min <= ideal_min <= ideal_max <= max",
                );
            }
        }
        let w = &self.weights;
        let all = [
            w.hook,
            w.payoff,
            w.proof,
            w.value,
            w.duration_fit,
            w.visual_support,
            w.platform_fit,
            w.brand_fit,
        ];
        if all.iter().any(|v| !v.is_finite() || *v < 0.0) || all.iter().sum::<f64>() <= 0.0 {
            return bad("weights must be finite, non-negative and not all zero");
        }
        if !(0.0..=1.0).contains(&self.dedupe.max_overlap_fraction)
            || !(0.0..=1.0).contains(&self.dedupe.diversity_similarity_threshold)
        {
            return bad("dedupe thresholds must be within 0..=1");
        }
        if self.windows.pad_ms < 0 || self.windows.pause_boundary_ms <= 0 {
            return bad("window rules out of range");
        }
        if !(0.0..=1.0).contains(&self.llm.blend)
            || self.llm.max_sentences_per_request < 2
            || self.llm.max_moments_per_request == 0
            || self.llm.max_tokens == 0
            || self.llm.prompt_token_budget == 0
        {
            return bad("llm rules out of range");
        }
        let a = &self.audio;
        if !(0.0..=1.0).contains(&a.max_adjustment)
            || !(0.0..=1.0).contains(&a.laughter_weight)
            || !a.loudness_target_lufs.is_finite()
            || a.loudness_tolerance_lu.is_nan()
            || a.loudness_tolerance_lu <= 0.0
        {
            return bad("audio rules out of range");
        }
        if self.title.max_chars < 8 {
            return bad("title.max_chars too small");
        }
        Ok(())
    }
}
