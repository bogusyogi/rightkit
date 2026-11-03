pub mod launch;
mod protocol;
pub mod provider_handoff;
mod secret;
mod validation;

pub use protocol::{
    check_duration_limit, last_pasted_for_engine, recording_delivery_policy,
    recording_delivery_policy_with_stop_proof, validate_engine_frame, DiagnosticsPayload,
    EngineContractError, EngineErrorPayload, EngineFrame, EngineHealthStatus, EnginePass,
    EnginePayload, EngineSchemaName, EngineTimeline, RecordingContext, RecordingContextState,
    RecordingDeliveryPolicy, RecordingStageMetrics, StopKind, VocabTermWire,
    CAPABILITY_LIVE_L3_RECONFIGURE, CURRENT_SCHEMA_VERSION, FREE_TRANSCRIBE_MAX_SECS,
    PROTOCOL_MAJOR, PROTOCOL_MINOR,
};
pub use rightkit_speech_client::{FileTranscript, TimedWord};
pub use secret::{wipe_json_strings, wipe_string, SecretString};
pub use validation::{validate_frame, FakeEngine};
