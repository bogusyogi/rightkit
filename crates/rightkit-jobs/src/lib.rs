#![forbid(unsafe_code)]
//! Shared engine base: job store, runner, and stdio/CLI command contract.

mod client;
mod contract;
mod error;
mod runner;
mod sched;
mod service;
mod store;
mod supervisor;

pub use client::{ClientOptions, EngineClient, DEFAULT_HELLO_TIMEOUT};
pub use supervisor::{EngineSupervisor, ExitReason, Lifecycle, LifecycleEvent, SupervisorConfig, SupervisorStatus, HealthCheck};
pub use contract::{
    parse_flags, run_cli, serve, CommandSet, Dispatcher, Flags, JobCommands, Outcome, Sink,
    CONTRACT_VERSION, DEFAULT_FRAME_LIMIT,
};
pub use error::{JobError, JobResult, HOST_HANDOFF, STALLED};
pub use sched::{Gate, InteractiveGuard, Priority};
pub use service::DispatcherHandler;
pub use runner::{
    EventSink, JobControl, JobHandler, JobLock, JobLocks, RunOutcome, Runner, DEFAULT_MAX_ATTEMPTS,
};
pub use store::{
    Artifact, Event, Job, JobStore, ListFilter, NewJob, CANCELLED, COMPLETE, FAILED, MAX_EVENTS,
    QUEUED, RUNNING,
};
