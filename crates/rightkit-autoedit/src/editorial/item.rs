//! The reviewable unit of an editorial plan.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// Pause shortened by the cut engine (leading, trailing or between words).
    Silence,
    /// A gap longer than any preset keeps, when the preset does not shorten it.
    LongPause,
    /// Audible breath in a gap.
    Breath,
    /// A stretch of quiet, trailing-off words.
    LowEnergy,
    /// A run of very quiet or low-confidence words: speech away from the mic.
    OffMic,
    Filler,
    /// Immediate repetition ("I I", "we should we should").
    Repetition,
    /// Word fragment before the real word ("th- the").
    Stutter,
    /// Abandoned clause restarted with a change ("we go- we went").
    SelfCorrection,
    /// Abandoned clause that was replaced by a different one.
    FalseStart,
    /// Earlier take replaced by a later one (or a later, cut-off take dropped).
    Retake,
    /// Attempt abandoned with a spoken cue ("let me start over").
    MarkedRetake,
    /// Silent-footage dead space: still picture with quiet/no audio.
    DeadSpace,
    /// Scene segment scoring poorly on motion + energy (best-segment pick).
    WeakSegment,
    /// The same line repeated in another source; this file's copy is cut.
    CrossFileRetake,
}

impl ItemKind {
    pub fn category(self) -> Category {
        match self {
            Self::Silence | Self::LongPause => Category::Pause,
            Self::Breath | Self::LowEnergy | Self::OffMic => Category::DeadAir,
            Self::Filler
            | Self::Repetition
            | Self::Stutter
            | Self::SelfCorrection
            | Self::FalseStart => Category::Disfluency,
            Self::Retake | Self::MarkedRetake | Self::CrossFileRetake => Category::Retake,
            Self::DeadSpace | Self::WeakSegment => Category::Visual,
        }
    }

    /// Priority when two items claim the same words (lower wins).
    pub(crate) fn priority(self) -> u8 {
        match self {
            Self::Retake => 0,
            Self::MarkedRetake => 1,
            Self::CrossFileRetake => 1,
            Self::FalseStart => 2,
            Self::SelfCorrection => 3,
            Self::Repetition => 4,
            Self::Stutter => 5,
            Self::Filler => 6,
            Self::OffMic => 7,
            Self::LowEnergy => 8,
            Self::Breath => 9,
            Self::LongPause => 10,
            Self::Silence => 11,
            Self::DeadSpace => 11,
            Self::WeakSegment => 12,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Pause,
    DeadAir,
    Disfluency,
    Retake,
    /// Transcript-free footage: dead space and weak segments.
    Visual,
}

/// Whether the engine is sure enough to act without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// Accepted by default.
    Auto,
    /// Proposed; rejected by default until the reviewer accepts it.
    Suggest,
}

/// What accepting the item does to the edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ItemAction {
    /// Remove whole transcript words `first..end` (end exclusive) and the
    /// pauses around them (the cut engine keeps its pads).
    RemoveWords { first: usize, end: usize },
    /// Remove a source range that lies wholly inside a gap between words.
    RemoveRange { start_ms: i64, end_ms: i64 },
    /// The cut engine shortens this pause; rejecting restores it.
    ShortenPause { start_ms: i64, end_ms: i64 },
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Evidence {
    /// The transcript text the item acts on (empty for pure silence).
    pub text: String,
    /// For retakes and corrections: the material that stays.
    pub kept_text: Option<String>,
    /// Cross-file winner identity; absent for single-source decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kept_source: Option<String>,
    /// Transcript word range the item covers (end exclusive).
    pub word_range: Option<[usize; 2]>,
    /// Measured signals (levels, similarity, pause lengths ...), rounded.
    pub signals: BTreeMap<String, f64>,
    pub notes: Vec<String>,
}

/// Links a retake item to its take group ("remove this take").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakeRef {
    pub group_id: String,
    /// Position of the take within the group (chronological).
    pub index: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditorialItem {
    /// Stable: derived from the kind and source range.
    pub id: String,
    pub kind: ItemKind,
    pub tier: Tier,
    /// 0.0..=1.0.
    pub confidence: f32,
    /// Source-time extent (ms) of the evidence.
    pub start_ms: i64,
    pub end_ms: i64,
    pub reason: String,
    pub evidence: Evidence,
    pub action: ItemAction,
    /// Narrative beat the item falls in.
    pub beat_id: Option<String>,
    /// A higher-priority item that already removes these words; fold in UIs.
    pub covered_by: Option<String>,
    /// Set on retake items: which take of which group this removes.
    pub take: Option<TakeRef>,
}

impl EditorialItem {
    pub fn category(&self) -> Category {
        self.kind.category()
    }

    pub fn default_accepted(&self) -> bool {
        self.tier == Tier::Auto
    }

    /// Words removed by this item, if it removes words.
    pub fn word_span(&self) -> Option<(usize, usize)> {
        match self.action {
            ItemAction::RemoveWords { first, end } => Some((first, end)),
            _ => None,
        }
    }
}

pub(crate) struct Draft {
    pub kind: ItemKind,
    pub tier: Tier,
    pub confidence: f32,
    pub start_ms: i64,
    pub end_ms: i64,
    pub reason: String,
    pub evidence: Evidence,
    pub action: ItemAction,
    pub take: Option<TakeRef>,
}

impl Draft {
    pub(crate) fn into_item(self) -> EditorialItem {
        EditorialItem {
            id: String::new(),
            kind: self.kind,
            tier: self.tier,
            confidence: self.confidence.clamp(0.0, 1.0),
            start_ms: self.start_ms,
            end_ms: self.end_ms,
            reason: self.reason,
            evidence: self.evidence,
            action: self.action,
            beat_id: None,
            covered_by: None,
            take: self.take,
        }
    }
}

/// A word-removal draft over `words[first..end]`.
pub(crate) fn word_draft(
    words: &[crate::editorial::util::Wd],
    kind: ItemKind,
    first: usize,
    end: usize,
    tier: Tier,
    confidence: f32,
    reason: String,
) -> Draft {
    Draft {
        kind,
        tier,
        confidence,
        start_ms: words[first].start,
        end_ms: words[end - 1].end,
        reason,
        evidence: Evidence {
            text: crate::editorial::util::join(words, first, end),
            word_range: Some([first, end]),
            ..Evidence::default()
        },
        action: ItemAction::RemoveWords { first, end },
        take: None,
    }
}
