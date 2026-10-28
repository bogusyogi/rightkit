use crate::process_handle::{lock_child, ProcessHandle};
#[cfg(unix)]
use crate::unix_child::{LeaderState, SignalOutcome, SignalTarget};
use std::{
    ffi::OsStr,
    io,
    num::NonZeroU32,
    process::{ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio},
    sync::{Arc, Mutex, MutexGuard},
    thread,
    time::{Duration, Instant, SystemTime},
};
#[cfg(unix)]
type PlatformChild = crate::unix_child::UnixChild;
#[cfg(windows)]
type PlatformChild = crate::spawn_windows::WindowsChild;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum JobAssignmentMode {
    #[default]
    Strict,
    BestEffort,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UnixContainment {
    #[default]
    ProcessGroup,
    Session,
}

/// Scheduling priority requested for the child. `Normal` (the default) means "inherit": it
/// leaves the spawn exactly as it was before this option existed, so the child keeps the
/// parent's priority class (Windows) or nice value (Unix). It does not reset a lowered parent
/// back to normal. The other values only lower priority, which needs no
/// privilege and never raises a value the child already inherits above a lower one.
///
/// Windows: `BelowNormal` adds `BELOW_NORMAL_PRIORITY_CLASS` and `Idle` adds
/// `IDLE_PRIORITY_CLASS` to the creation flags. Unix: the child's nice value is raised to at
/// least 10 (`BelowNormal`) or 19 (`Idle`) between fork and exec.
///
/// `Efficient` is `BelowNormal` plus the platform's energy-efficiency class, for background
/// work that should not wake the fast cores. Windows: `BELOW_NORMAL_PRIORITY_CLASS` plus
/// EcoQoS (`PROCESS_POWER_THROTTLING_EXECUTION_SPEED` through `SetProcessInformation`, set
/// while the child is still suspended; a Windows without EcoQoS ignores it, best effort).
/// macOS: nice 10 and the `QOS_CLASS_UTILITY` quality of service on the child's initial
/// thread (set between fork and exec; advisory). Other Unix: nice 10.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum ProcessPriority {
    #[default]
    Normal,
    BelowNormal,
    Idle,
    Efficient,
}

