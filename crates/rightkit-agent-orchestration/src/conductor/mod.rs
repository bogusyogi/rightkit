//! The conductor — runs a plan of worker tasks sequentially or in parallel.

pub mod execution;
pub mod types;

pub use execution::{synthesize_worker_outputs, Conductor, EchoWorker, FailWorker, Worker};
pub use types::{ConductorPlan, WorkerCollection, WorkerOutcome, WorkerResult, WorkerTask};
