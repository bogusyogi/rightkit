//! Daemon-side process supervision for external reasoning drivers.
//!
//! RightKit remains the capability and approval authority. This module only owns vendor process
//! lifecycle and wire normalization; callers persist bindings/events and execute capabilities.

use async_trait::async_trait;
use rightkit_agents::driver_host::cli_runtime::{
    CliPrompt, CliProviderAdapter, CliTransport, CreateVendorSession, LoadVendorSession,
    VendorSessionHandle, VendorTurnHandle,
};
use rightkit_agents::driver_host::{
    parse_claude_stream_events, parse_codex_jsonrpc_line, ClaudeAccountIdentity, CodexMessage,
    DriverCursor, DriverKind, LaunchSpec, NormalizedEvent, NormalizedEventKind, RUN_SCOPED_POLICY,
};
use rightkit_sandbox::execution::ProcessLaunchAdapter;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch, Mutex};

pub(crate) async fn capture_login_output(
    program: &Path,
    argv: Vec<String>,
    cwd: &Path,
) -> Result<(i32, Vec<u8>, Vec<u8>), String> {
    rightkit_agents::driver_host::cli_runtime::transports::process::StdioProcessIo::capture_with_native_adapter(
        program, argv, cwd, vec![("HOME".into(), cwd.to_string_lossy().into_owned())]
    ).await.map_err(|error| error.to_string())
}

const EVENT_CHANNEL_CAPACITY: usize = 128;
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const DRIVER_READY_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const CODEX_INITIALIZE_ID: u64 = 1;

