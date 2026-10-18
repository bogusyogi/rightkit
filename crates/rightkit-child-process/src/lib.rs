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
mod launch;
pub mod login_shell;
#[cfg(any(unix, windows))]
mod native_capture;
mod no_window;
mod owned;
mod power;
mod process_handle;
mod process_table;
#[cfg(unix)]
mod process_unix;
#[cfg(windows)]
mod process_windows;
mod registry;
mod restart;
pub mod reveal;
mod run;
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
    find_processes, suspend_current_process, sweep, sweep_descendants, sweep_matching,
    AdoptedIdentity, AdoptedSignal, IdentityCheck, MatchSweepOptions, MatchSweepReport,
    MatchedProcess, ProcessQuery, SignalResult, StartToken, SweepReport,
};
#[cfg(unix)]
pub use foreign::{resume_session, signal_session, suspend_session};
pub use health::{HealthOutcome, HealthPolicy, HealthProbe, HealthTracker};
pub use launch::{launch_app, open_url};
#[cfg(any(unix, windows))]
pub use native_capture::NativeOutputCapture;
pub use no_window::{command_std, NoWindow, CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW};
pub use owned::{
    AdoptedProcess, BreakawayDenied, JobAssignmentFailure, JobAssignmentMode, OwnedChild,
    OwnedCommand, ProcessPriority, RawHandleOrFd, ReapOutcome, TerminationOptions, TreeTermination,
    UnixContainment, WaitOutcome, DEFAULT_DROP_REAP_TIMEOUT, DEFAULT_ENV_ALLOWLIST,
};
pub use power::{PowerAssertion, PowerKind};
pub use process_handle::ProcessHandle;
pub use process_table::{process_table, ProcessEntry};
pub use registry::{owned_count, terminate_all_owned, TerminateAllReport};
pub use restart::{
    ConsecutiveFailures, Generation, RestartMode, RestartOutcome, RestartPermit, RestartPolicy,
    RestartTracker,
};
pub use run::{run, RunOptions, RunOutput};
#[cfg(windows)]
pub use spawn_windows::{with_windows_spawn_lock, WindowsChild, WindowsSpawnConfig, WindowsStdio};
pub use stall::{
    Ack, Cooldown, HeartbeatConfig, HeartbeatEvent, HeartbeatWatch, StallConfig, StallEvent,
    StallWatchdog,
};
pub use stderr_tail::StderrTail;
#[cfg(unix)]
pub use unix_child::UnixChild;
pub use usage::{
    observe_idle, process_usage, BudgetLabels, IdleObservation, ProcessUsage, UsageRates,
};
#[cfg(windows)]
pub use windows_job::{ExitReason, WindowsJob, WindowsJobBuilder, DEFAULT_ACTIVE_PROCESS_LIMIT};
