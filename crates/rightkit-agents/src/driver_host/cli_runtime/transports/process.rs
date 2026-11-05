//! Small, shared stdio supervisor used by every CLI transport.
//!
//! It deliberately receives a fully resolved launch spec: adapters decide argv/profile/policy,
//! while this layer applies environment denial, owns descendants, & exposes newline JSON frames.

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

use rightkit_sandbox::execution::{CommandLaunchSpec, ProcessLaunchAdapter, ProcessLaunchContext};
use serde_json::Value;
use std::sync::mpsc::{self, Receiver, Sender};

use crate::driver_host::cli_runtime::adapter::{
    CliLaunchSpec, CliProcessIo, CliRuntimeError, StartedCliProcess,
};

/// Output of [`StdioProcessIo::capture_bounded`]. `status` is `None` when the command was still
/// running at the deadline and was cancelled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedCapture {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Clone)]
pub struct StdioProcessIo {
    #[cfg(not(windows))]
    child: Arc<Mutex<rightkit_child_process::OwnedChild>>,
    stdin: Arc<Mutex<ChildStdin>>,
    stdout: Arc<Mutex<BufReader<ChildStdout>>>,
    #[cfg(windows)]
    windows_job: Arc<Mutex<Option<rightkit_agent_tools::coding_tools::sandbox::WindowsJob>>>,
}

impl StdioProcessIo {
    /// Bound for a vendor probe (`--version`, `auth status`): R19.
    pub const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    /// Run a vendor command to completion within [`Self::PROBE_TIMEOUT`]. A command that does not
    /// finish in time is killed and reported as an error (R19: no unbounded child wait).
    pub async fn capture_with_native_adapter(
        executable: &Path,
        argv: Vec<String>,
        cwd: &Path,
        env: Vec<(String, String)>,
    ) -> Result<(i32, Vec<u8>, Vec<u8>), CliRuntimeError> {
        let captured =
            Self::capture_bounded(executable, argv, cwd, env, Self::PROBE_TIMEOUT).await?;
        match captured.status {
            Some(status) => Ok((status, captured.stdout, captured.stderr)),
            None => Err(CliRuntimeError::new(
                "cli_process_timeout",
                format!(
                    "vendor command did not finish within {} s",
                    Self::PROBE_TIMEOUT.as_secs()
                ),
            )),
        }
    }