impl ProcessPriority {
    /// Minimum Unix nice value for this priority; `None` leaves the inherited value alone.
    #[cfg(unix)]
    pub(crate) fn nice_floor(self) -> Option<libc::c_int> {
        match self {
            Self::Normal => None,
            Self::BelowNormal | Self::Efficient => Some(10),
            Self::Idle => Some(19),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminationOptions {
    /// Send TERM immediately on Unix, then KILL after this tree-wide grace.
    pub grace: Duration,
    /// Passed to TerminateJobObject/TerminateProcess on Windows.
    pub windows_exit_code: u32,
}
impl Default for TerminationOptions {
    fn default() -> Self {
        Self {
            grace: Duration::ZERO,
            windows_exit_code: 1,
        }
    }
}
impl TerminationOptions {
    pub fn with_grace(mut self, grace: Duration) -> Self {
        self.grace = grace;
        self
    }
    pub fn with_windows_exit_code(mut self, code: u32) -> Self {
        self.windows_exit_code = code;
        self
    }
}

/// Every failed assignment in a best-effort Windows spawn is retained.
/// Windows job breakaway policy for an owned child; Unix ignores it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum BreakawayMode {
    /// Full containment.
    #[default]
    None,
    /// `JOB_OBJECT_LIMIT_BREAKAWAY_OK`: only processes that ask leave the job.
    Explicit,
    /// `BREAKAWAY_OK` plus `SILENT_BREAKAWAY_OK`: every created process leaves the job.
    Silent,
}

#[derive(Debug)]
pub struct JobAssignmentFailure {
    pub stage: &'static str,
    pub error: io::Error,
}
impl JobAssignmentFailure {
    pub fn raw_os_error(&self) -> Option<i32> {
        self.error.raw_os_error()
    }
}

/// Borrowed OS handle/fd value; keep it valid until spawn returns.
#[cfg(windows)]
pub type RawHandleOrFd = std::os::windows::io::RawHandle;
#[cfg(unix)]
pub type RawHandleOrFd = std::os::fd::RawFd;
#[cfg(windows)]
type InheritedValue = usize;
#[cfg(unix)]
type InheritedValue = RawHandleOrFd;

#[derive(Debug)]
pub struct OwnedCommand {
    command: Command,
    windows_hidden: bool,
    windows_breakaway: BreakawayMode,
    caller_job: CallerJob,
    job_assignment: JobAssignmentMode,
    unix_containment: UnixContainment,
    process_priority: ProcessPriority,
    kill_on_parent_exit: bool,
    // Set by `spawn_suspended`: Windows leaves the child suspended until `OwnedChild::resume`.
    hold_suspended: bool,
    // None preserves the existing unbounded partial-spawn reap.
    partial_spawn_cleanup_timeout: Option<Duration>,
    inherited_handles: Option<Vec<InheritedValue>>,
    // Unix: descriptors that stay open in the child on top of `inherited_handles`.
    #[cfg(unix)]
    inherited_fds: Vec<std::os::fd::RawFd>,
    // Opt-out of descriptor/handle sanitation (Orthic-Labs/rightkit#835).
    inherit_all_descriptors: bool,
    #[cfg(windows)]
    windows_spawn_config: Option<crate::WindowsSpawnConfig>,
    #[cfg(windows)]
    windows_spawn_config_auto: bool,
    // Set by `pipe_stdio` (the `run` family). Applied at spawn to BOTH launch paths, after every
    // caller edit, so a later `command_mut` cannot undo it.
    run_stdio: Option<RunStdio>,
    env_cleared: bool,
}

/// The piped stdio contract of the bounded runners: stdout and stderr are always piped, stdin
/// is piped when the caller has input to write and null otherwise.
#[derive(Clone, Copy, Debug)]
struct RunStdio {
    stdin_piped: bool,
}
#[cfg(windows)]
type CallerJob = Option<std::os::windows::io::OwnedHandle>;
#[cfg(not(windows))]
type CallerJob = Option<std::convert::Infallible>;

impl OwnedCommand {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        let command = Self::from_command(Command::new(program));
        #[cfg(windows)]
        let command = {
            let mut command = command;
            // Only a fresh Command has known inherited stdio/environment defaults. The
            // config is marked auto, and `command_mut` clears it.
            command.windows_spawn_config = Some(crate::WindowsSpawnConfig::default());
            command.windows_spawn_config_auto = true;
            command
        };
        command
    }
    /// GUI/service children are hidden on Windows by default. A wrapped `Command` may carry
    /// stdio, environment and raw-argument settings this crate cannot read back, so on
    /// Windows an allowlisted spawn (`inherit_handles_only`) from `from_command` requires an
    /// explicit `windows_spawn_config`. The crate never widens what a child inherits by
    /// default.
    pub fn from_command(command: Command) -> Self {
        Self {
            command,
            windows_hidden: true,
            windows_breakaway: BreakawayMode::None,
            caller_job: None,
            job_assignment: JobAssignmentMode::Strict,
            unix_containment: UnixContainment::ProcessGroup,
            process_priority: ProcessPriority::Normal,
            kill_on_parent_exit: false,
            hold_suspended: false,
            partial_spawn_cleanup_timeout: None,
            inherited_handles: None,
            #[cfg(unix)]
            inherited_fds: Vec::new(),
            inherit_all_descriptors: false,
            #[cfg(windows)]
            windows_spawn_config: None,
            #[cfg(windows)]
            windows_spawn_config_auto: false,
            run_stdio: None,
            env_cleared: false,
        }
    }
    /// Crate-private: the one place that decides the stdio of a bounded run. It records the
    /// request instead of editing `Command` now, and `spawn` applies it to the std `Command`
    /// and, for an allowlisted Windows spawn, to the native [`WindowsSpawnConfig`] (explicit or
    /// auto) in one step. It deliberately does not call [`command_mut`](Self::command_mut), which
    /// would clear an auto config. Environment, working directory, raw arguments and every
    /// security setting are left as the caller configured them.
    pub(crate) fn pipe_stdio(&mut self, stdin_piped: bool) -> &mut Self {
        self.run_stdio = Some(RunStdio { stdin_piped });
        self
    }
    pub fn command_mut(&mut self) -> &mut Command {
        #[cfg(windows)]
        if self.windows_spawn_config_auto {
            // A mutable std Command can change settings with no stable getter.
            self.windows_spawn_config = None;
        }
        &mut self.command
    }
    /// Inherit only these handles/fds plus configured stdio; values stay caller-owned.
    pub fn inherit_handles_only(
        &mut self,
        handles: impl IntoIterator<Item = RawHandleOrFd>,
    ) -> &mut Self {
        #[cfg(windows)]
        let handles = handles.into_iter().map(|handle| handle as usize).collect();
        #[cfg(unix)]
        let handles = handles.into_iter().collect();
        self.inherited_handles = Some(handles);
        self
    }
    /// Unix: keeps descriptor `fd` open in the child, in addition to stdio and anything listed
    /// through [`inherit_handles_only`](Self::inherit_handles_only) or other `inherit_fd(s)`
    /// calls. By default a spawned child inherits no descriptor above stderr: every one is
    /// marked close-on-exec between `fork` and `exec` (Orthic-Labs/rightkit#835). Listing `fd`
    /// clears that flag in the child only; the parent's flags are not changed. The descriptor
    /// must be open when [`spawn`](Self::spawn) runs, else the spawn fails with `EBADF`, and
    /// stays caller-owned. Use this, not a `pre_exec` of your own on
    /// [`command_mut`](Self::command_mut): the sweep is registered at spawn, after every hook
    /// you added, and would close what such a hook opened. Descriptors 0 to 2 are stdio and
    /// follow the command's stdio settings. Not available on Windows.
    #[cfg(unix)]
    pub fn inherit_fd(&mut self, fd: std::os::fd::RawFd) -> &mut Self {
        self.inherited_fds.push(fd);
        self
    }
    /// Unix: [`inherit_fd`](Self::inherit_fd) for each descriptor in `fds`.
    #[cfg(unix)]
    pub fn inherit_fds(&mut self, fds: &[std::os::fd::RawFd]) -> &mut Self {
        self.inherited_fds.extend_from_slice(fds);
        self
    }
    /// Opts out of descriptor sanitation. Unsafe by policy: with `true`, a Unix child inherits
    /// every descriptor of the parent that is not itself close-on-exec, a sandboxed child
    /// included, which is exactly what a sandbox must not allow (Orthic-Labs/rightkit#835).
    /// On Windows it keeps the ordinary `CreateProcess` with handle inheritance on and no
    /// handle list, which hands the child every inheritable handle of the parent. Only a caller
    /// that passes descriptors it cannot name (a launcher forwarding an unknown inherited set)
    /// has a reason. Conflicts with [`inherit_handles_only`](Self::inherit_handles_only) and
    /// `inherit_fd(s)`: the spawn fails with `InvalidInput` instead of guessing which wins.
    pub fn inherit_all_descriptors(&mut self, enabled: bool) -> &mut Self {
        self.inherit_all_descriptors = enabled;
        self
    }
    /// Supply opaque stdio/environment/raw-arg settings for native allowlisted spawns.
    /// Required after from_command/command_mut when inherit_handles_only is set.
    /// Ordinary std spawns ignore it; fresh new() Commands have known defaults. The bounded
    /// runners (`run`, `run_command`, `run_streaming`) replace its stdin, stdout and stderr with
    /// the pipes they capture, whether the config is explicit or automatic, and keep the rest.
    #[cfg(windows)]
    pub fn windows_spawn_config(&mut self, config: crate::WindowsSpawnConfig) -> &mut Self {
        self.windows_spawn_config = Some(config);
        self.windows_spawn_config_auto = false;
        self
    }
    /// Clear environment, then copy only named variables set in this process.
    pub fn env_allowlist<I, S>(&mut self, keys: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let kept: Vec<_> = keys
            .into_iter()
            .filter_map(|k| std::env::var_os(k.as_ref()).map(|v| (k.as_ref().to_os_string(), v)))
            .collect();
        self.command.env_clear();
        self.env_cleared = true;
        for (k, v) in kept {
            self.command.env(k, v);
        }
        self
    }
    /// [`Self::env_allowlist`] over [`DEFAULT_ENV_ALLOWLIST`] plus `extra` (a consumer's own
    /// variables, for instance a model-cache location). Call it once: like `env_allowlist` it
    /// starts from an empty environment, so a second call drops what the first kept.
    pub fn env_allowlist_with_defaults<I, S>(&mut self, extra: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let keys: Vec<std::ffi::OsString> = DEFAULT_ENV_ALLOWLIST
            .iter()
            .map(std::ffi::OsString::from)
            .chain(extra.into_iter().map(|k| k.as_ref().to_os_string()))
            .collect();
        self.env_allowlist(keys)
    }
    /// True once [`Self::env_allowlist`] (or `env_allowlist_with_defaults`) cleared the inherited
    /// environment on this command. A `Command` that was cleared with `env_clear` directly, or
    /// wrapped by `from_command` after it was cleared, reads `false`: the state is not observable
    /// from `std::process::Command`.
    pub fn env_is_cleared(&self) -> bool {
        self.env_cleared
    }
    pub fn env_strip<I, S>(&mut self, keys: I) -> &mut Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for k in keys {
            self.command.env_remove(k);
        }
        self
    }
    /// Compatibility no-op: hidden is already the default.
    pub fn windows_hide(&mut self) -> &mut Self {
        self.windows_hidden = true;
        self
    }
    /// Windows: leaves the console window visible, clearing the hidden default, for a
    /// user-facing program (an editor, a terminal at a project path) that must keep its window.
    /// Pair it with [`spawn_uncontained_detached`](Self::spawn_uncontained_detached) for a
    /// fire-and-forget launch. Unix ignores it.
    pub fn windows_visible(&mut self) -> &mut Self {
        self.windows_hidden = false;
        self
    }
    /// Windows: explicit-only breakaway (`JOB_OBJECT_LIMIT_BREAKAWAY_OK`). A process in the
    /// job leaves it only by asking (`CREATE_BREAKAWAY_FROM_JOB`); every descendant that does
    /// not ask stays contained and is terminated with the job. Dropping the owned child still
    /// terminates the helper's tree; a grandchild that asked and broke away stays running.
    /// `false` restores full containment. Unix ignores it. For descendants that escape without
    /// asking, see [`windows_allow_silent_breakaway`](Self::windows_allow_silent_breakaway).
    pub fn windows_allow_breakaway(&mut self, allow: bool) -> &mut Self {
        self.windows_breakaway = if allow {
            BreakawayMode::Explicit
        } else {
            BreakawayMode::None
        };
        self
    }
    /// Windows: silent breakaway (`JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK` plus `BREAKAWAY_OK`).
    /// Every process the job's processes create is outside the job without asking, so
    /// descendants are NOT contained and survive the helper. Use only for a helper whose
    /// children must all outlive it. `false` restores full containment. Unix ignores it.
    pub fn windows_allow_silent_breakaway(&mut self, allow: bool) -> &mut Self {
        self.windows_breakaway = if allow {
            BreakawayMode::Silent
        } else {
            BreakawayMode::None
        };
        self
    }
    /// Duplicate caller job; assign before the empty nested RightKit job.
    #[cfg(windows)]
    pub fn windows_job(
        &mut self,
        job: std::os::windows::io::BorrowedHandle<'_>,
    ) -> io::Result<&mut Self> {
        self.caller_job = Some(job.try_clone_to_owned()?);
        Ok(self)
    }
    /// Ignored on Unix. BestEffort reports errors on OwnedChild.
    pub fn job_assignment_mode(&mut self, mode: JobAssignmentMode) -> &mut Self {
        self.job_assignment = mode;
        self
    }
    /// Ignored on Windows. Session runs setsid before exec.
    pub fn unix_containment(&mut self, mode: UnixContainment) -> &mut Self {
        self.unix_containment = mode;
        self
    }
    /// Scheduling priority for the child. The default, `ProcessPriority::Normal`, adds nothing
    /// to the spawn. See `ProcessPriority` for the Windows and Unix mappings. Applies to
    /// detached spawns too.
    pub fn process_priority(&mut self, priority: ProcessPriority) -> &mut Self {
        self.process_priority = priority;
        self
    }
    /// Opt-in, off by default. Linux only: the kernel sends `SIGTERM` to the direct child
    /// (`PR_SET_PDEATHSIG`) when the thread that spawned it exits, which includes this
    /// process dying without running cleanup (for example SIGKILL). The signal reaches the
    /// direct child only, never the rest of its group, it can be caught, and there is no
    /// `SIGKILL` escalation. It is keyed to the spawning *thread*, so spawn from a thread
    /// that lives as long as the child.
    ///
    /// macOS has no `PR_SET_PDEATHSIG`, so the tie is a small watchdog process started right
    /// after the child (a `kqueue` with `EVFILT_PROC`/`NOTE_EXIT` on this process and on the
    /// child; see `process_unix::arm_parent_death_watch`). When this process dies, SIGKILL
    /// included, the watchdog sends `SIGTERM` to the child's whole process group and `SIGKILL`
    /// two seconds later. Differences from Linux: the trigger is the whole process exiting,
    /// not the spawning thread; the signal goes to the group and escalates; it costs one
    /// extra helper process per tied child (it exits when the child or this process does);
    /// the child runs untied between its spawn and the moment `spawn` returns (a few
    /// milliseconds), and `spawn` kills the child and fails if the watchdog cannot be armed.
    ///
    /// Other Unix hosts have no parent-death primitive this crate relies on: the spawn fails
    /// with `ErrorKind::Unsupported` before anything is started, rather than run the child
    /// untied. Windows ignores the option because kill-on-close job objects already cover
    /// parent death. Rejected (`InvalidInput`) for detached spawns.
    pub fn kill_on_parent_exit(&mut self, enabled: bool) -> &mut Self {
        self.kill_on_parent_exit = enabled;
        self
    }
    /// Bound failed-spawn cleanup. Timed-out children go to a background reaper.
    pub fn partial_spawn_cleanup_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.partial_spawn_cleanup_timeout = Some(timeout);
        self
    }
    pub(crate) fn bound_runner_cleanup(&mut self, timeout: Duration) {
        self.partial_spawn_cleanup_timeout = Some(
            self.partial_spawn_cleanup_timeout
                .map_or(timeout, |current| current.min(timeout)),
        );
    }
    pub fn spawn(self) -> io::Result<OwnedChild> {
        self.spawn_inner(false, false, None)
    }
    /// Like [`OwnedCommand::spawn`], but on Windows the child is created suspended, assigned to
    /// its Job Object, and held there: it executes no instruction until
    /// [`OwnedChild::resume`] is called. A caller can persist the child's identity (pid and
    /// start time) first, then release it. Dropping a child that was never resumed terminates it
    /// like any owned child.
    ///
    /// Unix has no suspended creation: this is `spawn`, the child runs at once, and `resume` is
    /// a no-op that returns `Ok`. A caller that needs a hold on Unix gates the child itself (for
    /// example a pipe the child blocks reading).
    pub fn spawn_suspended(mut self) -> io::Result<OwnedChild> {
        self.hold_suspended = true;
        self.spawn_inner(false, false, None)
    }
    /// Shared launch with no owner job or terminate-on-drop. Unix uses setsid.
    /// Stdio follows Command: dropping piped stdin can still deliver EOF.
    ///
    /// Windows first asks to break away from the caller's job object
    /// (`CREATE_BREAKAWAY_FROM_JOB`). If the caller's job denies breakaway
    /// (`ERROR_ACCESS_DENIED`), the launch is retried once without it, as
    /// `CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP` (`DETACHED_PROCESS` if `show_window`) with the same stdio and handle
    /// allowlist (an ordinary spawn without an allowlist still inherits what std inherits).
    /// The child is then detached from the console and process group but stays inside the
    /// caller's job, so the job's limits (including kill-on-close) still reach it. Check
    /// [`OwnedChild::stayed_in_parent_job`] to learn which happened. A caller that must not
    /// run inside the parent's job uses
    /// [`spawn_uncontained_detached_strict`](Self::spawn_uncontained_detached_strict). Any
    /// other launch failure is returned as is, without a retry. Unix behaviour is unchanged.
    pub fn spawn_uncontained_detached(self) -> io::Result<OwnedChild> {
        self.spawn_inner_with(true, false, None, true)
    }
    /// Like [`spawn_uncontained_detached`](Self::spawn_uncontained_detached), but never falls
    /// back. On Windows, when the caller's job denies breakaway the launch fails with an
    /// [`io::Error`] of kind `PermissionDenied` wrapping [`BreakawayDenied`] (recover it with
    /// `error.get_ref().and_then(|e| e.downcast_ref::<BreakawayDenied>())`), and nothing is
    /// started. On Unix this is identical to `spawn_uncontained_detached`.
    pub fn spawn_uncontained_detached_strict(self) -> io::Result<OwnedChild> {
        match self.spawn_inner_with(true, false, None, false) {
            Err(error) if cfg!(windows) && is_denied_breakaway(&error) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                BreakawayDenied,
            )),
            other => other,
        }
    }
    fn spawn_inner(
        self,
        detached: bool,
        fail_after_spawn: bool,
        captured_pid: Option<Arc<std::sync::atomic::AtomicU32>>,
    ) -> io::Result<OwnedChild> {
        self.spawn_inner_with(detached, fail_after_spawn, captured_pid, false)
    }
    fn spawn_inner_with(
        mut self,
        detached: bool,
        fail_after_spawn: bool,
        captured_pid: Option<Arc<std::sync::atomic::AtomicU32>>,
        in_job_fallback: bool,
    ) -> io::Result<OwnedChild> {
        if detached && self.caller_job.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "detached launch cannot join a caller job",
            ));
        }
        if detached && self.kill_on_parent_exit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "kill_on_parent_exit cannot combine with a detached launch",
            ));
        }
        #[cfg(unix)]
        let lists_fds = !self.inherited_fds.is_empty();
        #[cfg(not(unix))]
        let lists_fds = false;
        if self.inherit_all_descriptors && (self.inherited_handles.is_some() || lists_fds) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "inherit_all_descriptors conflicts with inherit_handles_only and inherit_fd(s)",
            ));
        }
        // Windows: a command whose stdio and environment are known (`new`, or an explicit
        // config) takes the native spawn with an empty handle list by default, so the child
        // inherits its stdio and nothing else. A bare `from_command` has no config and keeps
        // std's spawn (see the README).
        #[cfg(windows)]
        if !self.inherit_all_descriptors
            && self.inherited_handles.is_none()
            && self.windows_spawn_config.is_some()
        {
            self.inherited_handles = Some(Vec::new());
        }
        if let Some(RunStdio { stdin_piped }) = self.run_stdio {
            // Applied here, after every caller edit, to both launch paths: the std spawn reads
            // the `Command`, an allowlisted Windows spawn reads only the config.
            self.command
                .stdin(if stdin_piped {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            #[cfg(windows)]
            if let Some(config) = self.windows_spawn_config.as_mut() {
                config.stdin = if stdin_piped {
                    crate::WindowsStdio::Piped
                } else {
                    crate::WindowsStdio::Null
                };
                config.stdout = crate::WindowsStdio::Piped;
                config.stderr = crate::WindowsStdio::Piped;
            }
        }
        #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
        if self.kill_on_parent_exit {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "kill_on_parent_exit is supported on Linux and macOS only",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let session = detached || self.unix_containment == UnixContainment::Session;
            let nice_floor = self.process_priority.nice_floor();
            #[cfg(target_os = "macos")]
            let utility_qos = self.process_priority == ProcessPriority::Efficient;
            // Default: sweep every descriptor above stderr (Orthic-Labs/rightkit#835); the
            // allowlist and `inherit_fd(s)` are the exceptions. Built here, in the parent.
            let inheritance = if self.inherit_all_descriptors {
                None
            } else {
                let mut allowed = self.inherited_handles.take().unwrap_or_default();
                allowed.extend(self.inherited_fds.iter().copied());
                Some(crate::process_unix::Inheritance::new(allowed)?)
            };
            // The supervisor's pid, read here and compared in the child (see `die_with_parent`).
            #[cfg(target_os = "linux")]
            let supervisor = self
                .kill_on_parent_exit
                // SAFETY: getpid has no preconditions.
                .then(|| unsafe { libc::getpid() });
            // Only async-signal-safe libc calls run between fork and exec.
            unsafe {
                self.command.pre_exec(move || {
                    let result = if session {
                        libc::setsid()
                    } else {
                        libc::setpgid(0, 0)
                    };
                    if result < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if let Some(floor) = nice_floor {
                        crate::process_unix::raise_nice_to(floor)?;
                    }
                    #[cfg(target_os = "macos")]
                    if utility_qos {
                        crate::process_unix::set_utility_qos();
                    }
                    if let Some(inheritance) = &inheritance {
                        inheritance.apply()?;
                    }
                    #[cfg(target_os = "linux")]
                    if let Some(supervisor) = supervisor {
                        crate::process_unix::die_with_parent(supervisor)?;
                    }
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        crate::process_windows::configure(
            &mut self.command,
            detached,
            self.windows_hidden,
            self.process_priority,
        );
        #[cfg(unix)]
        let _ = (
            self.windows_hidden,
            self.windows_breakaway,
            self.job_assignment,
        );
        #[cfg(windows)]
        let _ = (self.unix_containment, self.kill_on_parent_exit);
        let spawned_at = SystemTime::now();
        let hold_suspended = self.hold_suspended && !detached;
        #[cfg(unix)]
        let _ = hold_suspended;
        #[cfg(unix)]
        let child = {
            let mut child = crate::unix_child::UnixChild::new(self.command.spawn()?);
            // A child whose pid cannot be tied to it is stopped here, while the pid is
            // certainly still its own: no later signal could be proven to reach it.
            if let Err(error) = child.require_identity() {
                let reaped = child.cleanup_unidentified(self.partial_spawn_cleanup_timeout);
                return Err(spawn_cleanup_failure(error, reaped));
            }
            // macOS: the tie is a watchdog armed while the pid is still certainly ours. A
            // child that could not be tied is stopped, never handed out untied.
            #[cfg(target_os = "macos")]
            if self.kill_on_parent_exit {
                if let Err(error) = crate::process_unix::arm_parent_death_watch(child.id()) {
                    let _ = child.signal(SignalTarget::Group, libc::SIGKILL);
                    let reaped = cleanup_direct(
                        Arc::new(Mutex::new(child)),
                        self.partial_spawn_cleanup_timeout,
                    );
                    return Err(spawn_cleanup_failure(error, reaped));
                }
            }
            child
        };
        #[cfg(windows)]
        let (child, stayed_in_parent_job) = match self.inherited_handles.as_deref() {
            None => {
                // An ordinary spawn inherits every inheritable handle. Held so it never runs
                // while an allowlisted spawn has private handles raised.
                let _spawning = crate::spawn_windows::spawn_guard();
                match self.command.spawn() {
                    Ok(child) => (PlatformChild::from(child), false),
                    // The caller's job denied breakaway: one retry, detached but inside it.
                    Err(error)
                        if detached
                            && in_job_fallback
                            && crate::process_windows::is_breakaway_denied(&error) =>
                    {
                        crate::process_windows::configure_in_job_detached(
                            &mut self.command,
                            self.windows_hidden,
                            self.process_priority,
                        );
                        (PlatformChild::from(self.command.spawn()?), true)
                    }
                    Err(error) => return Err(error),
                }
            }
            Some(handles) => {
                let config = self.windows_spawn_config.take().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput,
                        "allowlisted Windows spawn requires windows_spawn_config for opaque Command settings")
                })?;
                crate::spawn_windows::spawn(
                    &self.command,
                    config,
                    handles,
                    detached,
                    self.windows_hidden,
                    self.process_priority,
                    self.env_cleared,
                    self.partial_spawn_cleanup_timeout,
                    in_job_fallback,
                )?
            }
        };
        #[cfg(not(windows))]
        let _ = in_job_fallback;
        #[cfg(not(windows))]
        let stayed_in_parent_job = false;
        let pid = child.id();
        if let Some(captured) = captured_pid {
            captured.store(pid, std::sync::atomic::Ordering::SeqCst);
        }
        let child = Arc::new(Mutex::new(child));
        let handle = match ProcessHandle::new(&child, spawned_at) {
            Ok(handle) => handle,
            Err(error) => {
                let reaped = cleanup_direct(child, self.partial_spawn_cleanup_timeout);
                return Err(spawn_cleanup_failure(error, reaped));
            }
        };
        let mut partial = PartialChild {
            child: Some(OwnedChild {
                child,
                handle,
                pid,
                detached,
                terminate_on_drop: !detached,
                drop_reap_timeout: DEFAULT_DROP_REAP_TIMEOUT,
                tree_terminated: false,
                assignment_failures: Vec::new(),
                suspended: std::sync::atomic::AtomicBool::new(cfg!(windows) && hold_suspended),
                stayed_in_parent_job,
                #[cfg(windows)]
                job: None,
            }),
            timeout: self.partial_spawn_cleanup_timeout,
        };
        #[cfg(windows)]
        if !detached {
            let owned = partial.child.as_mut().expect("partial child is present");
            let assignment = crate::process_windows::assign_jobs(
                &owned.handle,
                self.caller_job.take(),
                self.job_assignment,
                self.windows_breakaway,
                &mut owned.job,
                &mut owned.assignment_failures,
            );
            if let Err(error) = assignment {
                return Err(partial.fail(error));
            }
        }
        #[cfg(windows)]
        {
            let owned = partial.child.as_ref().expect("partial child is present");
            if self.process_priority == ProcessPriority::Efficient {
                // Best effort: EcoQoS is a hint and older Windows builds lack it.
                let _ = crate::process_windows::apply_efficiency(&*owned.child());
            }
            if !hold_suspended {
                let resumed = owned.child().resume(detached);
                if let Err(error) = resumed {
                    return Err(partial.fail(error));
                }
            }
        }
        if fail_after_spawn {
            return Err(partial.fail(io::Error::other("injected post-spawn wrapping failure")));
        }
        let owned = partial.child.take().expect("partial child is present");
        if !detached {
            // Detached children are deliberately not owned: they outlive their handle.
            #[cfg(unix)]
            crate::registry::register(&owned.child, owned.pid);
            #[cfg(windows)]
            crate::registry::register(
                &owned.child,
                &owned.handle,
                owned
                    .job
                    .as_ref()
                    .map_or_else(std::sync::Weak::new, std::sync::Arc::downgrade),
                owned.pid,
            );
        }
        Ok(owned)
    }
}

/// The error source returned (inside an [`io::Error`] of kind `PermissionDenied`) by
/// [`OwnedCommand::spawn_uncontained_detached_strict`] on Windows when the caller's job object
/// denies `CREATE_BREAKAWAY_FROM_JOB`. The OS reports this as `ERROR_ACCESS_DENIED`, which this
/// crate cannot tell apart from other access-denied launch failures; a job that forbids
/// breakaway is the cause this crate expects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BreakawayDenied;

impl std::fmt::Display for BreakawayDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "detached launch denied: the caller's job object does not allow breakaway \
             (ERROR_ACCESS_DENIED); no fallback was attempted",
        )
    }
}

