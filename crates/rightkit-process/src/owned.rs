use crate::process_handle::{lock_child, ProcessHandle};
#[cfg(unix)]
use crate::unix_child::{LeaderState, SignalOutcome, SignalTarget};
use std::{
    ffi::OsStr,
    io,
    num::NonZeroU32,
    process::{ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus},
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
    caller_job: CallerJob,
    job_assignment: JobAssignmentMode,
    unix_containment: UnixContainment,
    kill_on_parent_exit: bool,
    // None preserves the existing unbounded partial-spawn reap.
    partial_spawn_cleanup_timeout: Option<Duration>,
    inherited_handles: Option<Vec<InheritedValue>>,
    #[cfg(windows)]
    windows_spawn_config: Option<crate::WindowsSpawnConfig>,
    #[cfg(windows)]
    windows_spawn_config_auto: bool,
    #[cfg(windows)]
    env_cleared: bool,
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
            caller_job: None,
            job_assignment: JobAssignmentMode::Strict,
            unix_containment: UnixContainment::ProcessGroup,
            kill_on_parent_exit: false,
            partial_spawn_cleanup_timeout: None,
            inherited_handles: None,
            #[cfg(windows)]
            windows_spawn_config: None,
            #[cfg(windows)]
            windows_spawn_config_auto: false,
            #[cfg(windows)]
            env_cleared: false,
        }
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
    /// Supply opaque stdio/environment/raw-arg settings for native allowlisted spawns.
    /// Required after from_command/command_mut when inherit_handles_only is set.
    /// Ordinary std spawns ignore it; fresh new() Commands have known defaults.
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
        #[cfg(windows)]
        {
            self.env_cleared = true;
        }
        for (k, v) in kept {
            self.command.env(k, v);
        }
        self
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
    /// Opt-in, off by default. Linux only: the kernel sends `SIGTERM` to the direct child
    /// (`PR_SET_PDEATHSIG`) when the thread that spawned it exits, which includes this
    /// process dying without running cleanup (for example SIGKILL). The signal reaches the
    /// direct child only, never the rest of its group, it can be caught, and there is no
    /// `SIGKILL` escalation. It is keyed to the spawning *thread*, so spawn from a thread
    /// that lives as long as the child.
    ///
    /// Other Unix hosts, macOS included, have no parent-death primitive this crate relies on:
    /// the spawn fails with `ErrorKind::Unsupported` before anything is started, rather than
    /// run the child untied. Windows ignores the option because kill-on-close job objects
    /// already cover parent death. Rejected (`InvalidInput`) for detached spawns.
    pub fn kill_on_parent_exit(&mut self, enabled: bool) -> &mut Self {
        self.kill_on_parent_exit = enabled;
        self
    }
    /// Bound failed-spawn cleanup. Timed-out children go to a background reaper.
    pub fn partial_spawn_cleanup_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.partial_spawn_cleanup_timeout = Some(timeout);
        self
    }
    pub fn spawn(self) -> io::Result<OwnedChild> {
        self.spawn_inner(false, false, None)
    }
    /// Shared launch with no owner job or terminate-on-drop. Unix uses setsid;
    /// Windows requires breakaway and never falls back when escape is denied.
    /// Stdio follows Command: dropping piped stdin can still deliver EOF.
    pub fn spawn_uncontained_detached(self) -> io::Result<OwnedChild> {
        self.spawn_inner(true, false, None)
    }
    fn spawn_inner(
        mut self,
        detached: bool,
        fail_after_spawn: bool,
        captured_pid: Option<Arc<std::sync::atomic::AtomicU32>>,
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
        #[cfg(all(unix, not(target_os = "linux")))]
        if self.kill_on_parent_exit {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "kill_on_parent_exit is supported on Linux only",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let session = detached || self.unix_containment == UnixContainment::Session;
            let inheritance = self
                .inherited_handles
                .take()
                .map(crate::process_unix::Inheritance::new)
                .transpose()?;
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
        crate::process_windows::configure(&mut self.command, detached, self.windows_hidden);
        #[cfg(unix)]
        let _ = (self.windows_hidden, self.job_assignment);
        #[cfg(windows)]
        let _ = (self.unix_containment, self.kill_on_parent_exit);
        let spawned_at = SystemTime::now();
        #[cfg(unix)]
        let child = {
            let mut child = crate::unix_child::UnixChild::new(self.command.spawn()?);
            // A child whose pid cannot be tied to it is stopped here, while the pid is
            // certainly still its own: no later signal could be proven to reach it.
            child.require_identity()?;
            child
        };
        #[cfg(windows)]
        let child: PlatformChild = match self.inherited_handles.as_deref() {
            None => {
                // An ordinary spawn inherits every inheritable handle. Held so it never runs
                // while an allowlisted spawn has private handles raised.
                let _spawning = crate::spawn_windows::spawn_guard();
                self.command.spawn()?.into()
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
                    self.env_cleared,
                    self.partial_spawn_cleanup_timeout,
                )?
            }
        };
        let pid = child.id();
        if let Some(captured) = captured_pid {
            captured.store(pid, std::sync::atomic::Ordering::SeqCst);
        }
        let child = Arc::new(Mutex::new(child));
        let handle = match ProcessHandle::new(&child, spawned_at) {
            Ok(handle) => handle,
            Err(error) => {
                cleanup_direct(child, self.partial_spawn_cleanup_timeout);
                return Err(error);
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
                #[cfg(windows)]
                job: None,
            }),
            timeout: self.partial_spawn_cleanup_timeout,
        };
        #[cfg(windows)]
        if !detached {
            let owned = partial.child.as_mut().expect("partial child is present");
            crate::process_windows::assign_jobs(
                &owned.handle,
                self.caller_job.take(),
                self.job_assignment,
                &mut owned.job,
                &mut owned.assignment_failures,
            )?;
        }
        #[cfg(windows)]
        partial
            .child
            .as_ref()
            .expect("partial child is present")
            .child()
            .resume(detached)?;
        if fail_after_spawn {
            return Err(io::Error::other("injected post-spawn wrapping failure"));
        }
        Ok(partial.child.take().expect("partial child is present"))
    }
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
    #[cfg(windows)]
    job: Option<crate::process_windows::KillOnCloseJob>,
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
        #[cfg(unix)]
        {
            if !self.tree_terminated {
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
        self.start_termination(options)?;
        Ok(
            match self.wait_timeout(timeout.saturating_sub(started.elapsed()))? {
                Some(status) => ReapOutcome::Exited(status),
                None => ReapOutcome::TimedOut,
            },
        )
    }
    fn start_termination(&mut self, options: TerminationOptions) -> io::Result<()> {
        #[cfg(unix)]
        if !self.tree_terminated {
            self.signal(libc::SIGTERM)?;
            // Stops waiting as soon as nothing but the exited, unreaped leader is left (or the
            // leader was reaped elsewhere, which ends signalling). The leader is not reaped
            // here: it must keep pinning the group id for the KILL below.
            self.await_tree(options.grace)?;
        }
        self.force_kill(options.windows_exit_code)
    }
    /// Waits, for at most `grace`, until the leader has exited and every other member of its
    /// group has too. The leader is observed without reaping it; then the group is listed
    /// every [`POLL_INTERVAL`](crate::exit_watch::POLL_INTERVAL) (the published 0.3.3 interval)
    /// until nothing but the leader is left. A group that cannot be listed is waited out for
    /// the rest of the grace in one timed sleep. Each listing takes the child lock briefly and
    /// releases it before the thread sleeps. Bounded by `grace` in every case.
    #[cfg(unix)]
    fn await_tree(&self, grace: Duration) -> io::Result<()> {
        use crate::{exit_watch::POLL_INTERVAL, unix_child::Members};
        let Some(deadline) = Instant::now().checked_add(grace) else {
            return Ok(());
        };
        let left = |deadline: Instant| deadline.saturating_duration_since(Instant::now());
        if !self.handle.wait_exited_timeout(left(deadline))? {
            return Ok(()); // the leader outlived the grace
        }
        if self.detached {
            return Ok(()); // a detached child owns only its direct pid
        }
        loop {
            match lock_child(&self.child).group_other_members()? {
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
        let started = Instant::now();
        loop {
            let (outcome, watch) = {
                let mut child = lock_child(&self.child);
                (child.signal(target, signal)?, child.exit_watch())
            };
            if outcome != SignalOutcome::MidExit {
                return Ok(());
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
    fn force_kill(&mut self, code: u32) -> io::Result<()> {
        if self.tree_terminated {
            return Ok(());
        }
        #[cfg(unix)]
        {
            let _ = code;
            self.signal(libc::SIGKILL)?;
        }
        #[cfg(windows)]
        crate::process_windows::terminate(&self.handle, self.job.as_ref(), code)?;
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
impl Drop for OwnedChild {
    fn drop(&mut self) {
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

struct PartialChild {
    child: Option<OwnedChild>,
    timeout: Option<Duration>,
}
impl Drop for PartialChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Disable unbounded normal Drop before bounded cleanup.
            child.terminate_on_drop = false;
            let _ = child.force_kill(1);
            cleanup_direct(Arc::clone(&child.child), self.timeout);
        }
    }
}
/// How long a group signal that macOS refused with `EPERM` (the last members are mid-exit) is
/// retried before the refusal is reported.
#[cfg(unix)]
const MID_EXIT_RETRY: Duration = Duration::from_millis(300);

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
pub(crate) fn cleanup_direct(child: Arc<Mutex<PlatformChild>>, timeout: Option<Duration>) {
    let _ = lock_child(&child).kill();
    let Some(timeout) = timeout else {
        // Unbounded by default, as before. Exclusive child: see above.
        let _ = lock_child(&child).wait();
        return;
    };
    #[cfg(unix)]
    {
        // Non-blocking looks, each under the lock and released at once, until the child is
        // reaped or the timeout passes. A killed child is gone within one hint or interval.
        let watch = lock_child(&child).exit_watch();
        let deadline = Instant::now().checked_add(timeout);
        if matches!(
            watch.wait_for(deadline, || lock_child(&child).try_wait()),
            Ok(Some(_))
        ) {
            return;
        }
    }
    #[cfg(windows)]
    {
        let started = Instant::now();
        loop {
            match lock_child(&child).try_wait() {
                Ok(Some(_)) => return,
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
    let pid = i32::try_from(pid.get()).map_err(io::Error::other)?;
    loop {
        // SAFETY: signal 0 performs only existence and permission checks; nothing is sent.
        if unsafe { libc::kill(pid, 0) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EPERM) => return Ok(true),
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
    fn kill_on_parent_exit_is_opt_in() {
        let mut command = super::OwnedCommand::new("unused");
        assert!(!command.kill_on_parent_exit);
        command.kill_on_parent_exit(true);
        assert!(command.kill_on_parent_exit);
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

    /// Hosts other than Linux have no parent-death primitive: the option fails closed, and the
    /// program is never started (it would create the marker file).
    #[cfg(all(unix, not(target_os = "linux")))]
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
        assert!(command
            .spawn_inner(false, true, Some(Arc::clone(&captured)))
            .is_err());
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

        assert!(super::spawn_for_test(command, Arc::clone(&spawned_pid)).is_err());
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
}