#[derive(Debug, thiserror::Error)]
pub enum DriverRuntimeError {
    #[error("driver binding '{0}' is already running")]
    AlreadyRunning(String),
    #[error("driver binding '{0}' is not running")]
    NotRunning(String),
    #[error("failed to spawn driver: {0}")]
    Spawn(String),
    #[error("failed to capture driver version: {0}")]
    Version(String),
    #[error("driver stdin is closed")]
    StdinClosed,
    #[error("driver write failed: {0}")]
    Write(String),
    #[error("driver did not initialize")]
    NotReady,
    #[error("operation is unsupported for {0:?}")]
    Unsupported(DriverKind),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticStream {
    Stderr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DriverRuntimeEvent {
    Normalized(Box<NormalizedEvent>),
    ServerRequest {
        id: Value,
        method: String,
        params: Value,
    },
    Diagnostic {
        stream: DiagnosticStream,
        message: String,
    },
    ProtocolError {
        message: String,
    },
    Exited {
        success: bool,
        code: Option<i32>,
    },
}

pub type DriverEventReceiver = mpsc::Receiver<DriverRuntimeEvent>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverProcessInfo {
    pub binding_id: String,
    pub driver: DriverKind,
    pub pid: Option<u32>,
    pub binary_version: String,
}

pub struct StartedDriver {
    pub info: DriverProcessInfo,
    pub events: DriverEventReceiver,
}

#[derive(Clone)]
pub struct DriverHostRuntime {
    processes: Arc<Mutex<HashMap<String, Arc<ManagedDriver>>>>,
    next_request_id: Arc<AtomicU64>,
    process_launch_adapter: Arc<dyn ProcessLaunchAdapter>,
    process_launch_context: rightkit_sandbox::execution::ProcessLaunchContext,
}

type StdinHandle = Arc<Mutex<Option<std::pin::Pin<Box<dyn AsyncWrite + Send>>>>>;

struct ManagedDriver {
    kind: DriverKind,
    child: Arc<Mutex<Box<dyn rightkit_sandbox::execution::ManagedChild>>>,
    stdin: StdinHandle,
    ready: watch::Receiver<bool>,
    #[cfg(windows)]
    _windows_job: rightkit_agent_tools::coding_tools::sandbox::WindowsJob,
}

impl Default for DriverHostRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl DriverHostRuntime {
    pub fn new() -> Self {
        Self {
            processes: Arc::new(Mutex::new(HashMap::new())),
            next_request_id: Arc::new(AtomicU64::new(CODEX_INITIALIZE_ID + 1)),
            process_launch_adapter: default_process_launch_adapter(),
            process_launch_context: rightkit_sandbox::execution::ProcessLaunchContext {
                workspace_root: String::new(),
                sandbox_profile: Some("driver".into()),
                sandbox_required: false,
                network_required: false,
            },
        }
    }

    pub fn with_process_launch(
        mut self,
        adapter: Arc<dyn ProcessLaunchAdapter>,
        context: rightkit_sandbox::execution::ProcessLaunchContext,
    ) -> Self {
        self.process_launch_adapter = adapter;
        self.process_launch_context = context;
        self
    }

    /// Start a driver from an already validated launch spec. Public so tests and future adapters
    /// can reuse the same supervisor without adding vendor conditionals to the daemon API.
    pub async fn start_with_spec(
        &self,
        binding_id: impl Into<String>,
        kind: DriverKind,
        spec: LaunchSpec,
        cursor: DriverCursor,
    ) -> Result<StartedDriver, DriverRuntimeError> {
        self.start_with_spec_for_account(binding_id, kind, spec, cursor, None)
            .await
    }

    /// `start_with_spec` plus the non-secret identity of the Claude account the spec was built
    /// for, stamped onto that driver's rate-limit and cost events.
    pub async fn start_with_spec_for_account(
        &self,
        binding_id: impl Into<String>,
        kind: DriverKind,
        spec: LaunchSpec,
        cursor: DriverCursor,
        account: Option<ClaudeAccountIdentity>,
    ) -> Result<StartedDriver, DriverRuntimeError> {
        let binding_id = binding_id.into();
        if self.processes.lock().await.contains_key(&binding_id) {
            return Err(DriverRuntimeError::AlreadyRunning(binding_id));
        }

        let binary_version = capture_version(&spec, self.process_launch_adapter.clone()).await?;
        let secret_values = spec
            .env
            .values()
            .filter(|value| !value.is_empty())
            .cloned()
            .collect::<Vec<_>>();
        // The execution launch contract carries only name/value pairs applied
        // on top of the daemon's inherited environment, so an ambient name
        // cannot be unset on the child from here. Pin every requested removal
        // to an empty value instead: the ambient secret never reaches the
        // vendor subprocess, and explicit `spec.env` entries still win.
        let mut launch_env = spec.env.clone();
        for name in &spec.env_remove {
            if !name.is_empty() && !name.contains('=') {
                launch_env.entry(name.clone()).or_default();
            }
        }
        let launch = rightkit_sandbox::execution::CommandLaunchSpec {
            program: spec.program.to_string_lossy().into_owned(),
            argv: spec.args.clone(),
            cwd: spec.cwd.to_string_lossy().into_owned(),
            context: rightkit_sandbox::execution::ProcessLaunchContext {
                workspace_root: spec.cwd.to_string_lossy().into_owned(),
                sandbox_profile: Some("driver".into()),
                sandbox_required: true,
                network_required: false,
            },
            env: launch_env.into_iter().collect(),
        };
        let mut child = self
            .process_launch_adapter
            .spawn(launch)
            .await
            .map_err(|error| DriverRuntimeError::Spawn(error.to_string()))?;
        let pid = child.pid();
        #[cfg(windows)]
        let windows_job = {
            let job =
                rightkit_agent_tools::coding_tools::sandbox::shell_job().map_err(|error| {
                    DriverRuntimeError::Spawn(format!("Windows Job Object: {error}"))
                })?;
            let pid = pid
                .ok_or_else(|| DriverRuntimeError::Spawn("child process id unavailable".into()))?;
            job.assign_process_id(pid).map_err(|error| {
                DriverRuntimeError::Spawn(format!("Windows Job Object assignment: {error}"))
            })?;
            job
        };
        let stdin = child.take_stdin().ok_or(DriverRuntimeError::StdinClosed)?;
        let stdout = child
            .take_stdout()
            .ok_or_else(|| DriverRuntimeError::Spawn("stdout pipe unavailable".into()))?;
        let stderr = child
            .take_stderr()
            .ok_or_else(|| DriverRuntimeError::Spawn("stderr pipe unavailable".into()))?;
        let child = Arc::new(Mutex::new(child));
        let stdin = Arc::new(Mutex::new(Some(stdin)));
        let (event_tx, event_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let (ready_tx, ready_rx) = watch::channel(false);
        let managed = Arc::new(ManagedDriver {
            kind,
            child: child.clone(),
            stdin: stdin.clone(),
            ready: ready_rx,
            #[cfg(windows)]
            _windows_job: windows_job,
        });

        {
            let mut processes = self.processes.lock().await;
            if processes.contains_key(&binding_id) {
                let mut child = child.lock().await;
                let _ = child.cancel().await;
                return Err(DriverRuntimeError::AlreadyRunning(binding_id));
            }
            processes.insert(binding_id.clone(), managed);
        }

        spawn_stdout_reader(StdoutReader {
            kind,
            stdout,
            initial_offset: cursor.output_offset,
            events: event_tx.clone(),
            stdin: stdin.clone(),
            child: child.clone(),
            ready: ready_tx,
            account,
        });
        spawn_stderr_reader(stderr, event_tx.clone(), secret_values);
        spawn_exit_monitor(
            binding_id.clone(),
            child.clone(),
            event_tx,
            self.processes.clone(),
        );

        if kind == DriverKind::Codex {
            if let Err(error) = write_json(
                &stdin,
                &json!({
                    "id": CODEX_INITIALIZE_ID,
                    "method": "initialize",
                    "params": {
                        "clientInfo": {
                            "name": "coderight",
                            "title": "RightKit",
                            "version": env!("CARGO_PKG_VERSION")
                        },
                        "capabilities": { "experimentalApi": false }
                    }
                }),
            )
            .await
            {
                self.processes.lock().await.remove(&binding_id);
                let mut child = child.lock().await;
                let _ = child.cancel().await;
                return Err(error);
            }
        }

        Ok(StartedDriver {
            info: DriverProcessInfo {
                binding_id,
                driver: kind,
                pid,
                binary_version,
            },
            events: event_rx,
        })
    }

    pub async fn send_prompt(
        &self,
        binding_id: &str,
        vendor_session_id: &str,
        text: &str,
    ) -> Result<u64, DriverRuntimeError> {
        let process = self.process(binding_id).await?;
        if process.kind == DriverKind::Codex {
            wait_ready(&process.ready).await?;
        }
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let payload = match process.kind {
            DriverKind::Codex => json!({
                "id": request_id,
                "method": "turn/start",
                "params": {
                    "threadId": vendor_session_id,
                    "input": [{ "type": "text", "text": text }]
                }
            }),
            // Antigravity runs per-turn outside this supervisor (agy_harness
            // launch); the Claude-shaped frame is kept only so the shared
            // match stays exhaustive.
            DriverKind::Claude | DriverKind::Antigravity => {
                let mut payload = json!({
                    "type": "user",
                    "message": {
                        "role": "user",
                        "content": [{ "type": "text", "text": text }]
                    }
                });
                if !vendor_session_id.is_empty() {
                    payload["session_id"] = Value::String(vendor_session_id.to_string());
                }
                payload
            }
        };
        write_json(&process.stdin, &payload).await?;
        Ok(request_id)
    }

    /// Start a Codex thread whose native execution surface is read-only. Mutating effects and
    /// approval decisions remain exclusively available through RightKit's session gateway.
    pub async fn start_codex_thread(
        &self,
        binding_id: &str,
        cwd: impl AsRef<Path>,
    ) -> Result<u64, DriverRuntimeError> {
        let process = self.process(binding_id).await?;
        wait_ready(&process.ready).await?;
        if process.kind != DriverKind::Codex {
            return Err(DriverRuntimeError::Unsupported(process.kind));
        }
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        write_json(
            &process.stdin,
            &json!({
                "id": request_id,
                "method": "thread/start",
                "params": {
                    "cwd": cwd.as_ref(),
                    "sandbox": "read-only",
                    "approvalPolicy": "never",
                    "developerInstructions": RUN_SCOPED_POLICY
                }
            }),
        )
        .await?;
        Ok(request_id)
    }

    /// Answer a Codex JSON-RPC server request after RightKit has made the approval decision.
    pub async fn respond_server_request(
        &self,
        binding_id: &str,
        id: Value,
        result: Value,
    ) -> Result<(), DriverRuntimeError> {
        let process = self.process(binding_id).await?;
        if process.kind == DriverKind::Claude {
            // #336: `id` is the control request id and `result` the daemon's answer. Only an
            // explicit allow reaches the CLI as allow; anything else (including the default
            // decline) is a deny. The original input is not retained here, so an allow must carry
            // its own `updatedInput` object; an allow without one is refused rather than run the
            // tool with empty arguments.
            let mut decision = rightkit_agents::driver_host::ClaudePermissionDecision::from_answer(
                &result,
                &json!({}),
            );
            if matches!(
                decision,
                rightkit_agents::driver_host::ClaudePermissionDecision::Allow { .. }
            ) && !result.get("updatedInput").is_some_and(Value::is_object)
            {
                decision = rightkit_agents::driver_host::ClaudePermissionDecision::deny(
                    "Approval answer carried no updatedInput; denied by RightKit",
                );
            }
            let request_id = id.as_str().unwrap_or_default();
            if request_id.is_empty() {
                return Err(DriverRuntimeError::Write(
                    "Claude control response needs a string request id".into(),
                ));
            }
            return write_json(
                &process.stdin,
                &rightkit_agents::driver_host::claude_permission_response_frame(
                    request_id, &decision,
                ),
            )
            .await;
        }
        wait_ready(&process.ready).await?;
        if process.kind != DriverKind::Codex {
            return Err(DriverRuntimeError::Unsupported(process.kind));
        }
        write_json(&process.stdin, &json!({ "id": id, "result": result })).await
    }

    pub async fn resume(
        &self,
        binding_id: &str,
        vendor_session_id: &str,
    ) -> Result<u64, DriverRuntimeError> {
        let process = self.process(binding_id).await?;
        wait_ready(&process.ready).await?;
        if process.kind != DriverKind::Codex {
            return Err(DriverRuntimeError::Unsupported(process.kind));
        }
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        write_json(
            &process.stdin,
            &json!({
                "id": request_id,
                "method": "thread/resume",
                "params": {
                    "threadId": vendor_session_id,
                    "developerInstructions": RUN_SCOPED_POLICY
                }
            }),
        )
        .await?;
        Ok(request_id)
    }

    pub async fn interrupt(
        &self,
        binding_id: &str,
        vendor_session_id: &str,
        vendor_turn_id: Option<&str>,
    ) -> Result<u64, DriverRuntimeError> {
        let process = self.process(binding_id).await?;
        wait_ready(&process.ready).await?;
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let payload = match process.kind {
            DriverKind::Codex => json!({
                "id": request_id,
                "method": "turn/interrupt",
                "params": {
                    "threadId": vendor_session_id,
                    "turnId": vendor_turn_id.ok_or_else(|| {
                        DriverRuntimeError::Write("Codex interrupt requires turn id".into())
                    })?
                }
            }),
            DriverKind::Claude | DriverKind::Antigravity => json!({
                "type": "control_request",
                "request_id": format!("interrupt-{request_id}"),
                "request": { "subtype": "interrupt" }
            }),
        };
        write_json(&process.stdin, &payload).await?;
        Ok(request_id)
    }

    pub async fn shutdown(&self, binding_id: &str) -> Result<(), DriverRuntimeError> {
        let process = self
            .processes
            .lock()
            .await
            .remove(binding_id)
            .ok_or_else(|| DriverRuntimeError::NotRunning(binding_id.into()))?;
        process.stdin.lock().await.take();
        if matches!(
            tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
                process.child.lock().await.wait().await
            })
            .await,
            Ok(Ok(_))
        ) {
            return Ok(());
        }
        // The monitor releases this lock between bounded wait attempts.
        // Cancellation must be followed by reap before releasing ownership.
        tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
            let mut child = process.child.lock().await;
            child.cancel().await?;
            child.wait().await
        })
        .await
        .map_err(|_| DriverRuntimeError::Write("driver shutdown/reap timed out".into()))?
        .map(|_| ())
        .map_err(|error| DriverRuntimeError::Write(error.to_string()))
    }

    pub async fn contains(&self, binding_id: &str) -> bool {
        self.processes.lock().await.contains_key(binding_id)
    }

    async fn process(&self, binding_id: &str) -> Result<Arc<ManagedDriver>, DriverRuntimeError> {
        self.processes
            .lock()
            .await
            .get(binding_id)
            .cloned()
            .ok_or_else(|| DriverRuntimeError::NotRunning(binding_id.into()))
    }
}

