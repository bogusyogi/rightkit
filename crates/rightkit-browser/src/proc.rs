//! Chrome as an owned child. Spawned through `rightkit-process` so it is a
//! process-group leader on Unix and a Windows job object member
//! (KILL_ON_JOB_CLOSE, no breakaway), bound first to the caller's own job when one
//! is supplied; a parent-death watchdog covers a host that dies without running
//! destructors on Unix.
use crate::error::{BrowserError, Result};
use rightkit_process::{OwnedChild, OwnedCommand};
#[cfg(windows)]
use rightkit_sandbox::{
    EnvPolicy, NetworkPolicy as SandboxNetworkPolicy, ResourceAuthority, ResourceProfile, Sandbox,
    SandboxCommand, SandboxPolicy, SandboxedChild, WindowsAuxiliaryIo,
};
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
#[cfg(windows)]
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom},
};

#[cfg(windows)]
const FIXTURE_LOG_TAIL_BYTES: u64 = 16 * 1024;

pub(crate) struct ChromeProc {
    child: ChromeChild,
    /// Unix only: kills Chrome's process group when this host process vanishes.
    pub _watchdog: Option<OwnedChild>,
    pub ws_url: String,
    /// The last lines Chrome wrote to stderr, for the detail of a failed navigation.
    pub stderr_tail: StderrTail,
    #[cfg(windows)]
    pipe_io: Option<WindowsAuxiliaryIo>,
    #[cfg(windows)]
    pipe_bridge: Option<crate::pipe_transport::PipeBridge>,
    /// Parent-created before Chrome launches. Diagnostics only ever read this retained handle,
    /// never a path the confined child could replace with a reparse point.
    #[cfg(windows)]
    fixture_log: Option<File>,
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
    ) -> std::io::Result<rightkit_process::ReapOutcome> {
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
                rightkit_process::ReapOutcome::Exited(_) => Ok(()),
                rightkit_process::ReapOutcome::TimedOut => Err(std::io::Error::new(
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
                rightkit_process::ReapOutcome::Exited(_) => {}
                rightkit_process::ReapOutcome::TimedOut => {
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
            rightkit_process::ReapOutcome::Exited(_) => {}
            rightkit_process::ReapOutcome::TimedOut => {
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
            .and_then(|file| bounded_file_tail(file).ok())
            .unwrap_or_else(|| "(retained fixture log unavailable)".into());
        format!(
            "windows fixture diagnostic chrome_exited={live_exit}; chrome_exit_code={exit_code}; \
             {launch_params}; acls=[{acls}]; stderr=NUL (pipe-pair stdio is NUL; Chrome output \
             is only in the retained log); retained_log_tail={log_tail}",
            launch_params = self.launch_params,
        )
    }

    #[cfg(windows)]
    pub(crate) fn start_fixture_pipe_bridge(
        &mut self,
        deadline: std::time::Instant,
    ) -> Result<String> {
        let io = self.pipe_io.take().ok_or_else(|| {
            BrowserError::Launch("Windows fixture CDP pipe ownership is absent".into())
        })?;
        let (to_child, from_child) = io.into_parent_handles();
        let bridge = crate::pipe_transport::PipeBridge::start(to_child, from_child, deadline)?;
        let url = bridge.websocket_url().to_owned();
        self.pipe_bridge = Some(bridge);
        Ok(url)
    }

    #[cfg(windows)]
    pub(crate) fn begin_fixture_transport_shutdown(&self) {
        if let Some(bridge) = &self.pipe_bridge {
            bridge.begin_shutdown();
        }
    }

    #[cfg(windows)]
    pub(crate) async fn finish_fixture_transport_shutdown(
        &mut self,
        deadline: std::time::Instant,
    ) -> Result<()> {
        let bridge = self.pipe_bridge.as_mut().ok_or_else(|| {
            BrowserError::Launch("Windows fixture transport ownership is absent".into())
        })?;
        bridge.shutdown(deadline).await
    }

    #[cfg(not(windows))]
    pub(crate) fn begin_fixture_transport_shutdown(&self) {}
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

fn chrome_command(exe: &Path, args: &[OsString]) -> OwnedCommand {
    let mut cmd = OwnedCommand::new(exe);
    cmd.windows_hide();
    cmd.command_mut()
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    cmd
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
    fixture_args
}

pub(crate) fn spawn_chrome(
    exe: &Path,
    args: &[OsString],
    timeout: Duration,
    job: CallerJob,
) -> Result<ChromeProc> {
    spawn_chrome_command(chrome_command(exe, args), timeout, job)
}

/// Fixture launches require a real macOS Seatbelt profile. Other platforms fail closed at
/// the caller before reaching this function; this cfg boundary prevents an accidental
/// unconfined fixture spawn from being added later.
#[cfg(target_os = "macos")]
pub(crate) fn spawn_chrome_fixture(
    exe: &Path,
    args: &[OsString],
    timeout: Duration,
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
    let mut cmd = chrome_command(exe, &fixture_args);
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
    spawn_chrome_command(cmd, timeout, None)
}

fn spawn_chrome_command(
    #[cfg_attr(not(windows), allow(unused_mut))] mut cmd: OwnedCommand,
    timeout: Duration,
    job: CallerJob,
) -> Result<ChromeProc> {
    #[cfg(windows)]
    if let Some(job) = &job {
        use std::os::windows::io::AsHandle;
        cmd.windows_job(job.as_handle())
            .map_err(|e| BrowserError::Launch(e.to_string()))?;
    }
    #[cfg(not(windows))]
    let _ = job;
    let mut child = cmd
        .spawn()
        .map_err(|e| BrowserError::Launch(e.to_string()))?;
    let stderr = child
        .take_stderr()
        .ok_or_else(|| BrowserError::Launch("no stderr".into()))?;
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    // Keeps draining stderr after the URL line so Chrome never blocks on a full pipe. The
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
            let text = String::from_utf8_lossy(&line);
            if let Some(u) = text.split("DevTools listening on ").nth(1) {
                let _ = tx.send(u.trim().to_string());
            } else {
                tail.push(&text);
            }
        }
    });
    let ws_url = match rx.recv_timeout(timeout) {
        Ok(url) => url,
        Err(_) => {
            // Let the stderr drain deliver Chrome's final lines before reporting them.
            std::thread::sleep(Duration::from_millis(200));
            let exit = match child.try_wait() {
                Ok(Some(status)) => format!("exited with {status}"),
                Ok(None) => "still running".to_string(),
                Err(error) => format!("status unavailable: {error}"),
            };
            return Err(BrowserError::Launch(format!(
                "chrome did not report a DevTools endpoint; process {exit}; stderr: {}",
                stderr_tail.text()
            )));
        }
    };
    let watchdog = spawn_watchdog(child.id());
    Ok(ChromeProc {
        child: ChromeChild::Owned(child),
        _watchdog: watchdog,
        ws_url,
        stderr_tail,
        #[cfg(windows)]
        pipe_io: None,
        #[cfg(windows)]
        pipe_bridge: None,
        #[cfg(windows)]
        fixture_log: None,
        #[cfg(windows)]
        launch_params: String::new(),
        #[cfg(windows)]
        acl_targets: Vec::new(),
    })
}

/// Windows fixture Chrome has no DevTools TCP listener & no network authority.
/// Its sole CDP channel is a sandbox-created inherited pipe pair.
#[cfg(windows)]
pub(crate) fn spawn_chrome_fixture_windows(
    exe: &Path,
    args: &[OsString],
    profile_dir: &Path,
) -> Result<ChromeProc> {
    // Outer AppContainer confines the tree; Chrome's inner sandbox cannot nest inside it.
    let args = fixture_chrome_args(args);
    let args = args.as_slice();
    let log_path = profile_dir.join("fixture-chrome.log");
    // Open before the confined process starts so diagnostics read this exact handle, even if
    // Chrome later replaces its path. The profile is an owned temporary directory.
    let fixture_log = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&log_path)
        .map_err(|error| {
            BrowserError::Launch(format!(
                "create Windows fixture diagnostic log {}: {error}",
                log_path.display()
            ))
        })?;
    let policy = SandboxPolicy::new()
        .allow_exec(exe)
        .allow_write(profile_dir)
        .network(SandboxNetworkPolicy::Deny)
        .env(EnvPolicy {
            inherit: false,
            // Chrome CHECKs when SystemRoot/windir are missing; everything else that Chrome
            // reads for scratch or profile state points into the write grant.
            keep: vec!["SystemRoot".into(), "windir".into()],
            set: [
                ("TEMP", profile_dir),
                ("TMP", profile_dir),
                ("LOCALAPPDATA", profile_dir),
                ("APPDATA", profile_dir),
                ("USERPROFILE", profile_dir),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string_lossy().into_owned()))
            .collect(),
        });
    let mut log_arg = OsString::from("--log-file=");
    log_arg.push(log_path.as_os_str());
    let command = SandboxCommand::new(exe)
        .args(args.iter().cloned())
        .arg("--enable-logging=stderr")
        .arg("--v=0")
        .arg(log_arg);
    let launch_params = fixture_launch_params(exe, args, profile_dir);
    let sandbox = Sandbox::new(ResourceAuthority::new(ResourceProfile::Practical));
    let spawn = sandbox
        .spawn_with_pipe_pair(&policy, command, "--remote-debugging-io-pipes")
        .map_err(|error| {
            BrowserError::Launch(format!(
                "Windows fixture AppContainer: {error}; {launch_params}"
            ))
        })?;
    Ok(ChromeProc {
        child: ChromeChild::Confined(spawn.child),
        _watchdog: None,
        ws_url: String::new(),
        stderr_tail: StderrTail::default(),
        pipe_io: Some(spawn.io),
        pipe_bridge: None,
        fixture_log: Some(fixture_log),
        launch_params,
        acl_targets: vec![
            profile_dir.to_path_buf(),
            exe.parent().map(Path::to_path_buf).unwrap_or_default(),
        ],
    })
}