impl std::error::Error for BreakawayDenied {}

#[cfg(windows)]
fn is_denied_breakaway(error: &io::Error) -> bool {
    crate::process_windows::is_breakaway_denied(error)
}
#[cfg(not(windows))]
fn is_denied_breakaway(_error: &io::Error) -> bool {
    false
}

/// How long `Drop` waits for a killed leader to be reaped before handing the reap to a
/// background thread.
pub const DEFAULT_DROP_REAP_TIMEOUT: Duration = Duration::from_secs(2);

pub const DEFAULT_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SystemRoot",
    "SYSTEMROOT",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "COMSPEC",
    "PATHEXT",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
];

#[derive(Debug)]
pub struct OwnedChild {
    child: Arc<Mutex<PlatformChild>>,
    handle: ProcessHandle,
    // Captured before any wait can reap the direct child.
    pid: u32,
    detached: bool,
    terminate_on_drop: bool,
    drop_reap_timeout: Duration,
    tree_terminated: bool,
    assignment_failures: Vec<JobAssignmentFailure>,
    // True from `spawn_suspended` (Windows) until `resume` releases the child.
    suspended: std::sync::atomic::AtomicBool,
    // Windows detached launch only: breakaway was denied and the child runs inside the parent's job.
    stayed_in_parent_job: bool,
    // Shared so the owned-children registry can reach the job without keeping it open
    // (it holds a `Weak`): the kill-on-close job still closes with this child.
    #[cfg(windows)]
    job: Option<std::sync::Arc<crate::process_windows::KillOnCloseJob>>,
}
/// The result of [`OwnedChild::terminate_tree_deep`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeTermination {
    /// The leader's exit status.
    pub status: ExitStatus,
    /// What happened to the descendants recorded before the leader was stopped.
    pub descendants: crate::SweepReport,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReapOutcome {
    Exited(ExitStatus),
    TimedOut,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitOutcome {
    Exited(ExitStatus),
    Terminated(ExitStatus),
}

impl OwnedChild {
    pub fn id(&self) -> u32 {
        self.pid
    }
    /// True only for a Windows [`OwnedCommand::spawn_uncontained_detached`] child that could
    /// not break away because the caller's job object denied it, so it was launched detached
    /// but still inside that job (the job's limits, such as kill-on-close, still reach it).
    /// False for every other child, including every Unix child and any child that broke away.
    pub fn stayed_in_parent_job(&self) -> bool {
        self.stayed_in_parent_job
    }
    /// Borrow child state, including stdio, wait and try_wait. Windows uses a
    /// native view with std pipe types for both spawn paths; Unix uses `UnixChild`, which
    /// has the same members and does not expose the std `Child`. Release this guard
    /// before another accessor or exit reader; it holds the shared reap lock.
    ///
    /// Unix: a `wait` or `try_wait` through the guard that reaps the leader permanently
    /// ends process-group signalling for this child, exactly like `OwnedChild::wait`.
    ///
    /// Unix: `wait` through the guard holds the shared lock until the child has exited, because
    /// the guard is the lock. A bounded wait on a `ProcessHandle` clone, `kill`,
    /// `terminate_tree` and `Drop` on other threads queue behind it and can exceed their own
    /// deadlines, as with `std::process::Child::wait` through this accessor in 0.3.3. Wait
    /// through `OwnedChild::wait`/`wait_timeout` or a `ProcessHandle` when anything else must
    /// stay live meanwhile. (Windows: `WindowsChild::wait` through the guard blocks in
    /// `WaitForSingleObject` while holding the lock, as in 0.3.3.)
    pub fn child(&self) -> MutexGuard<'_, PlatformChild> {
        lock_child(&self.child)
    }
    /// Release a child created by [`OwnedCommand::spawn_suspended`]. It is already in its Job
    /// Object; this lets it run. Returns `Ok` and does nothing for a child that is not held
    /// (an ordinary spawn, a child already resumed, and every Unix child). On error the child
    /// stays held and `resume` can be retried.
    pub fn resume(&self) -> io::Result<()> {
        if self
            .suspended
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            #[cfg(windows)]
            if let Err(error) = self.child().resume(self.detached) {
                self.suspended
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                return Err(error);
            }
        }
        Ok(())
    }
    /// True while a child from [`OwnedCommand::spawn_suspended`] has not been resumed.
    pub fn is_suspended(&self) -> bool {
        self.suspended.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// Runs `act` with the leader's pid while the leader is proven to be this child and
    /// unreaped, holding the shared lock so no reap can fall between the proof and `act`.
    /// Returns `Ok(None)` without calling `act` once the leader has been reaped or its pid no
    /// longer provably names it. For code that must signal by pid (a descendant sweep) and
    /// would otherwise act on a number that may have been recycled. The lock is held for as
    /// long as `act` runs (that is what keeps a reap out), so every other thread's wait,
    /// including a bounded one, is delayed by that long: keep `act` short, and never call back
    /// into this child from it. Windows has no recyclable-pid hazard here and always runs it.
    pub fn while_unreaped<R>(&self, act: impl FnOnce(u32) -> R) -> io::Result<Option<R>> {
        #[cfg(unix)]
        {
            let mut child = lock_child(&self.child);
            if child.leader_state()? == LeaderState::Reaped {
                return Ok(None);
            }
            Ok(Some(act(self.pid)))
        }
        #[cfg(windows)]
        Ok(Some(act(self.pid)))
    }
    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child().stdin.take()
    }
    /// Closes the child's stdin (delivering EOF) without waiting. Idempotent: a child whose
    /// stdin was taken or already closed is untouched. For a child that already exited this
    /// is a harmless drop of the unused pipe end, never an error.
    pub fn close_stdin(&mut self) {
        drop(self.child().stdin.take());
    }
    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child().stdout.take()
    }
    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child().stderr.take()
    }
    pub fn job_assignment_failures(&self) -> &[JobAssignmentFailure] {
        &self.assignment_failures
    }
    /// An independent exit reader. A monitor thread that only needs to learn that the leader
    /// exited should call [`ProcessHandle::wait_exited`], which does not reap; `wait` reaps, and
    /// on Unix that ends the owner's ability to signal descendants that outlive the leader.
    pub fn process_handle(&self) -> io::Result<ProcessHandle> {
        self.handle.try_clone()
    }
    pub fn creation_time(&self) -> Option<SystemTime> {
        self.handle.creation_time()
    }
    #[cfg(windows)]
    pub fn creation_time_ticks(&self) -> Option<u64> {
        self.handle.creation_time_ticks()
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        // Through the handle so the reap also seals the process group's state.
        self.handle.try_wait()
    }
    // Preserve std Child::wait's stdin-close behavior. ProcessHandle performs
    // native blocking waits & keeps Unix reap results in std's shared cache.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        drop(self.child().stdin.take());
        self.handle.wait()
    }
    pub fn wait_timeout(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        self.handle.wait_timeout(timeout)
    }
    /// Asks the tree to stop, waits up to `grace` for the leader, then forces the tree down.
    ///
    /// Unix: TERM goes to the group while the leader is still unreaped. If the leader exits
    /// within `grace` the result is `Exited`, but descendants that outlived it (or ignored
    /// TERM) are killed before the leader is reaped, while it still pins the group id. If it
    /// does not exit in time the whole group is killed and the result is `Terminated`.
    ///
    /// Windows: if the leader exits within `grace` the result is `Exited` with its status, but
    /// the retained job is terminated first, so descendants that outlived it do not survive.
    pub fn wait_or_kill(&mut self, grace: Duration) -> io::Result<WaitOutcome> {
        self.wait_or_kill_inner(grace, true)
    }
    /// Like [`wait_or_kill`](Self::wait_or_kill) but without the opening TERM on Unix: the
    /// leader gets the whole `grace` to exit on its own (for example after its stdin was
    /// closed) before the tree is killed. A leader that exits in time is never signalled, so
    /// its status is its own; descendants are still killed before the reap. Windows has no
    /// TERM step, so it behaves exactly like `wait_or_kill`.
    pub fn wait_then_kill(&mut self, grace: Duration) -> io::Result<WaitOutcome> {
        self.wait_or_kill_inner(grace, false)
    }
    fn wait_or_kill_inner(&mut self, grace: Duration, term_first: bool) -> io::Result<WaitOutcome> {
        #[cfg(not(unix))]
        let _ = term_first;
        #[cfg(unix)]
        {
            if term_first && !self.tree_terminated {
                self.signal(libc::SIGTERM)?;
            }
            // Looks without reaping until the leader has exited or the grace has passed.
            if self.handle.wait_exited_timeout(grace)? {
                // Exited, not reaped yet: the zombie still pins the group id, so the
                // stragglers can be killed safely, and only then reaped. (If a handle clone
                // reaped it, the signal is a no-op, the cached status is returned and group
                // signalling is over.)
                self.force_kill(1)?;
                return self.wait().map(WaitOutcome::Exited);
            }
            self.terminate_tree().map(WaitOutcome::Terminated)
        }
        #[cfg(windows)]
        {
            if let Some(status) = self.wait_timeout(grace)? {
                // The leader left, but descendants may still be in the retained job (and hold
                // the pipes). Bring the job down before reporting; the leader's own status,
                // already collected, is what the caller gets.
                self.force_kill(1)?;
                return Ok(WaitOutcome::Exited(status));
            }
            self.terminate_tree().map(WaitOutcome::Terminated)
        }
    }
    /// Waits up to `timeout` for the leader to exit, without a signal. When it has, brings the
    /// rest of the tree down before reaping (the exited branch of
    /// [`wait_then_kill`](Self::wait_then_kill): Unix kills the group while the unreaped leader
    /// still pins its id, Windows terminates the retained job) and returns the leader's own
    /// status. `None` when it is still running. Lets a caller poll for exit between other work.
    pub(crate) fn wait_exit_then_sweep(
        &mut self,
        timeout: Duration,
    ) -> io::Result<Option<ExitStatus>> {
        if self.handle.wait_exited_timeout(timeout)? {
            self.force_kill(1)?;
            return self.wait().map(Some);
        }
        Ok(None)
    }
    /// Immediate forced termination, preserving the existing default.
    /// Windows: the retained job is signalled even after the parent was reaped.
    /// Unix: the group is signalled only while the leader is unreaped (see the type
    /// documentation); once any path has reaped it, only the reap result is returned.
    pub fn terminate_tree(&mut self) -> io::Result<ExitStatus> {
        self.force_kill(1)?;
        self.wait()
    }
    /// Unix: TERM now, tree-wide grace, then KILL, all while the leader is unreaped (an
    /// exited leader stays unreaped until this call reaps it, so descendants still receive
    /// the KILL). If another path already reaped the leader, nothing is signalled.
    /// Windows: immediately terminate retained job with supplied exit code.
    pub fn terminate_tree_with_options(
        &mut self,
        options: TerminationOptions,
    ) -> io::Result<ExitStatus> {
        self.start_termination(options)?;
        self.wait()
    }
    pub fn terminate_tree_bounded(&mut self, timeout: Duration) -> io::Result<ReapOutcome> {
        self.terminate_tree_bounded_with_options(timeout, TerminationOptions::default())
    }
    /// timeout covers grace plus reap; grace is clipped to timeout.
    pub fn terminate_tree_bounded_with_options(
        &mut self,
        timeout: Duration,
        mut options: TerminationOptions,
    ) -> io::Result<ReapOutcome> {
        let started = Instant::now();
        options.grace = options.grace.min(timeout);
        // One absolute budget covers grace, kill, root reap and job emptiness: every step gets
        // only what is left of it, so no step can wait past it (rightkit#1260).
        self.start_termination_within(options, timeout)?;
        #[cfg(test)]
        if cleanup_test_fault::refuse(timeout.saturating_sub(started.elapsed())) {
            return Ok(ReapOutcome::TimedOut);
        }
        let status = match self.wait_timeout(timeout.saturating_sub(started.elapsed()))? {
            Some(status) => status,
            None => return Ok(ReapOutcome::TimedOut),
        };
        // Windows: the leader's exit does not prove the tree is gone (TerminateJobObject is
        // asynchronous). Exited is reported only once the retained job holds no active process,
        // within the same deadline; otherwise the outcome stays unconfirmed (rightkit#1260). A
        // child with no retained job (detached or uncontained) has no tree to prove: its leader's
        // exit stands, as before.
        #[cfg(windows)]
        if let Some(job) = self.job.as_ref() {
            if !job.wait_empty(timeout.saturating_sub(started.elapsed()))? {
                return Ok(ReapOutcome::TimedOut);
            }
        }
        Ok(ReapOutcome::Exited(status))
    }
    /// On-demand termination of the whole tree, for a child that is not terminated on drop (a
    /// [`spawn_uncontained_detached`](OwnedCommand::spawn_uncontained_detached) child whose
    /// helpers should die now) as well as for an ordinary one. Unlike
    /// [`terminate_tree_with_options`](Self::terminate_tree_with_options), a detached child's
    /// descendants are not left alone:
    ///
    /// 1. While the leader is proven to be this child, the identities of its descendants are
    ///    recorded (parent links; descendants that already outlived a parent that exited are
    ///    not findable, as for [`descendant_identities`](Self::descendant_identities)).
    /// 2. Unix: `SIGTERM` to the leader's whole process group (a detached child leads its own
    ///    session, so that group is its own), up to `options.grace` for the group to leave,
    ///    then `SIGKILL` to the group, all before the leader is reaped, so the group id cannot
    ///    be recycled. Windows: the retained job if there is one (ordinary children), and the
    ///    leader (`options.windows_exit_code`).
    /// 3. The leader is reaped.
    /// 4. The recorded descendants that are still the same process (pid and start time; a
    ///    recycled pid is refused, never signalled) are swept, leaves first, with
    ///    `options.grace`. This is what reaches a helper that left the group (a daemon that
    ///    called `setsid`) and, on Windows, the descendants of a detached child.
    ///
    /// If another path already reaped the leader on Unix, nothing is signalled and the cached
    /// status is returned with an empty sweep. No terminate-on-drop is implied: calling this
    /// or not does not change what `Drop` does.
    pub fn terminate_tree_deep(
        &mut self,
        options: TerminationOptions,
    ) -> io::Result<TreeTermination> {
        let mut found = self.descendant_identities()?;
        found.reverse(); // leaves first
        #[cfg(unix)]
        if !self.tree_terminated {
            signal_shared(&self.child, SignalTarget::Group, libc::SIGTERM)?;
            await_tree_shared(&self.child, &self.handle, options.grace, true)?;
            signal_shared(&self.child, SignalTarget::Group, libc::SIGKILL)?;
            self.tree_terminated = true;
        }
        #[cfg(windows)]
        self.force_kill(options.windows_exit_code)?;
        let status = self.wait()?;
        let descendants = crate::sweep(&found, options.grace)?;
        Ok(TreeTermination {
            status,
            descendants,
        })
    }
    fn start_termination(&mut self, options: TerminationOptions) -> io::Result<()> {
        self.start_termination_within(options, Duration::MAX)
    }
    /// [`start_termination`](Self::start_termination) inside `budget`: the grace is already
    /// clipped by the caller, and on Windows the wait for an exiting leader is clipped to what
    /// is left of `budget` after the grace.
    fn start_termination_within(
        &mut self,
        options: TerminationOptions,
        budget: Duration,
    ) -> io::Result<()> {
        let started = Instant::now();
        #[cfg(unix)]
        if !self.tree_terminated {
            self.signal(libc::SIGTERM)?;
            // Stops waiting as soon as nothing but the exited, unreaped leader is left (or the
            // leader was reaped elsewhere, which ends signalling). The leader is not reaped
            // here: it must keep pinning the group id for the KILL below.
            self.await_tree(options.grace)?;
        }
        self.force_kill_within(
            options.windows_exit_code,
            budget.saturating_sub(started.elapsed()),
        )
    }
    /// Waits, for at most `grace`, until the leader has exited and every other member of its
    /// group has too. The leader is observed without reaping it; then the group is listed
    /// every [`POLL_INTERVAL`](crate::exit_watch::POLL_INTERVAL) (the published 0.3.3 interval)
    /// until nothing but the leader is left. A group that cannot be listed is waited out for
    /// the rest of the grace in one timed sleep. Each listing takes the child lock briefly and
    /// releases it before the thread sleeps. Bounded by `grace` in every case.
    #[cfg(unix)]
    fn await_tree(&self, grace: Duration) -> io::Result<()> {
        // A detached child owns only its direct pid.
        await_tree_shared(&self.child, &self.handle, grace, !self.detached)
    }
    /// Sends `signal` to the group (or, for a detached child, to the leader alone) while the
    /// leader is unreaped and proven. Takes the child lock for each attempt and releases it
    /// between attempts.
    #[cfg(unix)]
    fn signal(&self, signal: i32) -> io::Result<()> {
        // Detached children only own their direct pid. Under the child's lock, and only while
        // the leader is unreaped, so the id cannot have been recycled.
        let target = if self.detached {
            SignalTarget::Leader
        } else {
            SignalTarget::Group
        };
        signal_shared(&self.child, target, signal).map(|_| ())
    }
    fn force_kill(&mut self, code: u32) -> io::Result<()> {
        self.force_kill_within(code, Duration::MAX)
    }
    /// The kill. Windows: `budget` caps the wait for a leader already exiting when
    /// `TerminateProcess` refuses it (at most the usual two seconds). Unix sends KILL and waits
    /// for nothing.
    fn force_kill_within(&mut self, code: u32, budget: Duration) -> io::Result<()> {
        if self.tree_terminated {
            return Ok(());
        }
        #[cfg(unix)]
        {
            let _ = (code, budget);
            self.signal(libc::SIGKILL)?;
        }
        #[cfg(windows)]
        crate::process_windows::terminate(
            &self.handle,
            self.job.as_deref(),
            code,
            budget.min(crate::process_windows::EXITING_LEADER_WAIT),
        )?;
        self.tree_terminated = true;
        Ok(())
    }
}
impl OwnedChild {
    /// How long dropping this child waits for the killed leader to be reaped (default
    /// [`DEFAULT_DROP_REAP_TIMEOUT`]). `Drop` sends the kill at once and never waits longer than
    /// this: a leader that has not been reaped by then (it is in an uninterruptible state, for
    /// instance) is reaped later by a background thread on Unix, and on Windows the process
    /// handle is closed. `Duration::ZERO` means "do not wait". Does not affect
    /// [`terminate_tree`](Self::terminate_tree), which still waits for the reap.
    pub fn set_drop_reap_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.drop_reap_timeout = timeout;
        self
    }
    /// The bound `Drop` applies to the reap.
    pub fn drop_reap_timeout(&self) -> Duration {
        self.drop_reap_timeout
    }
    /// Reaps the leader on a thread of its own, so an unbounded wait never blocks the caller.
    fn reap_in_background(&self) {
        #[cfg(unix)]
        if let Ok(reader) = self.handle.try_clone() {
            let _ = thread::Builder::new()
                .name("rightkit-drop-reap".into())
                .spawn(move || {
                    let _ = reader.wait();
                });
        }
    }
    /// The pids of everything this child left running besides the leader, read without
    /// signalling or reaping anything: the call a caller makes to record survivors before a kill
    /// removes the evidence.
    ///
    /// Windows: every member of the retained job (grandchildren included, even after the leader
    /// exited), plus parent-link descendants of the leader. Unix: members of the leader's process
    /// group (while the leader is unreaped) plus parent-link descendants (see
    /// [`descendant_identities`](Self::descendant_identities)). Order is unspecified, the leader
    /// is never listed, and the list is a snapshot: a member can exit as soon as it is read. A
    /// listing that fails is reported as the error, never as an empty tree.
    pub fn tree_members(&self) -> io::Result<Vec<u32>> {
        let leader = self.pid;
        let mut pids: Vec<u32> = Vec::new();
        #[cfg(windows)]
        if let Some(job) = self.job.as_ref() {
            pids.extend(job.process_ids()?);
        }
        #[cfg(unix)]
        if !self.detached {
            let group = match lock_child(&self.child).group_other_members()? {
                crate::unix_child::Members::Pids(group) => Some(group),
                crate::unix_child::Members::Unknown => None,
            };
            pids.extend(listed_or_error(group)?);
        }
        for identity in self.descendant_identities().unwrap_or_default() {
            pids.push(identity.pid());
        }
        pids.retain(|p| *p != leader);
        pids.sort_unstable();
        pids.dedup();
        Ok(pids)
    }
    /// Identities of this child's descendants (see [`crate::AdoptedIdentity::descendants`]), taken
    /// while the leader is proven to be this child. Empty once the leader has been reaped:
    /// its children are re-parented then and can no longer be found by their parent.
    pub fn descendant_identities(&self) -> io::Result<Vec<crate::AdoptedIdentity>> {
        let root = self.while_unreaped(|pid| match NonZeroU32::new(pid) {
            Some(pid) => crate::AdoptedIdentity::capture(pid),
            None => Ok(None),
        })?;
        match root {
            Some(Ok(Some(root))) => root.descendants(),
            Some(Err(error)) => Err(error),
            _ => Ok(Vec::new()),
        }
    }
}
/// A group listing that the host could not give (`None`: too large for one listing, or the
/// enumeration failed while the group exists) is an error, never an empty tree: a caller that
/// records survivors before a kill must not read "unknown" as "nobody".
#[cfg_attr(not(unix), allow(dead_code))]
fn listed_or_error(listing: Option<Vec<u32>>) -> io::Result<Vec<u32>> {
    listing.ok_or_else(|| {
        io::Error::other("the process group could not be fully listed (too large, or the host refused the enumeration)")
    })
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        // No longer owned: leave the registry before anything below can take long.
        crate::registry::forget(&self.child);
        if self.terminate_on_drop {
            // Bounded: the kill is sent at once, the reap is awaited for at most
            // `drop_reap_timeout`, and a child that has not been reaped by then is reaped by
            // a background thread. Dropping never waits on a process that will not die.
            if self.force_kill(1).is_ok() {
                drop(self.child().stdin.take());
                if !matches!(
                    self.handle.wait_timeout(self.drop_reap_timeout),
                    Ok(Some(_))
                ) {
                    self.reap_in_background();
                }
            }
        } else if self.detached {
            #[cfg(unix)]
            {
                // Shared launch still needs eventual reaping. Close owner pipes
                // now; the reaper thread makes non-blocking looks and releases the child
                // lock between them, so exit readers and kills are not held behind it.
                {
                    let mut child = self.child();
                    drop(child.stdin.take());
                    drop(child.stdout.take());
                    drop(child.stderr.take());
                }
                if let Ok(reader) = self.handle.try_clone() {
                    let _ = thread::Builder::new()
                        .name("rightkit-detached-reap".into())
                        .spawn(move || {
                            let _ = reader.wait();
                        });
                }
            }
        }
    }
}