    /// Run a vendor command under the native sandbox adapter and wait at most `limit`. On timeout
    /// the child is cancelled, `status` is `None` and whatever it wrote so far is returned, so the
    /// caller can show the vendor's own words (login R13).
    pub async fn capture_bounded(
        executable: &Path,
        argv: Vec<String>,
        cwd: &Path,
        env: Vec<(String, String)>,
        limit: std::time::Duration,
    ) -> Result<BoundedCapture, CliRuntimeError> {
        let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
        let spec = CommandLaunchSpec {
            program: executable.to_string_lossy().into_owned(),
            argv,
            cwd: cwd.to_string_lossy().into_owned(),
            context: ProcessLaunchContext {
                workspace_root: cwd.to_string_lossy().into_owned(),
                sandbox_profile: Some("vendor-probe".into()),
                sandbox_required: true,
                network_required: true,
            },
            env,
        };
        let mut child = adapter
            .spawn(spec)
            .await
            .map_err(|error| CliRuntimeError::new("cli_process_spawn", error.to_string()))?;
        let stdout_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let stderr_buf = Arc::new(Mutex::new(Vec::<u8>::new()));
        let readers = [
            (child.take_stdout(), Arc::clone(&stdout_buf)),
            (child.take_stderr(), Arc::clone(&stderr_buf)),
        ]
        .into_iter()
        .filter_map(|(stream, sink)| {
            let mut stream = stream?;
            Some(tokio::spawn(async move {
                let mut chunk = [0u8; 4096];
                loop {
                    match tokio::io::AsyncReadExt::read(&mut stream, &mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => sink
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .extend_from_slice(&chunk[..n]),
                    }
                }
            }))
        })
        .collect::<Vec<_>>();
        let status = match tokio::time::timeout(limit, child.wait()).await {
            Ok(status) => {
                Some(status.map_err(|e| CliRuntimeError::new("cli_process_wait", e.to_string()))?)
            }
            Err(_) => {
                let _ = child.cancel().await;
                None
            }
        };
        // Pipes close when the child (or its killed tree) exits; bound the drain regardless.
        for reader in readers {
            let abort = reader.abort_handle();
            if tokio::time::timeout(std::time::Duration::from_secs(2), reader)
                .await
                .is_err()
            {
                abort.abort();
            }
        }
        let take = |buf: &Arc<Mutex<Vec<u8>>>| {
            std::mem::take(&mut *buf.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
        };
        Ok(BoundedCapture {
            status,
            stdout: take(&stdout_buf),
            stderr: take(&stderr_buf),
        })
    }

    pub fn spawn_with_native_adapter(
        launch: &CliLaunchSpec,
        generation: u64,
    ) -> Result<StartedCliProcess, CliRuntimeError> {
        let context = ProcessLaunchContext {
            workspace_root: launch.cwd.to_string_lossy().into_owned(),
            sandbox_profile: Some("vendor-cli".to_string()),
            sandbox_required: true,
            network_required: true,
        };
        Self::spawn_with_adapter(
            launch,
            generation,
            Arc::new(rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter),
            context,
        )
    }

    /// Adapter-backed constructor. A dedicated runtime thread bridges async
    /// managed streams to synchronous CliProcessIo framing.
    pub fn spawn_with_adapter(
        launch: &CliLaunchSpec,
        generation: u64,
        adapter: Arc<dyn ProcessLaunchAdapter>,
        context: ProcessLaunchContext,
    ) -> Result<StartedCliProcess, CliRuntimeError> {
        let (requests, incoming) = mpsc::channel::<BridgeRequest>();
        let (outgoing, responses) = mpsc::channel::<BridgeResponse>();
        let (program, argv) = sandboxed_program(launch)?;
        let spec = CommandLaunchSpec {
            program: program.to_string_lossy().into_owned(),
            argv,
            cwd: launch.cwd.to_string_lossy().into_owned(),
            context,
            env: launch.env.clone(),
        };
        let _thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            let Ok(runtime) = runtime else {
                let _ = outgoing.send(BridgeResponse::Error("runtime unavailable".into()));
                return;
            };
            runtime.block_on(async move {
                let mut child = match adapter.spawn(spec).await {
                    Ok(child) => child,
                    Err(error) => {
                        let _ = outgoing.send(BridgeResponse::Error(error.to_string()));
                        return;
                    }
                };
                let pid = child.pid();
                let mut stdin = child.take_stdin();
                let stdout = child.take_stdout();
                // Native adapters expose stderr as a pipe. Drain it while the
                // synchronous bridge waits for protocol requests, otherwise a
                // verbose vendor CLI can block before stdout makes progress.
                if let Some(stderr) = child.take_stderr() {
                    tokio::spawn(async move {
                        // Keep the vendor CLI's own diagnostics: each line (bounded) goes
                        // to the daemon log, so a driver that dies says why.
                        use tokio::io::AsyncBufReadExt;
                        let mut lines = tokio::io::BufReader::new(stderr).lines();
                        let mut logged = 0usize;
                        while let Ok(Some(line)) = lines.next_line().await {
                            if logged < 200 {
                                let line: String = line.chars().take(500).collect();
                                tracing::warn!(target: "driver_stderr", %line, "driver stderr");
                                logged += 1;
                            }
                        }
                    });
                }
                let mut lines = stdout
                    .map(tokio::io::BufReader::new)
                    .map(tokio::io::AsyncBufReadExt::lines);
                let _ = outgoing.send(BridgeResponse::Pid(pid));
                let mut cancelled = false;
                while let Ok(request) = incoming.recv() {
                    match request {
                        BridgeRequest::Write(line) => {
                            let result = async {
                                use tokio::io::AsyncWriteExt;
                                let writer = stdin.as_mut().ok_or("stdin closed")?;
                                writer
                                    .write_all(line.as_bytes())
                                    .await
                                    .map_err(|e| e.to_string())?;
                                writer.write_all(b"\n").await.map_err(|e| e.to_string())?;
                                writer.flush().await.map_err(|e| e.to_string())
                            }
                            .await;
                            let _ = outgoing.send(
                                result.map_or_else(BridgeResponse::Error, |_| BridgeResponse::Unit),
                            );
                        }
                        BridgeRequest::Read => {
                            let result = match lines.as_mut() {
                                Some(lines) => lines.next_line().await.map_err(|e| e.to_string()),
                                None => Ok(None),
                            };
                            let _ =
                                outgoing.send(result.map_or_else(BridgeResponse::Error, |line| {
                                    BridgeResponse::Line(line)
                                }));
                        }
                        BridgeRequest::Cancel => {
                            let _ = child.cancel().await;
                            let _ = child.wait().await;
                            let _ = outgoing.send(BridgeResponse::Unit);
                            cancelled = true;
                            break;
                        }
                    }
                }
                if !cancelled {
                    let _ = child.cancel().await;
                    let _ = child.wait().await;
                }
            });
        });
        let pid = match responses
            .recv()
            .map_err(|_| CliRuntimeError::new("cli_process_spawn", "bridge failed"))?
        {
            BridgeResponse::Pid(pid) => pid.unwrap_or(0),
            BridgeResponse::Error(error) => {
                return Err(CliRuntimeError::new("cli_process_spawn", error))
            }
            _ => {
                return Err(CliRuntimeError::new(
                    "cli_process_spawn",
                    "bridge protocol failed",
                ))
            }
        };
        let io = AdapterProcessIo {
            requests,
            responses: Arc::new(Mutex::new(responses)),
            pid,
            _thread: Arc::new(Mutex::new(Some(pid))),
        };
        Ok(StartedCliProcess {
            pid,
            generation,
            inner: Box::new(io),
        })
    }

    pub fn spawn(
        launch: &CliLaunchSpec,
        generation: u64,
    ) -> Result<StartedCliProcess, CliRuntimeError> {
        let (program, argv) = sandboxed_program(launch)?;
        let mut command = rightkit_child_process::command_std(program);
        command
            .args(argv)
            .current_dir(&launch.cwd)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        apply_launch_env(&mut command, launch);
        // agy's detached auto-updater spawns a console grandchild that our
        // CREATE_NO_WINDOW cannot reach; harmless for every other vendor.
        command.env(
            crate::driver_host::vendor_cli::AGY_DISABLE_AUTO_UPDATE_ENV,
            "1",
        );
        configure_sandbox(&mut command, launch)?;
        // Unix: RightKit's OwnedCommand makes the child lead its own process group and owns the
        // tree (terminate_tree). Windows: the std child is assigned to the Job Object below.
        #[cfg(not(windows))]
        let spawned = rightkit_child_process::OwnedCommand::from_command(command).spawn();
        #[cfg(windows)]
        let spawned = command.spawn();
        let mut child = spawned.map_err(|error| {
            CliRuntimeError::new(
                "cli_process_spawn",
                format!("failed to start {}: {error}", launch.executable.display()),
            )
        })?;
        let pid = child.id();
        #[cfg(windows)]
        let windows_job = {
            let job = rightkit_agent_tools::coding_tools::sandbox::shell_job()
                .map_err(|error| CliRuntimeError::new("cli_process_job", error.to_string()))?;
            job.assign_process_id(pid)
                .map_err(|error| CliRuntimeError::new("cli_process_job", error.to_string()))?;
            Arc::new(Mutex::new(Some(job)))
        };
        #[cfg(windows)]
        let (child_stdin, child_stdout) = (child.stdin.take(), child.stdout.take());
        #[cfg(not(windows))]
        let (child_stdin, child_stdout) = (child.take_stdin(), child.take_stdout());
        let stdin = child_stdin.ok_or_else(|| {
            CliRuntimeError::new("cli_process_stdin", "child did not expose stdin")
        })?;
        let stdout = child_stdout.ok_or_else(|| {
            CliRuntimeError::new("cli_process_stdout", "child did not expose stdout")
        })?;
        Ok(StartedCliProcess {
            pid,
            generation,
            inner: Box::new(Self {
                #[cfg(not(windows))]
                child: Arc::new(Mutex::new(child)),
                stdin: Arc::new(Mutex::new(stdin)),
                stdout: Arc::new(Mutex::new(BufReader::new(stdout))),
                #[cfg(windows)]
                windows_job,
            }),
        })
    }
}

