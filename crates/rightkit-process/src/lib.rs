//! `OwnedCommand::inherit_handles_only` restricts inheritance to caller handles/fds plus stdio; Windows requires `windows_spawn_config` for opaque Command settings.

mod health;
#[cfg(all(test, unix))]
mod identity_tests;
mod owned;
mod process_handle;
#[cfg(unix)]
mod process_unix;
#[cfg(windows)]
mod process_windows;
mod restart;
#[cfg(windows)]
mod spawn_windows;
mod stall;
mod stderr_tail;
#[cfg(unix)]
mod unix_child;
mod usage;
#[cfg(windows)]
pub mod windows_job;

pub use health::{HealthOutcome, HealthPolicy, HealthProbe, HealthTracker};
#[cfg(windows)]
pub use owned::DetachedBreakaway;
pub use owned::{
    AdoptedProcess, JobAssignmentFailure, JobAssignmentMode, OwnedChild, OwnedCommand,
    RawHandleOrFd, ReapOutcome, TerminationOptions, UnixContainment, WaitOutcome,
    DEFAULT_ENV_ALLOWLIST,
};
pub use process_handle::ProcessHandle;
pub use restart::{
    Generation, RestartMode, RestartOutcome, RestartPermit, RestartPolicy, RestartTracker,
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
