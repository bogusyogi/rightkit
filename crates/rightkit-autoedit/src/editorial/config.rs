//! Editorial configuration. Every threshold is data; nothing is hidden in code.

use crate::cuts::{CutConfig, Preset};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisfluencyConfig {
    pub repetitions: bool,
    pub stutters: bool,
    pub self_corrections: bool,
    pub false_starts: bool,
    /// Longest n-gram examined for immediate repetition ("I want to I want to").
    pub max_ngram: usize,
    /// Longest pause (ms) between repeated copies.
    pub repetition_max_gap_ms: i64,
    /// Words that are legitimately doubled for emphasis; never flagged.
    pub intentional_repeats: Vec<String>,
    /// Longest word fragment (letters) treated as a stutter.
    pub stutter_max_fragment: usize,
    /// Longest abandoned clause (words) a correction/false start may remove.
    pub correction_max_words: usize,
    /// LCS-Dice similarity of abandoned clause vs restart for a correction.
    pub correction_min_similarity: f32,
    /// A pause (ms) that, mid-sentence, signals the speaker broke off.
    pub interrupt_pause_ms: i64,
    /// Spoken correction cues ("I mean", "sorry"): removed together with the
    /// abandoned clause.
    pub correction_markers: Vec<String>,
    /// Fewest words a replacement clause needs before a non-repeating
    /// abandoned clause is removed (a "complete replacement").
    pub false_start_min_replacement: usize,
    /// Abandoned clauses up to this many words are removed automatically;
    /// longer ones are suggestions.
    pub false_start_auto_words: usize,
}

