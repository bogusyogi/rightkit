#![forbid(unsafe_code)]
//! Shared engine base: job store, runner, and stdio/CLI command contract.

mod client;
mod contract;
mod error;
mod runner;
mod store;

pub use client::EngineClient;
pub use contract::{
    parse_flags, run_cli, serve, CommandSet, Dispatcher, Flags, JobCommands, Outcome, Sink,
    CONTRACT_VERSION, DEFAULT_FRAME_LIMIT,
};
pub use error::{JobError, JobResult};
pub use runner::{
    EventSink, JobControl, JobHandler, JobLock, JobLocks, RunOutcome, Runner, DEFAULT_MAX_ATTEMPTS,
};
pub use store::{
    Artifact, Event, Job, JobStore, ListFilter, NewJob, CANCELLED, COMPLETE, FAILED, MAX_EVENTS,
    QUEUED, RUNNING,
};