/// A process was created, but its wrapper could not be completed. PartialChild/direct
/// cleanup is best effort; its lack of a reap receipt must never become NotStarted.
pub(crate) fn spawn_setup_failure(error: io::Error) -> io::Error {
    crate::run::failure(
        crate::RunnerPhase::Setup,
        crate::LeaderTermination::Unconfirmed,
        error,
    )
}

pub(crate) fn spawn_cleanup_failure(error: io::Error, reaped: bool) -> io::Error {
    if reaped {
        spawn_setup_failure(error)
    } else {
        crate::run::failure(
            crate::RunnerPhase::Cleanup,
            crate::LeaderTermination::Unconfirmed,
            error,
        )
    }
}

struct PartialChild {
    child: Option<OwnedChild>,
    timeout: Option<Duration>,
}
impl PartialChild {
    fn cleanup(&mut self) -> bool {
        if let Some(mut child) = self.child.take() {
            child.terminate_on_drop = false;
            let _ = child.force_kill(1);
            cleanup_direct(Arc::clone(&child.child), self.timeout)
        } else {
            true
        }
    }

    fn fail(mut self, error: io::Error) -> io::Error {
        let reaped = self.cleanup();
        spawn_cleanup_failure(error, reaped)
    }
}
impl Drop for PartialChild {
    fn drop(&mut self) {
        self.cleanup();
    }
}
/// How long a group signal that macOS refused with `EPERM` (the last members are mid-exit) is
/// retried before the refusal is reported.
#[cfg(unix)]
const MID_EXIT_RETRY: Duration = Duration::from_millis(300);

