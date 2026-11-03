//! `OwnedCommand::inherit_handles_only` restricts inheritance to caller handles/fds plus stdio (Unix children inherit no other descriptor by default, `inherit_fds` adds some, `inherit_all_descriptors(true)` opts out); Windows requires `windows_spawn_config` for opaque Command settings.

mod capture;
mod child_usage;
#[cfg(unix)]
mod descriptors;
#[cfg(unix)]
mod exit_watch;
mod foreign;
mod health;
#[cfg(unix)]
mod identity;
mod journal;
mod launch;
pub mod login_shell;
#[cfg(any(unix, windows))]
mod native_capture;
mod no_window;
mod owned;
#[cfg(feature = "tokio")]
mod owned_tokio;
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
mod terminal;
#[cfg(feature = "tokio")]
pub mod tokio_support;
#[cfg(unix)]
mod unix_child;
mod usage;
mod usage_pid;
#[cfg(windows)]
pub mod windows_job;

pub use capture::{read_bounded, CapturedOutput, OutputCapture};
pub use child_usage::ChildUsage;
#[cfg(unix)]
pub use descriptors::SanitizeDescriptors;
pub use foreign::{
    find_processes, process_group_exists, suspend_current_process, sweep, sweep_descendants,
    sweep_matching, wait_process_group_empty, AdoptedIdentity, AdoptedSignal, CloseRequest,
    IdentityCheck, MatchSweepOptions, MatchSweepReport, MatchedProcess, ProcessQuery, SignalResult,
    StartToken, SweepReport,
};
#[cfg(unix)]
pub use foreign::{resume_session, signal_session, suspend_session};
pub use health::{HealthOutcome, HealthPolicy, HealthProbe, HealthTracker};
pub use journal::{recover as recover_journal, JournalRecovery, WorkerJournal};
pub use launch::{launch_app, open_url};
#[cfg(any(unix, windows))]
pub use native_capture::NativeOutputCapture;
pub use no_window::{command_std, NoWindow, CREATE_BREAKAWAY_FROM_JOB, CREATE_NO_WINDOW};
pub use owned::{
    AdoptedProcess, BreakawayDenied, JobAssignmentFailure, JobAssignmentMode, OwnedChild,
    OwnedCommand, ProcessPriority, RawHandleOrFd, ReapOutcome, TerminationOptions, TreeTermination,
    UnixContainment, WaitOutcome, DEFAULT_DROP_REAP_TIMEOUT, DEFAULT_ENV_ALLOWLIST,
};
#[cfg(feature = "tokio")]
pub use owned_tokio::{AsyncStderr, AsyncStdin, AsyncStdout, OwnedTokioChild};
pub use power::{PowerAssertion, PowerKind};
pub use process_handle::ProcessHandle;
pub use process_table::{process_table, ProcessEntry};
pub use registry::{owned_count, terminate_all_owned, TerminateAllReport};
pub use restart::{
    ConsecutiveFailures, Generation, RestartMode, RestartOutcome, RestartPermit, RestartPolicy,
    RestartTracker,
};
pub use run::{
    run, run_command, run_command_capture, run_command_capture_secret_stdin, run_streaming,
    run_with_stdin, status_with_timeout, CaptureOptions, LeaderTermination, RunOptions, RunOutput,
    RunnerFailure, RunnerPhase, StreamOptions, StreamOutput,
};
#[cfg(windows)]
pub use spawn_windows::{with_windows_spawn_lock, WindowsChild, WindowsSpawnConfig, WindowsStdio};
pub use stall::{
    Ack, Cooldown, HeartbeatConfig, HeartbeatEvent, HeartbeatWatch, StallConfig, StallEvent,
    StallWatchdog,
};
pub use stderr_tail::StderrTail;
pub use terminal::status_inherited_terminal;
#[cfg(unix)]
pub use unix_child::UnixChild;
pub use usage::{
    observe_idle, observe_idle_with_threads, process_usage, process_usage_with_threads,
    thread_deltas, tree_usage, BudgetLabels, IdleObservation, IdleThreadObservation,
    ProcessThreads, ProcessUsage, ThreadDelta, ThreadDeltas, ThreadSample, TreeUsage, UsageRates,
};
pub use usage_pid::{
    process_usage_of, process_usage_of_identity, run_soak, PidUsage, SoakEnd, SoakSampler,
};
#[cfg(windows)]
pub use windows_job::{ExitReason, WindowsJob, WindowsJobBuilder, DEFAULT_ACTIVE_PROCESS_LIMIT};