impl Drop for DriverHostRuntime {
    fn drop(&mut self) {
        // The final runtime owner actively terminates every child; `kill_on_drop(true)` remains
        // the backstop after task-held references drain. Cloned handles leave the map intact.
        if Arc::strong_count(&self.processes) == 1 {
            if let Ok(mut processes) = self.processes.try_lock() {
                for process in processes.values() {
                    if let Ok(mut child) = process.child.try_lock() {
                        let _ = child.start_kill();
                    }
                }
                processes.clear();
            }
        }
    }
}

/// Daemon implementation of the driver-host crate's narrow CLI bridge.
///
/// This translates an adapter-produced launch spec into the supervised process
/// launch contract. It deliberately maps only legacy wire kinds here: providers
/// whose transports are not supported by this supervisor fail at bridge lookup
/// instead of falling through to a different vendor runtime.
#[derive(Clone)]
pub struct DaemonCliDriverBridge {
    runtime: DriverHostRuntime,
    registry: Arc<rightkit_agents::driver_host::cli_runtime::CliAdapterRegistry>,
    generic: Arc<Mutex<HashMap<String, GenericCliSession>>>,
}

struct GenericCliSession {
    transport: Arc<dyn CliTransport>,
    adapter: Arc<dyn CliProviderAdapter>,
    spec: rightkit_agents::driver_host::cli_runtime::CliLaunchSpec,
    session: VendorSessionHandle,
    turn: Option<VendorTurnHandle>,
    events: mpsc::Sender<NormalizedEvent>,
}

