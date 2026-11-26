//! Chrome as an owned child. Spawned through `rightkit-child-process` so it is a
//! process-group leader on Unix and a Windows job object member
//! (KILL_ON_JOB_CLOSE, no breakaway), bound first to the caller's own job when one
//! is supplied; a parent-death watchdog covers a host that dies without running
//! destructors on Unix.
//!
//! Chrome opens no debugging port. Every launch passes `--remote-debugging-pipe` and a
//! descriptor pair (Unix: Chrome reads fd 3 and writes fd 4) or an inherited handle pair
//! (Windows: `--remote-debugging-io-pipes`). The parent's ends go to the `PipeBridge`.
use crate::error::{BrowserError, Result};
use crate::pipe_transport::{ParentPipes, PipeBridge};
use rightkit_child_process::{OwnedChild, OwnedCommand};
#[cfg(windows)]
use rightkit_sandbox::{
    EnvPolicy, NetworkPolicy as SandboxNetworkPolicy, ResourceAuthority, ResourceProfile, Sandbox,
    SandboxCommand, SandboxPolicy, SandboxedChild,
};
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
#[cfg(windows)]
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};

#[cfg(windows)]
const FIXTURE_LOG_TAIL_BYTES: u64 = 16 * 1024;

pub(crate) struct ChromeProc {
    child: ChromeChild,
    /// Unix only: kills Chrome's process group when this host process vanishes.
    pub _watchdog: Option<OwnedChild>,
    /// The last lines Chrome wrote to stderr, for the detail of a failed navigation.
    pub stderr_tail: StderrTail,
    /// The parent's ends of Chrome's DevTools pipe pair, until the bridge adopts them.
    pipe_parent: Option<ParentPipes>,
    /// Declared after `child`, so Chrome is gone before the relay's ends close.
    pipe_bridge: Option<PipeBridge>,
    /// Path of the stdio file the sandbox opens (and validates) for Chrome. No parent handle is
    /// held across the spawn: the sandbox opens it share-read-only, so a lingering write handle
    /// here would be a sharing violation. Diagnostics read it by path after the fact.
    #[cfg(windows)]
    fixture_log: Option<std::path::PathBuf>,
    /// Exact AppContainer launch parameters (grants, capabilities, Chrome flags), recorded at
    /// spawn so a failed launch can say what was actually requested.
    #[cfg(windows)]
    launch_params: String,
    /// Profile dir and Chrome image dir, for the failure-time ACL dump.
    #[cfg(windows)]
    acl_targets: Vec<std::path::PathBuf>,
}

/// Process ownership stays explicit: fixture Windows children live in their
/// AppContainer Job, while ordinary launches keep their existing OwnedChild.
enum ChromeChild {
    Owned(OwnedChild),
    #[cfg(windows)]
    Confined(SandboxedChild),
}

impl ChromeChild {
    fn id(&self) -> u32 {
        match self {
            Self::Owned(child) => child.id(),
            #[cfg(windows)]
            Self::Confined(child) => child.id(),
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match self {
            Self::Owned(child) => child.try_wait(),
            #[cfg(windows)]
            Self::Confined(child) => child.try_wait(),
        }
    }

    /// Observe only: fixture cleanup must retain ownership until the tree has
    /// been killed & the Windows Job reports no active descendants.
    fn has_exited(&self) -> std::io::Result<bool> {
        match self {
            Self::Owned(child) => child.process_handle()?.wait_exited_timeout(Duration::ZERO),
            #[cfg(windows)]
            Self::Confined(child) => child.has_exited(),
        }
    }

    #[cfg(windows)]
    fn observed_exit_code(&self) -> std::io::Result<Option<u32>> {
        match self {
            Self::Owned(_) => Ok(None),
            Self::Confined(child) => child.observed_exit_code(),
        }
    }

    fn terminate_tree_bounded(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<rightkit_child_process::ReapOutcome> {
        match self {
            Self::Owned(child) => child.terminate_tree_bounded(timeout),
            #[cfg(windows)]
            Self::Confined(child) => child.terminate_tree_bounded(timeout),
        }
    }

    #[cfg(unix)]
    fn wait_or_kill(&mut self, grace: Duration) -> std::io::Result<()> {
        match self {
            Self::Owned(child) => child.wait_or_kill(grace).map(|_| ()),
            #[cfg(windows)]
            Self::Confined(child) => match child.terminate_tree_bounded(grace)? {
                rightkit_child_process::ReapOutcome::Exited(_) => Ok(()),
                rightkit_child_process::ReapOutcome::TimedOut => Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "confined browser process tree did not exit",
                )),
            },
        }
    }
}

/// The most recent stderr lines of the Chrome process, bounded in count and length.
#[derive(Clone, Default)]
pub(crate) struct StderrTail(std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>);

impl StderrTail {
    const LINES: usize = 30;
    const LINE_CHARS: usize = 300;

    fn push(&self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let mut lines = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if lines.len() == Self::LINES {
            lines.pop_front();
        }
        lines.push_back(line.chars().take(Self::LINE_CHARS).collect());
    }

