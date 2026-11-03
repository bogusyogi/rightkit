// Engine IPC protocol — frame schema, payloads, and contract validation.
// Pure serde + validation; the process spawning / stdin-stdout machinery
// lives in the host application.

use super::validation::{reject_ui_concepts, validate_frame};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Parallel wire shape for vocabulary entries carrying sound-alike aliases.
/// `term_details` rides beside `terms` in `ReplaceVocabulary` so an old engine
/// without this field falls back gracefully to the bare `terms` list (no
/// aliases); a new engine that receives no `term_details` also falls back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VocabTermWire {
    pub term: String,
    #[serde(default)]
    pub sounds_like: Vec<String>,
}

pub const PROTOCOL_MAJOR: u16 = 1;
pub const PROTOCOL_MINOR: u16 = 0;
pub const CURRENT_SCHEMA_VERSION: u16 = 1;
const FAKE_ENGINE_VERSION: &str = "fake-engine/0.1.0";

/// Max audio length (seconds) the free tier may transcribe from a file. Pro is
/// unlimited (including time-bound trials) — there is no hard ceiling above this.
pub const FREE_TRANSCRIBE_MAX_SECS: u32 = 600; // 10 minutes

/// Gate a file's duration against tier limits. Pure — no I/O. `secs` is the
/// decoded (or probed) audio length in seconds; `is_pro` is the entitlement.
/// Free is capped at FREE_TRANSCRIBE_MAX_SECS; Pro has no length limit.
pub fn check_duration_limit(secs: u32, is_pro: bool) -> Result<(), String> {
    if !is_pro && secs > FREE_TRANSCRIBE_MAX_SECS {
        return Err(format!(
            "Free transcription is limited to {} minutes per file. Upgrade to Pro for unlimited file length.",
            FREE_TRANSCRIBE_MAX_SECS / 60
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineSchemaName {
    EngineHealth,
    EngineCapabilities,
    EngineInfo,
    EngineAck,
    ReplaceEngineConfig,
    /// BYOK provider keys, shell -> sidecar, private stdin only. The body is
    /// secret: never log a frame of this schema beyond its name.
    SetProviderKeys,
    /// Live L3 (cloud polish) reconfigure, shell -> sidecar. Non-secret; keys
    /// ride `SetProviderKeys`. Only sent when the sidecar advertised
    /// [`CAPABILITY_LIVE_L3_RECONFIGURE`]; otherwise the shell restarts it.
    ReconfigureL3,
    ReplaceVocabulary,
    EngineStateRequest,
    EngineStateSnapshot,
    RecentHistoryRequest,
    RecentHistoryResult,
    ReplaceRecentHistory,
    RepasteLastRequest,
    ManualDeliveryResult,
    CopyLastRequest,
    CopyLastResult,
    FileTranscriptionRequest,
    /// Shell -> engine: stop the file transcription started by the request with this id (its
    /// shell-side wait ended). Answered with an `EngineAck`.
    FileTranscriptionCancel,
    FileTranscriptionResult,
    RecordingStarted,
    RecordingContextCaptured,
    RecordingLevel,
    TranscribingStarted,
    TranscriptPartial,
    TranscriptFinal,
    EngineError,
    // Phase A2 wake-word listener (always-listening mode)
    WakeListenStarted,
    WakeListenStopped,
    WakeEvent,
}

/// Capability string a sidecar advertises in its `EngineCapabilities` reply when
/// it accepts `ReconfigureL3`. Absent = the shell must restart the sidecar.
pub const CAPABILITY_LIVE_L3_RECONFIGURE: &str = "live_l3_reconfigure";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineHealthStatus {
    Ok,
    Degraded,
    Unavailable,
}

/// Typed replacement for the legacy `request_id` suffix convention
/// (`":cancel"` / `":send_enter"`). Protocol v2 additive field — carried on
/// `TranscribingStarted` requests so the IPC decoder no longer has to parse
/// intent out of a string. Optional + `skip_serializing_if` so OLD frames
/// (no `stop_kind`) still deserialize on both old and new binaries; the
/// legacy suffix is still written/read in parallel during the transition
/// (see the engine runtime's request decoder).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopKind {
    /// Plain stop — finalize without an Enter keystroke.
    Stop,
    /// Stop and deliver with a trailing Enter keystroke.
    SendEnter,
    /// Abandon the in-flight recording/transcription; never deliver.
    Cancel,
    /// A cancel (any source) that still finalizes the in-flight recording —
    /// unlike `Cancel`, the engine runs ASR to completion — but the shell
    /// stores the result to encrypted history as `Cancelled` instead of
    /// delivering it. No clipboard write, no paste, no Enter. The shell
    /// prefers the raw/L0 transcript for the stored record, so the engine
    /// should skip AI polish (L1/L2/L3) for this stop kind where practical.
    CancelToHistory,
}

/// Typed replacement for the stringly-keyed `TranscriptFinal.diagnostics`
/// JSON map (`"delivery_record"`, `"shell_delivery"`, `"send_enter"`,
/// `"raw_text"`, `"recording_ms"`, `"reset_to_armed"`, `"command_dispatched"`,
/// `"command_detail"`, `"command_failed"`).
/// Protocol v2 additive: the wire field stays `Option<serde_json::Value>` (no
/// schema-shape break); senders now build this struct and serialize it into
/// that slot, and readers deserialize it FIRST, falling back to the old
/// string-keyed extraction for frames from an older engine build that hasn't
/// switched over yet. All fields optional/`skip_serializing_if` so old and
/// new frames both round-trip through `Option<Value>` unchanged. See
/// dispatch #9/#10.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecordingStageMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vad_observed_frame_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vad_speech_frame_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asr_elapsed_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polish_elapsed_ms: Option<u64>,
}

/// One ASR decode pass (a window) timed inside the engine. Content-free.
/// `mel_ms`/`encode_ms`/`search_ms` are `None` where the backend does not
/// report the split (only `total_ms` and `samples` are always present).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EnginePass {
    #[serde(default)]
    pub samples: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mel_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encode_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_ms: Option<f64>,
    #[serde(default)]
    pub total_ms: f64,
}

/// Engine-side stage timeline for one dictation, for the cross-platform
/// per-stage latency table. Offsets are milliseconds from the engine's
/// `begin_stop` instant (stage-local origin, never wall time). Three fields
/// are DURATIONS, not offsets: `inference_wait_ms`, `local_polish_ms` and
/// `cloud_polish_ms`. Carries no text content.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EngineTimeline {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_pickup_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_sealed_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_queued_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_started_ms: Option<f64>,
    /// Final-decode passes after the stop began.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub passes: Vec<EnginePass>,
    /// Passes decoded while recording (count only).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub background_passes: u32,
    /// Duration spent waiting on the inference gate after the stop began.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference_wait_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asr_done_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalize_started_ms: Option<f64>,
    /// Duration of the local (L0) polish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_polish_ms: Option<f64>,
    /// Duration of the cloud polish/transform lane, when one ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_polish_ms: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_sent_ms: Option<f64>,
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticsPayload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_record: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_delivery: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_enter: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_to_armed: Option<bool>,
    /// This finalize was a cancel-to-history (`StopKind::CancelToHistory`):
    /// the shell must store the transcript to history and deliver nothing.
    /// Carried on the wire so a spoken `zephyr cancel` the ENGINE recognized
    /// reaches the shell's cancel routing even when the shell never latched
    /// `cancel_to_history_session` (it only latches for its own UI cancel).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<bool>,
    /// Capture interruption feedback carried atomically with saved history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_warning: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_dispatched: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_failed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_stage_metrics: Option<RecordingStageMetrics>,
    /// Per-session receipt: true only when this finalize built and attempted
    /// an actual serialized provider request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_request_attempted: Option<bool>,
    /// True only when non-empty start context was included in that request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_original_binding: Option<bool>,
    /// Engine stage timeline for the `utterance_timeline` telemetry event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline: Option<EngineTimeline>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineErrorPayload {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default)]
    pub diagnostics: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnginePayload {
    Health {
        status: EngineHealthStatus,
        #[serde(default)]
        diagnostics: Option<Value>,
    },
    Capabilities {
        capabilities: Vec<String>,
    },
    EngineInfo {
        mode: String,
        #[serde(default)]
        cold_load_s: Option<f64>,
        #[serde(default)]
        engine_version: Option<String>,
    },
    EngineAck {
        status: String,
        #[serde(default)]
        detail: Option<String>,
    },
    ReplaceEngineConfig {
        config: Value,
    },
    /// BYOK provider keys as the owner's hand-off frame (`rightkit-credentials` `handoff`):
    /// hex of one snapshot Reply, always a full replacement of the sidecar's key set (see
    /// `provider_handoff`). The engine answers with the owner's Ack in `EngineAck.detail`.
    SetProviderKeys {
        frame: SecretString,
    },
    /// Replaces the env-derived L3 state the sidecar otherwise reads once at
    /// spawn (cleanup, cloud consent and provider state variables).
    ReconfigureL3 {
        ai_polish_enabled: bool,
        provider_state: String,
        provider_ready: bool,
    },
    ReplaceVocabulary {
        terms: Vec<String>,
        /// Parallel mirror carrying `sounds_like` aliases for terms that have
        /// them. Backward-compatible: an old shell that sends no
        /// `term_details` falls back to the bare `terms` list (no aliases);
        /// an old engine that ignores the unknown field still works from
        /// `terms`. New code prefers `term_details` when present.
        #[serde(default)]
        term_details: Option<Vec<VocabTermWire>>,
    },
    EngineStateRequest,
    EngineStateSnapshot {
        state: String,
    },
    RecentHistoryRequest {
        limit: usize,
    },
    RecentHistoryResult {
        records: Value,
    },
    ReplaceRecentHistory {
        records: Value,
    },
    RepasteLastRequest,
    ManualDeliveryResult {
        record: Value,
    },
    CopyLastRequest,
    CopyLastResult {
        text: String,
    },
    FileTranscriptionRequest {
        path: String,
    },
    /// The `request_id` of the `FileTranscriptionRequest` to stop.
    FileTranscriptionCancel {
        request_id: String,
    },
    FileTranscriptionResult {
        text: String,
        srt: String,
        vtt: String,
        #[serde(default)]
        words: Vec<TimedWord>,
    },
    RecordingStarted {
        session_id: String,
        /// The shell's last real paste (pasted, or sent but unconfirmed), so the engine can check
        /// "delete that" and the casing commands against the text the shell actually delivered.
        /// Absent on an old shell and when nothing qualifies; an old frame still deserializes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_pasted: Option<rightkit_transcript_library::delivery::DeliveryRecord>,
    },
    RecordingContextCaptured {
        session_id: String,
        context: RecordingContext,
        #[serde(default)]
        recording_target: Option<rightkit_transcript_library::delivery::TargetSnapshot>,
    },
    RecordingLevel {
        level: f32,
    },
    TranscribingStarted {
        /// Protocol v2 additive field (see `StopKind` doc comment). Absent on
        /// legacy frames and on the fresh-recording `TranscribingStarted`
        /// notification the worker pump emits (no stop intent there).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_kind: Option<StopKind>,
        /// Destination frozen by the engine at the hands-free stop moment,
        /// before the IPC round trip. The shell uses it instead of taking a
        /// fresh capture on receipt. Absent on shell-initiated stops (the
        /// shell captures at its own stop moment) and on legacy frames.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_target: Option<rightkit_transcript_library::delivery::TargetSnapshot>,
    },
    TranscriptPartial {
        text: String,
        revision: u64,
        #[serde(default)]
        audio_secs: Option<f32>,
        #[serde(default)]
        diagnostics: Option<Value>,
    },
    TranscriptFinal {
        text: String,
        #[serde(default)]
        confidence: Option<f32>,
        #[serde(default)]
        diagnostics: Option<Value>,
    },
    WakeListenStarted {
        status: String,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        threshold: Option<f32>,
        #[serde(default)]
        fake: Option<bool>,
    },
    WakeListenStopped {
        status: String,
        #[serde(default)]
        total_fires: Option<u64>,
    },
    WakeFired {
        ts: f64,
        #[serde(default)]
        fire_count: Option<u64>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        threshold: Option<f32>,
        /// `softmax[1]` of the firing window (fable audit #5 — propagate score so
        /// the UI/telemetry can show confidence). `None` until the Phase F
        /// scorer is wired (see `crate::wake`).
        #[serde(default)]
        score: Option<f32>,
    },
}

/// Start-bound field context. This is content provenance for one recording;
/// native editor handles stay in the shell and never cross the sidecar wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingContext {
    #[serde(default)]
    pub app_name: Option<String>,
    #[serde(default)]
    pub window_title: Option<String>,
    #[serde(default)]
    pub field_text: Option<String>,
    #[serde(default)]
    pub selected_text: Option<String>,
    pub state: RecordingContextState,
    /// True only after this exact context was included in an LLM request.
    #[serde(default)]
    pub request_attempted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingContextState {
    ConfirmedEmpty,
    NonEmpty,
    Unreadable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecordingDeliveryPolicy {
    ConfirmedEmptyStop,
    OriginalBound,
    /// Start context was read and nonempty but never reached an LLM request
    /// (polish off, provider down, utterance under the polish gate). Nothing
    /// binds the transcript to the start editor, so deliver like any ordinary
    /// dictation: the frozen first stop target.
    UnboundStop,
    Recovery,
}

pub fn recording_delivery_policy(
    state: Option<RecordingContextState>,
    request_attempted: bool,
    original_binding: bool,
) -> RecordingDeliveryPolicy {
    match state {
        Some(RecordingContextState::ConfirmedEmpty) => RecordingDeliveryPolicy::ConfirmedEmptyStop,
        Some(RecordingContextState::NonEmpty) if request_attempted && original_binding => {
            RecordingDeliveryPolicy::OriginalBound
        }
        Some(RecordingContextState::NonEmpty) => RecordingDeliveryPolicy::UnboundStop,
        // Unreadable or missing start state stays fail-closed.
        Some(RecordingContextState::Unreadable) | None => RecordingDeliveryPolicy::Recovery,
    }
}

/// Shell-only recovery override. Explicit negative receipts are required:
/// missing receipts do not prove that no field/selection reached a provider.
/// `stop_destination_available` admits an unbound transcript to the frozen
/// stop-target delivery path. That path must still validate destination
/// continuity and secure-field policy before dispatch; start proof is irrelevant.
pub fn recording_delivery_policy_with_stop_proof(
    state: Option<RecordingContextState>,
    request_attempted: Option<bool>,
    original_binding: Option<bool>,
    stop_destination_available: bool,
) -> RecordingDeliveryPolicy {
    let policy = recording_delivery_policy(
        state,
        request_attempted.unwrap_or(false),
        original_binding.unwrap_or(false),
    );
    // `original_binding` is the precise receipt: true only when field context
    // was IN the provider payload. A polish request made without any field
    // context (`request_attempted` true, binding false) binds nothing, so it
    // must not push an otherwise provable stop-editor paste to the clipboard.
    let _ = request_attempted;
    if policy == RecordingDeliveryPolicy::Recovery
        && original_binding == Some(false)
        && stop_destination_available
    {
        RecordingDeliveryPolicy::UnboundStop
    } else {
        policy
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EngineFrame {
    pub protocol_major: u16,
    pub protocol_minor: u16,
    pub schema_name: EngineSchemaName,
    pub schema_version: u16,
    pub engine_version: String,
    pub request_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    /// Hotkey-time delivery target captured by the shell. Start requests use
    /// this to keep sidecar focus/context reads bound to the exact app which
    /// owned focus when recording began instead of sampling another app later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_target: Option<rightkit_transcript_library::delivery::TargetSnapshot>,
    /// Start-bound, serializable context. Native focus identity remains shell
    /// local; this field carries only bounded prompt context/provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_context: Option<RecordingContext>,
    pub trace_id: String,
    #[serde(default)]
    pub payload: Option<EnginePayload>,
    #[serde(default)]
    pub error: Option<EngineErrorPayload>,
}

impl EngineFrame {
    pub fn health(request_id: &str, trace_id: &str) -> Self {
        Self::base(
            EngineSchemaName::EngineHealth,
            request_id,
            None,
            trace_id,
            Some(EnginePayload::Health {
                status: EngineHealthStatus::Ok,
                diagnostics: None,
            }),
            None,
        )
    }

    /// Construct an `EngineFrame` from its wire components. Public so the
    /// sidecar binary and shell supervisor can both build frames without a
    /// parallel constructor.
    pub fn base(
        schema_name: EngineSchemaName,
        request_id: &str,
        session_id: Option<String>,
        trace_id: &str,
        payload: Option<EnginePayload>,
        error: Option<EngineErrorPayload>,
    ) -> Self {
        Self {
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
            schema_name,
            schema_version: CURRENT_SCHEMA_VERSION,
            engine_version: FAKE_ENGINE_VERSION.to_string(),
            request_id: request_id.to_string(),
            session_id,
            recording_target: None,
            recording_context: None,
            trace_id: trace_id.to_string(),
            payload,
            error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineContractError {
    ProtocolMajorMismatch { expected: u16, actual: u16 },
    UnsupportedSchemaVersion { actual: u16 },
    MissingPayload,
    MissingError,
    MissingSessionId,
    UnexpectedErrorPayload,
    PayloadSchemaMismatch,
    UiConceptLeaked(String),
}

impl std::fmt::Display for EngineContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProtocolMajorMismatch { expected, actual } => {
                write!(
                    f,
                    "protocol_major mismatch: expected {expected}, got {actual}"
                )
            }
            Self::UnsupportedSchemaVersion { actual } => {
                write!(f, "unsupported schema_version: {actual}")
            }
            Self::MissingPayload => write!(f, "engine frame missing payload"),
            Self::MissingError => write!(f, "engine error frame missing error"),
            Self::MissingSessionId => write!(f, "session-scoped frame missing session_id"),
            Self::UnexpectedErrorPayload => write!(f, "normal frame carried error payload"),
            Self::PayloadSchemaMismatch => write!(f, "schema_name does not match payload"),
            Self::UiConceptLeaked(field) => write!(f, "engine leaked Rust-owned field {field}"),
        }
    }
}

impl std::error::Error for EngineContractError {}

pub fn validate_engine_frame(value: &Value) -> Result<EngineFrame, EngineContractError> {
    reject_ui_concepts(value)?;
    // Deserialize from the existing `&Value` instead of `value.clone()` +
    // `serde_json::from_value` — `&serde_json::Value` implements `Deserializer`
    // directly, so this walks the same tree once instead of deep-cloning the
    // whole frame (strings, nested objects, everything) before reading it. Every
    // IPC frame pays this, including 10Hz RecordingLevel/partial traffic for the
    // whole recording. Same validation, same error on the same malformed input —
    // `from_value`'s error is discarded into `PayloadSchemaMismatch` either way.
    let frame: EngineFrame =
        EngineFrame::deserialize(value).map_err(|_| EngineContractError::PayloadSchemaMismatch)?;
    validate_frame(&frame)?;
    Ok(frame)
}

/// The record the engine should treat as the shell's last paste, or `None` when the last
/// delivery did not put text in a field. Only a paste (or one sent but unconfirmed) qualifies: a
/// copied fallback, a cancel, a draft or an error never reached the field, so "delete that" and
/// the casing commands must not act on it.
pub fn last_pasted_for_engine(
    record: &rightkit_transcript_library::delivery::DeliveryRecord,
) -> Option<rightkit_transcript_library::delivery::DeliveryRecord> {
    use rightkit_transcript_library::delivery::DeliveryOutcome;
    matches!(
        record.outcome,
        DeliveryOutcome::Pasted | DeliveryOutcome::PasteSentUnconfirmed { .. }
    )
    .then(|| record.clone())
}
