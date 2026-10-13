#![deny(unsafe_code)]
//! Shared engine base: job store, runner, and stdio/CLI command contract.

#[cfg(feature = "supervisor")]
mod client;
#[cfg(feature = "store")]
mod config;
#[cfg(feature = "contract")]
mod contract;
mod error;
#[cfg(feature = "ffi")]
#[allow(unsafe_code)]
pub mod ffi;
#[cfg(feature = "store")]
pub mod pool;
#[cfg(feature = "store")]
mod runner;
#[cfg(feature = "store")]
mod sched;
#[cfg(feature = "service")]
mod service;
#[cfg(feature = "store")]
mod store;
#[cfg(feature = "supervisor")]
mod supervisor;
#[cfg(feature = "protocol")]
mod wire;

#[cfg(feature = "supervisor")]
pub use client::{
    ChildExit, ClientOptions, ContractRequirement, EngineClient, EventFrame, OnEventFrame,
    ProgressDeadline, ProtocolError, ProtocolErrorSink, DEFAULT_HELLO_TIMEOUT,
};
#[cfg(feature = "store")]
pub use config::{
    ArtifactNames, ColumnType, Ext, ExtColumn, ProgressPolicy, StatusModel, StoreConfig, StoreHook,
};
#[cfg(feature = "contract")]
pub use contract::{
    parse_flags, run_cli, serve, CommandSet, Dispatcher, DoctorFn, Flags, JobCommands, Outcome,
    PayloadMapper, Sink, JOB_METHODS,
};
#[cfg(feature = "store")]
pub use error::OpenFailure;
pub use error::{
    ErrorOrigin, JobError, JobResult, CONTRACT_VERSION_MISMATCH, CURSOR_MISMATCH, DB_UNREADABLE,
    DELIVERY_UNKNOWN, HANDLER_UNSETTLED, HOST_HANDOFF, INTERRUPTED, INVALID_CURSOR, JOB_EXISTS,
    STALE_ATTEMPT, STALLED, SUPERSEDED,
};
#[cfg(feature = "store")]
pub use pool::{OutcomeFn, PoolConfig, Staged, StagedHandler, StagedPool};
#[cfg(feature = "store")]
/// The `rusqlite` this crate links (via `rightkit-db`); hooks receive its `Connection`.
pub use rightkit_db::rusqlite;
#[cfg(feature = "store")]
/// Recovery policy for [`JobStore::open_recovering`] (re-exported from `rightkit-db`; also under
/// [`db`]).
pub use rightkit_db::RecoveryPolicy;
#[cfg(feature = "store")]
pub use runner::{
    AttemptPolicy, EventSink, HandlerOutcome, JobControl, JobHandler, JobLock, JobLocks,
    RunOutcome, Runner, UnsettledRecovery, DEFAULT_MAX_ATTEMPTS,
};
#[cfg(feature = "store")]
pub use sched::{Gate, InteractiveGuard, Priority};
#[cfg(feature = "service")]
pub use service::DispatcherHandler;
#[cfg(feature = "store")]
pub use store::{
    Adoption, Artifact, CancelToken, ClaimScope, Cursor, DeleteOptions, DeleteReport, Event,
    FlushReport, ImportOptions, ImportRecord, Imported, Job, JobRecord, JobStore, ListFilter,
    ListPage, NewJob, OpenRecovery, ProgressStats, PrunePolicy, PruneReport, Recovery, Resolution,
    Settlement, SettlementShutdown, Submission, SubmissionRecord, SubmitOptions, Submitted,
    Transition, UnsettledJob, CANCELLED, COMPLETE, DELETING_TABLE, FAILED, MAX_EVENTS,
    MAX_IMPORT_BATCH, MAX_PENDING_SETTLEMENTS, MAX_PENDING_SETTLEMENT_BYTES, META_TABLE,
    PATH_OUTSIDE_ROOT, QUEUED, RUNNING, SUBMISSIONS_TABLE,
};
#[cfg(feature = "supervisor")]
pub use supervisor::{
    CallOptions, EngineSupervisor, ExitObserver, ExitReason, HealthCheck, InitialFailure,
    Lifecycle, LifecycleEvent, Readiness, ShutdownOutcome, ShutdownReport, SupervisorConfig,
    SupervisorStatus, Terminal,
};
#[cfg(feature = "crash")]
mod crash_observer;
#[cfg(feature = "crash")]
pub use crash_observer::CrashExitObserver;
#[cfg(feature = "protocol")]
pub use wire::{CONTRACT_VERSION, DEFAULT_FRAME_LIMIT};
// Re-exported so a host that only depends on rightkit-jobs can build the policy and spawn command.
#[cfg(feature = "supervisor")]
pub use rightkit_process::{JobAssignmentMode, OwnedCommand, RestartMode, RestartPolicy};

#[cfg(feature = "store")]
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
