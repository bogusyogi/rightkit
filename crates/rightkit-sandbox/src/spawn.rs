use crate::process_tree;
use crate::pty::{PtyError, PtySession, PtySize};
use crate::{
    classify_denial, DenialSignature, DenialSource, EnvPolicy, Reservation, ResourceAuthority,
    ResourceKind, SandboxCommand, SandboxError, SandboxPolicy,
};
use rightkit_process::OwnedCommand;
use std::io::Read;
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

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
    pub timed_out: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Set when the exit was non-zero and stderr carries a recognized sandbox denial.
    pub denial: Option<DenialSignature>,
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
    let job = crate::windows_job::WindowsJob::new(crate::windows_job::DEFAULT_ACTIVE_PROCESS_LIMIT)
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
    fn wait_timeout(&mut self, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
        match self {
            Self::Owned(c) => c.wait_timeout(timeout),
            #[cfg(windows)]
            Self::Confined(c) => c.wait_timeout(timeout),
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
        let _ = process_tree::kill_tree(self.id(), Duration::from_millis(200));
        match &mut self.inner {
            Inner::Owned(c) => c.terminate_tree(),
            #[cfg(windows)]
            Inner::Confined(c) => c.terminate_tree(),
        }
    }

    /// Wait up to `timeout`, collecting stdout/stderr. On timeout the whole tree is killed.
    pub fn wait_with_output(mut self, timeout: Duration) -> std::io::Result<SandboxOutput> {
        fn drain(r: Option<Box<dyn Read + Send>>) -> std::thread::JoinHandle<Vec<u8>> {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                if let Some(mut r) = r {
                    let _ = r.read_to_end(&mut buf);
                }
                buf
            })
        }
        let out = drain(self.stdout.take());
        let err = drain(self.stderr.take());
        let (status, timed_out) = match self.inner.wait_timeout(timeout)? {
            Some(s) => (Some(s), false),
            None => (Some(self.terminate_tree()?), true),
        };
        let stdout = out.join().unwrap_or_default();
        let stderr = err.join().unwrap_or_default();
        let denial = match status {
            Some(s) if !s.success() && !timed_out => {
                let sig = classify_denial(&String::from_utf8_lossy(&stderr), self.source);
                sig.is_recognized().then_some(sig)
            }
            _ => None,
        };
        Ok(SandboxOutput {
            status,
            timed_out,
            stdout,
            stderr,
            denial,
        })
    }
}