/// One signal to a shared child's group or leader (see `OwnedChild::signal`), retrying for a
/// bounded time while macOS reports the last members mid-exit. The lock is taken for each
/// attempt and released between attempts. Returns the final outcome.
#[cfg(unix)]
pub(crate) fn signal_shared(
    shared: &Mutex<crate::unix_child::UnixChild>,
    target: SignalTarget,
    signal: i32,
) -> io::Result<SignalOutcome> {
    let started = Instant::now();
    loop {
        let (outcome, watch) = {
            let mut child = lock_child(shared);
            (child.signal(target, signal)?, child.exit_watch())
        };
        if outcome != SignalOutcome::MidExit {
            return Ok(outcome);
        }
        // macOS refused the group signal while its last members are mid-exit. The group is
        // gone once the exit completes: look again, for a bounded time, without the lock.
        let remaining = MID_EXIT_RETRY.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(io::Error::from_raw_os_error(libc::EPERM));
        }
        watch.pause(remaining);
    }
}

/// `OwnedChild::await_tree` over a shared child. `whole_group` false waits only for the leader.
#[cfg(unix)]
pub(crate) fn await_tree_shared(
    shared: &Mutex<crate::unix_child::UnixChild>,
    handle: &ProcessHandle,
    grace: Duration,
    whole_group: bool,
) -> io::Result<()> {
    use crate::{exit_watch::POLL_INTERVAL, unix_child::Members};
    let Some(deadline) = Instant::now().checked_add(grace) else {
        return Ok(());
    };
    let left = |deadline: Instant| deadline.saturating_duration_since(Instant::now());
    if !handle.wait_exited_timeout(left(deadline))? {
        return Ok(()); // the leader outlived the grace
    }
    if !whole_group {
        return Ok(());
    }
    loop {
        match lock_child(shared).group_other_members()? {
            Members::Pids(pids) if pids.is_empty() => return Ok(()),
            Members::Pids(_) => {}
            Members::Unknown => return wait_out(left(deadline)),
        }
        let remaining = left(deadline);
        if remaining.is_zero() {
            return Ok(()); // the grace is over
        }
        thread::sleep(POLL_INTERVAL.min(remaining));
    }
}

