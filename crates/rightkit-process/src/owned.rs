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
        Self::from_command(Command::new(program))
    }

    pub fn from_command(command: Command) -> Self {
        Self {
            command,
            windows_hidden: false,
        }
    }

    pub fn command_mut(&mut self) -> &mut Command {
        &mut self.command
    }

    pub fn windows_hide(&mut self) -> &mut Self {
        self.windows_hidden = true;
        self
    }

    pub fn spawn(self) -> io::Result<OwnedChild> {
        spawn_owned(self.command, self.windows_hidden, false, None)
    }
}

#[derive(Debug)]
pub struct OwnedChild {
    child: Option<Box<dyn StdChildWrapper>>,
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
        Ok(Box::new(CleanupChild { child: Some(child) }))
    }
}

#[derive(Debug)]
struct CleanupChild {
    child: Option<Box<dyn StdChildWrapper>>,
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
    #[cfg(windows)]
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

        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 60",
        ]);
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
