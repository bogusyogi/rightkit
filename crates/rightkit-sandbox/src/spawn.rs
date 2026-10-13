use crate::process_tree;
#[cfg(feature = "pty")]
use crate::pty::{PtyError, PtySession, PtySize};
use crate::{
    classify_denial, DenialSignature, DenialSource, EnvPolicy, Reservation, ResourceAuthority,
    ResourceKind, SandboxCommand, SandboxError, SandboxPolicy,
};
use rightkit_process::OwnedCommand;
use std::io::Read;
#[cfg(windows)]
use std::os::windows::io::OwnedHandle;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Default capture cap per stream (stdout, stderr) for [`SandboxedChild::wait_with_output`].
pub const DEFAULT_OUTPUT_CAP: usize = 16 * 1024 * 1024;

/// After the deadline kills the tree, how long readers get to reach end-of-pipe.
const KILL_DRAIN_GRACE: Duration = Duration::from_millis(500);

/// Spawns children under a [`SandboxPolicy`], reserving resources first.
#[derive(Debug, Clone)]
pub struct Sandbox {
    authority: ResourceAuthority,
}

/// A supervised child: its resource reservation is held until drop, and drop
/// kills the whole process tree (via `rightkit-process` ownership).
pub struct SandboxedChild {
    inner: Inner,
    stdout: Option<Box<dyn Read + Send>>,
    stderr: Option<Box<dyn Read + Send>>,
    source: DenialSource,
    _reservation: Reservation,
    #[cfg(windows)]
    _job: Option<crate::windows_job::WindowsJob>,
}

/// Windows-only result of [`Sandbox::spawn_with_pipe_pair`].
///
/// `io` owns parent sides of two AppContainer-authorized named pipes. Child
/// stdout/stderr are NUL on this dedicated path.
#[cfg(windows)]
pub struct WindowsPipeSpawn {
    pub child: SandboxedChild,
    pub io: WindowsAuxiliaryIo,
}

/// Parent-owned named-pipe server handles for [`WindowsPipeSpawn`].
///
/// Both handles are non-inheritable, overlapped server handles, ready for
/// `tokio::net::windows::named_pipe::NamedPipeServer::from_raw_handle`.
#[cfg(windows)]
pub struct WindowsAuxiliaryIo {
    to_child: OwnedHandle,
    from_child: OwnedHandle,
}

#[cfg(windows)]
impl WindowsAuxiliaryIo {
    /// Transfer parent server handles ordered `(to_child, from_child)`.
    pub fn into_parent_handles(self) -> (OwnedHandle, OwnedHandle) {
        (self.to_child, self.from_child)
    }
}

/// Supervised children are std processes; confined Windows children are created directly
/// through `CreateProcessW` (the only way to enter an AppContainer).
enum Inner {
    Owned(rightkit_process::OwnedChild),
    #[cfg(windows)]
    Confined(crate::windows_appcontainer::ConfinedChild),
}

/// Collected result of [`SandboxedChild::wait_with_output`].
#[derive(Debug, Clone)]
pub struct SandboxOutput {
    pub status: Option<ExitStatus>,
    /// The deadline passed before the tree finished (the tree was killed), or
    /// pipes were still held open by a descendant at the deadline.
    pub timed_out: bool,
    /// Captured stdout, at most the per-stream cap.
    pub stdout: Vec<u8>,
    /// Captured stderr, at most the per-stream cap.
    pub stderr: Vec<u8>,
    /// Some output was discarded past the cap, or a stream did not reach
    /// end-of-pipe before the deadline (so its capture may be incomplete).
    pub truncated: bool,
    /// Set when the exit was non-zero and stderr carries a recognized sandbox denial.
    pub denial: Option<DenialSignature>,
}

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