    /// The retained lines joined with ` | `, or `(nothing)`.
    pub(crate) fn text(&self) -> String {
        let lines = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if lines.is_empty() {
            "(nothing)".into()
        } else {
            lines.iter().cloned().collect::<Vec<_>>().join(" | ")
        }
    }
}

impl ChromeProc {
    /// Unix: SIGTERM Chrome's whole process group (Chrome's own clean-shutdown path),
    /// wait up to `grace`, then SIGKILL the group. Windows: terminate the job's tree.
    /// Then stops the watchdog.
    pub(crate) fn terminate_gracefully(&mut self, grace: Duration) -> std::io::Result<()> {
        // Stop relaying before the kill, so the end of Chrome's pipe is not a failure.
        self.begin_transport_shutdown();
        #[cfg(unix)]
        {
            if !matches!(self.child.try_wait(), Ok(Some(_))) {
                self.child.wait_or_kill(grace)?;
            }
        }
        // Windows: always terminate the retained job, even when the browser process has
        // already exited: its renderer/GPU/utility helpers outlive it and keep the profile
        // directory open. Bounded so Drop can never hang.
        #[cfg(not(unix))]
        {
            let _ = grace;
            match self.child.terminate_tree_bounded(Duration::from_secs(2))? {
                rightkit_child_process::ReapOutcome::Exited(_) => {}
                rightkit_child_process::ReapOutcome::TimedOut => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "browser process tree did not exit after termination",
                    ));
                }
            }
        }
        if let Some(w) = self._watchdog.as_mut() {
            w.wait_or_kill(Duration::from_secs(1))?;
        }
        Ok(())
    }

    /// Fixture cleanup keeps the process-group leader unreaped until its whole tree is dead.
    pub(crate) fn terminate_fixture_tree(&mut self) -> std::io::Result<()> {
        match self.child.terminate_tree_bounded(Duration::from_secs(5))? {
            rightkit_child_process::ReapOutcome::Exited(_) => {}
            rightkit_child_process::ReapOutcome::TimedOut => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "fixture browser process tree did not stop",
                ));
            }
        }
        if let Some(watchdog) = self._watchdog.as_mut() {
            watchdog.wait_or_kill(Duration::from_secs(1))?;
        }
        Ok(())
    }

    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }

    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }

    pub(crate) fn has_exited(&self) -> std::io::Result<bool> {
        self.child.has_exited()
    }

    /// Snapshot bounded diagnostics from parent-owned state after a fixture CDP failure.
    /// This is observe-only: process ownership & transport state stay unchanged.
    #[cfg(windows)]
    pub(crate) fn fixture_diagnostics(&self) -> String {
        let live_exit = match self.has_exited() {
            Ok(exited) => exited.to_string(),
            Err(error) => format!("unavailable ({error})"),
        };
        let exit_code = match self.child.observed_exit_code() {
            Ok(Some(code)) => describe_exit_code(code),
            Ok(None) => "running".into(),
            Err(error) => format!("unavailable ({error})"),
        };
        let acls = self
            .acl_targets
            .iter()
            .filter(|path| !path.as_os_str().is_empty())
            .map(|path| format!("{} => {}", path.display(), icacls_summary(path)))
            .collect::<Vec<_>>()
            .join(" | ");
        let log_tail = self
            .fixture_log
            .as_ref()
            .and_then(|path| bounded_file_tail(path).ok())
            .unwrap_or_else(|| "(chrome stderr file unavailable)".into());
        format!(
            "windows fixture diagnostic chrome_exited={live_exit}; chrome_exit_code={exit_code}; \
             {launch_params}; acls=[{acls}]; chrome_stderr_tail (stdout+stderr inherited file \
             chrome-stderr.txt)={log_tail}",
            launch_params = self.launch_params,
        )
    }

    /// Exit status and diagnostics of a Chrome that did not answer on its DevTools pipe.
    pub(crate) fn launch_detail(&mut self) -> String {
        let exit = match self.child.try_wait() {
            Ok(Some(status)) => format!("exited with {status}"),
            Ok(None) => "still running".to_string(),
            Err(error) => format!("status unavailable: {error}"),
        };
        #[cfg(windows)]
        if self.fixture_log.is_some() {
            return format!("process {exit}; {}", self.fixture_diagnostics());
        }
        format!("process {exit}; stderr: {}", self.stderr_tail.text())
    }

    /// Hand the parent's pipe ends to one loopback relay and return the secret WebSocket URL
    /// chromiumoxide connects to. Runs inside the Tokio runtime. `handshake_deadline` bounds
    /// waiting for the one client; `relay_deadline` bounds the whole relay (fixture lease only).
    pub(crate) fn start_pipe_bridge(
        &mut self,
        handshake_deadline: std::time::Instant,
        relay_deadline: Option<std::time::Instant>,
    ) -> Result<String> {
        let (to_child, from_child) = self
            .pipe_parent
            .take()
            .ok_or_else(|| BrowserError::Launch("CDP pipe ownership is absent".into()))?;
        let bridge = PipeBridge::start(to_child, from_child, handshake_deadline, relay_deadline)?;
        let url = bridge.websocket_url().to_owned();
        self.pipe_bridge = Some(bridge);
        Ok(url)
    }

    /// Stop the relay admitting I/O; the process kill that follows is then not a transport fault.
    pub(crate) fn begin_transport_shutdown(&self) {
        if let Some(bridge) = &self.pipe_bridge {
            bridge.begin_shutdown();
        }
    }

    /// Prove the relay inert once the browser process tree is gone.
    pub(crate) async fn finish_transport_shutdown(
        &mut self,
        deadline: std::time::Instant,
    ) -> Result<()> {
        let bridge = self
            .pipe_bridge
            .as_mut()
            .ok_or_else(|| BrowserError::Launch("CDP transport ownership is absent".into()))?;
        bridge.shutdown(deadline).await
    }
}

