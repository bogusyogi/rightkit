//! Host -> generation engine orchestration (GenRight engine contract).
//!
//! Library API for a host engine: construct an [`Orchestrator`] with the
//! discovered installed engine path and the project directory, then
//! `submit` -> `wait` -> `import`, or `recover` after a restart. See
//! [`orchestrator`] for the crash-safety model.

#[cfg(all(not(debug_assertions), feature = "fake-engine"))]
compile_error!("fake-engine is restricted to development/test builds");

#[cfg(feature = "channel-recipe")]
pub mod channel_recipe;
pub mod engine;
pub mod error;
pub mod generated_source;
pub mod journal;
pub mod models;
pub mod orchestrator;

pub use engine::{
    media_needs_probe, progress_event, AssetDescriptor, AssetResolution, EngineInfo,
    GenRightClient, GenerateReply, GenerateSpec, JobListQuery, JobPage, JobRecord, MediaKind,
    ProcessRunner, RawOutput, ResultDescriptor, Runner, MAX_INPUT_ARG_BYTES, REQUIRED_CAPABILITIES,
    REQUIRED_CONTRACT_VERSION,
};
pub use error::{Error, Result};
pub use journal::{ImportRecord, Journal, JournalData, RequestEntry, RequestState};
pub use models::{parse_label_durations, DurationRule, ModelCatalog, ModelInfo, SpecViolation};
pub use orchestrator::{new_request_id, Orchestrator, Submission, WaitOptions, DEFAULT_CONSUMER};