enum BridgeRequest {
    Write(String),
    Read,
    Cancel,
}
enum BridgeResponse {
    Pid(Option<u32>),
    Unit,
    Line(Option<String>),
    Error(String),
}

#[derive(Clone)]
struct AdapterProcessIo {
    requests: Sender<BridgeRequest>,
    responses: Arc<Mutex<Receiver<BridgeResponse>>>,
    pid: u32,
    _thread: Arc<Mutex<Option<u32>>>,
}

impl CliProcessIo for AdapterProcessIo {
    fn write_line(&self, line: &str) -> Result<(), CliRuntimeError> {
        self.requests
            .send(BridgeRequest::Write(line.to_string()))
            .map_err(|e| CliRuntimeError::new("cli_process_write", e.to_string()))?;
        match self
            .responses
            .lock()
            .unwrap()
            .recv()
            .map_err(|e| CliRuntimeError::new("cli_process_write", e.to_string()))?
        {
            BridgeResponse::Unit => Ok(()),
            BridgeResponse::Error(e) => Err(CliRuntimeError::new("cli_process_write", e)),
            _ => Err(CliRuntimeError::new(
                "cli_process_write",
                "bridge protocol failed",
            )),
        }
    }
    fn read_event(&self) -> Result<Option<Value>, CliRuntimeError> {
        self.requests
            .send(BridgeRequest::Read)
            .map_err(|e| CliRuntimeError::new("cli_process_read", e.to_string()))?;
        match self
            .responses
            .lock()
            .unwrap()
            .recv()
            .map_err(|e| CliRuntimeError::new("cli_process_read", e.to_string()))?
        {
            BridgeResponse::Line(None) => Ok(None),
            BridgeResponse::Line(Some(line)) => serde_json::from_str(line.trim())
                .map(Some)
                .map_err(|e| CliRuntimeError::new("cli_process_invalid_json", e.to_string())),
            BridgeResponse::Error(e) => Err(CliRuntimeError::new("cli_process_read", e)),
            _ => Err(CliRuntimeError::new(
                "cli_process_read",
                "bridge protocol failed",
            )),
        }
    }
    fn shutdown(&self) -> Result<(), CliRuntimeError> {
        self.requests
            .send(BridgeRequest::Cancel)
            .map_err(|e| CliRuntimeError::new("cli_process_shutdown", e.to_string()))?;
        Ok(())
    }
    fn clone_box(&self) -> Box<dyn CliProcessIo> {
        Box::new(self.clone())
    }
}