impl DaemonCliDriverBridge {
    pub fn new(
        runtime: DriverHostRuntime,
        registry: Arc<rightkit_agents::driver_host::cli_runtime::CliAdapterRegistry>,
    ) -> Self {
        Self {
            runtime,
            registry,
            generic: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn contains(&self, binding_id: &str) -> bool {
        self.generic.lock().await.contains_key(binding_id)
            || self.runtime.contains(binding_id).await
    }

    async fn start_generic(
        &self,
        binding_id: &str,
        mut spec: rightkit_agents::driver_host::cli_runtime::CliLaunchSpec,
    ) -> Result<
        rightkit_agents::driver_host::cli_runtime::StartedBridgeSession,
        rightkit_agents::driver_host::cli_runtime::BridgeError,
    > {
        let adapter = self.registry.adapter(&spec.provider_id).ok_or_else(|| {
            rightkit_agents::driver_host::cli_runtime::BridgeError::new(
                "adapter_not_registered",
                "generic provider adapter is not registered",
            )
        })?;
        let transport = self.registry.transport(spec.transport).ok_or_else(|| {
            rightkit_agents::driver_host::cli_runtime::BridgeError::new(
                "transport_not_registered",
                "generic provider transport is not registered",
            )
        })?;
        let model_id = spec
            .env
            .iter()
            .find(|(name, _)| name == "RIGHTKIT_INTERNAL_MODEL_ID")
            .map(|(_, value)| value.clone());
        spec.env
            .retain(|(name, _)| name != "RIGHTKIT_INTERNAL_MODEL_ID");
        let process = transport
            .start(&spec)
            .await
            .map_err(cli_transport_bridge_error)?;
        let session = if let Some(vendor_session_id) = spec.session_id.strip_prefix("resume:") {
            transport
                .load_session(
                    &process,
                    LoadVendorSession {
                        vendor_session_id: vendor_session_id.to_string(),
                        model_id: model_id.clone(),
                        mcp_server: spec.mcp_server.clone(),
                    },
                )
                .await
                .map_err(cli_transport_bridge_error)?
        } else {
            transport
                .create_session(
                    &process,
                    CreateVendorSession {
                        model_id: model_id.clone(),
                        mcp_server: spec.mcp_server.clone(),
                        system_policy: RUN_SCOPED_POLICY.to_string(),
                    },
                )
                .await
                .map_err(cli_transport_bridge_error)?
        };
        let vendor_session_id = session.vendor_session_id.clone();
        let (events, receiver) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        self.generic.lock().await.insert(
            binding_id.to_string(),
            GenericCliSession {
                transport,
                adapter,
                spec,
                session,
                turn: None,
                events,
            },
        );
        Ok(
            rightkit_agents::driver_host::cli_runtime::StartedBridgeSession {
                vendor_session_id,
                model_id,
                events: receiver,
            },
        )
    }

    async fn prompt_generic(
        &self,
        binding_id: &str,
        text: &str,
    ) -> Result<(), rightkit_agents::driver_host::cli_runtime::BridgeError> {
        let mut sessions = self.generic.lock().await;
        let state = sessions.get_mut(binding_id).ok_or_else(|| {
            rightkit_agents::driver_host::cli_runtime::BridgeError::new(
                "binding_not_running",
                "generic binding is not running",
            )
        })?;
        let turn = state
            .transport
            .prompt(
                &state.session,
                CliPrompt {
                    text: text.to_string(),
                    tool_ceiling: Vec::new(),
                    model_id: state.session.model_id.clone(),
                },
            )
            .await
            .map_err(cli_transport_bridge_error)?;
        state.session = clone_vendor_session(&turn.session);
        state.turn = Some(turn);
        // Transport calls return only after their protocol request is accepted.
        // The raw process reader stays daemon-owned and normalizes each event
        // through its provider adapter before the executor folds it.
        let transport = state.transport.clone();
        let session = clone_vendor_session(&state.session);
        let adapter = state.adapter.clone();
        let events = state.events.clone();
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || loop {
            match runtime.block_on(transport.read_event(&session)) {
                Ok(Some(raw)) => match adapter.normalize_provider_event(raw) {
                    Ok(Some(event)) => {
                        let terminal = matches!(
                            event.kind,
                            NormalizedEventKind::TurnCompleted
                                | NormalizedEventKind::TurnInterrupted
                                | NormalizedEventKind::TurnFailed { .. }
                        );
                        if events.blocking_send(event).is_err() || terminal {
                            break;
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let _ = events.blocking_send(generic_failure_event(error.to_string()));
                        break;
                    }
                },
                Ok(None) => {
                    let _ = events.blocking_send(generic_failure_event(
                        "vendor event stream closed".to_string(),
                    ));
                    break;
                }
                Err(error) => {
                    let _ = events.blocking_send(generic_failure_event(error.message));
                    break;
                }
            }
        });
        Ok(())
    }
}

#[async_trait]
impl rightkit_agents::driver_host::cli_runtime::CliDriverHostBridge for DaemonCliDriverBridge {
    async fn start(
        &self,
        binding_id: &str,
        spec: rightkit_agents::driver_host::cli_runtime::CliLaunchSpec,
        cursor: DriverCursor,
    ) -> Result<
        rightkit_agents::driver_host::cli_runtime::StartedBridgeSession,
        rightkit_agents::driver_host::cli_runtime::BridgeError,
    > {
        if !matches!(spec.provider_id.as_str(), "claude" | "codex") {
            return self.start_generic(binding_id, spec).await;
        }
        let vendor_session_id = spec
            .session_id
            .strip_prefix("resume:")
            .unwrap_or(&spec.session_id)
            .to_string();
        let launch = rightkit_agents::driver_host::LaunchSpec {
            program: spec.executable.clone(),
            args: spec.argv.clone(),
            env: spec.env.into_iter().collect::<BTreeMap<_, _>>(),
            env_remove: spec.env_remove.into_iter().collect::<BTreeSet<_>>(),
            cwd: spec.cwd,
        };
        let legacy_kind = match spec.provider_id.as_str() {
            "claude" => DriverKind::Claude,
            "codex" => DriverKind::Codex,
            _ => {
                return Err(rightkit_agents::driver_host::cli_runtime::BridgeError::new(
                    "generic_provider_supervisor_unavailable",
                    "generic provider transport supervision is not wired in daemon bridge",
                ))
            }
        };
        let started = self
            .runtime
            .start_with_spec(binding_id.to_string(), legacy_kind, launch, cursor)
            .await
            .map_err(driver_bridge_error)?;
        let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let failure_driver = legacy_kind;
        tokio::spawn(async move {
            let mut events = started.events;
            while let Some(event) = events.recv().await {
                match event {
                    DriverRuntimeEvent::Normalized(event) => {
                        if events_tx.send(*event).await.is_err() {
                            break;
                        }
                    }
                    DriverRuntimeEvent::ProtocolError { message } => {
                        let _ = events_tx
                            .send(NormalizedEvent {
                                event_key: format!("bridge-protocol-{message}"),
                                driver: failure_driver,
                                source_offset: 0,
                                vendor_method: "bridge/protocol-error".to_string(),
                                kind: NormalizedEventKind::TurnFailed { message },
                            })
                            .await;
                        break;
                    }
                    DriverRuntimeEvent::Exited { success, code } if !success => {
                        let _ = events_tx
                            .send(NormalizedEvent {
                                event_key: format!("bridge-exit-{}", code.unwrap_or(-1)),
                                driver: failure_driver,
                                source_offset: 0,
                                vendor_method: "bridge/process-exit".to_string(),
                                kind: NormalizedEventKind::TurnFailed {
                                    message: format!("vendor process exited with {:?}", code),
                                },
                            })
                            .await;
                        break;
                    }
                    DriverRuntimeEvent::ServerRequest { .. }
                    | DriverRuntimeEvent::Diagnostic { .. }
                    | DriverRuntimeEvent::Exited { .. } => {}
                }
            }
        });
        Ok(
            rightkit_agents::driver_host::cli_runtime::StartedBridgeSession {
                // Claude creates this lazily on first prompt; Codex replaces it
                // with its native thread identity in persisted binding progress.
                vendor_session_id,
                model_id: None,
                events: events_rx,
            },
        )
    }

    async fn prompt(
        &self,
        binding_id: &str,
        vendor_session_id: &str,
        text: &str,
    ) -> Result<(), rightkit_agents::driver_host::cli_runtime::BridgeError> {
        if self.generic.lock().await.contains_key(binding_id) {
            return self.prompt_generic(binding_id, text).await;
        }
        self.runtime
            .send_prompt(binding_id, vendor_session_id, text)
            .await
            .map(|_| ())
            .map_err(driver_bridge_error)
    }

    async fn interrupt(
        &self,
        binding_id: &str,
        vendor_session_id: &str,
        vendor_turn_id: Option<&str>,
    ) -> Result<(), rightkit_agents::driver_host::cli_runtime::BridgeError> {
        if let Some(state) = self.generic.lock().await.get(binding_id) {
            let turn = state.turn.as_ref().ok_or_else(|| {
                rightkit_agents::driver_host::cli_runtime::BridgeError::new(
                    "no_active_turn",
                    "generic binding has no active turn",
                )
            })?;
            return state
                .transport
                .interrupt(turn)
                .await
                .map_err(cli_transport_bridge_error);
        }
        self.runtime
            .interrupt(binding_id, vendor_session_id, vendor_turn_id)
            .await
            .map(|_| ())
            .map_err(driver_bridge_error)
    }

    async fn shutdown(
        &self,
        binding_id: &str,
    ) -> Result<(), rightkit_agents::driver_host::cli_runtime::BridgeError> {
        if let Some(state) = self.generic.lock().await.remove(binding_id) {
            return state
                .transport
                .close(&state.session)
                .await
                .map_err(cli_transport_bridge_error);
        }
        match self.runtime.shutdown(binding_id).await {
            Ok(()) | Err(DriverRuntimeError::NotRunning(_)) => Ok(()),
            Err(error) => Err(driver_bridge_error(error)),
        }
    }

    async fn resume(
        &self,
        binding_id: &str,
        vendor_session_id: &str,
    ) -> Result<(), rightkit_agents::driver_host::cli_runtime::BridgeError> {
        if let Some(state) = self.generic.lock().await.get_mut(binding_id) {
            let request = LoadVendorSession {
                vendor_session_id: vendor_session_id.to_string(),
                model_id: state.session.model_id.clone(),
                mcp_server: state.spec.mcp_server.clone(),
            };
            let loaded = match state
                .transport
                .load_session(&state.session.process, request.clone())
                .await
            {
                Ok(session) => session,
                Err(_) => {
                    let _ = state.transport.close(&state.session).await;
                    let process = state
                        .transport
                        .start(&state.spec)
                        .await
                        .map_err(cli_transport_bridge_error)?;
                    state
                        .transport
                        .load_session(&process, request)
                        .await
                        .map_err(cli_transport_bridge_error)?
                }
            };
            state.session = loaded;
            state.turn = None;
            return Ok(());
        }
        self.runtime
            .resume(binding_id, vendor_session_id)
            .await
            .map(|_| ())
            .map_err(driver_bridge_error)
    }
}

fn driver_bridge_error(
    error: DriverRuntimeError,
) -> rightkit_agents::driver_host::cli_runtime::BridgeError {
    rightkit_agents::driver_host::cli_runtime::BridgeError::new(
        "driver_supervisor",
        error.to_string(),
    )
}

fn cli_transport_bridge_error(
    error: rightkit_agents::driver_host::cli_runtime::CliRuntimeError,
) -> rightkit_agents::driver_host::cli_runtime::BridgeError {
    rightkit_agents::driver_host::cli_runtime::BridgeError::new(error.code, error.message)
}

fn clone_vendor_session(session: &VendorSessionHandle) -> VendorSessionHandle {
    VendorSessionHandle {
        provider_id: session.provider_id.clone(),
        transport: session.transport,
        vendor_session_id: session.vendor_session_id.clone(),
        model_id: session.model_id.clone(),
        process: rightkit_agents::driver_host::cli_runtime::StartedCliProcess {
            pid: session.process.pid,
            generation: session.process.generation,
            inner: session.process.inner.clone_box(),
        },
    }
}

fn generic_failure_event(message: String) -> NormalizedEvent {
    NormalizedEvent {
        event_key: format!("generic-supervisor-{}", crate::api::now_millis()),
        driver: DriverKind::Codex,
        source_offset: 0,
        vendor_method: "generic-supervisor".to_string(),
        kind: NormalizedEventKind::TurnFailed { message },
    }
}

async fn capture_version(
    spec: &LaunchSpec,
    adapter: Arc<dyn ProcessLaunchAdapter>,
) -> Result<String, DriverRuntimeError> {
    // Version probes use the same governed adapter as driver launch so their
    // permits follow the probe child until it is waited and dropped.
    let launch = rightkit_sandbox::execution::CommandLaunchSpec {
        program: spec.program.to_string_lossy().into_owned(),
        argv: vec!["--version".to_string()],
        cwd: spec.cwd.to_string_lossy().into_owned(),
        context: rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: spec.cwd.to_string_lossy().into_owned(),
            sandbox_profile: Some("driver-version".into()),
            sandbox_required: true,
            network_required: false,
        },
        env: Vec::new(),
    };
    let mut child = adapter
        .spawn(launch)
        .await
        .map_err(|error| DriverRuntimeError::Version(error.to_string()))?;
    let mut stdout = child.take_stdout();
    let mut stderr = child.take_stderr();
    let output = tokio::time::timeout(VERSION_TIMEOUT, async {
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            async {
                let mut bytes = Vec::new();
                if let Some(stream) = stdout.as_mut() {
                    let _ = tokio::io::AsyncReadExt::read_to_end(stream, &mut bytes).await;
                }
                bytes
            },
            async {
                let mut bytes = Vec::new();
                if let Some(stream) = stderr.as_mut() {
                    let _ = tokio::io::AsyncReadExt::read_to_end(stream, &mut bytes).await;
                }
                bytes
            }
        );
        Ok::<_, DriverRuntimeError>((
            status.map_err(|error| DriverRuntimeError::Version(error.to_string()))?,
            stdout,
            stderr,
        ))
    })
    .await
    .map_err(|_| DriverRuntimeError::Version("timed out".into()))??;
    if output.0 != 0 {
        return Err(DriverRuntimeError::Version(format!(
            "exited with status {}",
            output.0
        )));
    }
    let stdout = String::from_utf8_lossy(&output.1).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.2).trim().to_string();
    let version = if stdout.is_empty() { stderr } else { stdout };
    if version.is_empty() {
        return Err(DriverRuntimeError::Version("empty output".into()));
    }
    Ok(version)
}

