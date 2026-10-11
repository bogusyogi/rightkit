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
    ErrorOrigin, JobError, JobResult, CONTRACT_VERSION_MISMATCH, HOST_HANDOFF, INTERRUPTED,
    STALE_ATTEMPT, STALLED, SUPERSEDED,
};
pub use pool::{OutcomeFn, PoolConfig, Staged, StagedHandler, StagedPool};
/// The `rusqlite` this crate links (via `rightkit-db`); hooks receive its `Connection`.
pub use rightkit_db::rusqlite;
/// Recovery policy for [`JobStore::open_recovering`] (re-exported from `rightkit-db`).
pub use rightkit_db::RecoveryPolicy;
pub use runner::{
    AttemptPolicy, EventSink, JobControl, JobHandler, JobLock, JobLocks, RunOutcome, Runner,
    DEFAULT_MAX_ATTEMPTS,
};
pub use sched::{Gate, InteractiveGuard, Priority};
#[cfg(feature = "service")]
pub use service::DispatcherHandler;
pub use store::{
    Adoption, Artifact, CancelToken, Event, Job, JobRecord, JobStore, ListFilter, NewJob,
    ProgressStats, PrunePolicy, PruneReport, Recovery, Resolution, Submission, SubmissionRecord,
    SubmitOptions, Submitted, Transition, CANCELLED, COMPLETE, FAILED, MAX_EVENTS, META_TABLE,
    QUEUED, RUNNING, SUBMISSIONS_TABLE,
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