impl CliProcessIo for StdioProcessIo {
    fn write_line(&self, line: &str) -> Result<(), CliRuntimeError> {
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| CliRuntimeError::new("cli_process_lock", "stdin lock poisoned"))?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|error| CliRuntimeError::new("cli_process_write", error.to_string()))
    }

    fn read_event(&self) -> Result<Option<Value>, CliRuntimeError> {
        let mut stdout = self
            .stdout
            .lock()
            .map_err(|_| CliRuntimeError::new("cli_process_lock", "stdout lock poisoned"))?;
        // Vendor CLIs are trusted and emit large frames, so the line is uncapped.
        let mut buffer = Vec::new();
        rightkit_sidecar::read_json_line::<_, Value>(
            &mut *stdout,
            &mut buffer,
            rightkit_sidecar::UNCAPPED,
        )
        .map_err(|error| match error {
            rightkit_sidecar::FrameError::Io(error) => {
                CliRuntimeError::new("cli_process_read", error.to_string())
            }
            error => CliRuntimeError::new("cli_process_invalid_json", error.to_string()),
        })
    }

    fn shutdown(&self) -> Result<(), CliRuntimeError> {
        #[cfg(windows)]
        {
            self.windows_job
                .lock()
                .map_err(|_| CliRuntimeError::new("cli_process_lock", "job lock poisoned"))?
                .take();
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let mut child = self
                .child
                .lock()
                .map_err(|_| CliRuntimeError::new("cli_process_lock", "child lock poisoned"))?;
            child
                .terminate_tree()
                .map(|_| ())
                .map_err(|error| CliRuntimeError::new("cli_process_shutdown", error.to_string()))
        }
    }

    fn clone_box(&self) -> Box<dyn CliProcessIo> {
        Box::new(self.clone())
    }
}