fn default_process_launch_adapter() -> Arc<dyn ProcessLaunchAdapter> {
    {
        let governor =
            rightkit_agent_tools::coding_tools::process_resource_authority::require_process_resource_governor()
                .expect("driver process authority must be installed by host composition");
        Arc::new(
            rightkit_agent_tools::coding_tools::process_resource_authority::GovernedProcessLaunchAdapter::new(
                rightkit_agent_tools::coding_tools::process_launch::LocalProcessLaunchAdapter,
                governor,
                rightkit_agent_tools::coding_tools::process_resource_authority::ResourceKind::ChildProcess,
                true,
            ),
        )
    }
}

async fn wait_ready(ready: &watch::Receiver<bool>) -> Result<(), DriverRuntimeError> {
    let mut ready = ready.clone();
    if *ready.borrow() {
        return Ok(());
    }
    tokio::time::timeout(DRIVER_READY_TIMEOUT, ready.wait_for(|value| *value))
        .await
        .map_err(|_| DriverRuntimeError::NotReady)?
        .map_err(|_| DriverRuntimeError::NotReady)?;
    Ok(())
}

async fn write_json(stdin: &StdinHandle, value: &Value) -> Result<(), DriverRuntimeError> {
    let mut guard = stdin.lock().await;
    let writer = guard.as_mut().ok_or(DriverRuntimeError::StdinClosed)?;
    let bytes = rightkit_sidecar::encode_json_line(value, rightkit_sidecar::UNCAPPED)
        .map_err(|error| DriverRuntimeError::Write(error.to_string()))?;
    writer
        .write_all(&bytes)
        .await
        .map_err(|error| DriverRuntimeError::Write(error.to_string()))?;
    writer
        .flush()
        .await
        .map_err(|error| DriverRuntimeError::Write(error.to_string()))
}

