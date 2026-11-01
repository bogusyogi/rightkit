//! Cut-engine configuration and the `tight` / `natural` presets.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Preset {
    Tight,
    Natural,
    /// Beyond tight: shortest kept pauses, minimal pads and lead-ins.
    Punchy,
}

/// A user-requested removal of a source range. Cuts never fall inside a word:
/// every word whose midpoint lies inside the range is removed, nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualRemoval {
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetakeConfig {
    pub enabled: bool,
    /// Shortest earlier attempt (in non-filler words) treated as a take.
    pub min_words: usize,
    /// Longest earlier attempt examined, in words.
    pub max_words: usize,
    /// Normalised similarity (LCS Dice over tokens) needed to call a repeat.
    pub threshold: f32,
    /// Debris words allowed between the abandoned attempt and the restart.
    pub max_between_words: usize,
    /// Longest gap (ms) between the attempt's end and the restart's start.
    pub max_between_ms: i64,
    /// Pause (ms) that marks the end of an attempt when there is no punctuation.
    pub boundary_pause_ms: i64,
}

impl Default for RetakeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_words: 4,
            max_words: 40,
            threshold: 0.8,
            max_between_words: 8,
            max_between_ms: 6_000,
            boundary_pause_ms: 200,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CutConfig {
    pub preset: Preset,
    pub remove_silence: bool,
    pub remove_fillers: bool,
    /// Longest pause kept between words (ms).
    pub max_pause_ms: i64,
    /// Longest pause kept after a sentence-final word (ms).
    pub sentence_pause_ms: i64,
    /// Minimum silence kept after the word before a cut (ms).
    pub release_pad_ms: i64,
    /// Minimum silence kept before the word after a cut (ms).
    pub attack_pad_ms: i64,
    /// Silence kept before the first kept word when the head is trimmed (ms).
    pub lead_in_ms: i64,
    /// Silence kept after the last kept word when the tail is trimmed (ms).
    pub tail_out_ms: i64,
    /// A silence cut shorter than this is not worth making (ms).
    pub min_cut_ms: i64,
    /// Shortest believable word (ms) when refining approximate word ends.
    pub min_word_ms: i64,
    /// Search radius (ms) around a cut point for the quietest envelope frame.
    pub snap_window_ms: i64,
    /// Fillers removed wherever they occur (case/punctuation-insensitive;
    /// multi-word entries allowed).
    pub fillers: Vec<String>,
    /// Fillers removed only when isolated by pauses on both sides.
    pub context_fillers: Vec<String>,
    /// Pause (ms) on each side that makes a context filler "isolated".
    pub filler_isolation_ms: i64,
    pub retake: RetakeConfig,
    pub manual_removals: Vec<ManualRemoval>,
}

fn default_fillers() -> Vec<String> {
    [
        "um", "umm", "uh", "uhh", "uhm", "erm", "er", "err", "ah", "ahh", "hmm", "hmmm", "hm", "mm",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_context_fillers() -> Vec<String> {
    ["you know", "like"].iter().map(|s| s.to_string()).collect()
}

impl CutConfig {
    pub fn for_preset(preset: Preset) -> Self {
        match preset {
            Preset::Tight => Self::tight(),
            Preset::Natural => Self::natural(),
            Preset::Punchy => Self::punchy(),
        }
    }

    /// Max kept pause 200 ms (300 ms after a sentence); 40/30 ms release/attack pads.
    pub fn tight() -> Self {
        Self {
            preset: Preset::Tight,
            remove_silence: true,
            remove_fillers: true,
            max_pause_ms: 200,
            sentence_pause_ms: 300,
            release_pad_ms: 40,
            attack_pad_ms: 30,
            lead_in_ms: 80,
            tail_out_ms: 130,
            min_cut_ms: 40,
            min_word_ms: 40,
            snap_window_ms: 40,
            fillers: default_fillers(),
            context_fillers: default_context_fillers(),
            filler_isolation_ms: 120,
            retake: RetakeConfig::default(),
            manual_removals: Vec::new(),
        }
    }

    /// Max kept pause 500 ms (700 ms after a sentence); 80/50 ms release/attack pads.
    pub fn natural() -> Self {
        Self {
            preset: Preset::Natural,
            max_pause_ms: 500,
            sentence_pause_ms: 700,
            release_pad_ms: 80,
            attack_pad_ms: 50,
            lead_in_ms: 150,
            tail_out_ms: 250,
            min_cut_ms: 60,
            ..Self::tight()
        }
    }

    /// Max kept pause 120 ms (200 ms after a sentence); 25/20 ms release/attack
    /// pads and 40 ms lead-in: breaths and sentence-start slack are trimmed.
    pub fn punchy() -> Self {
        Self {
            preset: Preset::Punchy,
            max_pause_ms: 120,
            sentence_pause_ms: 200,
            release_pad_ms: 25,
            attack_pad_ms: 20,
            lead_in_ms: 40,
            tail_out_ms: 80,
            min_cut_ms: 30,
            snap_window_ms: 30,
            filler_isolation_ms: 100,
            ..Self::tight()
        }
    }
}