/// Credential variables that outrank a managed Claude profile login (#344).
const CLAUDE_GUARDED_CREDENTIAL_ENV: [&str; 3] = [
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
];

/// Credential precedence guard (#344). Under a managed profile (the launch pins the vendor home)
/// the profile login must be the only credential in play, so the vendor's ambient credential
/// variables are removed from the child. Claude: always. Codex: `OPENAI_API_KEY` only when the
/// launch does not set it itself (an API-key account sets its own; a subscription must not
/// inherit one).
pub(crate) fn credential_guard_names(launch: &CliLaunchSpec) -> Vec<&'static str> {
    let pins = |name: &str| launch.env.iter().any(|(key, _)| key == name);
    let mut guarded = Vec::new();
    if pins("CLAUDE_CONFIG_DIR") {
        guarded.extend(CLAUDE_GUARDED_CREDENTIAL_ENV);
    }
    if pins("CODEX_HOME") && !pins("OPENAI_API_KEY") {
        guarded.push("OPENAI_API_KEY");
    }
    guarded
}

/// Build the child environment: keep list, explicit removals, the credential guard, then the
/// launch's own variables. Guarded names are also dropped from `launch.env` (a managed Claude
/// profile never gets an API key or token beside it).
fn apply_launch_env(command: &mut Command, launch: &CliLaunchSpec) {
    for name in &launch.env_keep {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    for name in &launch.env_remove {
        command.env_remove(name);
    }
    let guarded = credential_guard_names(launch);
    for name in &guarded {
        command.env_remove(name);
    }
    for (name, value) in &launch.env {
        if guarded.contains(&name.as_str()) {
            continue;
        }
        command.env(name, value);
    }
}

/// A managed profile is mandatory: sandboxing a vendor process around an arbitrary
/// host home would merely make host state reachable through another path.
#[cfg(unix)]
fn managed_home(launch: &CliLaunchSpec) -> Result<PathBuf, CliRuntimeError> {
    for name in [
        "HOME",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "COMMANDCODE_HOME",
    ] {
        if let Some((_, value)) = launch.env.iter().find(|(key, _)| key == name) {
            let path = PathBuf::from(value);
            if path.is_absolute() {
                return Ok(path);
            }
            return Err(CliRuntimeError::new(
                "cli_process_managed_home",
                "managed CLI home must be absolute",
            ));
        }
    }
    if launch.cwd.is_absolute() {
        return Ok(launch.cwd.clone());
    }
    Err(CliRuntimeError::new(
        "cli_process_managed_home",
        "sandboxed CLI launch requires an absolute managed home or cwd",
    ))
}

fn sandboxed_program(launch: &CliLaunchSpec) -> Result<(PathBuf, Vec<String>), CliRuntimeError> {
    let argv = launch
        .argv
        .first()
        .filter(|arg| Path::new(arg) == launch.executable.as_path())
        .map(|_| launch.argv[1..].to_vec())
        .unwrap_or_else(|| launch.argv.clone());
    // macOS no longer wraps in /usr/bin/sandbox-exec: the profile is applied
    // in-process from pre_exec (see configure_sandbox), which
    // avoids the deprecated CLI, an extra process, and leaking the profile via
    // argv. The executable is spawned directly on every platform.
    Ok((launch.executable.clone(), argv))
}

/// Confine the driver process (Linux Landlock, macOS Seatbelt, prepared in the parent by
/// RightKit and applied in the child before exec). Fail closed: no confinement, no launch.
/// Vendor CLIs talk to their own provider, so egress stays allowed. The process group is set up
/// by `OwnedCommand` at spawn (after this hook; neither backend restricts `setpgid`).
fn configure_sandbox(
    _command: &mut Command,
    _launch: &CliLaunchSpec,
) -> Result<(), CliRuntimeError> {
    #[cfg(unix)]
    {
        let command = _command;
        let launch = _launch;
        let home = managed_home(launch)?;
        rightkit_agent_tools::coding_tools::sandbox::confine_driver_command(
            command,
            &launch.executable,
            &launch.cwd,
            true,
            &home,
        )
        .map_err(|error| {
            CliRuntimeError::new(
                "cli_process_sandbox",
                format!("driver sandbox unavailable; refusing unsandboxed launch: {error}"),
            )
        })?;
    }
    Ok(())
}