/// Rejected `RateLimit` event for a Codex usage-limit failure line (empty for every other line),
/// stamped with the launching account.
fn codex_usage_limit_events(
    line: &str,
    offset: u64,
    account: Option<&ClaudeAccountIdentity>,
) -> Vec<DriverRuntimeEvent> {
    match rightkit_agents::driver_host::parse_codex_usage_limit_event(line, offset) {
        Ok(Some(mut event)) => {
            rightkit_agents::driver_host::stamp_account(&mut event.kind, account);
            vec![DriverRuntimeEvent::Normalized(Box::new(event))]
        }
        _ => Vec::new(),
    }
}

/// Everything the stdout reader task owns for one driver process.
struct StdoutReader {
    kind: DriverKind,
    stdout: std::pin::Pin<Box<dyn AsyncRead + Send>>,
    initial_offset: u64,
    events: mpsc::Sender<DriverRuntimeEvent>,
    stdin: StdinHandle,
    child: Arc<Mutex<Box<dyn rightkit_sandbox::execution::ManagedChild>>>,
    ready: watch::Sender<bool>,
    account: Option<ClaudeAccountIdentity>,
}

fn spawn_stdout_reader(reader: StdoutReader) {
    let StdoutReader {
        kind,
        stdout,
        initial_offset,
        events,
        stdin,
        child,
        ready,
        account,
    } = reader;
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        let mut offset = initial_offset;
        while let Ok(Some(line)) = lines.next_line().await {
            offset = offset.saturating_add(1);
            // Events a single line implies beyond the last one (Claude `result` also yields
            // cost); they are delivered in order before the last.
            let mut leading: Vec<DriverRuntimeEvent> = Vec::new();
            let parsed = match kind {
                DriverKind::Codex => {
                    // #34: a usage-limit failure also becomes a rejected RateLimit event,
                    // delivered ahead of the ordinary TurnFailed for the same line.
                    leading.extend(codex_usage_limit_events(&line, offset, account.as_ref()));
                    match parse_codex_jsonrpc_line(&line, offset) {
                    Ok(Some(CodexMessage::Response { id, error, .. }))
                        if id.as_u64() == Some(CODEX_INITIALIZE_ID) =>
                    {
                        if let Some(error) = error {
                            Err(format!("Codex initialization failed: {error}"))
                        } else {
                            match write_json(&stdin, &json!({ "method": "initialized" })).await {
                                Ok(()) => {
                                    let _ = ready.send(true);
                                    Ok(None)
                                }
                                Err(error) => Err(error.to_string()),
                            }
                        }
                    }
                    Ok(Some(CodexMessage::Response { .. })) | Ok(None) => Ok(None),
                    Ok(Some(CodexMessage::Notification(mut event))) => {
                        rightkit_agents::driver_host::stamp_account(&mut event.kind, account.as_ref());
                        let is_ready = matches!(
                            event.kind,
                            rightkit_agents::driver_host::NormalizedEventKind::GatewayReady
                        );
                        if is_ready {
                            let _ = ready.send(true);
                        }
                        Ok(Some(DriverRuntimeEvent::Normalized(event)))
                    }
                    Ok(Some(CodexMessage::ServerRequest { id, method, params })) => {
                        Ok(Some(DriverRuntimeEvent::ServerRequest {
                            id,
                            method,
                            params,
                        }))
                    }
                    Err(error) => Err(error.to_string()),
                    }
                }
                DriverKind::Antigravity => {
                    rightkit_agents::driver_host::agy_harness::parse_agy_stream_line(&line, offset)
                        .map(|event| {
                            event.map(|event| {
                                if matches!(
                                    event.kind,
                                    rightkit_agents::driver_host::NormalizedEventKind::GatewayReady
                                        | rightkit_agents::driver_host::NormalizedEventKind::SessionStarted { .. }
                                ) {
                                    let _ = ready.send(true);
                                }
                                DriverRuntimeEvent::Normalized(Box::new(event))
                            })
                        })
                        .map_err(|error| error.to_string())
                }
                // #336: a CLI permission prompt is surfaced as a server request so the daemon can
                // route it through the approval flow; the answer goes back via
                // `respond_server_request` as a `control_response`.
                DriverKind::Claude => match rightkit_agents::driver_host::parse_claude_can_use_tool(&line) {
                    Err(error) => Err(error.to_string()),
                    Ok(Some(request)) => Ok(Some(DriverRuntimeEvent::ServerRequest {
                        id: Value::String(request.request_id),
                        method: "can_use_tool".into(),
                        params: json!({
                            "tool_name": request.tool_name,
                            "input": request.input,
                            "tool_use_id": request.tool_use_id,
                            "agent_id": request.agent_id,
                            "blocked_path": request.blocked_path,
                        }),
                    })),
                    Ok(None) => parse_claude_stream_events(&line, offset, account.as_ref())
                    .map(|batch| {
                        let mut batch = batch
                            .into_iter()
                            .map(|event| {
                                if matches!(
                                    event.kind,
                                    rightkit_agents::driver_host::NormalizedEventKind::GatewayReady
                                        | rightkit_agents::driver_host::NormalizedEventKind::SessionStarted { .. }
                                ) {
                                    let _ = ready.send(true);
                                }
                                DriverRuntimeEvent::Normalized(Box::new(event))
                            })
                            .collect::<Vec<_>>();
                        let last = batch.pop();
                        leading = batch;
                        last
                    })
                    .map_err(|error| error.to_string()),
                },
            };
            if parsed.is_ok() {
                let mut closed = false;
                for event in leading {
                    if events.send(event).await.is_err() {
                        closed = true;
                        break;
                    }
                }
                if closed {
                    break;
                }
            }
            match parsed {
                Ok(Some(event)) => {
                    if events.send(event).await.is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(message) => {
                    // Preserve a sanitized diagnostic before failing closed. The parser error
                    // contains only protocol shape/method information, never the raw vendor
                    // line, so credentials emitted by a misbehaving driver cannot leak here.
                    let diagnostic = format!(
                        "driver protocol rejected {kind:?} output at cursor {offset}: {message}"
                    );
                    if events
                        .send(DriverRuntimeEvent::Diagnostic {
                            stream: DiagnosticStream::Stderr,
                            message: diagnostic,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                    let _ = events
                        .send(DriverRuntimeEvent::ProtocolError { message })
                        .await;
                    let mut child = child.lock().await;
                    let _ = child.cancel().await;
                    break;
                }
            }
        }
    });
}

fn spawn_stderr_reader(
    stderr: std::pin::Pin<Box<dyn AsyncRead + Send>>,
    events: mpsc::Sender<DriverRuntimeEvent>,
    secrets: Vec<String>,
) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let message = secrets.iter().fold(line, |message, secret| {
                message.replace(secret, "[redacted]")
            });
            if events
                .send(DriverRuntimeEvent::Diagnostic {
                    stream: DiagnosticStream::Stderr,
                    message,
                })
                .await
                .is_err()
            {
                break;
            }
        }
    });
}

fn spawn_exit_monitor(
    binding_id: String,
    child: Arc<Mutex<Box<dyn rightkit_sandbox::execution::ManagedChild>>>,
    events: mpsc::Sender<DriverRuntimeEvent>,
    processes: Arc<Mutex<HashMap<String, Arc<ManagedDriver>>>>,
) {
    tokio::spawn(async move {
        {
            // Holding the lock for an unbounded wait prevents shutdown and
            // protocol-error cancellation from ever reaching the child.
            let status = loop {
                let attempt = {
                    let mut guard = child.lock().await;
                    tokio::time::timeout(Duration::from_millis(50), guard.wait()).await
                };
                if let Ok(status) = attempt {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            };
            match status {
                Ok(code) => {
                    let _ = events
                        .send(DriverRuntimeEvent::Exited {
                            success: code == 0,
                            code: Some(code),
                        })
                        .await;
                    processes.lock().await.remove(&binding_id);
                }
                Err(error) => {
                    let _ = events
                        .send(DriverRuntimeEvent::ProtocolError {
                            message: format!("driver status failed: {error}"),
                        })
                        .await;
                    processes.lock().await.remove(&binding_id);
                }
            }
        }
    });
}