/// What the child's side of the DevTools pipe pair needs for the spawn only: the parent's copies
/// of Chrome's ends (closed as soon as the spawn returns, so Chrome's exit is seen as end of
/// file) and, on Unix, the placeholders that keep the low descriptor numbers occupied.
struct ChildEnds {
    #[cfg(unix)]
    _held: Vec<std::os::fd::OwnedFd>,
    #[cfg(windows)]
    _held: Vec<std::os::windows::io::OwnedHandle>,
}

/// Both halves of one Chrome DevTools pipe pair.
struct DevtoolsPipe {
    parent: ParentPipes,
    child: ChildEnds,
    /// Windows: the decimal inheritable handle values `(read, write)` Chrome is told about.
    #[cfg(windows)]
    child_values: (u32, u32),
    /// Unix: the parent's numbers for Chrome's ends, dup2'ed onto Chrome's fd 3 and fd 4.
    #[cfg(unix)]
    child_fds: (std::os::fd::RawFd, std::os::fd::RawFd),
}

/// Chrome's hard-coded DevTools pipe descriptors on Unix: it reads commands on 3, writes on 4.
#[cfg(unix)]
const CHROME_READ_FD: std::os::fd::RawFd = 3;
#[cfg(unix)]
const CHROME_WRITE_FD: std::os::fd::RawFd = 4;
/// The parent keeps all four ends at or above this number, clear of fd 3 and fd 4.
#[cfg(unix)]
const FIRST_PARENT_FD: std::os::fd::RawFd = 5;

#[cfg(unix)]
fn devtools_pipe() -> Result<DevtoolsPipe> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    fn launch(error: std::io::Error) -> BrowserError {
        BrowserError::Launch(format!("DevTools pipe: {error}"))
    }
    // Moves `fd` to a number >= FIRST_PARENT_FD (the copy is close-on-exec; the original closes).
    fn lift(fd: OwnedFd) -> Result<OwnedFd> {
        // SAFETY: F_DUPFD_CLOEXEC on a descriptor this function owns; no memory is passed.
        let lifted = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, FIRST_PARENT_FD) };
        if lifted < 0 {
            return Err(launch(std::io::Error::last_os_error()));
        }
        // SAFETY: `lifted` is a fresh descriptor nothing else owns.
        Ok(unsafe { OwnedFd::from_raw_fd(lifted) })
    }

    // std's spawn creates its own close-on-exec exec-error pipe from the lowest free numbers.
    // If that landed on 3 or 4, the dup2 in the child would replace it and a failed exec would
    // go unreported. Occupying every free number below FIRST_PARENT_FD keeps it out of there.
    let mut held = Vec::new();
    for _ in 0..=FIRST_PARENT_FD {
        let placeholder = OwnedFd::from(std::fs::File::open("/dev/null").map_err(launch)?);
        let above = placeholder.as_raw_fd() >= FIRST_PARENT_FD;
        held.push(placeholder);
        if above {
            break;
        }
    }
    // std::io::pipe ends are close-on-exec. Pipe A carries commands (the parent writes, Chrome
    // reads); pipe B carries replies and events (Chrome writes, the parent reads).
    let (command_read, command_write) = std::io::pipe().map_err(launch)?;
    let (reply_read, reply_write) = std::io::pipe().map_err(launch)?;
    let child_read = lift(OwnedFd::from(command_read))?;
    let to_child = lift(OwnedFd::from(command_write))?;
    let from_child = lift(OwnedFd::from(reply_read))?;
    let child_write = lift(OwnedFd::from(reply_write))?;
    let child_fds = (child_read.as_raw_fd(), child_write.as_raw_fd());
    held.push(child_read);
    held.push(child_write);
    Ok(DevtoolsPipe {
        parent: (to_child, from_child),
        child: ChildEnds { _held: held },
        child_fds,
    })
}

