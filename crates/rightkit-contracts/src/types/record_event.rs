use serde::{Deserialize, Serialize};

use super::Rect;

/// `record-event` v1. One line of events.ndjson: rightkit-record dispatch telemetry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RecordEvent {
    pub contract: String,
    pub contract_version: String,
    pub session_id: String,
    pub seq: u64,
    pub kind: EventKind,
    #[serde(default)]
    pub input_kind: Option<InputKind>,
    #[serde(default)]
    pub phase: Option<Phase>,
    #[serde(default)]
    pub effect_id: Option<String>,
    #[serde(default)]
    pub sub_event_id: Option<String>,
    pub step_index: u32,
    #[serde(default)]
    pub step_id: Option<String>,
    pub host_ns: u64,
    #[serde(default)]
    pub timing: Option<Timing>,
    pub geometry_epoch: u32,
    #[serde(default)]
    pub dispatch: Option<Dispatch>,
    #[serde(default)]
    pub pointer: Option<Pointer>,
    #[serde(default)]
    pub target: Option<Target>,
    #[serde(default)]
    pub text: Option<Redacted>,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub modifiers: Vec<Modifier>,
    #[serde(default)]
    pub delta: Option<Delta>,
    #[serde(default)]
    pub admitted: Option<bool>,
    #[serde(default)]
    pub denied: Option<Denied>,
    #[serde(default)]
    pub postcondition: Option<Postcondition>,
    #[serde(default)]
    pub frame: Option<Frame>,
    #[serde(default)]
    pub frame_dropped: Option<FrameDropped>,
    #[serde(default)]
    pub intruders: Vec<Intruder>,
    pub redaction_policy: PolicyRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Admission,
    Input,
    Postcondition,
    Cursor,
    FrameReady,
    Denied,
    Stall,
    RenderComplete,
    StageCheck,
    SessionStart,
    SessionEnd,
    FrameDropped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    Click,
    RightClick,
    Type,
    Key,
    Scroll,
    Drag,
    Focus,
    Wait,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Down,
    Move,
    Up,
    Press,
    Release,
    Scroll,
    Text,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Modifier {
    Cmd,
    Ctrl,
    Alt,
    Shift,
    Fn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Timing {
    #[serde(default)]
    pub admission_ns: Option<u64>,
    #[serde(default)]
    pub dispatch_ns: Option<u64>,
    #[serde(default)]
    pub completion_ns: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Dispatch {
    pub space: CoordinateSpace,
    pub window_bounds_pt: Rect,
    pub scale: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSpace {
    WindowPoints,
    ScreenPoints,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Pointer {
    pub x_pt: f64,
    pub y_pt: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Target {
    #[serde(default)]
    pub target_id: Option<String>,
    #[serde(default)]
    pub identity: Option<Redacted>,
    pub role: String,
    #[serde(default)]
    pub label: Option<Redacted>,
    pub rect_pt: Rect,
}

/// A text-bearing field under the redaction policy. `full` keeps the value,
/// `hash` keeps a label-salted digest, and `omit` keeps nothing at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum Redacted {
    Full {
        value: String,
    },
    #[serde(rename_all = "camelCase")]
    Hash {
        digest: String,
        salt_id: String,
    },
    Omit,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delta {
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Denied {
    pub reason: DenyReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    PermissionDenied,
    NotForegroundSafe,
    TargetMissing,
    TimedOutThenExecuted,
    Cancelled,
    HelperLost,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Postcondition {
    pub observed: bool,
    pub observed_ns: u64,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Frame {
    pub index: u64,
    pub sample_pts_ns: u64,
}

/// A frame the recorder could not keep. `intervalMs` is the gap between the
/// presentation times the drop spans, so a gap of one frame reads as one frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FrameDropped {
    pub count: u32,
    pub interval_ms: f64,
    pub reason: DropReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    Backpressure,
    Encoder,
    CaptureGap,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intruder {
    pub owner: String,
    #[serde(default)]
    pub title: Option<String>,
    pub bounds: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PolicyRef {
    pub policy_id: String,
    pub policy_version: String,
}