/// Hex exit code plus the well-known meaning of the NTSTATUS values a failed Chrome start
/// produces, so a CI log names the cause without a lookup.
#[cfg(any(windows, test))]
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

/// The launch parameters of the Windows fixture, as the AppContainer launcher applies them:
/// grants come from `spawn_chrome_fixture_windows`'s policy, capabilities from the sandbox crate
/// (`NetworkPolicy::Deny` requests none, so no `internetClient`, `lpacChromeInstallFiles` or
/// `registryRead`).
#[cfg(any(windows, test))]
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
         capabilities=[] (no internetClient, lpacChromeInstallFiles or registryRead), \
         env=cleared, no_sandbox={}, disable_gpu={}, headless={}, flags=[{}]}}",
        exe.display(),
        exe.display(),
        profile_dir.display(),
        has("--no-sandbox"),
        has("--disable-gpu"),
        flags.iter().any(|flag| flag.starts_with("--headless")),
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
fn bounded_file_tail(file: &File) -> std::io::Result<String> {
    let mut reader = file.try_clone()?;
    let length = reader.metadata()?.len();
    reader.seek(SeekFrom::Start(
        length.saturating_sub(FIXTURE_LOG_TAIL_BYTES),
    ))?;
    let mut bytes = Vec::with_capacity(FIXTURE_LOG_TAIL_BYTES as usize);
    reader
        .take(FIXTURE_LOG_TAIL_BYTES)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() {
        Ok("(retained fixture log empty)".into())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_sandbox_is_added_only_by_the_fixture_arg_builder() {
        let base: Vec<OsString> = vec!["--headless=new".into(), "about:blank".into()];
        let fixture = fixture_chrome_args(&base);
        assert_eq!(
            fixture.iter().filter(|a| *a == "--no-sandbox").count(),
            1,
            "{fixture:?}"
        );
        // Idempotent.
        assert_eq!(fixture_chrome_args(&fixture), fixture);
        for flag in [
            "--disable-gpu",
            "--disable-crash-reporter",
            "--disable-breakpad",
        ] {
            assert_eq!(fixture.iter().filter(|a| *a == flag).count(), 1, "{flag}");
        }
        // The ordinary path hands its args to Chrome unchanged.
        let mut ordinary = chrome_command(Path::new("chrome"), &base);
        let seen: Vec<_> = ordinary.command_mut().get_args().collect();
        assert!(!seen.iter().any(|a| *a == "--no-sandbox"), "{seen:?}");
        assert!(!seen.iter().any(|a| *a == "--disable-gpu"), "{seen:?}");
        for flag in ["--disable-crash-reporter", "--disable-breakpad"] {
            assert!(!seen.iter().any(|a| *a == flag), "{flag}: {seen:?}");
        }
        // BrowserSession::launch's own source never names the flag.
        let session = include_str!("session.rs");
        assert!(!session.contains("\"--no-sandbox\""));
        assert!(!session.contains("fixture_chrome_args"));
    }

    #[test]
    fn profile_path_is_one_native_argv_element() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("profile with spaces & ü 'quotes'");
        std::fs::create_dir(&path).unwrap();
        let arg = profile_arg(&path);
        let mut expected = OsString::from("--user-data-dir=");
        expected.push(path.as_os_str());
        let mut cmd = chrome_command(Path::new("unused-chrome"), &[arg]);
        let argv: Vec<_> = cmd.command_mut().get_args().map(|a| a.to_owned()).collect();
        assert_eq!(argv, vec![expected]);
    }

    // APFS (macOS) rejects non-UTF-8 file names with EILSEQ; Linux filesystems accept them.
    #[cfg(target_os = "linux")]
    #[test]
    fn profile_argv_preserves_non_utf8_bytes_and_shell_characters() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let temp = tempfile::tempdir().unwrap();
        let path = temp
            .path()
            .join(OsString::from_vec(b"profile \xff \"$;\n".to_vec()));
        std::fs::create_dir(&path).unwrap();
        let mut cmd = chrome_command(Path::new("unused-chrome"), &[profile_arg(&path)]);
        let mut expected = b"--user-data-dir=".to_vec();
        expected.extend_from_slice(path.as_os_str().as_bytes());
        let argv: Vec<_> = cmd.command_mut().get_args().map(|a| a.as_bytes()).collect();
        assert_eq!(argv, vec![expected.as_slice()]);
    }
}