#[cfg(windows)]
mod win32 {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreateNamedPipeW(
            name: *const u16,
            open_mode: u32,
            pipe_mode: u32,
            max_instances: u32,
            out_buffer_size: u32,
            in_buffer_size: u32,
            default_timeout: u32,
            attributes: *const c_void,
        ) -> *mut c_void;
        pub fn CreateFileW(
            name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security: *const c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template: *mut c_void,
        ) -> *mut c_void;
    }

    pub const PIPE_ACCESS_INBOUND: u32 = 0x0000_0001;
    pub const PIPE_ACCESS_OUTBOUND: u32 = 0x0000_0002;
    pub const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
    pub const FILE_FLAG_FIRST_PIPE_INSTANCE: u32 = 0x0008_0000;
    pub const PIPE_REJECT_REMOTE_CLIENTS: u32 = 0x0000_0008;
    pub const GENERIC_READ: u32 = 0x8000_0000;
    pub const GENERIC_WRITE: u32 = 0x4000_0000;
    pub const OPEN_EXISTING: u32 = 3;
    pub const INVALID_HANDLE_VALUE: isize = -1;
}

/// One unnamed-by-anyone pipe: the parent keeps an overlapped, non-inheritable server end (ready
/// for Tokio); Chrome gets the synchronous client end. `parent_reads` is true when Chrome writes.
/// The name carries 128 random bits, the server allows one instance and refuses remote clients,
/// and the client is opened before this function returns, so nothing else can connect.
#[cfg(windows)]
fn windows_pipe(
    parent_reads: bool,
) -> Result<(
    std::os::windows::io::OwnedHandle,
    std::os::windows::io::OwnedHandle,
)> {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let mut random = [0u8; 16];
    getrandom::fill(&mut random)
        .map_err(|error| BrowserError::Launch(format!("DevTools pipe random source: {error}")))?;
    let name: Vec<u16> = format!(
        r"\\.\pipe\rightkit-browser-cdp-{}-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
        crate::pipe_transport::hex(&random)
    )
    .encode_utf16()
    .chain(std::iter::once(0))
    .collect();
    let (server_access, client_access) = if parent_reads {
        (win32::PIPE_ACCESS_INBOUND, win32::GENERIC_WRITE)
    } else {
        (win32::PIPE_ACCESS_OUTBOUND, win32::GENERIC_READ)
    };
    // SAFETY: `name` is NUL-terminated and outlives the call; no security attributes are passed,
    // so the handle is non-inheritable.
    let server = unsafe {
        win32::CreateNamedPipeW(
            name.as_ptr(),
            server_access | win32::FILE_FLAG_OVERLAPPED | win32::FILE_FLAG_FIRST_PIPE_INSTANCE,
            win32::PIPE_REJECT_REMOTE_CLIENTS,
            1,
            64 * 1024,
            64 * 1024,
            0,
            std::ptr::null(),
        )
    };
    if server.is_null() || server as isize == win32::INVALID_HANDLE_VALUE {
        return Err(BrowserError::Launch(format!(
            "DevTools pipe CreateNamedPipeW: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: `server` is a fresh handle nothing else owns; it closes on every path below.
    let server = unsafe { OwnedHandle::from_raw_handle(server) };
    // SAFETY: opens the unique client end of the instance just created; `name` is valid.
    let client = unsafe {
        win32::CreateFileW(
            name.as_ptr(),
            client_access,
            0,
            std::ptr::null(),
            win32::OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if client.is_null() || client as isize == win32::INVALID_HANDLE_VALUE {
        return Err(BrowserError::Launch(format!(
            "DevTools pipe client open: {}",
            std::io::Error::last_os_error()
        )));
    }
    // SAFETY: `client` is a fresh handle nothing else owns.
    Ok((server, unsafe { OwnedHandle::from_raw_handle(client) }))
}

#[cfg(windows)]
fn devtools_pipe() -> Result<DevtoolsPipe> {
    use std::os::windows::io::AsRawHandle;

    // Chrome reads from the first pipe and writes to the second.
    let (to_child, child_read) = windows_pipe(false)?;
    let (from_child, child_write) = windows_pipe(true)?;
    let value = |handle: &std::os::windows::io::OwnedHandle| {
        u32::try_from(handle.as_raw_handle() as usize)
            .map_err(|_| BrowserError::Launch("DevTools pipe handle does not fit in u32".into()))
    };
    let child_values = (value(&child_read)?, value(&child_write)?);
    Ok(DevtoolsPipe {
        parent: (to_child, from_child),
        child: ChildEnds {
            _held: vec![child_read, child_write],
        },
        child_values,
    })
}

/// The caller's job object to bind Chrome to (Windows only).
#[cfg(windows)]
pub(crate) type CallerJob = Option<std::sync::Arc<std::os::windows::io::OwnedHandle>>;
#[cfg(not(windows))]
pub(crate) type CallerJob = Option<std::convert::Infallible>;

/// One argv element, preserving native path bytes. Command handles OS quoting;
/// adding shell quotes here would make them part of the directory name.
pub(crate) fn profile_arg(path: &Path) -> OsString {
    let mut arg = OsString::from("--user-data-dir=");
    arg.push(path.as_os_str());
    arg
}

/// The Chrome command with its DevTools pipe pair wired in.
///
/// Unix: Chrome reads CDP on fd 3 and writes it on fd 4. The sweep `OwnedCommand` runs in the
/// child (every descriptor above stderr made close-on-exec) comes after any `pre_exec` of
/// `command_mut`, so the dup2 onto 3 and 4 is registered with `pre_exec_last`, which runs after
/// the sweep as the last step before exec; dup2 clears close-on-exec on its target. The sources
/// are numbered 5 or higher (see `devtools_pipe`), so neither dup2 overwrites the other's
/// source, and they stay close-on-exec, so exactly fd 0 to 2, 3 and 4 reach Chrome.
///
/// Windows: only the two child handles are inherited, by handle list, and Chrome is told their
/// values with `--remote-debugging-io-pipes`.
fn chrome_command(exe: &Path, args: &[OsString], pipe: &DevtoolsPipe) -> OwnedCommand {
    let mut cmd = OwnedCommand::new(exe);
    cmd.windows_hide();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    cmd.kill_on_parent_exit(true);
    #[cfg(windows)]
    let pipe_flag = [OsString::from(format!(
        "--remote-debugging-io-pipes={},{}",
        pipe.child_values.0, pipe.child_values.1
    ))];
    #[cfg(not(windows))]
    let pipe_flag: [OsString; 0] = [];
    cmd.command_mut()
        .args(pipe_flag)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        let (read_fd, write_fd) = pipe.child_fds;
        // SAFETY: the hook calls only dup2, which is async-signal-safe; it allocates nothing and
        // takes no lock. Its error path builds an `io::Error` from errno, which does not
        // allocate. The descriptor numbers are plain integers copied into the closure.
        unsafe {
            cmd.pre_exec_last(move || {
                dup_onto(read_fd, CHROME_READ_FD)?;
                dup_onto(write_fd, CHROME_WRITE_FD)
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        // A std `Command` spawn cannot name the handles to inherit, so the native spawn is
        // used: its stdio is described here (the same as the `Command` calls above).
        cmd.windows_spawn_config(rightkit_child_process::WindowsSpawnConfig {
            stdin: rightkit_child_process::WindowsStdio::Null,
            stdout: rightkit_child_process::WindowsStdio::Null,
            stderr: rightkit_child_process::WindowsStdio::Piped,
            inherit_environment: true,
            raw_arguments: Vec::new(),
        });
        let mut inherited = Vec::new();
        for handle in &pipe.child._held {
            inherited.push(handle.as_raw_handle());
        }
        cmd.inherit_handles_only(inherited);
    }
    cmd
}

/// `dup2(from, to)` retried on `EINTR`; the result has close-on-exec cleared.
#[cfg(unix)]
fn dup_onto(from: std::os::fd::RawFd, to: std::os::fd::RawFd) -> std::io::Result<()> {
    loop {
        // SAFETY: dup2 takes two descriptor numbers and touches no memory.
        if unsafe { libc::dup2(from, to) } >= 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Err(error);
        }
    }
}

/// The only place `--no-sandbox` is added. Chrome's inner sandbox cannot nest inside the
/// fixture's outer confinement (macOS Seatbelt, Windows AppContainer), which confines the whole
/// process tree instead. `BrowserSession::launch` never calls this (test-asserted).
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
pub(crate) fn fixture_chrome_args(args: &[OsString]) -> Vec<OsString> {
    let mut fixture_args = args.to_vec();
    if !fixture_args.iter().any(|arg| arg == "--no-sandbox") {
        fixture_args.push("--no-sandbox".into());
    }
    // The GPU process and the crashpad handler are not needed for the CDP fixture and fail to
    // start under the outer confinement (crashpad close_stdio CHECK on macOS; GPU/handler
    // CHECK, exit 0x80000003, in the Windows AppContainer).
    for flag in [
        "--disable-gpu",
        "--disable-crash-reporter",
        "--disable-breakpad",
    ] {
        if !fixture_args.iter().any(|arg| arg == flag) {
            fixture_args.push(flag.into());
        }
    }
    // HYPOTHESIS (unproven): Chrome's renderer code-integrity mitigation needs process
    // policy an AppContainer launch with --no-sandbox cannot grant, and CHECKs at startup.
    // Fixture-only; read the captured stderr tail to confirm or discard.
    if !fixture_args
        .iter()
        .any(|arg| arg.to_string_lossy().starts_with("--disable-features="))
    {
        fixture_args.push("--disable-features=RendererCodeIntegrity".into());
    }
    fixture_args
}

pub(crate) fn spawn_chrome(exe: &Path, args: &[OsString], job: CallerJob) -> Result<ChromeProc> {
    let pipe = devtools_pipe()?;
    let cmd = chrome_command(exe, args, &pipe);
    spawn_chrome_command(cmd, pipe, job)
}

/// Fixture launches require a real macOS Seatbelt profile. Other platforms fail closed at
/// the caller before reaching this function; this cfg boundary prevents an accidental
/// unconfined fixture spawn from being added later.
#[cfg(target_os = "macos")]
pub(crate) fn spawn_chrome_fixture(
    exe: &Path,
    args: &[OsString],
    profile_dir: &Path,
    proxy_addr: std::net::SocketAddr,
) -> Result<ChromeProc> {
    use rightkit_sandbox::seatbelt::{BrowserFixturePolicy, Prepared};
    use std::os::unix::process::CommandExt;

    let policy = BrowserFixturePolicy {
        chrome_executable: exe.to_path_buf(),
        profile_dir: profile_dir.to_path_buf(),
        proxy_addr,
    };
    let prepared = Prepared::for_browser_fixture(&policy).map_err(|error| {
        BrowserError::Launch(format!("fixture confinement unavailable: {error}"))
    })?;
    // Chrome's own helper sandbox calls sandbox_init, which the enclosing Seatbelt profile
    // refuses; the profile confines the whole process tree instead.
    let fixture_args = fixture_chrome_args(args);
    let pipe = devtools_pipe()?;
    let mut cmd = chrome_command(exe, &fixture_args, &pipe);
    cmd.env_allowlist(std::iter::empty::<&str>());
    cmd.command_mut()
        .env("HOME", profile_dir)
        .env("TMPDIR", profile_dir)
        .env("TMP", profile_dir)
        .env("TEMP", profile_dir);
    cmd.inherit_handles_only(std::iter::empty());
    // SAFETY: the closure only applies a precomputed Seatbelt profile in the forked child;
    // it performs no allocation, lock acquisition, or interaction with parent state.
    unsafe {
        cmd.command_mut().pre_exec(move || prepared.apply());
    }
    spawn_chrome_command(cmd, pipe, None)
}

/// A unit of work for the launcher thread.
type LauncherJob = Box<dyn FnOnce() + Send>;

/// The process-wide launcher thread's job queue. Created by the first launch and never closed.
static LAUNCHER: std::sync::OnceLock<std::sync::mpsc::Sender<LauncherJob>> =
    std::sync::OnceLock::new();

fn launcher_sender() -> std::io::Result<&'static std::sync::mpsc::Sender<LauncherJob>> {
    if let Some(sender) = LAUNCHER.get() {
        return Ok(sender);
    }
    let (tx, rx) = std::sync::mpsc::channel::<LauncherJob>();
    // The receiver lives only in this thread, which never exits while the process runs.
    std::thread::Builder::new()
        .name("rightkit-browser-launcher".into())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                job();
            }
        })?;
    // If another launch won the race, `tx` is dropped here; the thread spawned above then
    // ends on its own and the winner's sender is used.
    let _ = LAUNCHER.set(tx);
    LAUNCHER.get().ok_or_else(launcher_gone)
}

fn launcher_gone() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "browser launcher thread is gone",
    )
}

/// Runs `job` on the dedicated launcher thread and returns its result. A panic in `job` is
/// contained: the caller receives `BrokenPipe` and the launcher thread keeps serving jobs.
fn run_on_launcher_thread<T: Send + 'static>(
    job: impl FnOnce() -> T + Send + 'static,
) -> std::io::Result<T> {
    let sender = launcher_sender()?;
    let (result_tx, result_rx) = std::sync::mpsc::channel::<T>();
    let boxed: LauncherJob = Box::new(move || {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)) {
            Ok(value) => {
                let _ = result_tx.send(value);
            }
            // Dropping `result_tx` on a panic makes the caller's `recv` fail.
            Err(_) => drop(result_tx),
        }
    });
    sender.send(boxed).map_err(|_| launcher_gone())?;
    result_rx.recv().map_err(|_| launcher_gone())
}