/// The rest of a grace for a group that cannot be listed. There is nothing to look at, so this
/// is one timed sleep, not a loop.
#[cfg(unix)]
fn wait_out(remaining: Duration) -> io::Result<()> {
    thread::sleep(remaining);
    Ok(())
}
/// Stops and reaps a child whose spawn failed after the process existed. Every caller passes
/// an `Arc` that nothing but the spawning thread holds (the partial `OwnedChild`, from which no
/// `ProcessHandle` was cloned and which no caller has seen, or a child that was never wrapped),
/// so no other thread can contend for its lock. That is why the two blocking-under-lock waits
/// below, the unbounded one and the `rightkit-partial-reap` thread, cannot hold a deadline-
/// bearing caller back; both are what 0.3.3 did with `std::process::Child::wait`.
pub(crate) fn cleanup_direct(child: Arc<Mutex<PlatformChild>>, timeout: Option<Duration>) -> bool {
    let _ = lock_child(&child).kill();
    let Some(timeout) = timeout else {
        // Unbounded by default, as before. Exclusive child: see above.
        return lock_child(&child).wait().is_ok();
    };
    #[cfg(test)]
    let refused = cleanup_test_fault::refuse(timeout);
    #[cfg(not(test))]
    let refused = false;
    #[cfg(unix)]
    {
        // Non-blocking looks, each under the lock and released at once, until the child is
        // reaped or the timeout passes. A killed child is gone within one hint or interval.
        let watch = lock_child(&child).exit_watch();
        let deadline = Instant::now().checked_add(timeout);
        if !refused
            && matches!(
                watch.wait_for(deadline, || lock_child(&child).try_wait()),
                Ok(Some(_))
            )
        {
            return true;
        }
    }
    #[cfg(windows)]
    if !refused {
        let started = Instant::now();
        loop {
            match lock_child(&child).try_wait() {
                Ok(Some(_)) => return true,
                Err(_) => break,
                Ok(None) => {}
            }
            if started.elapsed() >= timeout {
                break;
            }
            thread::sleep(Duration::from_millis(5).min(timeout.saturating_sub(started.elapsed())));
        }
    }
    // std Child does not reap on Drop. Transfer timed-out children to a waiter. This thread
    // holds the child's lock for the whole wait and is the only holder of the `Arc` (see
    // above), so nothing can queue behind it.
    let _ = thread::Builder::new()
        .name("rightkit-partial-reap".into())
        .spawn(move || {
            let _ = lock_child(&child).wait();
        });
    false
}

#[cfg(test)]
pub(crate) mod cleanup_test_fault {
    use std::{cell::Cell, time::Duration};
    thread_local! { static REFUSE_REAP: Cell<bool> = const { Cell::new(false) }; }
    thread_local! { static JOB_EMPTY_AT: Cell<Option<std::time::Instant>> = const { Cell::new(None) }; }
    thread_local! { static TERMINATION_REFUSED: Cell<bool> = const { Cell::new(false) }; }
    /// Test only: on this thread, retained-job emptiness queries report one active process
    /// until `at` and none from then on (models a descendant that exits at a chosen moment).
    #[allow(dead_code)]
    pub(crate) fn arm_job_empty_at(at: std::time::Instant) {
        JOB_EMPTY_AT.with(|armed| armed.set(Some(at)));
    }
    #[allow(dead_code)]
    pub(crate) fn disarm_job_empty_at() {
        JOB_EMPTY_AT.with(|armed| armed.set(None));
    }
    /// The armed active-process count, or `None` to query the job.
    #[allow(dead_code)]
    pub(crate) fn job_count() -> Option<u32> {
        JOB_EMPTY_AT
            .with(|armed| armed.get())
            .map(|at| u32::from(std::time::Instant::now() < at))
    }
    /// Test only: the next Windows termination on this thread neither terminates the job nor
    /// the leader, and reports `TerminateProcess` as refused (models a leader that is exiting
    /// slowly), so the exiting-leader wait runs in full.
    #[allow(dead_code)]
    pub(crate) fn arm_termination_refused() {
        TERMINATION_REFUSED.with(|armed| armed.set(true));
    }
    #[allow(dead_code)]
    pub(crate) fn termination_refused() -> bool {
        TERMINATION_REFUSED.with(|armed| armed.replace(false))
    }
    pub(crate) fn arm() {
        REFUSE_REAP.with(|armed| armed.set(true));
    }
    pub(crate) fn refuse(timeout: Duration) -> bool {
        if REFUSE_REAP.with(|armed| armed.replace(false)) {
            // Model an observation that never acknowledges shutdown. Real kill still runs;
            // eventual native reaping remains owned by Drop or the partial-spawn reaper.
            std::thread::sleep(timeout);
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdoptedProcess {
    pid: NonZeroU32,
}
impl AdoptedProcess {
    pub fn new(pid: NonZeroU32) -> Self {
        Self { pid }
    }
    pub fn id(&self) -> u32 {
        self.pid.get()
    }
    /// Whether a process with this pid is running. Unix: a zombie (exited, unreaped by its
    /// own parent) reads `false`. This is a bare-pid probe with no start-time check; use
    /// [`identity`](Self::identity) when the pid must still name a remembered process.
    pub fn is_running(&self) -> io::Result<bool> {
        process_is_running(self.pid)
    }
    /// The identity (start time and executable) of the process behind this pid now, for
    /// identity-checked signalling through [`AdoptedIdentity`](crate::AdoptedIdentity).
    /// `Ok(None)` when no process has the pid. Capture it only when the pid is known to name
    /// the intended process.
    pub fn identity(&self) -> io::Result<Option<crate::AdoptedIdentity>> {
        crate::AdoptedIdentity::capture(self.pid)
    }
}
#[cfg(test)]
fn spawn_for_test(
    command: Command,
    captured_pid: Arc<std::sync::atomic::AtomicU32>,
) -> io::Result<OwnedChild> {
    OwnedCommand::from_command(command).spawn_inner(false, true, Some(captured_pid))
}

#[cfg(windows)]
fn process_is_running(pid: NonZeroU32) -> io::Result<bool> {
    use windows::Win32::{
        Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, WAIT_TIMEOUT},
        System::Threading::{
            OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE,
        },
    };

    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid.get(),
        )
    };
    let handle = match handle {
        Ok(handle) => handle,
        Err(error) => {
            if error.code() == ERROR_INVALID_PARAMETER.to_hresult() {
                return Ok(false);
            }
            return Err(io::Error::other(error.to_string()));
        }
    };
    let wait = unsafe { WaitForSingleObject(handle, 0) };
    let close = unsafe { CloseHandle(handle) };
    close.map_err(|error| io::Error::other(error.to_string()))?;
    match wait.0 {
        0 => Ok(false),
        value if value == WAIT_TIMEOUT.0 => Ok(true),
        _ => Err(io::Error::last_os_error()),
    }
}

