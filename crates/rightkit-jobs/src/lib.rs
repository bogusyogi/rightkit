#![deny(unsafe_code)]
//! Shared engine base: job store, runner, and stdio/CLI command contract.

mod client;
mod config;
mod contract;
mod error;
mod runner;
mod sched;
#[cfg(feature = "service")]
mod service;
mod store;
mod supervisor;

pub use client::{
    ClientOptions, ContractRequirement, EngineClient, EventFrame, OnEventFrame, ProtocolError,
    ProtocolErrorSink, DEFAULT_HELLO_TIMEOUT,
};
pub use config::{ArtifactNames, ColumnType, Ext, ExtColumn, StatusModel, StoreConfig, StoreHook};
pub use contract::{
    parse_flags, run_cli, serve, CommandSet, Dispatcher, DoctorFn, Flags, JobCommands, Outcome,
    PayloadMapper, Sink, CONTRACT_VERSION, DEFAULT_FRAME_LIMIT, JOB_METHODS,
};
pub use error::{
    ErrorOrigin, JobError, JobResult, CONTRACT_VERSION_MISMATCH, HOST_HANDOFF, STALE_ATTEMPT,
    STALLED,
};
/// The `rusqlite` this crate links (via `rightkit-db`); hooks receive its `Connection`.
pub use rightkit_db::rusqlite;
pub use runner::{
    AttemptPolicy, EventSink, JobControl, JobHandler, JobLock, JobLocks, RunOutcome, Runner,
    DEFAULT_MAX_ATTEMPTS,
};
pub use sched::{Gate, InteractiveGuard, Priority};
#[cfg(feature = "service")]
pub use service::DispatcherHandler;
pub use store::{
    Adoption, Artifact, Event, Job, JobRecord, JobStore, ListFilter, NewJob, Submission,
    SubmissionRecord, Transition, CANCELLED, COMPLETE, FAILED, MAX_EVENTS, QUEUED, RUNNING,
    SUBMISSIONS_TABLE,
};
pub use supervisor::{
    CallOptions, EngineSupervisor, ExitReason, HealthCheck, InitialFailure, Lifecycle,
    LifecycleEvent, Readiness, SupervisorConfig, SupervisorStatus, Terminal,
};
// Re-exported so a host that only depends on rightkit-jobs can build the policy and spawn command.
pub use rightkit_process::{JobAssignmentMode, OwnedCommand, RestartMode, RestartPolicy};