fn spawn_chrome_command(
    #[cfg_attr(not(windows), allow(unused_mut))] mut cmd: OwnedCommand,
    pipe: DevtoolsPipe,
    job: CallerJob,
) -> Result<ChromeProc> {
    let DevtoolsPipe {
        parent: pipe_parent,
        child: child_ends,
        ..
    } = pipe;
    #[cfg(windows)]
    if let Some(job) = &job {
        use std::os::windows::io::AsHandle;
        cmd.windows_job(job.as_handle())
            .map_err(|e| BrowserError::Launch(e.to_string()))?;
    }
    #[cfg(not(windows))]
    let _ = job;
    // Spawned on the launcher thread: Linux PDEATHSIG (kill_on_parent_exit) is keyed to the
    // spawning thread, and a pooled blocking thread exits after its idle keep-alive.
    // The parent's copies of Chrome's pipe ends close inside this closure, as soon as the spawn
    // returns: a kept copy of Chrome's write end would hide Chrome's exit (no end of file on
    // the reply pipe) and a kept copy of its read end would hide a dead Chrome from writers.
    let mut child = run_on_launcher_thread(move || {
        let spawned = cmd.spawn();
        drop(child_ends);
        spawned
    })
    .map_err(|e| BrowserError::Launch(e.to_string()))?
    .map_err(|e| BrowserError::Launch(e.to_string()))?;
    let stderr = child
        .take_stderr()
        .ok_or_else(|| BrowserError::Launch("no stderr".into()))?;
    // Keeps draining stderr for the life of Chrome so it never blocks on a full pipe. The
    // drain reads bytes, not text: a line that is not UTF-8 (Windows writes some messages in
    // the system code page) must not end it, or the pipe fills and Chrome stalls on its next
    // write.
    let stderr_tail = StderrTail::default();
    let tail = stderr_tail.clone();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            tail.push(&String::from_utf8_lossy(&line));
        }
    });
    let watchdog = spawn_watchdog(child.id());
    Ok(ChromeProc {
        child: ChromeChild::Owned(child),
        _watchdog: watchdog,
        stderr_tail,
        pipe_parent: Some(pipe_parent),
        pipe_bridge: None,
        #[cfg(windows)]
        fixture_log: None,
        #[cfg(windows)]
        launch_params: String::new(),
        #[cfg(windows)]
        acl_targets: Vec::new(),
    })
}

