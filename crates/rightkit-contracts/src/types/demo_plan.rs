use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::delivery_envelope::Aspect;

/// `demo-plan` v1. The director's timeline actions and the narration time map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DemoPlan {
    pub contract: String,
    pub contract_version: String,
    pub plan_id: String,
    pub brief_id: String,
    pub brief_revision: u64,
    pub bundle_id: String,
    pub bundle_blake3: String,
    pub aspects: Vec<Aspect>,
    pub narration: Narration,
    pub timeline: Vec<Action>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Narration {
    #[serde(default)]
    pub voice_id: Option<String>,
    pub script_hash: String,
    pub duration_ms: u64,
    pub words: Vec<Word>,
    pub time_map: Vec<Segment>,
}

/// A word with the two separate confidences R8 requires: lexical (does the
/// aligned word match the script) and timing (boundary error).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Word {
    pub word_id: String,
    pub step_id: String,
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub lexical_confidence: f64,
    pub timing_confidence: f64,
}

/// A time-map segment. A play segment maps a source span to an output span at a
/// speed. A hold segment shows one source frame over an output span, and has
/// no source span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Segment {
    #[serde(rename_all = "camelCase")]
    Play {
        source_start_ms: u64,
        source_end_ms: u64,
        output_start_ms: u64,
        output_end_ms: u64,
        speed: f64,
    },
    #[serde(rename_all = "camelCase")]
    Hold {
        source_at_ms: u64,
        output_start_ms: u64,
        output_end_ms: u64,
        #[serde(default)]
        step_id: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Primitive {
    Transform,
    Crop,
    Opacity,
    Transition,
    Retime,
    Reverse,
    Hold,
    OverlayCursor,
    OverlayBadge,
    OverlayCaption,
    AudioCue,
    WebcamBubble,
}

/// An explicit reference to a step, word or target. Labels are never anchors,
/// because duplicate labels are common.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Anchor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
}

/// A point on the timeline: an anchor plus an offset, or an absolute time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TimeRef {
    #[serde(rename_all = "camelCase")]
    Anchored { anchor: Anchor, offset_ms: i64 },
    #[serde(rename_all = "camelCase")]
    Absolute { absolute_ms: u64 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Action {
    pub action_id: String,
    pub primitive: Primitive,
    pub start: TimeRef,
    #[serde(default)]
    pub end: Option<TimeRef>,
    #[serde(default)]
    pub keyframes: Vec<Keyframe>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub safe_region: Option<SafeRegion>,
    pub provenance: Provenance,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Keyframe {
    pub offset_ms: u64,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafeRegion {
    Clear,
    CaptionBand,
    WebcamBubble,
}

/// Director actions can be re-generated. Manual and locked actions are kept
/// when a plan is re-directed or re-voiced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Provenance {
    pub source: Source,
    pub locked: bool,
    #[serde(default)]
    pub generator_revision: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Director,
    Manual,
}