/// One stream's capture: bytes kept up to the cap, and whether bytes were dropped.
struct Captured {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Read a pipe to end-of-file on its own thread, keeping at most `cap` bytes.
/// Bytes past the cap are still read and discarded so the child never blocks on
/// a full pipe. Always sends exactly one result (empty when there is no pipe).
fn spawn_reader(
    r: Option<Box<dyn Read + Send>>,
    which: Stream,
    cap: usize,
    tx: mpsc::Sender<(Stream, Captured)>,
) {
    std::thread::spawn(move || {
        let mut cap_out = Captured {
            bytes: Vec::new(),
            truncated: false,
        };
        if let Some(mut r) = r {
            let mut chunk = [0u8; 8192];
            loop {
                match r.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        let room = cap.saturating_sub(cap_out.bytes.len());
                        cap_out.bytes.extend_from_slice(&chunk[..n.min(room)]);
                        if n > room {
                            cap_out.truncated = true;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }
        let _ = tx.send((which, cap_out));
    });
}

fn apply_env(cmd: &mut Command, env: &EnvPolicy, extra: &[(String, String)]) {
    if !env.inherit {
        cmd.env_clear();
        for k in &env.keep {
            if let Some(v) = std::env::var_os(k) {
                cmd.env(k, v);
            }
        }
    }
    for (k, v) in env.set.iter() {
        cmd.env(k, v);
    }
    for (k, v) in extra {
        cmd.env(k, v);
    }
}

fn validate_command(c: &SandboxCommand) -> Result<(), SandboxError> {
    if c.program.is_empty() {
        return Err(SandboxError::InvalidPolicy("empty program".into()));
    }
    if let Some(cwd) = &c.cwd {
        if !cwd.is_absolute() || !cwd.is_dir() {
            return Err(SandboxError::InvalidPolicy(format!(
                "cwd must be an existing absolute directory: {}",
                cwd.display()
            )));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn validate_handle_flag(command: &SandboxCommand, handle_flag: &str) -> Result<(), SandboxError> {
    let valid = handle_flag.starts_with("--")
        && handle_flag.len() > 2
        && handle_flag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if !valid {
        return Err(SandboxError::InvalidPolicy(
            "pipe handle flag must begin with -- and contain only ASCII letters, digits, - or _"
                .into(),
        ));
    }
    let assigned = format!("{handle_flag}=");
    if command.args.iter().any(|arg| {
        let arg = arg.to_string_lossy();
        arg == handle_flag || arg.starts_with(&assigned)
    }) {
        return Err(SandboxError::InvalidPolicy(format!(
            "command already supplies pipe handle flag {handle_flag}"
        )));
    }
    Ok(())
}

impl Sandbox {
    pub fn new(authority: ResourceAuthority) -> Self {
        Self { authority }
    }

    pub fn authority(&self) -> &ResourceAuthority {
        &self.authority
    }

    /// Reserve, confine, spawn. Reservation happens before any process exists;
    /// a denial (e.g. `Strict`) returns `SandboxError::ResourceDenied`.
    pub fn spawn(
        &self,
        policy: &SandboxPolicy,
        command: SandboxCommand,
    ) -> Result<SandboxedChild, SandboxError> {
        policy.validate()?;
        validate_command(&command)?;
        let reservation = self.authority.reserve(ResourceKind::ChildProcess)?;
        #[cfg(windows)]
        {
            let spawned = crate::windows_appcontainer::spawn(policy, &command)?;
            Ok(SandboxedChild {
                inner: Inner::Confined(spawned.child),
                stdout: Some(Box::new(spawned.stdout)),
                stderr: Some(Box::new(spawned.stderr)),
                source: DenialSource::WindowsAppContainer,
                _reservation: reservation,
                _job: None,
            })
        }
        #[cfg(not(windows))]
        {
            // windowless: `finish` spawns it through OwnedCommand (CREATE_NO_WINDOW on Windows).
            let mut cmd = Command::new(&command.program);
            cmd.args(&command.args);
            if let Some(cwd) = &command.cwd {
                cmd.current_dir(cwd);
            }
            apply_env(&mut cmd, &policy.env, &command.env);
            let source = confine(&mut cmd, policy)?;
            finish(cmd, reservation, source)
        }
    }

    /// Spawn a network-denied AppContainer child with two inherited named-pipe
    /// client endpoints. `handle_flag` is appended as
    /// `<handle_flag>=<child-read>,<child-write>`.
    #[cfg(windows)]
    pub fn spawn_with_pipe_pair(
        &self,
        policy: &SandboxPolicy,
        command: SandboxCommand,
        handle_flag: &str,
    ) -> Result<WindowsPipeSpawn, SandboxError> {
        self.spawn_with_pipe_pair_stdio_file(policy, command, handle_flag, None)
    }

    /// As [`Self::spawn_with_pipe_pair`], but child stdout and stderr append to `stdio_file`
    /// (opened by the parent, inherited by handle) instead of NUL. Diagnostics only.
    #[cfg(windows)]
    pub fn spawn_with_pipe_pair_stdio_file(
        &self,
        policy: &SandboxPolicy,
        command: SandboxCommand,
        handle_flag: &str,
        stdio_file: Option<&std::path::Path>,
    ) -> Result<WindowsPipeSpawn, SandboxError> {
        policy.validate()?;
        validate_command(&command)?;
        validate_handle_flag(&command, handle_flag)?;
        if policy.network != crate::NetworkPolicy::Deny {
            return Err(SandboxError::InvalidPolicy(
                "pipe-pair sandbox requires NetworkPolicy::Deny".into(),
            ));
        }
        let reservation = self.authority.reserve(ResourceKind::ChildProcess)?;
        let spawned = crate::windows_appcontainer::spawn_with_pipe_pair(
            policy,
            &command,
            handle_flag,
            stdio_file,
        )?;
        Ok(WindowsPipeSpawn {
            child: SandboxedChild {
                inner: Inner::Confined(spawned.child),
                stdout: spawned
                    .stdout
                    .map(|stream| Box::new(stream) as Box<dyn Read + Send>),
                stderr: spawned
                    .stderr
                    .map(|stream| Box::new(stream) as Box<dyn Read + Send>),
                source: DenialSource::WindowsAppContainer,
                _reservation: reservation,
                _job: None,
            },
            io: WindowsAuxiliaryIo {
                to_child: spawned.to_child,
                from_child: spawned.from_child,
            },
        })
    }

    /// Reserve and spawn without filesystem/network confinement but with whole-tree
    /// ownership (process group on Unix, Job Object on Windows).
    pub fn spawn_supervised(
        &self,
        command: SandboxCommand,
        env: &EnvPolicy,
    ) -> Result<SandboxedChild, SandboxError> {
        validate_command(&command)?;
        let reservation = self.authority.reserve(ResourceKind::ChildProcess)?;
        // windowless: `finish` spawns it through OwnedCommand (CREATE_NO_WINDOW on Windows).
        let mut cmd = Command::new(&command.program);
        cmd.args(&command.args);
        if let Some(cwd) = &command.cwd {
            cmd.current_dir(cwd);
        }
        apply_env(&mut cmd, env, &command.env);
        finish(cmd, reservation, DenialSource::Unknown)
    }

    #[cfg(feature = "pty")]
    /// Spawn under a PTY with lease-epoch input fencing. With `policy`, the child is
    /// confined (macOS only today; other platforms fail closed). Reserves a
    /// `BackgroundProcess` slot for the session's lifetime.
    pub fn spawn_pty(
        &self,
        policy: Option<&SandboxPolicy>,
        command: SandboxCommand,
        size: PtySize,
    ) -> Result<PtySession, PtyError> {
        validate_command(&command).map_err(PtyError::Sandbox)?;
        let reservation = self
            .authority
            .reserve(ResourceKind::BackgroundProcess)
            .map_err(|d| PtyError::Sandbox(d.into()))?;
        let (program, args) = match policy {
            None => (command.program.clone(), command.args.clone()),
            Some(p) => {
                p.validate().map_err(PtyError::Sandbox)?;
                #[cfg(target_os = "macos")]
                {
                    crate::seatbelt::wrap_argv(p, &command.program, &command.args)
                }
                #[cfg(not(target_os = "macos"))]
                {
                    return Err(PtyError::Sandbox(SandboxError::Unsupported(
                        "sandboxed PTY is implemented for macOS only".into(),
                    )));
                }
            }
        };
        let env = policy.map(|p| p.env.clone()).unwrap_or_else(|| EnvPolicy {
            inherit: true,
            ..EnvPolicy::default()
        });
        PtySession::spawn(
            program,
            args,
            command.cwd.clone(),
            &env,
            &command.env,
            size,
            reservation,
        )
    }
}

#[cfg(target_os = "macos")]
fn confine(cmd: &mut Command, policy: &SandboxPolicy) -> Result<DenialSource, SandboxError> {
    use std::os::unix::process::CommandExt;
    let prepared = crate::seatbelt::Prepared::new(policy)?;
    // SAFETY: runs in the forked child before exec; the profile was prepared in the parent.
    unsafe {
        cmd.pre_exec(move || prepared.apply());
    }
    Ok(DenialSource::MacosSeatbelt)
}

#[cfg(target_os = "linux")]
fn confine(cmd: &mut Command, policy: &SandboxPolicy) -> Result<DenialSource, SandboxError> {
    use std::os::unix::process::CommandExt;
    let prepared = crate::linux::Prepared::new(policy)?;
    // SAFETY: Prepared::apply performs only syscalls.
    unsafe {
        cmd.pre_exec(move || prepared.apply());
    }
    Ok(DenialSource::LinuxLandlockSeccomp)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn confine(_cmd: &mut Command, _policy: &SandboxPolicy) -> Result<DenialSource, SandboxError> {
    Err(SandboxError::Unsupported(
        "no confinement backend on this platform; use spawn_supervised".into(),
    ))
}

fn finish(
    mut cmd: Command,
    reservation: Reservation,
    source: DenialSource,
) -> Result<SandboxedChild, SandboxError> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut inner = OwnedCommand::from_command(cmd);
    inner.windows_hide();
    let mut child = inner.spawn()?;
    #[cfg(windows)]
    let job =
        crate::windows_job::WindowsJob::new(Some(crate::windows_job::DEFAULT_ACTIVE_PROCESS_LIMIT))
            .and_then(|j| j.assign_process_id(child.id()).map(|_| j))
            .ok();
    Ok(SandboxedChild {
        stdout: child
            .take_stdout()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
        stderr: child
            .take_stderr()
            .map(|r| Box::new(r) as Box<dyn Read + Send>),
        inner: Inner::Owned(child),
        source,
        _reservation: reservation,
        #[cfg(windows)]
        _job: job,
    })
}

impl Inner {
    /// Waits up to `timeout` for the child to exit and says whether it did.
    ///
    /// Unix: the exit is only observed, never reaped. An unreaped leader keeps pinning its
    /// process-group id, so the group can still be killed while descendants hold the pipes; a
    /// reaped one cannot be signalled any more (the id may be recycled). The caller reaps
    /// with `wait` once the pipes are drained. The status comes back only where the host
    /// reaps as part of the wait (the Windows confined child).
    fn wait_exited(&mut self, timeout: Duration) -> std::io::Result<(bool, Option<ExitStatus>)> {
        match self {
            Self::Owned(c) => Ok((c.process_handle()?.wait_exited_timeout(timeout)?, None)),
            #[cfg(windows)]
            Self::Confined(c) => {
                let status = c.wait_timeout(timeout)?;
                Ok((status.is_some(), status))
            }
        }
    }

    fn has_exited(&self) -> std::io::Result<bool> {
        match self {
            Self::Owned(child) => child.process_handle()?.wait_exited_timeout(Duration::ZERO),
            #[cfg(windows)]
            Self::Confined(child) => child.has_exited(),
        }
    }

    fn terminate_tree_bounded(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<rightkit_process::ReapOutcome> {
        match self {
            Self::Owned(child) => child.terminate_tree_bounded(timeout),
            #[cfg(windows)]
            Self::Confined(child) => child.terminate_tree_bounded(timeout),
        }
    }
}

impl SandboxedChild {
    pub fn id(&self) -> u32 {
        match &self.inner {
            Inner::Owned(c) => c.id(),
            #[cfg(windows)]
            Inner::Confined(c) => c.id(),
        }
    }

    /// Take stdout for bounded caller-managed draining.
    pub fn take_stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        self.stdout.take()
    }

    /// Take stderr for bounded caller-managed draining.
    pub fn take_stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        self.stderr.take()
    }

    /// Observe leader exit without consuming child ownership or output streams.
    pub fn has_exited(&self) -> std::io::Result<bool> {
        self.inner.has_exited()
    }

    /// Terminate owned process tree, waiting at most `timeout` for leader exit.
    /// A timed-out result retains this child, its reservation, & its leases.
    pub fn terminate_tree_bounded(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<rightkit_process::ReapOutcome> {
        self.inner.terminate_tree_bounded(timeout)
    }

    /// Observe the exit code of a confined Windows child without reaping it (raw `u32`, so an
    /// NTSTATUS such as `0xC0000005` stays readable). `Ok(None)` while it still runs.
    #[cfg(windows)]
    pub fn observed_exit_code(&self) -> std::io::Result<Option<u32>> {
        match &self.inner {
            Inner::Confined(c) => c.observed_exit_code(),
            Inner::Owned(_) => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "exit code observation is only for the confined Windows child",
            )),
        }
    }

    pub fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        match &mut self.inner {
            Inner::Owned(c) => c.try_wait(),
            #[cfg(windows)]
            Inner::Confined(c) => c.try_wait(),
        }
    }

    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        match &mut self.inner {
            Inner::Owned(c) => c.wait(),
            #[cfg(windows)]
            Inner::Confined(c) => c.wait(),
        }
    }

    /// Kill the child and every descendant (including ones that left the process group).
    pub fn terminate_tree(&mut self) -> std::io::Result<ExitStatus> {
        // The descendant sweep names the leader's pid, so it runs only while the leader is
        // proven to be this child and unreaped; a reaped leader's number may be someone else's.
        match &self.inner {
            Inner::Owned(c) => {
                let _ = c
                    .while_unreaped(|pid| process_tree::kill_tree(pid, Duration::from_millis(200)));
            }
            #[cfg(windows)]
            Inner::Confined(c) => {
                let _ = process_tree::kill_tree(c.id(), Duration::from_millis(200));
            }
        }
        match &mut self.inner {
            Inner::Owned(c) => c.terminate_tree(),
            #[cfg(windows)]
            Inner::Confined(c) => c.terminate_tree(),
        }
    }

    /// Wait up to `timeout` for the child, collecting stdout/stderr with each
    /// stream capped at [`DEFAULT_OUTPUT_CAP`].
    ///
    /// The deadline covers the whole process tree and its pipes: if the child
    /// outlives `timeout`, or a descendant still holds stdout/stderr open at
    /// `timeout`, the tree is killed (process group on Unix, Job Object on
    /// Windows) and readers get a bounded grace period to reach end-of-pipe.
    pub fn wait_with_output(self, timeout: Duration) -> std::io::Result<SandboxOutput> {
        self.wait_with_output_capped(timeout, DEFAULT_OUTPUT_CAP)
    }

    /// [`SandboxedChild::wait_with_output`] with an explicit per-stream cap.
    pub fn wait_with_output_capped(
        mut self,
        timeout: Duration,
        cap_per_stream: usize,
    ) -> std::io::Result<SandboxOutput> {
        let deadline = Instant::now() + timeout;
        let (tx, rx) = mpsc::channel();
        spawn_reader(
            self.stdout.take(),
            Stream::Stdout,
            cap_per_stream,
            tx.clone(),
        );
        spawn_reader(
            self.stderr.take(),
            Stream::Stderr,
            cap_per_stream,
            tx.clone(),
        );
        drop(tx);

        // The leader is observed, not reaped: if it exited while a descendant still holds a
        // pipe, the group must stay killable (an unreaped leader pins its group id) until the
        // pipes are drained or the deadline passes. It is reaped below, after the drain.
        let (exited, mut status) = self.inner.wait_exited(timeout)?;
        let mut timed_out = !exited;
        if !exited {
            status = Some(self.terminate_tree()?);
        }

        let mut out: Option<Captured> = None;
        let mut err: Option<Captured> = None;
        let mut collect = |msg: (Stream, Captured)| match msg.0 {
            Stream::Stdout => out = Some(msg.1),
            Stream::Stderr => err = Some(msg.1),
        };
        let mut pending = 2;
        while pending > 0 {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(msg) => {
                    collect(msg);
                    pending -= 1;
                }
                Err(_) => break,
            }
        }
        if pending > 0 {
            // A descendant holds a pipe past the deadline: kill what is left of
            // the tree, then give the readers a short bounded grace to finish.
            timed_out = true;
            // Kills the group while the leader is still unreaped, then reaps it.
            if let Ok(killed) = self.terminate_tree() {
                status = status.or(Some(killed));
            }
            let grace_end = Instant::now() + KILL_DRAIN_GRACE;
            while pending > 0 {
                match rx.recv_timeout(grace_end.saturating_duration_since(Instant::now())) {
                    Ok(msg) => {
                        collect(msg);
                        pending -= 1;
                    }
                    Err(_) => break,
                }
            }
        }

        // Close out the tree before the final reap, never a bare `wait`. Closed pipes do not
        // mean the tree is gone: a descendant can have redirected its output and kept running.
        // Reaping first would end the group's killability for good (Unix) and leave it behind
        // when the owner drops. So while the leader is still unreaped it pins the group id and
        // the group is killed, then reaped; a confined Windows child is already reaped, so its
        // Job Object is terminated by handle.
        let status = match status {
            Some(status) => {
                #[cfg(windows)]
                {
                    if let Inner::Confined(c) = &mut self.inner {
                        let _ = c.terminate_tree();
                    }
                }
                Some(status)
            }
            None => Some(self.terminate_tree()?),
        };

        let finish = |c: Option<Captured>| {
            c.unwrap_or(Captured {
                bytes: Vec::new(),
                truncated: true,
            })
        };
        let (out, err) = (finish(out), finish(err));
        let truncated = out.truncated || err.truncated;
        let denial = match status {
            Some(s) if !s.success() && !timed_out => {
                let sig = classify_denial(&String::from_utf8_lossy(&err.bytes), self.source);
                sig.is_recognized().then_some(sig)
            }
            _ => None,
        };
        Ok(SandboxOutput {
            status,
            timed_out,
            stdout: out.bytes,
            stderr: err.bytes,
            truncated,
            denial,
        })
    }
}
