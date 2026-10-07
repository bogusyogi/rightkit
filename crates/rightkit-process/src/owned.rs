use std::{
    ffi::OsStr,
    io,
    num::NonZeroU32,
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use process_wrap::std::{StdChildWrapper, StdCommandWrap, StdCommandWrapper};

#[derive(Debug)]
pub struct OwnedCommand {
    command: Command,
    windows_hidden: bool,
}

impl OwnedCommand {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        // windowless: from_command enables CREATE_NO_WINDOW for every OwnedCommand.
        Self::from_command(Command::new(program))
    }

    /// On Windows the child gets CREATE_NO_WINDOW by default: RightKit hosts are GUI apps and
    /// background services, so a console window would only ever flash on the user's desktop.
    pub fn from_command(command: Command) -> Self {
        Self {
            command,
            windows_hidden: true,
        }
    }

    pub fn command_mut(&mut self) -> &mut Command {
        &mut self.command
    }

    /// Run the child with a scrubbed environment: everything is cleared, then
    /// only the named variables that are set in this process are copied
    /// across (HeardRight's sidecar whitelist). Include `PATH` and the
    /// platform basics yourself, or start from [`DEFAULT_ENV_ALLOWLIST`].
    /// Variables set explicitly afterwards through [`Self::command_mut`] win.
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
        for (k, v) in kept {
            self.command.env(k, v);
        }
        self
    }

    /// Remove the named variables from the child's inherited environment
    /// (secrets, BYOK keys) without clearing the rest.
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

    /// Kept for callers written before hiding became the default; it is now a no-op.
    pub fn windows_hide(&mut self) -> &mut Self {
        self.windows_hidden = true;
        self
    }

    pub fn spawn(self) -> io::Result<OwnedChild> {
        spawn_owned(self.command, self.windows_hidden, false, None)
    }
}

/// Variables a sidecar normally needs to run at all; a base for
/// [`OwnedCommand::env_allowlist`].
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
    child: Option<Box<dyn StdChildWrapper>>,
}

/// Result of [`OwnedChild::terminate_tree_bounded`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReapOutcome {
    /// Already gone, or the exit was observed within the bound.
    Exited(ExitStatus),
    /// The kill was issued but the OS did not report the exit in time (a
    /// process wedged in the kernel, e.g. releasing a GPU model). The caller is
    /// not blocked; the owned handle still reaps on drop.
    TimedOut,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitOutcome {
    Exited(ExitStatus),
    Terminated(ExitStatus),
}

impl OwnedChild {
    pub fn id(&self) -> u32 {
        self.child().id()
    }

    pub fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child_mut().stdin().take()
    }

    pub fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child_mut().stdout().take()
    }

    pub fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child_mut().stderr().take()
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child_mut().try_wait()
    }

    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child_mut().wait()
    }

    pub fn wait_timeout(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        let started = Instant::now();
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if started.elapsed() >= timeout {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(5).min(timeout.saturating_sub(started.elapsed())));
        }
    }

    pub fn wait_or_kill(&mut self, grace: Duration) -> io::Result<WaitOutcome> {
        #[cfg(unix)]
        self.child().signal(nix::libc::SIGTERM)?;

        if let Some(status) = self.wait_timeout(grace)? {
            return Ok(WaitOutcome::Exited(status));
        }
        self.terminate_tree().map(WaitOutcome::Terminated)
    }

    pub fn terminate_tree(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.try_wait()? {
            return Ok(status);
        }
        self.child_mut().start_kill()?;
        self.child_mut().wait()
    }

    /// Kill the whole tree now and wait at most `timeout` for the OS to report
    /// the exit. Unlike [`Self::terminate_tree`] this never blocks past the
    /// bound, so tray-quit / window-close paths stay responsive even if the
    /// child cannot be reaped promptly (HeardRight's bounded reap).
    pub fn terminate_tree_bounded(&mut self, timeout: Duration) -> io::Result<ReapOutcome> {
        if let Some(status) = self.try_wait()? {
            return Ok(ReapOutcome::Exited(status));
        }
        self.child_mut().start_kill()?;
        Ok(match self.wait_timeout(timeout)? {
            Some(status) => ReapOutcome::Exited(status),
            None => ReapOutcome::TimedOut,
        })
    }

    fn child(&self) -> &dyn StdChildWrapper {
        self.child
            .as_deref()
            .expect("owned child is always present")
    }

    fn child_mut(&mut self) -> &mut dyn StdChildWrapper {
        self.child
            .as_deref_mut()
            .expect("owned child is always present")
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.terminate_tree();
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

    pub fn is_running(&self) -> io::Result<bool> {
        process_is_running(self.pid)
    }
}

#[derive(Debug)]
struct CleanupWrapper {
    captured_pid: Option<Arc<AtomicU32>>,
}

impl StdCommandWrapper for CleanupWrapper {
    fn post_spawn(&mut self, child: &mut Child, _core: &StdCommandWrap) -> io::Result<()> {
        if let Some(pid) = &self.captured_pid {
            pid.store(child.id(), Ordering::SeqCst);
        }
        Ok(())
    }

    fn wrap_child(
        &mut self,
        child: Box<dyn StdChildWrapper>,
        _core: &StdCommandWrap,
    ) -> io::Result<Box<dyn StdChildWrapper>> {
        // Runs first, while process-wrap's JobObject still holds the child suspended, so the
        // child and everything it later spawns land in this kill-on-close job.
        #[cfg(windows)]
        let kill_on_close = Some(KillOnCloseJob::assign(child.inner())?);
        Ok(Box::new(CleanupChild {
            child: Some(child),
            #[cfg(windows)]
            _kill_on_close: kill_on_close,
        }))
    }
}