/// Windows fixture Chrome has no network authority. Its sole CDP channel is a sandbox-created
/// inherited pipe pair, relayed by the same bridge as every other launch.
#[cfg(windows)]
pub(crate) fn spawn_chrome_fixture_windows(
    exe: &Path,
    args: &[OsString],
    profile_dir: &Path,
) -> Result<ChromeProc> {
    // Outer AppContainer confines the tree; Chrome's inner sandbox cannot nest inside it.
    // `exe` is chrome-headless-shell.exe: headless by construction, so `--headless=new` (a
    // chrome.exe mode switch) is dropped. chrome.exe is not used here because its browser
    // process unconditionally starts the crashpad handler, whose CreateNamedPipe is denied in an
    // AppContainer (components/crash/core/app/crashpad_win.cc,
    // headless/lib/headless_content_main_delegate.cc).
    let args: Vec<OsString> = fixture_chrome_args(args)
        .into_iter()
        .filter(|arg| !arg.to_string_lossy().starts_with("--headless"))
        .collect();
    let args = args.as_slice();
    // Chrome's stderr/stdout (the CHECK text) land here: the sandbox opens this file in the
    // parent and inherits the handle, so it works before Chrome's own logging initialises.
    let log_path = profile_dir.join("chrome-stderr.txt");
    // The sandbox creates/opens this file itself (append, FILE_SHARE_READ only, validated against
    // the write grant); this function must not hold a handle to it across the spawn.
    let policy = SandboxPolicy::new()
        .allow_exec(exe)
        .allow_write(profile_dir)
        .network(SandboxNetworkPolicy::Deny)
        .env(EnvPolicy {
            inherit: false,
            // Chrome CHECKs when SystemRoot/windir are missing; everything else that Chrome
            // reads for scratch or profile state points into the write grant.
            keep: FIXTURE_ENV_KEEP.iter().map(|k| k.to_string()).collect(),
            set: FIXTURE_ENV_SET
                .iter()
                .map(|k| (k.to_string(), profile_dir.to_string_lossy().into_owned()))
                .collect(),
        });
    // Fixture-only (never network): Chrome under LPAC reads its install dir and the registry
    // policy keys; without these it can CHECK during startup.
    let policy = FIXTURE_NAMED_CAPABILITIES
        .iter()
        .fold(policy, |policy, name| policy.named_capability(*name));
    let command = SandboxCommand::new(exe)
        .args(args.iter().cloned())
        .args(FIXTURE_LOGGING_ARGS);
    let launch_params = fixture_launch_params(exe, args, profile_dir);
    let sandbox = Sandbox::new(ResourceAuthority::new(ResourceProfile::Practical));
    let spawn = sandbox
        .spawn_with_pipe_pair_stdio_file(
            &policy,
            command,
            "--remote-debugging-io-pipes",
            Some(&log_path),
        )
        .map_err(|error| {
            BrowserError::Launch(format!(
                "Windows fixture AppContainer: {error}; {launch_params}"
            ))
        })?;
    Ok(ChromeProc {
        child: ChromeChild::Confined(spawn.child),
        _watchdog: None,
        stderr_tail: StderrTail::default(),
        pipe_parent: Some(spawn.io.into_parent_handles()),
        pipe_bridge: None,
        fixture_log: Some(log_path),
        launch_params,
        acl_targets: vec![
            profile_dir.to_path_buf(),
            exe.parent().map(Path::to_path_buf).unwrap_or_default(),
        ],
    })
}

