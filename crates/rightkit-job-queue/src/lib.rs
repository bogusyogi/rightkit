#![allow(clippy::result_large_err)] // errors carry structured diagnostic context by design, as in rightkit-agent-orchestration
#![deny(unsafe_code)]
//! Shared engine base: job store, runner, and stdio/CLI command contract.

#[cfg(feature = "supervisor")]
mod child_supervisor;
#[cfg(feature = "supervisor")]
mod client;
#[cfg(feature = "store")]
mod config;
#[cfg(feature = "contract")]
mod contract;
#[cfg(feature = "doctor")]
mod doctor_probe;
mod error;
#[cfg(feature = "ffi")]
#[allow(unsafe_code)]
pub mod ffi;
mod inline;
#[cfg(feature = "protocol")]
mod launch;
pub mod lease;
#[cfg(feature = "store")]
mod lock_queue;
#[cfg(feature = "doctor")]
mod one_shot;
#[cfg(feature = "store")]
pub mod pool;
#[cfg(feature = "store")]
mod runner;
#[cfg(feature = "store")]
mod sched;
#[cfg(feature = "service")]
mod service;
#[cfg(feature = "protocol")]
#[allow(unsafe_code)]
mod stdio_takeover;
#[cfg(feature = "store")]
mod store;
#[cfg(feature = "supervisor")]
mod supervisor;
#[cfg(feature = "protocol")]
mod wire;

#[cfg(feature = "supervisor")]
pub use child_supervisor::{
    ChildBoot, ChildSession, ChildStatus, ChildStdio, ChildSupervisor, ChildSupervisorConfig,
    DEFAULT_CHILD_READY_TIMEOUT,
};
#[cfg(feature = "supervisor")]
pub use client::{
    ChildExit, ClientOptions, ContractRequirement, EngineClient, EventFrame, OnEventFrame,
    ProgressDeadline, ProtocolError, ProtocolErrorSink, DEFAULT_HELLO_TIMEOUT,
};
#[cfg(feature = "store")]
pub use config::{
    ArtifactNames, ClockFn, ColumnType, Ext, ExtColumn, ProgressPolicy, StatusModel, StoreConfig,
    StoreHook, DEFAULT_JOBS_TABLE,
};
#[cfg(feature = "contract")]
pub use contract::{
    parse_flags, run_cli, serve, serve_report, CommandSet, Dispatcher, DoctorFn, ErrorRedactor,
    EventEmitter, Flags, JobCommands, Outcome, PayloadMapper, ReadinessFn, ServeReport, Sink,
    JOB_METHODS,
};
#[cfg(feature = "doctor")]
pub use doctor_probe::{
    parse_doctor, run_doctor, DoctorOptions, DoctorProbe, DEFAULT_DOCTOR_TIMEOUT,
    DOCTOR_CLEANUP_TIMEOUT, MAX_DOCTOR_BYTES, MAX_DOCTOR_CACHE_ENTRIES,
};
#[cfg(feature = "store")]
pub use error::OpenFailure;
pub use error::{
    ErrorOrigin, JobError, JobResult, CONTRACT_VERSION_MISMATCH, CURSOR_MISMATCH, DB_UNREADABLE,
    DELIVERY_UNKNOWN, HANDLER_UNSETTLED, HOST_HANDOFF, INTERRUPTED, INVALID_CURSOR, JOB_EXISTS,
    REVISION_CONFLICT, STALE_ATTEMPT, STALLED, SUPERSEDED,
};
pub use inline::{
    ActivityOptions, ActivityRegistry, ActivitySnapshot, CountUnit, Counts, InlineCancelRegistry,
    InlineJobGuard,
};
#[cfg(feature = "doctor")]
pub use one_shot::{
    OneShot, RawOutput, DEFAULT_CAPTURE_LIMIT, DEFAULT_MAX_ARG_BYTES, DEFAULT_ONE_SHOT_TIMEOUT,
    ONE_SHOT_CLEANUP_TIMEOUT,
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
    AttemptPolicy, EventSink, FifoScope, HandlerOutcome, JobControl, JobHandler, JobLock, JobLocks,
    LockWait, RunOutcome, Runner, UnsettledRecovery, DEFAULT_MAX_ATTEMPTS,
};
#[cfg(feature = "store")]
pub use sched::{Gate, InteractiveGuard, Priority};
#[cfg(feature = "service")]
pub use service::DispatcherHandler;
#[cfg(feature = "store")]
pub use store::{
    Admitted, Adoption, Artifact, CancelToken, ClaimScope, Cursor, DeferredFlush, DeferredKind,
    DeferredOp, DeleteLater, DeleteMode, DeleteOptions, DeleteReport, Event, Expect, FlushReport,
    ImportOptions, ImportRecord, Imported, Job, JobCursor, JobOrder, JobPage, JobQuery, JobRecord,
    JobStore, JobTable, ListFilter, ListPage, NewJob, OpenRecovery, OutboxEntry, ProgressStats,
    ProvisionalJob, PrunePolicy, PruneReport, Recovery, Resolution, Settlement, SettlementShutdown,
    Submission, SubmissionRecord, SubmitOptions, Submitted, Transition, UnsettledJob, CANCELLED,
    COMPLETE, DEFAULT_QUERY_LIMIT, DELETING_TABLE, FAILED, MAX_DEFERRED_OPS, MAX_EVENTS,
    MAX_IMPORT_BATCH, MAX_OUTBOX_READ, MAX_PENDING_SETTLEMENTS, MAX_PENDING_SETTLEMENT_BYTES,
    MAX_QUERY_LIMIT, META_TABLE, OUTBOX_CURSOR_TABLE, OUTBOX_TABLE, OWNER_TABLE, PATH_OUTSIDE_ROOT,
    QUEUED, RUNNING, SUBMISSIONS_TABLE, UNCERTAIN,
};
#[cfg(feature = "supervisor")]
pub use supervisor::{
    CallOptions, EngineSupervisor, ExitObserver, ExitReason, HandshakeCall, HandshakeMode,
    HealthCheck, InitialFailure, LaunchFrame, Lifecycle, LifecycleEvent, RawParams, Readiness,
    ShutdownOutcome, ShutdownReport, SupervisorConfig, SupervisorStatus, Terminal,
    LAUNCH_FRAME_FAILED, MAX_LAUNCH_FRAME_BYTES,
};
/// The buffer type [`HandshakeCall::new`] takes for its params, re-exported so a host builds it
/// with the `zeroize` this crate links.
#[cfg(feature = "supervisor")]
pub use zeroize::Zeroizing;
#[cfg(feature = "crash")]
mod crash_observer;
#[cfg(feature = "crash")]
pub use crash_observer::CrashExitObserver;
#[cfg(feature = "protocol")]
pub use launch::{read_launch_frame, LaunchError};
#[cfg(feature = "protocol")]
pub use stdio_takeover::take_frame_io;
#[cfg(feature = "protocol")]
pub use wire::{finite_frame_limit, CONTRACT_VERSION, DEFAULT_FRAME_LIMIT, MAX_FRAME_LIMIT};
// Re-exported so a host that only depends on rightkit-job-queue can build the policy and spawn command.
#[cfg(feature = "supervisor")]
pub use rightkit_child_process::{JobAssignmentMode, OwnedCommand, RestartMode, RestartPolicy};

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