/// process-wrap's std `JobObject` never sets JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, so a host that
/// dies without running destructors (crash, TerminateProcess) left its tree running on Windows.
/// This second job's only handle lives in the host; when the host goes away for any reason the
/// kernel closes it and kills every process in the tree.
#[cfg(windows)]
#[derive(Debug)]
struct KillOnCloseJob(windows::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl KillOnCloseJob {
    fn assign(child: &Child) -> io::Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::HANDLE,
            System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            },
        };
        let job = Self(unsafe { CreateJobObjectW(None, None) }.map_err(io::Error::other)?);
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            )
        }
        .map_err(io::Error::other)?;
        unsafe { AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle() as _)) }
            .map_err(io::Error::other)?;
        Ok(job)
    }
}

#[cfg(windows)]
impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}

// The raw job handle is only closed on drop; moving it between threads is sound.
#[cfg(windows)]
unsafe impl Send for KillOnCloseJob {}
#[cfg(windows)]
unsafe impl Sync for KillOnCloseJob {}

#[derive(Debug)]
struct CleanupChild {
    child: Option<Box<dyn StdChildWrapper>>,
    // Dropped after `child`'s own Drop ran; closing it kills anything still in the tree.
    #[cfg(windows)]
    _kill_on_close: Option<KillOnCloseJob>,
}

impl CleanupChild {
    fn child(&self) -> &dyn StdChildWrapper {
        self.child.as_deref().expect("cleanup child is present")
    }

    fn child_mut(&mut self) -> &mut dyn StdChildWrapper {
        self.child.as_deref_mut().expect("cleanup child is present")
    }
}

impl StdChildWrapper for CleanupChild {
    fn inner(&self) -> &Child {
        self.child().inner()
    }

    fn inner_mut(&mut self) -> &mut Child {
        self.child_mut().inner_mut()
    }

    fn into_inner(mut self: Box<Self>) -> Child {
        self.child
            .take()
            .expect("cleanup child is present")
            .into_inner()
    }

    fn stdin(&mut self) -> &mut Option<ChildStdin> {
        self.child_mut().stdin()
    }

    fn stdout(&mut self) -> &mut Option<ChildStdout> {
        self.child_mut().stdout()
    }

    fn stderr(&mut self) -> &mut Option<ChildStderr> {
        self.child_mut().stderr()
    }

    fn id(&self) -> u32 {
        self.child().id()
    }

    fn start_kill(&mut self) -> io::Result<()> {
        self.child_mut().start_kill()
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child_mut().try_wait()
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        self.child_mut().wait()
    }

    #[cfg(unix)]
    fn signal(&self, signal: i32) -> io::Result<()> {
        self.child().signal(signal)
    }
}

impl Drop for CleanupChild {
    fn drop(&mut self) {
        let Some(child) = self.child.as_deref_mut() else {
            return;
        };
        if !matches!(child.try_wait(), Ok(Some(_))) {
            let _ = child.start_kill();
            let _ = child.wait();
        }
    }
}

#[derive(Debug)]
struct FailAfterSpawn;

impl StdCommandWrapper for FailAfterSpawn {
    fn wrap_child(
        &mut self,
        _child: Box<dyn StdChildWrapper>,
        _core: &StdCommandWrap,
    ) -> io::Result<Box<dyn StdChildWrapper>> {
        Err(io::Error::other("injected post-spawn wrapping failure"))
    }
}

fn spawn_owned(
    command: Command,
    windows_hidden: bool,
    fail_after_spawn: bool,
    captured_pid: Option<Arc<AtomicU32>>,
) -> io::Result<OwnedChild> {
    let mut command = StdCommandWrap::from(command);
    command.wrap(CleanupWrapper { captured_pid });

    #[cfg(windows)]
    {
        use process_wrap::std::{CreationFlags, JobObject};
        use windows::Win32::System::Threading::CREATE_NO_WINDOW;

        if windows_hidden {
            command.wrap(CreationFlags(CREATE_NO_WINDOW));
        }
        command.wrap(JobObject);
    }

    #[cfg(unix)]
    {
        use process_wrap::std::ProcessGroup;
        let _ = windows_hidden;
        command.wrap(ProcessGroup::leader());
    }

    if fail_after_spawn {
        command.wrap(FailAfterSpawn);
    }

    command
        .spawn()
        .map(|child| OwnedChild { child: Some(child) })
}

#[cfg(test)]
fn spawn_for_test(command: Command, captured_pid: Arc<AtomicU32>) -> io::Result<OwnedChild> {
    spawn_owned(command, true, true, Some(captured_pid))
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
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};

    let pid = i32::try_from(pid.get())
        .map(Pid::from_raw)
        .map_err(io::Error::other)?;
    match kill(pid, None) {
        Ok(()) | Err(Errno::EPERM) => Ok(true),
        Err(Errno::ESRCH) => Ok(false),
        Err(error) => Err(io::Error::from(error)),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn owned_commands_hide_their_console_window_by_default() {
        assert!(super::OwnedCommand::new("x").windows_hidden);
        assert!(super::OwnedCommand::from_command(std::process::Command::new("x")).windows_hidden);
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