/// Fixture-only logging: stderr is the retained `chrome-stderr.txt` handle.
#[cfg(windows)]
const FIXTURE_LOGGING_ARGS: [&str; 2] = ["--enable-logging=stderr", "--v=1"];

/// Env vars kept from the parent, and vars pointed at the write grant, for the fixture.
#[cfg(windows)]
const FIXTURE_ENV_KEEP: [&str; 2] = ["SystemRoot", "windir"];
#[cfg(windows)]
const FIXTURE_ENV_SET: [&str; 5] = ["TEMP", "TMP", "LOCALAPPDATA", "APPDATA", "USERPROFILE"];
/// Chrome LPAC capabilities requested for the fixture only. `internetClient` is never one.
#[cfg(windows)]
const FIXTURE_NAMED_CAPABILITIES: [&str; 2] = ["lpacChromeInstallFiles", "registryRead"];

/// Hex exit code plus the well-known meaning of the NTSTATUS values a failed Chrome start
/// produces, so a CI log names the cause without a lookup.
#[cfg(windows)]
fn describe_exit_code(code: u32) -> String {
    let meaning = match code {
        0 => "success",
        0xC000_0005 => "STATUS_ACCESS_VIOLATION",
        0xC000_0022 => "STATUS_ACCESS_DENIED",
        0xC000_0135 => "STATUS_DLL_NOT_FOUND",
        0xC000_0142 => "STATUS_DLL_INIT_FAILED",
        0xC000_0409 => "STATUS_STACK_BUFFER_OVERRUN (fast-fail)",
        0xC000_041D => "STATUS_FATAL_USER_CALLBACK_EXCEPTION",
        0x8000_0003 => "breakpoint (Chrome CHECK/fatal)",
        _ => "unclassified",
    };
    format!("{code} (0x{code:08X}, {meaning})")
}

