use crate::process_handle::{lock_child, ProcessHandle};
#[cfg(unix)]
use crate::unix_child::{LeaderState, SignalTarget};
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

/// Windows: how a tolerant detached launch ended up. See
/// [`OwnedCommand::spawn_detached_tolerating_denied_breakaway`].
#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DetachedBreakaway {
    /// The child left the parent's job (`CREATE_BREAKAWAY_FROM_JOB` was accepted).
    BrokeAway,
    /// Breakaway was denied (`ERROR_ACCESS_DENIED`), so the child was started with
    /// `DETACHED_PROCESS | CREATE_NO_WINDOW` and remains a member of the parent's job.
    StayedInJob,
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
    #[cfg(windows)]
    tolerate_denied_breakaway: bool,
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
            #[cfg(windows)]
            tolerate_denied_breakaway: false,
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
    /// Opt-in, off by default. When this process dies without running cleanup
    /// (for example SIGKILL), the child's process group is terminated.
    /// Linux sends SIGTERM to the direct child (`PR_SET_PDEATHSIG`); macOS runs a
    /// helper in the child's group that sends SIGTERM, waits 2 s, then SIGKILL.
    /// Other Unix hosts fail the spawn. Windows ignores it because kill-on-close
    /// job objects already cover parent death. Rejected for detached spawns.
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
    /// Windows, explicit opt-in. Like [`spawn_uncontained_detached`](Self::spawn_uncontained_detached),
    /// but when the parent job denies breakaway (`ERROR_ACCESS_DENIED`) the launch is retried
    /// once with `DETACHED_PROCESS | CREATE_NO_WINDOW` and the child stays inside that job.
    /// The result says which happened. Any other failure is returned as is, and the plain
    /// `spawn_uncontained_detached` keeps failing closed when breakaway is denied.
    #[cfg(windows)]
    pub fn spawn_detached_tolerating_denied_breakaway(
        mut self,
    ) -> io::Result<(OwnedChild, DetachedBreakaway)> {
        self.tolerate_denied_breakaway = true;
        let child = self.spawn_inner(true, false, None)?;
        let outcome = if child.stayed_in_job {
            DetachedBreakaway::StayedInJob
        } else {
            DetachedBreakaway::BrokeAway
        };
        Ok((child, outcome))
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
        // On macOS this forks the parent-death helper, so it happens here, in the supervisor,
        // before the new process exists. A later `?` drops the tie, which kills the helper.
        #[cfg(unix)]
        let parent_tie = self
            .kill_on_parent_exit
            .then(|| {
                crate::process_unix::ParentDeathTie::prepare(
                    self.unix_containment == UnixContainment::Session,
                )
            })
            .transpose()?;
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let session = detached || self.unix_containment == UnixContainment::Session;
            let inheritance = self
                .inherited_handles
                .take()
                .map(crate::process_unix::Inheritance::new)
                .transpose()?;
            let engine_tie = parent_tie
                .as_ref()
                .map(crate::process_unix::ParentDeathTie::engine_side);
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
                    // Last, after setpgid/setsid, so the macOS helper joins the child's group.
                    if let Some(tie) = &engine_tie {
                        tie.apply_in_child()?;
                    }
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        crate::process_windows::configure(&mut self.command, detached, self.windows_hidden, false);
        #[cfg(unix)]
        let _ = (self.windows_hidden, self.job_assignment);
        #[cfg(windows)]
        let _ = (self.unix_containment, self.kill_on_parent_exit);
        let spawned_at = SystemTime::now();
        #[cfg(unix)]
        let child = {
            let spawned = self.command.spawn();
            // Settle the parent-death helper whatever the outcome: kept on success, released
            // (told to stand down, never signalled) and collected on failure, so a failed
            // spawn leaves nothing running untied.
            let helper = parent_tie.and_then(|tie| tie.settle(spawned.is_ok()));
            crate::unix_child::UnixChild::new(spawned?, helper)
        };
        #[cfg(windows)]
        let mut stayed_in_job = false;
        #[cfg(windows)]
        let child: PlatformChild = match self.inherited_handles.as_deref() {
            None => match self.command.spawn() {
                Ok(child) => child.into(),
                Err(error)
                    if detached && self.tolerate_denied_breakaway && is_access_denied(&error) =>
                {
                    // Breakaway denied by the parent job: retry once, detached, in the job.
                    crate::process_windows::configure(
                        &mut self.command,
                        true,
                        self.windows_hidden,
                        true,
                    );
                    stayed_in_job = true;
                    self.command.spawn()?.into()
                }
                Err(error) => return Err(error),
            },
            Some(handles) => {
                let config = self.windows_spawn_config.take().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput,
                        "allowlisted Windows spawn requires windows_spawn_config for opaque Command settings")
                })?;
                let (child, stayed) = crate::spawn_windows::spawn(
                    &self.command,
                    config,
                    handles,
                    detached,
                    detached && self.tolerate_denied_breakaway,
                    self.windows_hidden,
                    self.env_cleared,
                    self.partial_spawn_cleanup_timeout,
                )?;
                stayed_in_job = stayed;
                child
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
                tree_terminated: false,
                assignment_failures: Vec::new(),
                #[cfg(windows)]
                job: None,
                #[cfg(windows)]
                stayed_in_job,
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
    tree_terminated: bool,
    assignment_failures: Vec<JobAssignmentFailure>,
    #[cfg(windows)]
    job: Option<crate::process_windows::KillOnCloseJob>,
    #[cfg(windows)]
    stayed_in_job: bool,
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
    pub fn child(&self) -> MutexGuard<'_, PlatformChild> {
        lock_child(&self.child)
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
            let started = Instant::now();
            loop {
                match self.leader_state()? {
                    LeaderState::Running => {}
                    LeaderState::Exited => {
                        // Not reaped yet: the zombie still pins the group id, so the
                        // stragglers can be killed safely, and only then reaped.
                        self.force_kill(1)?;
                        return self.wait().map(WaitOutcome::Exited);
                    }
                    LeaderState::Reaped => {
                        // Reaped elsewhere (a handle clone): the cached status is returned
                        // and group signalling is over.
                        return self.wait().map(WaitOutcome::Exited);
                    }
                }
                if started.elapsed() >= grace {
                    return self.terminate_tree().map(WaitOutcome::Terminated);
                }
                thread::sleep(
                    Duration::from_millis(5).min(grace.saturating_sub(started.elapsed())),
                );
            }
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
            let started = Instant::now();
            while started.elapsed() < options.grace {
                // Stop waiting once nothing but the exited, unreaped leader is left (or the
                // leader was reaped elsewhere, which ends signalling). The leader is not
                // reaped here: it must keep pinning the group id for the KILL below.
                if self.tree_settled()? {
                    break;
                }
                thread::sleep(
                    Duration::from_millis(5).min(options.grace.saturating_sub(started.elapsed())),
                );
            }
        }
        self.force_kill(options.windows_exit_code)
    }
    #[cfg(unix)]
    fn signal(&self, signal: i32) -> io::Result<()> {
        // Detached children only own their direct pid. Under the child's lock, and only while
        // the leader is unreaped, so the id cannot have been recycled.
        let target = if self.detached {
            SignalTarget::Leader
        } else {
            SignalTarget::Group
        };
        lock_child(&self.child).signal(target, signal).map(|_| ())
    }
    #[cfg(unix)]
    fn leader_state(&self) -> io::Result<LeaderState> {
        lock_child(&self.child).leader_state()
    }
    #[cfg(unix)]
    fn tree_settled(&self) -> io::Result<bool> {
        let mut child = lock_child(&self.child);
        Ok(match child.leader_state()? {
            LeaderState::Running => false,
            LeaderState::Reaped => true,
            LeaderState::Exited => self.detached || !child.group_has_other_members()?,
        })
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
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.terminate_on_drop {
            let _ = self.terminate_tree();
        } else if self.detached {
            #[cfg(unix)]
            {
                // Shared launch still needs eventual reaping. Close owner pipes
                // now; poll cached std state without blocking exit-reader locks.
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
pub(crate) fn cleanup_direct(child: Arc<Mutex<PlatformChild>>, timeout: Option<Duration>) {
    let _ = lock_child(&child).kill();
    let Some(timeout) = timeout else {
        let _ = lock_child(&child).wait();
        return;
    };
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
    // std Child does not reap on Drop. Transfer timed-out children to a waiter.
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
}
#[cfg(test)]
fn spawn_for_test(
    command: Command,
    captured_pid: Arc<std::sync::atomic::AtomicU32>,
) -> io::Result<OwnedChild> {
    OwnedCommand::from_command(command).spawn_inner(false, true, Some(captured_pid))
}

#[cfg(windows)]
fn is_access_denied(error: &io::Error) -> bool {
    error.raw_os_error() == Some(5) // ERROR_ACCESS_DENIED
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

    /// Runs wherever the suite runs: if the host job allows breakaway the child breaks
    /// away, and if it denies it the child stays in the job. Either way the call succeeds and
    /// reports which. (The denied branch needs a host job without breakaway to be exercised.)
    #[cfg(windows)]
    #[test]
    fn tolerant_detached_spawn_succeeds_and_reports_how_it_started() {
        use super::DetachedBreakaway;
        let mut command = super::OwnedCommand::new("cmd.exe");
        command.command_mut().args(["/c", "exit 0"]);
        let (mut child, outcome) = command
            .spawn_detached_tolerating_denied_breakaway()
            .expect("tolerant detached spawn");
        assert!(matches!(
            outcome,
            DetachedBreakaway::BrokeAway | DetachedBreakaway::StayedInJob
        ));
        assert!(child.wait().expect("wait").success());
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