impl Default for DisfluencyConfig {
    fn default() -> Self {
        Self {
            repetitions: true,
            stutters: true,
            self_corrections: true,
            false_starts: true,
            max_ngram: 4,
            repetition_max_gap_ms: 600,
            intentional_repeats: [
                "very", "really", "no", "so", "bye", "ha", "far", "many", "more",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            stutter_max_fragment: 3,
            correction_max_words: 12,
            correction_min_similarity: 0.5,
            interrupt_pause_ms: 250,
            correction_markers: [
                "i mean",
                "sorry",
                "or rather",
                "no wait",
                "wait no",
                "let me rephrase",
                "what i meant",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            false_start_min_replacement: 3,
            false_start_auto_words: 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeadAirConfig {
    pub enabled: bool,
    /// Shortest breath (ms) worth flagging; shorter ones are natural.
    pub breath_min_ms: i64,
    /// Breaths at least this long (ms) are removed automatically.
    pub breath_auto_ms: i64,
    /// Breath band, as fractions of (speech level - noise floor).
    pub breath_lo: f32,
    pub breath_hi: f32,
    /// Shortest run of quiet words (count / ms) that counts as low energy.
    pub quiet_min_words: usize,
    pub quiet_min_ms: i64,
    /// Word level (fraction of the speaker's median) below which it is quiet.
    pub quiet_ratio: f32,
    /// Below this fraction a quiet run is "off-mic" rather than low energy.
    pub off_mic_ratio: f32,
    /// Largest pause (ms) that still joins quiet words into one run.
    pub quiet_join_gap_ms: i64,
    /// Report gaps at least this long (ms) when the cut preset keeps pauses.
    pub long_pause_ms: i64,
}

impl Default for DeadAirConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            breath_min_ms: 220,
            breath_auto_ms: 400,
            breath_lo: 0.08,
            breath_hi: 0.45,
            quiet_min_words: 3,
            quiet_min_ms: 900,
            quiet_ratio: 0.5,
            off_mic_ratio: 0.3,
            quiet_join_gap_ms: 800,
            long_pause_ms: 2_500,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetakePolicy {
    /// Keep the last COMPLETE take: a later take cut off mid-sentence loses to
    /// an earlier complete one.
    pub prefer_complete: bool,
    /// Spoken retake cues ("take two", "let me start over") remove the
    /// abandoned attempt that precedes them.
    pub marked_retakes: bool,
    pub cues: Vec<String>,
    /// Longest attempt (words) a cue removes.
    pub cue_max_back_words: usize,
    /// Pause (ms) that ends the attempt before a cue.
    pub cue_boundary_pause_ms: i64,
    /// Pause (ms) that ends a take when measuring how far it runs.
    pub take_end_pause_ms: i64,
    /// Fraction of the replayed take's words a complete unpunctuated take needs.
    pub min_take_ratio: f32,
    /// Take-ranking weights (completeness, fluency, ending, pace, energy);
    /// missing evidence (no envelope) drops its term and renormalises.
    pub weights: TakeWeights,
    /// Per-position bonus favouring later takes when scores are close.
    pub recency_bonus: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TakeWeights {
    pub completeness: f32,
    pub fluency: f32,
    pub ending: f32,
    pub pace: f32,
    pub energy: f32,
}

impl Default for TakeWeights {
    fn default() -> Self {
        Self {
            completeness: 0.35,
            fluency: 0.25,
            ending: 0.15,
            pace: 0.1,
            energy: 0.15,
        }
    }
}

impl Default for RetakePolicy {
    fn default() -> Self {
        Self {
            prefer_complete: true,
            marked_retakes: true,
            cues: [
                "let me start over",
                "let me start again",
                "let's start over",
                "let me try that again",
                "let me try again",
                "let me say that again",
                "let me redo that",
                "start over",
                "try that again",
                "try again",
                "take two",
                "take 2",
                "take three",
                "scratch that",
                "from the top",
                "one more time",
                "let me redo",
                "do that again",
                "say that again",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            cue_max_back_words: 30,
            cue_boundary_pause_ms: 700,
            take_end_pause_ms: 700,
            min_take_ratio: 0.7,
            weights: TakeWeights::default(),
            recency_bonus: 0.04,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeatConfig {
    pub enabled: bool,
    /// Pause (ms) that ends a sentence when punctuation is absent.
    pub sentence_pause_ms: i64,
    /// Longest sentence (words) before a forced split.
    pub max_sentence_words: usize,
    /// Pause (ms) above which same-label sentences do not merge into one beat.
    pub beat_gap_ms: i64,
    /// Window (ms) from the start in which a hook is expected.
    pub hook_window_ms: i64,
}

impl Default for BeatConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sentence_pause_ms: 700,
            max_sentence_words: 45,
            beat_gap_ms: 1_500,
            hook_window_ms: 12_000,
        }
    }
}

/// Thresholds for transcript-free (visual/audio) footage analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VisualConfig {
    pub enabled: bool,
    /// Sample stride (ms) used when scanning for dead space.
    pub sample_stride_ms: i64,
    /// Motion at or below this level (milli of `lavfi.scene_score`) is "still".
    pub motion_dead_milli: i32,
    /// Scene-score at or above this level marks a hard scene cut.
    pub scene_cut_milli: i32,
    /// A dead run is quiet when its envelope is below this fraction of the
    /// source's median RMS.
    pub energy_dead_ratio: f32,
    /// Absolute RMS floor so near-silent tracks still count as quiet.
    pub energy_dead_floor: f32,
    /// Shortest dead span (ms) worth cutting after handles are kept.
    pub min_dead_ms: i64,
    /// Handle (ms) of dead space kept at each edge of a removal.
    pub dead_pad_ms: i64,
    /// Shortest scene segment (ms) considered for best-segment scoring.
    pub min_segment_ms: i64,
    /// Segments scoring below this fraction of the median are "weak".
    pub weak_ratio: f32,
    /// Cap on the fraction of source duration weak-segment suggestions may cover.
    pub max_weak_fraction: f32,
}

impl Default for VisualConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sample_stride_ms: 100,
            motion_dead_milli: 60,
            scene_cut_milli: 300,
            energy_dead_ratio: 0.35,
            energy_dead_floor: 0.004,
            min_dead_ms: 1_200,
            dead_pad_ms: 120,
            min_segment_ms: 1_500,
            weak_ratio: 0.5,
            max_weak_fraction: 0.5,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorialConfig {
    /// The cut engine's configuration: silence, fillers, retakes.
    pub cuts: CutConfig,
    pub disfluency: DisfluencyConfig,
    pub dead_air: DeadAirConfig,
    pub retake: RetakePolicy,
    pub beats: BeatConfig,
    /// Silent/mixed-footage analysis (no usable transcript).
    pub visual: VisualConfig,
}

impl EditorialConfig {
    pub fn for_preset(preset: Preset) -> Self {
        let mut dead_air = DeadAirConfig::default();
        let mut visual = VisualConfig::default();
        if preset == Preset::Punchy {
            // Punchy trims every audible breath and shorter quiet runs.
            dead_air.breath_min_ms = 150;
            dead_air.breath_auto_ms = 150;
            dead_air.long_pause_ms = 1_200;
            visual.min_dead_ms = 700;
            visual.dead_pad_ms = 80;
            visual.weak_ratio = 0.65;
        } else if preset == Preset::Tight {
            visual.min_dead_ms = 900;
            visual.weak_ratio = 0.6;
        }
        Self {
            cuts: CutConfig::for_preset(preset),
            disfluency: DisfluencyConfig::default(),
            dead_air,
            retake: RetakePolicy::default(),
            beats: BeatConfig::default(),
            visual,
        }
    }

    pub fn tight() -> Self {
        Self::for_preset(Preset::Tight)
    }

    pub fn natural() -> Self {
        Self::for_preset(Preset::Natural)
    }

    pub fn punchy() -> Self {
        Self::for_preset(Preset::Punchy)
    }
}
