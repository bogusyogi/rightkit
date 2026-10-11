#![deny(unsafe_code)]
//! Shared engine base: job store, runner, and stdio/CLI command contract.

#[cfg(feature = "supervisor")]
mod client;
mod config;
#[cfg(feature = "contract")]
mod contract;
mod error;
pub mod pool;
mod runner;
mod sched;
#[cfg(feature = "service")]
mod service;
mod store;
#[cfg(feature = "supervisor")]
mod supervisor;

#[cfg(feature = "supervisor")]
pub use client::{
    ChildExit, ClientOptions, ContractRequirement, EngineClient, EventFrame, OnEventFrame,
    ProtocolError, ProtocolErrorSink, DEFAULT_HELLO_TIMEOUT,
};
pub use config::{
    ArtifactNames, ColumnType, Ext, ExtColumn, ProgressPolicy, StatusModel, StoreConfig, StoreHook,
};
#[cfg(feature = "contract")]
pub use contract::{
    parse_flags, run_cli, serve, CommandSet, Dispatcher, DoctorFn, Flags, JobCommands, Outcome,
    PayloadMapper, Sink, CONTRACT_VERSION, DEFAULT_FRAME_LIMIT, JOB_METHODS,
};
pub use error::{
    ErrorOrigin, JobError, JobResult, CONTRACT_VERSION_MISMATCH, CURSOR_MISMATCH, DB_UNREADABLE,
    HOST_HANDOFF, INTERRUPTED, INVALID_CURSOR, JOB_EXISTS, STALE_ATTEMPT, STALLED, SUPERSEDED,
};
pub use pool::{OutcomeFn, PoolConfig, Staged, StagedHandler, StagedPool};
/// The `rusqlite` this crate links (via `rightkit-db`); hooks receive its `Connection`.
pub use rightkit_db::rusqlite;
/// Recovery policy for [`JobStore::open_recovering`] (re-exported from `rightkit-db`; also under
/// [`db`]).
pub use rightkit_db::RecoveryPolicy;
pub use runner::{
    AttemptPolicy, EventSink, JobControl, JobHandler, JobLock, JobLocks, RunOutcome, Runner,
    DEFAULT_MAX_ATTEMPTS,
};
pub use sched::{Gate, InteractiveGuard, Priority};
#[cfg(feature = "service")]
pub use service::DispatcherHandler;
pub use store::{
    Adoption, Artifact, CancelToken, Cursor, DeleteOptions, DeleteReport, Event, ImportOptions,
    ImportRecord, Imported, Job, JobRecord, JobStore, ListFilter, ListPage, NewJob, OpenRecovery,
    ProgressStats, PrunePolicy, PruneReport, Recovery, Resolution, Submission, SubmissionRecord,
    SubmitOptions, Submitted, Transition, CANCELLED, COMPLETE, DELETING_TABLE, FAILED, MAX_EVENTS,
    MAX_IMPORT_BATCH, META_TABLE, PATH_OUTSIDE_ROOT, QUEUED, RUNNING, SUBMISSIONS_TABLE,
};
#[cfg(feature = "supervisor")]
pub use supervisor::{
    CallOptions, EngineSupervisor, ExitReason, HealthCheck, InitialFailure, Lifecycle,
    LifecycleEvent, Readiness, ShutdownOutcome, ShutdownReport, SupervisorConfig, SupervisorStatus,
    Terminal,
};
// Re-exported so a host that only depends on rightkit-jobs can build the policy and spawn command.
#[cfg(feature = "supervisor")]
pub use rightkit_process::{JobAssignmentMode, OwnedCommand, RestartMode, RestartPolicy};

/// The `rightkit-db` types that appear in this crate's public API, so a consumer that only opens
/// a [`JobStore`] needs no `rightkit-db` dependency of its own (and no version to keep in step
/// with this crate's): the options and policies `JobStore::open_with`, `open_with_config`,
/// `open_recovering` and `open_recovering_unreadable` take, the types those options are built
/// from, and the error type a database failure converts from. They are the `rightkit-db` types
/// themselves, not copies.
pub mod db {
    pub use rightkit_db::{
        CacheProfile, ConnInit, DbError, DbOptions, DurabilityProfile, IntegrityCheck,
        JournalSelect, QuarantineStamp, RecoveryAction, RecoveryPolicy, SidecarPolicy,
        StorageProblem, UnreadableCause, UnreadablePolicy,
    };
}
