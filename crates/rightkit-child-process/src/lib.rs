//! `OwnedCommand::inherit_handles_only` restricts inheritance to caller handles/fds plus stdio; Windows requires `windows_spawn_config` for opaque Command settings.

mod capture;
#[cfg(unix)]
mod exit_watch;
mod foreign;
mod health;
#[cfg(unix)]
mod identity;
#[cfg(all(test, unix))]
mod identity_tests;
mod no_window;
mod owned;
mod process_handle;
#[cfg(unix)]
mod process_unix;
#[cfg(windows)]
mod process_windows;
mod registry;
mod restart;
pub mod reveal;
#[cfg(windows)]
mod spawn_windows;
mod stall;
mod stderr_tail;
#[cfg(feature = "tokio")]
pub mod tokio_support;
#[cfg(unix)]
mod unix_child;
mod usage;
#[cfg(windows)]
pub mod windows_job;

pub use capture::{read_bounded, CapturedOutput, OutputCapture};
pub use foreign::{
    sweep, sweep_descendants, AdoptedIdentity, AdoptedSignal, IdentityCheck, SignalResult,
    StartToken, SweepReport,
};
pub use health::{HealthOutcome, HealthPolicy, HealthProbe, HealthTracker};
pub use no_window::{NoWindow, CREATE_NO_WINDOW};
pub use owned::{
    AdoptedProcess, JobAssignmentFailure, JobAssignmentMode, OwnedChild, OwnedCommand,
    ProcessPriority, RawHandleOrFd, ReapOutcome, TerminationOptions, TreeTermination,
    UnixContainment, WaitOutcome, DEFAULT_DROP_REAP_TIMEOUT, DEFAULT_ENV_ALLOWLIST,
};
pub use process_handle::ProcessHandle;
pub use registry::{owned_count, terminate_all_owned, TerminateAllReport};
pub use restart::{
    ConsecutiveFailures, Generation, RestartMode, RestartOutcome, RestartPermit, RestartPolicy,
    RestartTracker,
};
#[cfg(windows)]
pub use spawn_windows::{with_windows_spawn_lock, WindowsChild, WindowsSpawnConfig, WindowsStdio};
pub use stall::{Ack, Cooldown, StallConfig, StallEvent, StallWatchdog};
pub use stderr_tail::StderrTail;
#[cfg(unix)]
pub use unix_child::UnixChild;
pub use usage::{
    observe_idle, process_usage, BudgetLabels, IdleObservation, ProcessUsage, UsageRates,
};
#[cfg(windows)]
pub use windows_job::{WindowsJob, DEFAULT_ACTIVE_PROCESS_LIMIT};
