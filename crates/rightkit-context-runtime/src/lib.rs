//! Membrane runtime — one implementation surface for Pull, Cortex, Blueprint,
//! Ledger, and Adapt, served to one-shot callers in process and to the resident
//! through its owner-only endpoint. Cortex durable storage remains isolated from Pull
//! acquisition and reduction.

/// Diagnostic line on stderr that can never panic. `eprintln!` panics when stderr is a closed pipe
/// (a resident outliving its supervisor, or a service with no console); in a watcher thread that
/// killed the thread and left Blueprint `watcher_unavailable` (#555). A failed write is dropped.
macro_rules! stderr_line {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

pub mod adapt;
pub mod adapt_efficiency;
pub mod authorization;
pub mod background_review;
pub mod background_review_input;
mod blueprint_one_shot;
pub mod cache_prefix;
pub mod catalog;
pub mod checkpoint;
pub mod cli;
pub mod code_batch;
pub mod cortex_lifecycle;
pub mod digest;
pub mod doctor;
pub mod feedback;
pub mod freshness;
pub mod graph_health;
pub mod hook;
pub mod installation_manifest;
pub mod ledger;
mod lifecycle_log;
mod markdown;
pub mod memory_provider;
pub mod paths;
pub mod provenance;
pub mod pull;
pub mod qa_env;
pub mod receipt;
pub mod release_identity;
pub mod runtime_receipt;
pub mod scratchpad;
pub mod serve;
pub mod service;
pub mod source_resolution;
pub mod store;
mod wake;
pub use provenance::{
    capture_working_tree, observe, record_provenance, ProvenanceError, ProvenanceRowV1,
    WorkingTreeSnapshotV1, PROVENANCE_ROW_SCHEMA_VERSION, WORKING_TREE_SNAPSHOT_SCHEMA_VERSION,
};
/// Legacy source-compatible alias for reduction APIs now owned by Pull.
#[doc(hidden)]
pub use pull as push;
pub use rightkit_memory_store::db::{record_observable_event, StoreError};
pub use rightkit_memory_store::{context_telemetry, installation_identity, memdb, scope, time};

pub use authorization::{
    authorize as authorize_native_request, intersect_authority, permits_level, AuthorityLevel,
    AuthorizationDecisionV1, AuthorizationDenial, AuthorizationGate, InstallationRegistryV1,
    RepositoryBindingV1, AUTHORIZATION_REQUEST_SCHEMA_VERSION, INSTALLATION_AUTHORITY_LEVEL,
};

pub use background_review::{
    execute_background_review, BackgroundReviewCompletion, BackgroundReviewDecision,
    BackgroundReviewLearner, BackgroundReviewLearnerResult, BackgroundReviewObservationSink,
    BackgroundReviewProducer, BackgroundReviewProduction, BackgroundReviewProposalSink,
    BackgroundReviewScheduler, BackgroundReviewSinkError, JsonlBackgroundReviewObservationSink,
    NoSemanticReviewLearner, CONFIG_PATH_ENV, DEFAULT_CONFIG_RELATIVE_PATH,
    DEFAULT_OBSERVATIONS_RELATIVE_PATH, MAX_ATTEMPTS, MAX_OBSERVATION_BATCH,
    MAX_OBSERVATION_FILE_BYTES, MAX_OBSERVATION_RECORD_BYTES, OBSERVATIONS_PATH_ENV,
};

pub use checkpoint::{
    CheckpointError, CheckpointSourceRefV1, CheckpointSourceResolutionV1, CheckpointV1,
};
pub use paths::{cache_root, PRODUCT_DIR_NAME};
pub use rightkit_memory_store::MemDb;
pub use rightkit_memory_store::{
    TemporalFact, TemporalFactQuery, TemporalFactReceipt, TemporalFactStore, TemporalTransition,
};
pub use scope::{
    normalize_scope, path_to_scope, scope_chain, ScopeDescriptorError, ScopeDescriptorV1,
};
pub use store::{
    ApprovedProposalAdmissionV1, CortexBackupLinkV1, CortexBackupRowV1, CortexBackupV1,
    MemoryEventContext, MemoryLifecycleError, MemoryLifecycleEventV1, MemoryLifecycleInputV1,
    MemoryLifecycleKind, MemoryLifecycleOperation, MemoryLifecycleOperationV1,
    MemoryLifecycleReceiptV1, MemoryPriorityError, MemoryStore, RecallResult, VerifiedMemoryActor,
};

pub mod adapt_service;

pub mod adapt_observations;