/// The launch parameters of the Windows fixture (`exe` and `exec_grant` are `chrome-headless-shell.exe`,
/// headless by construction), as the AppContainer launcher applies them:
/// grants come from `spawn_chrome_fixture_windows`'s policy, capabilities from the sandbox crate
/// (`NetworkPolicy::Deny` requests no `internetClient`; the fixture adds only the named Chrome
/// LPAC capabilities).
#[cfg(windows)]
fn fixture_launch_params(exe: &Path, args: &[OsString], profile_dir: &Path) -> String {
    let flags: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .filter(|arg| arg.starts_with("--") && !arg.starts_with("--user-data-dir"))
        .collect();
    let has = |name: &str| {
        flags
            .iter()
            .any(|flag| flag == name || flag.starts_with(&format!("{name}=")))
    };
    format!(
        "launch_params{{exe={}, exec_grant={}, write_grant={}, network=Deny, \
         capabilities=[{}] (no internetClient), \
         env=[inherit=false; kept {}; set to write grant {}; sandbox adds ComSpec Path SystemDrive \
         SystemRoot windir LOCALAPPDATA TEMP TMP NODE_OPTIONS], no_sandbox={}, disable_gpu={}, headless={}, flags=[{}]}}",
        exe.display(),
        exe.display(),
        profile_dir.display(),
        FIXTURE_NAMED_CAPABILITIES.join(", "),
        FIXTURE_ENV_KEEP.join(","),
        FIXTURE_ENV_SET.join(","),
        has("--no-sandbox"),
        has("--disable-gpu"),
        flags.iter().any(|flag| flag.starts_with("--headless"))
            || exe
                .file_name()
                .is_some_and(|name| name.to_string_lossy().contains("headless-shell")),
        flags.join(" "),
    )
}

/// Bounded `icacls` dump of one path for failure evidence; observe only.
#[cfg(windows)]
fn icacls_summary(path: &Path) -> String {
    use std::os::windows::process::CommandExt;
    match std::process::Command::new("icacls")
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .arg(path)
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout);
            let joined = text.split_whitespace().collect::<Vec<_>>().join(" ");
            joined.chars().take(1500).collect()
        }
        Err(error) => format!("icacls unavailable ({error})"),
    }
}

#[cfg(windows)]
fn bounded_file_tail(path: &Path) -> std::io::Result<String> {
    let mut reader = File::open(path)?;
    let length = reader.metadata()?.len();
    reader.seek(SeekFrom::Start(
        length.saturating_sub(FIXTURE_LOG_TAIL_BYTES),
    ))?;
    let mut bytes = Vec::with_capacity(FIXTURE_LOG_TAIL_BYTES as usize);
    reader
        .take(FIXTURE_LOG_TAIL_BYTES)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() {
        Ok("(chrome stderr file empty: Chrome wrote nothing before dying)".into())
    } else {
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

#[cfg(unix)]
fn spawn_watchdog(chrome_pgid: u32) -> Option<OwnedChild> {
    // Polls the host and Chrome; when the host is gone, SIGTERMs Chrome's whole
    // process group, waits up to 5 s, then SIGKILLs what is left. Exits by itself
    // once Chrome is gone.
    let script = r#"while kill -0 "$1" 2>/dev/null && kill -0 "$2" 2>/dev/null; do sleep 0.2; done
if kill -0 "$2" 2>/dev/null; then
  kill -TERM -- "-$2" 2>/dev/null
  i=0
  while [ "$i" -lt 25 ] && kill -0 -- "-$2" 2>/dev/null; do sleep 0.2; i=$((i+1)); done
  kill -KILL -- "-$2" 2>/dev/null
fi
exit 0"#;
    let mut cmd = OwnedCommand::new("/bin/sh");
    cmd.command_mut()
        .args([
            "-c",
            script,
            "rk-watchdog",
            &std::process::id().to_string(),
            &chrome_pgid.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().ok()
}

// Windows: the job object created by `OwnedCommand` closes with this process
// and kills Chrome (KILL_ON_JOB_CLOSE), so no watchdog is needed.
#[cfg(not(unix))]
fn spawn_watchdog(_chrome_pid: u32) -> Option<OwnedChild> {
    None
}
