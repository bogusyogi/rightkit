//! Client of the generation engine contract: an engine (or app) drives an installed media-generation
//! engine (`rightkit-media-gen`'s CLI) with crash-safe orchestration.
//!
//! Library API for a host engine: construct an [`Orchestrator`] with the
//! discovered installed engine path and the project directory, then
//! `submit` -> `wait` -> `import`, or `recover` after a restart. See
//! [`orchestrator`] for the crash-safety model.

pub mod engine;
pub mod error;
pub mod generated_source;
pub mod journal;
pub mod models;
pub mod orchestrator;

pub use engine::{
    media_needs_probe, progress_event, AssetDescriptor, AssetResolution, EngineInfo, GenerateReply,
    GenerateSpec, GenerationClient, JobListQuery, JobPage, JobRecord, MediaKind, ProcessRunner,
    RawOutput, ResultDescriptor, Runner, MAX_INPUT_ARG_BYTES, REQUIRED_CAPABILITIES,
    REQUIRED_CONTRACT_VERSION,
};
pub use error::{Error, Result};
pub use journal::{ImportRecord, Journal, JournalData, RequestEntry, RequestState};
pub use models::{parse_label_durations, DurationRule, ModelCatalog, ModelInfo, SpecViolation};
pub use orchestrator::{new_request_id, Orchestrator, Submission, WaitOptions, DEFAULT_CONSUMER};