#[cfg(test)]
mod fixture_diagnostics_tests {
    use super::*;

    #[test]
    fn exit_code_names_the_ntstatus() {
        let text = describe_exit_code(0xC000_0022);
        assert!(text.contains("0xC0000022") && text.contains("STATUS_ACCESS_DENIED"));
        assert!(describe_exit_code(7).contains("unclassified"));
    }

    #[test]
    fn launch_params_report_sandbox_and_gpu_flags() {
        let args: Vec<OsString> = ["--headless=new", "--remote-debugging-pipe", "about:blank"]
            .iter()
            .map(OsString::from)
            .collect();
        let text = fixture_launch_params(Path::new("chrome.exe"), &args, Path::new("profile"));
        assert!(text.contains("no_sandbox=false"), "{text}");
        assert!(text.contains("disable_gpu=false"), "{text}");
        assert!(text.contains("headless=true"), "{text}");
        assert!(text.contains("capabilities=[]"), "{text}");
        let args: Vec<OsString> = ["--no-sandbox", "--disable-gpu"]
            .iter()
            .map(OsString::from)
            .collect();
        let text = fixture_launch_params(Path::new("chrome.exe"), &args, Path::new("profile"));
        assert!(
            text.contains("no_sandbox=true") && text.contains("disable_gpu=true"),
            "{text}"
        );
    }
}