#[cfg(unix)]
fn process_is_running(pid: NonZeroU32) -> io::Result<bool> {
    let raw_pid = i32::try_from(pid.get()).map_err(io::Error::other)?;
    loop {
        // SAFETY: signal 0 performs only existence and permission checks; nothing is sent.
        if unsafe { libc::kill(raw_pid, 0) } == 0 {
            // Existence includes zombies: an exited process nobody reaped is dead, not
            // running (and is never reaped here - that is its parent's job).
            return Ok(!crate::foreign::process_is_zombie(pid.get()));
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EPERM) => return Ok(!crate::foreign::process_is_zombie(pid.get())),
            Some(libc::ESRCH) => return Ok(false),
            _ => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn option_defaults_preserve_existing_behavior() {
        let command = super::OwnedCommand::new("unused");
        assert_eq!(command.job_assignment, super::JobAssignmentMode::Strict);
        assert_eq!(
            command.unix_containment,
            super::UnixContainment::ProcessGroup
        );
        assert_eq!(command.partial_spawn_cleanup_timeout, None);
        assert_eq!(
            super::TerminationOptions::default().grace,
            std::time::Duration::ZERO
        );
        assert_eq!(super::TerminationOptions::default().windows_exit_code, 1);
    }

    #[cfg(not(windows))]
    #[test]
    fn new_and_from_command_share_spawn_settings() {
        use super::OwnedCommand;
        use std::process::Command;
        // Debug covers every field. Off Windows nothing differs between the constructors.
        let by_new = OwnedCommand::new("unused");
        let by_from = OwnedCommand::from_command(Command::new("unused"));
        assert_eq!(format!("{by_new:?}"), format!("{by_from:?}"));
    }

    /// Only `new` knows its Command's stdio and environment are std defaults. A wrapped
    /// Command may carry settings the crate cannot read, so `from_command` must not invent a
    /// default config: the allowlisted spawn then refuses instead of widening inheritance.
    #[cfg(windows)]
    #[test]
    fn only_new_supplies_a_default_windows_spawn_config() {
        use super::OwnedCommand;
        use std::process::Command;
        let by_new = OwnedCommand::new("unused");
        let by_from = OwnedCommand::from_command(Command::new("unused"));
        assert!(by_new.windows_spawn_config.is_some() && by_new.windows_spawn_config_auto);
        assert!(by_from.windows_spawn_config.is_none() && !by_from.windows_spawn_config_auto);
    }

    #[test]
    fn process_priority_defaults_to_normal_and_keeps_the_requested_value() {
        use super::{OwnedCommand, ProcessPriority};
        let mut command = OwnedCommand::new("unused");
        assert_eq!(command.process_priority, ProcessPriority::Normal);
        command.process_priority(ProcessPriority::BelowNormal);
        assert_eq!(command.process_priority, ProcessPriority::BelowNormal);
        command.process_priority(ProcessPriority::Idle);
        assert_eq!(command.process_priority, ProcessPriority::Idle);
    }

    /// Normal adds no nice change, so the default Unix spawn is unchanged. Lower priorities
    /// map to the documented floors.
    #[cfg(unix)]
    #[test]
    fn process_priority_maps_to_nice_floors() {
        use super::ProcessPriority;
        assert_eq!(ProcessPriority::Normal.nice_floor(), None);
        assert_eq!(ProcessPriority::BelowNormal.nice_floor(), Some(10));
        assert_eq!(ProcessPriority::Idle.nice_floor(), Some(19));
        assert_eq!(ProcessPriority::Efficient.nice_floor(), Some(10));
    }

    /// A real child at `BelowNormal` reports the class Windows was asked for.
    #[cfg(windows)]
    #[test]
    fn below_normal_child_has_below_normal_priority_class() {
        use super::{OwnedCommand, ProcessPriority};
        use std::process::Stdio;
        use windows::Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                GetPriorityClass, OpenProcess, BELOW_NORMAL_PRIORITY_CLASS,
                PROCESS_QUERY_LIMITED_INFORMATION,
            },
        };
        let mut command = OwnedCommand::new("ping");
        command
            .command_mut()
            .args(["-n", "6", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command.process_priority(ProcessPriority::BelowNormal);
        let mut child = command.spawn().expect("spawn ping");
        // SAFETY: the handle is opened for this live child, queried once and closed.
        let class = unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, child.id())
                .expect("open the child");
            let class = GetPriorityClass(handle);
            let _ = CloseHandle(handle);
            class
        };
        let _ = child.terminate_tree();
        assert_eq!(class, BELOW_NORMAL_PRIORITY_CLASS.0);
    }

    /// A real child at `Idle` or `BelowNormal` runs with a nice value at or above the floor.
    #[cfg(unix)]
    #[test]
    fn lowered_priority_child_runs_at_or_above_the_nice_floor() {
        use super::{OwnedCommand, ProcessPriority};
        use std::{io::Read, process::Stdio};
        for (priority, floor) in [
            (ProcessPriority::BelowNormal, 10),
            (ProcessPriority::Idle, 19),
        ] {
            let mut command = OwnedCommand::new("/bin/sh");
            command
                .command_mut()
                .args(["-c", "ps -o ni= -p $$"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            command.process_priority(priority);
            let mut child = command.spawn().expect("spawn sh");
            let mut text = String::new();
            child
                .take_stdout()
                .expect("piped stdout")
                .read_to_string(&mut text)
                .expect("read ps output");
            let _ = child.wait();
            let nice: i32 = text.trim().parse().expect("ps printed a nice value");
            assert!(nice >= floor, "nice {nice} below floor {floor}");
        }
    }

    #[test]
    fn breakaway_is_off_by_default_explicit_by_default_opt_in_and_silent_is_separate() {
        use super::BreakawayMode;
        let mut command = super::OwnedCommand::new("unused");
        assert_eq!(command.windows_breakaway, BreakawayMode::None);
        command.windows_allow_breakaway(true);
        assert_eq!(command.windows_breakaway, BreakawayMode::Explicit);
        command.windows_allow_silent_breakaway(true);
        assert_eq!(command.windows_breakaway, BreakawayMode::Silent);
        command.windows_allow_silent_breakaway(false);
        assert_eq!(command.windows_breakaway, BreakawayMode::None);
    }

    #[test]
    fn kill_on_parent_exit_is_opt_in() {
        let mut command = super::OwnedCommand::new("unused");
        assert!(!command.kill_on_parent_exit);
        command.kill_on_parent_exit(true);
        assert!(command.kill_on_parent_exit);
    }

    #[test]
    fn breakaway_denied_is_a_typed_recoverable_error() {
        let error =
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, super::BreakawayDenied);
        assert!(error
            .get_ref()
            .and_then(|source| source.downcast_ref::<super::BreakawayDenied>())
            .is_some());
        assert!(super::BreakawayDenied.to_string().contains("breakaway"));
    }

    #[test]
    fn a_strict_detached_spawn_passes_other_failures_through_untyped() {
        let command = super::OwnedCommand::new("rightkit-no-such-program-for-strict-detach");
        let Err(error) = command.spawn_uncontained_detached_strict() else {
            panic!("a missing program must not spawn");
        };
        assert!(error
            .get_ref()
            .and_then(|source| source.downcast_ref::<super::BreakawayDenied>())
            .is_none());
    }

    #[test]
    fn a_strict_detached_spawn_still_rejects_kill_on_parent_exit() {
        let mut command = super::OwnedCommand::new("unused");
        command.kill_on_parent_exit(true);
        let Err(error) = command.spawn_uncontained_detached_strict() else {
            panic!("a detached spawn must not accept the parent-death tie");
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_detached_spawn_rejects_kill_on_parent_exit() {
        // Rejected before anything is started, so the program never needs to exist.
        let mut command = super::OwnedCommand::new("unused");
        command.kill_on_parent_exit(true);
        let Err(error) = command.spawn_uncontained_detached() else {
            panic!("a detached spawn must not accept the parent-death tie");
        };
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    /// Hosts other than Linux and macOS have no parent-death primitive: the option fails closed,
    /// and the program is never started (it would create the marker file).
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    #[test]
    fn a_host_without_a_parent_death_primitive_refuses_before_starting_anything() {
        let marker =
            std::env::temp_dir().join(format!("rightkit-process-untied-{}", std::process::id()));
        let _ = std::fs::remove_file(&marker);
        let mut command = super::OwnedCommand::new("/usr/bin/touch");
        command.command_mut().arg(&marker);
        command.kill_on_parent_exit(true);
        let Err(error) = command.spawn() else {
            panic!("kill_on_parent_exit must not run the child untied");
        };
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!marker.exists(), "the program ran");
    }

    #[test]
    fn option_builders_keep_every_requested_value() {
        use super::{JobAssignmentMode, OwnedCommand, TerminationOptions, UnixContainment};
        use std::time::Duration;
        let mut command = OwnedCommand::new("unused");
        command
            .job_assignment_mode(JobAssignmentMode::BestEffort)
            .unix_containment(UnixContainment::Session)
            .partial_spawn_cleanup_timeout(Duration::from_secs(5));
        assert_eq!(command.job_assignment, JobAssignmentMode::BestEffort);
        assert_eq!(command.unix_containment, UnixContainment::Session);
        assert_eq!(
            command.partial_spawn_cleanup_timeout,
            Some(Duration::from_secs(5))
        );
        let options = TerminationOptions::default()
            .with_grace(Duration::from_millis(100))
            .with_windows_exit_code(1067);
        assert_eq!(options.grace, Duration::from_millis(100));
        assert_eq!(options.windows_exit_code, 1067);
    }

    #[test]
    fn zero_bound_partial_spawn_failure_still_reaps_the_child() {
        use std::{
            num::NonZeroU32,
            process::Command,
            sync::{
                atomic::{AtomicU32, Ordering},
                Arc,
            },
            thread,
            time::{Duration, Instant},
        };
        let command = if cfg!(windows) {
            let mut command = Command::new("powershell.exe");
            command.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ]);
            command
        } else {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", "exec sleep 30"]);
            command
        };
        let mut command = super::OwnedCommand::from_command(command);
        command.partial_spawn_cleanup_timeout(Duration::ZERO);
        let captured = Arc::new(AtomicU32::new(0));
        let started = Instant::now();
        super::cleanup_test_fault::arm();
        let error = command
            .spawn_inner(false, true, Some(Arc::clone(&captured)))
            .err()
            .unwrap();
        let detail = crate::RunnerFailure::from_io(&error).unwrap();
        assert_eq!(detail.phase(), crate::RunnerPhase::Cleanup);
        assert_eq!(detail.termination(), crate::LeaderTermination::Unconfirmed);
        assert!(started.elapsed() < Duration::from_secs(3));
        let pid = captured.load(Ordering::SeqCst);
        assert_ne!(pid, 0);
        let process = super::AdoptedProcess::new(NonZeroU32::new(pid).unwrap());
        let started = Instant::now();
        while process.is_running().unwrap() {
            assert!(started.elapsed() < Duration::from_secs(5));
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn owned_commands_hide_their_console_window_by_default() {
        assert!(super::OwnedCommand::new("x").windows_hidden);
        assert!(super::OwnedCommand::from_command(std::process::Command::new("x")).windows_hidden);
    }

    /// A kill-on-close job standing in for an in-process host's own job object.
    #[cfg(windows)]
    fn host_job() -> std::os::windows::io::OwnedHandle {
        use std::os::windows::io::FromRawHandle;
        use windows::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        let job = unsafe { CreateJobObjectW(None, None) }.unwrap();
        let job_handle = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(job.0 as _) };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            )
        }
        .unwrap();
        job_handle
    }

    #[cfg(windows)]
    fn in_job(pid: u32, job: &std::os::windows::io::OwnedHandle) -> bool {
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut std::ffi::c_void;
            fn IsProcessInJob(
                process: *mut std::ffi::c_void,
                job: *mut std::ffi::c_void,
                result: *mut i32,
            ) -> i32;
            fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
        }
        unsafe {
            let process = OpenProcess(0x1000, 0, pid); // PROCESS_QUERY_LIMITED_INFORMATION
            assert!(!process.is_null());
            let mut inside = 0;
            assert_ne!(
                IsProcessInJob(process, job.as_raw_handle() as _, &mut inside),
                0
            );
            CloseHandle(process);
            inside != 0
        }
    }

    #[cfg(windows)]
    #[test]
    fn children_join_the_callers_job_and_die_when_the_caller_closes_it() {
        use std::os::windows::io::AsHandle;
        use std::time::{Duration, Instant};
        let job = host_job();
        let sleeper = || {
            let mut command = super::OwnedCommand::new("powershell.exe");
            command.command_mut().args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 60",
            ]);
            command.windows_job(job.as_handle()).unwrap();
            command.spawn().unwrap()
        };
        // The second child joins a job that already holds a process: binding must happen
        // before RightKit's own jobs, or Windows refuses it.
        let (first, second) = (sleeper(), sleeper());
        let pids = [first.id(), second.id()];
        assert!(
            pids.iter().all(|pid| in_job(*pid, &job)),
            "children must be in the caller's job"
        );

        // The caller closes its job while the owned children are still alive: they die.
        drop(job);
        let deadline = Instant::now() + Duration::from_secs(5);
        for pid in pids {
            let process = super::AdoptedProcess::new(std::num::NonZeroU32::new(pid).unwrap());
            while process.is_running().unwrap_or(false) {
                assert!(
                    Instant::now() < deadline,
                    "child {pid} survived its caller's job"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        drop((first, second));
    }

    #[test]
    fn a_failure_after_spawn_cleans_up_the_partially_wrapped_child() {
        use std::{
            process::Command,
            sync::{
                atomic::{AtomicU32, Ordering},
                Arc,
            },
            thread,
            time::{Duration, Instant},
        };

        // A long-lived child that survives on its own if the partial-spawn cleanup fails.
        let command = if cfg!(windows) {
            // windowless: spawned through spawn_owned (CREATE_NO_WINDOW).
            let mut command = Command::new("powershell.exe");
            command.args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 60",
            ]);
            command
        } else {
            // windowless: spawned through spawn_owned (CREATE_NO_WINDOW).
            let mut command = Command::new("sleep");
            command.arg("60");
            command
        };
        let spawned_pid = Arc::new(AtomicU32::new(0));

        let error = match super::spawn_for_test(command, Arc::clone(&spawned_pid)) {
            Ok(_) => panic!("injected wrapping failure must fail after spawning"),
            Err(error) => error,
        };
        let detail = crate::RunnerFailure::from_io(&error).expect("partial-spawn provenance");
        assert_eq!(detail.phase(), crate::RunnerPhase::Setup);
        assert_eq!(detail.termination(), crate::LeaderTermination::Unconfirmed);
        let pid = spawned_pid.load(Ordering::SeqCst);
        assert_ne!(pid, 0, "test must fail after the OS process was spawned");
        let process = super::AdoptedProcess::new(std::num::NonZeroU32::new(pid).unwrap());
        let deadline = Instant::now() + Duration::from_secs(5);
        while process.is_running().unwrap_or(false) {
            assert!(
                Instant::now() < deadline,
                "partially spawned process leaked"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// An owned tree whose root starts a grandchild (left in the root's private job) and
    /// records its pid; with `wait` the root then waits on it, else the root exits at once.
    #[cfg(windows)]
    fn tree_with_grandchild(pid_file: &std::path::Path, wait: bool) -> (super::OwnedChild, u32) {
        use std::time::{Duration, Instant};
        let _ = std::fs::remove_file(pid_file);
        let quoted = pid_file.display().to_string().replace('\'', "''");
        let tail = if wait {
            "; Wait-Process -Id $child.Id"
        } else {
            ""
        };
        let mut command = std::process::Command::new("powershell.exe");
        command
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!(
                    "$child = Start-Process powershell.exe -WindowStyle Hidden -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 60' -PassThru; Set-Content -LiteralPath '{quoted}' -Value $child.Id{tail}"
                ),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut command = super::OwnedCommand::from_command(command);
        command.windows_hide();
        let owned = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let grandchild = loop {
            if let Some(pid) = std::fs::read_to_string(pid_file)
                .ok()
                .and_then(|text| text.trim().parse().ok())
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "grandchild pid was not recorded");
            std::thread::sleep(Duration::from_millis(20));
        };
        (owned, grandchild)
    }

    #[cfg(windows)]
    fn job_active(owned: &super::OwnedChild) -> u32 {
        let job = owned
            .job
            .as_ref()
            .expect("an owned child keeps its private job");
        crate::windows_job::WindowsJob::active_count_of(crate::process_windows::raw_job(job))
            .unwrap()
    }

    /// rightkit#1260: `Exited` means the whole private job is empty, not only that the root
    /// exited. Read with no wait after the call returns.
    #[cfg(windows)]
    #[test]
    fn bounded_reap_reports_exited_only_once_the_job_is_empty() {
        use std::time::Duration;
        let pid_file =
            std::env::temp_dir().join(format!("rightkit-1260-live-{}.pid", std::process::id()));
        let (mut owned, grandchild) = tree_with_grandchild(&pid_file, true);
        let grandchild = super::AdoptedProcess::new(std::num::NonZeroU32::new(grandchild).unwrap());
        assert!(grandchild.is_running().unwrap());
        assert!(
            job_active(&owned) >= 2,
            "root and grandchild are in the job"
        );

        let outcome = owned
            .terminate_tree_bounded(Duration::from_secs(10))
            .unwrap();
        assert!(
            matches!(outcome, super::ReapOutcome::Exited(_)),
            "{outcome:?}"
        );
        assert_eq!(
            job_active(&owned),
            0,
            "Exited while the job still held a process"
        );
        assert!(
            !grandchild.is_running().unwrap_or(false),
            "grandchild outlived Exited"
        );
        let _ = std::fs::remove_file(&pid_file);
    }

    /// rightkit#1260: a root that already exited (and was reaped) still has its descendants
    /// swept, and `Exited` still waits for the job to empty.
    #[cfg(windows)]
    #[test]
    fn bounded_reap_sweeps_descendants_of_an_already_exited_root() {
        use std::time::Duration;
        let pid_file =
            std::env::temp_dir().join(format!("rightkit-1260-orphan-{}.pid", std::process::id()));
        let (mut owned, grandchild) = tree_with_grandchild(&pid_file, false);
        let grandchild = super::AdoptedProcess::new(std::num::NonZeroU32::new(grandchild).unwrap());
        assert!(
            owned
                .wait_timeout(Duration::from_secs(15))
                .unwrap()
                .is_some(),
            "root must exit on its own"
        );
        assert!(
            grandchild.is_running().unwrap(),
            "grandchild outlives its root"
        );
        assert!(
            job_active(&owned) >= 1,
            "grandchild stays in the root's job"
        );

        let outcome = owned
            .terminate_tree_bounded(Duration::from_secs(10))
            .unwrap();
        assert!(
            matches!(outcome, super::ReapOutcome::Exited(_)),
            "{outcome:?}"
        );
        assert_eq!(job_active(&owned), 0);
        assert!(
            !grandchild.is_running().unwrap_or(false),
            "descendant was not swept"
        );
        let _ = std::fs::remove_file(&pid_file);
    }

    /// rightkit#1260: a job that is not seen empty within the budget is unconfirmed, even
    /// though the root itself exited, and the call returns at the budget, not later.
    #[cfg(windows)]
    #[test]
    fn bounded_reap_is_unconfirmed_while_the_job_is_not_empty() {
        use std::time::{Duration, Instant};
        let pid_file =
            std::env::temp_dir().join(format!("rightkit-1260-timeout-{}.pid", std::process::id()));
        let (mut owned, _) = tree_with_grandchild(&pid_file, true);
        let budget = Duration::from_millis(500);
        super::cleanup_test_fault::arm_job_empty_at(Instant::now() + Duration::from_secs(3600));
        let started = Instant::now();
        let outcome = owned.terminate_tree_bounded(budget);
        let elapsed = started.elapsed();
        super::cleanup_test_fault::disarm_job_empty_at();
        assert!(
            matches!(outcome, Ok(super::ReapOutcome::TimedOut)),
            "{outcome:?}"
        );
        assert!(
            elapsed < budget + Duration::from_millis(250),
            "budget overrun: {elapsed:?}"
        );
        let _ = std::fs::remove_file(&pid_file);
    }

    /// rightkit#1260 review: an emptiness observed only after the deadline (the last poll's
    /// sleep overran it) is not success.
    #[cfg(windows)]
    #[test]
    fn job_seen_empty_only_after_the_deadline_is_not_success() {
        use std::time::{Duration, Instant};
        let pid_file =
            std::env::temp_dir().join(format!("rightkit-1260-late-{}.pid", std::process::id()));
        let (owned, _) = tree_with_grandchild(&pid_file, true);
        let job = owned.job.clone().expect("private job");
        let timeout = Duration::from_millis(100);
        // Empty only from 5 ms after the wait's own deadline.
        super::cleanup_test_fault::arm_job_empty_at(
            Instant::now() + timeout + Duration::from_millis(5),
        );
        let late = job.wait_empty(timeout);
        // Control: empty well inside the budget is success.
        super::cleanup_test_fault::arm_job_empty_at(Instant::now() + Duration::from_millis(20));
        let early = job.wait_empty(Duration::from_millis(500));
        super::cleanup_test_fault::disarm_job_empty_at();
        assert!(
            !late.unwrap(),
            "a job seen empty after the deadline was reported empty"
        );
        assert!(
            early.unwrap(),
            "control: a job empty inside the budget is reported empty"
        );
        drop(owned);
        let _ = std::fs::remove_file(&pid_file);
    }

    /// rightkit#1260 review: the bounded call's kill step never waits its fixed two seconds for
    /// an exiting leader when less budget is left.
    #[cfg(windows)]
    #[test]
    fn bounded_reap_never_waits_past_its_budget_for_an_exiting_leader() {
        use std::time::{Duration, Instant};
        let pid_file =
            std::env::temp_dir().join(format!("rightkit-1260-refused-{}.pid", std::process::id()));
        let (mut owned, _) = tree_with_grandchild(&pid_file, true);
        let budget = Duration::from_millis(300);
        super::cleanup_test_fault::arm_termination_refused();
        let started = Instant::now();
        let outcome = owned.terminate_tree_bounded(budget);
        let elapsed = started.elapsed();
        assert!(
            !matches!(outcome, Ok(super::ReapOutcome::Exited(_))),
            "a refused termination was confirmed: {outcome:?}"
        );
        assert!(
            elapsed < budget + Duration::from_millis(250),
            "the kill step outlasted the budget: {elapsed:?}"
        );
        drop(owned);
        let _ = std::fs::remove_file(&pid_file);
    }
}

#[cfg(test)]
mod tree_listing_tests {
    use super::listed_or_error;

    #[test]
    fn an_unknown_group_listing_is_an_error_not_an_empty_tree() {
        let error = listed_or_error(None).expect_err("unknown must not read as empty");
        assert!(
            error.to_string().contains("could not be fully listed"),
            "{error}"
        );
    }

    #[test]
    fn a_known_listing_passes_through_including_an_empty_one() {
        assert_eq!(listed_or_error(Some(vec![7, 9])).unwrap(), vec![7, 9]);
        assert!(listed_or_error(Some(Vec::new())).unwrap().is_empty());
    }
}
