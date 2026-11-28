//! Daemon-side process supervision for external reasoning drivers.
//!
//! The host remains the capability and approval authority. This module only owns vendor process
//! lifecycle and wire normalization; callers persist bindings/events and execute capabilities.

use async_trait::async_trait;
use rightkit_agents::driver_host::cli_runtime::{
    CliPrompt, CliProviderAdapter, CliTransport, CreateVendorSession, LoadVendorSession,
    VendorSessionHandle, VendorTurnHandle,
};
use rightkit_agents::driver_host::{
    parse_claude_stream_events, ClaudeAccountIdentity, DriverCursor, DriverKind, LaunchSpec,
    NormalizedEvent, NormalizedEventKind,
};
use rightkit_sandbox::execution::ProcessLaunchAdapter;
use rightkit_sidecar::{FrameError, Incoming, ReadOptions};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, watch, Mutex};

const EVENT_CHANNEL_CAPACITY: usize = 128;
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const DRIVER_READY_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const FIRST_REQUEST_ID: u64 = 1;
/// The longest single stdout line a vendor process may emit before it is cut off.
const MAX_VENDOR_LINE_BYTES: usize = 16 * 1024 * 1024;

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
    Normalized(NormalizedEvent),
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
    /// Which start of this binding id the process is; a stale process never matches it.
    pub generation: u64,
}

pub struct StartedDriver {
    pub info: DriverProcessInfo,
    pub events: DriverEventReceiver,
}

#[derive(Clone)]
pub struct DriverHostRuntime {
    processes: Arc<Mutex<HashMap<String, Arc<ManagedDriver>>>>,
    next_request_id: Arc<AtomicU64>,
    next_generation: Arc<AtomicU64>,
    process_launch_adapter: Arc<dyn ProcessLaunchAdapter>,
    process_launch_context: rightkit_sandbox::execution::ProcessLaunchContext,
}

type StdinHandle = Arc<Mutex<Option<std::pin::Pin<Box<dyn AsyncWrite + Send>>>>>;

struct ManagedDriver {
    kind: DriverKind,
    generation: u64,
    child: Arc<Mutex<Box<dyn rightkit_sandbox::execution::ManagedChild>>>,
    stdin: StdinHandle,
    ready: watch::Receiver<bool>,
    #[cfg(windows)]
    _windows_job: rightkit_agent_coding_tools::sandbox::WindowsJob,
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
            next_request_id: Arc::new(AtomicU64::new(FIRST_REQUEST_ID + 1)),
            next_generation: Arc::new(AtomicU64::new(1)),
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
        if kind == DriverKind::Codex {
            return Err(DriverRuntimeError::Unsupported(kind));
        }
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
                // A vendor CLI talks to its own provider and its MCP helper to the daemon:
                // filesystem confinement stays, egress is allowed (sandbox config docs).
                network_required: true,
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
            let job = rightkit_agent_coding_tools::sandbox::shell_job().map_err(|error| {
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
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let managed = Arc::new(ManagedDriver {
            kind,
            generation,
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

        spawn_stdout_reader(
            kind,
            stdout,
            cursor.output_offset,
            StdoutReaderHandles {
                events: event_tx.clone(),
                stdin: stdin.clone(),
                child: child.clone(),
                ready: ready_tx,
            },
            account,
        );
        spawn_stderr_reader(stderr, event_tx.clone(), secret_values);
        spawn_exit_monitor(
            binding_id.clone(),
            child.clone(),
            event_tx,
            self.processes.clone(),
            generation,
        );

        Ok(StartedDriver {
            info: DriverProcessInfo {
                binding_id,
                driver: kind,
                pid,
                binary_version,
                generation,
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
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let payload = match process.kind {
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
            // The host does not run Codex as a driver process (it is not a built-in provider),
            // and `start_with_spec_for_account` refuses it, so no process of this kind exists.
            DriverKind::Codex => return Err(DriverRuntimeError::Unsupported(DriverKind::Codex)),
        };
        write_json(&process.stdin, &payload).await?;
        Ok(request_id)
    }

    /// Answer a vendor permission request after the host has made the approval decision.
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
                decision = rightkit_agents::driver_host::ClaudePermissionDecision::deny(format!(
                    "Approval answer carried no updatedInput; denied by {}",
                    crate::host_identity::app()
                ));
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
        Err(DriverRuntimeError::Unsupported(process.kind))
    }

    pub async fn resume(
        &self,
        binding_id: &str,
        _vendor_session_id: &str,
    ) -> Result<u64, DriverRuntimeError> {
        let process = self.process(binding_id).await?;
        wait_ready(&process.ready).await?;
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        match process.kind {
            // Claude's stream-json process is bound to its session from launch (`--resume`
            // on start); a live process is already attached, so resume is an acknowledgement.
            // The route checks the requested session against the binding first.
            DriverKind::Claude => Ok(request_id),
            kind => Err(DriverRuntimeError::Unsupported(kind)),
        }
    }

    pub async fn interrupt(
        &self,
        binding_id: &str,
        _vendor_session_id: &str,
        _vendor_turn_id: Option<&str>,
    ) -> Result<u64, DriverRuntimeError> {
        let process = self.process(binding_id).await?;
        wait_ready(&process.ready).await?;
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let payload = match process.kind {
            DriverKind::Claude | DriverKind::Antigravity => json!({
                "type": "control_request",
                "request_id": format!("interrupt-{request_id}"),
                "request": { "subtype": "interrupt" }
            }),
            DriverKind::Codex => return Err(DriverRuntimeError::Unsupported(DriverKind::Codex)),
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

    /// Is the process of this `generation` the one now registered under `binding_id`?
    pub async fn is_current(&self, binding_id: &str, generation: u64) -> bool {
        self.processes
            .lock()
            .await
            .get(binding_id)
            .is_some_and(|process| process.generation == generation)
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
    /// Where a copy of every driver event goes so the chat's own event stream gets it
    /// (`ensure_runtime_session_events`). Unset until the first agent runtime is opened.
    session_events: Arc<std::sync::OnceLock<SessionEventTap>>,
}

/// `(binding id, event)` pairs on their way to the chat the binding belongs to.
pub(crate) type SessionEventTap = mpsc::UnboundedSender<(String, NormalizedEvent)>;

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
            session_events: Arc::new(std::sync::OnceLock::new()),
        }
    }

    pub(crate) fn session_events(&self) -> &std::sync::OnceLock<SessionEventTap> {
        &self.session_events
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
        let model_id_env = rightkit_agents::driver_host::host_names::internal_model_id_env_name();
        let model_id = spec
            .env
            .iter()
            .find(|(name, _)| *name == model_id_env)
            .map(|(_, value)| value.clone());
        spec.env.retain(|(name, _)| *name != model_id_env);
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
                        system_policy: rightkit_agents::driver_host::run_scoped_policy(),
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
        if spec.provider_id.as_str() != "claude" {
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
            _ => {
                return Err(rightkit_agents::driver_host::cli_runtime::BridgeError::new(
                    "generic_provider_supervisor_unavailable",
                    "generic provider transport supervision is not wired in daemon bridge",
                ))
            }
        };
        // The account the program runs on is the one whose folder the launch pins
        // (`CLAUDE_CONFIG_DIR`). Name it, so its rate-limit and cost reports carry the
        // account and not "unknown" (the `/driver/start` route already does).
        let account = launch.env.get("CLAUDE_CONFIG_DIR").and_then(|home| {
            use rightkit_agents::driver_host::cli_runtime::account_identity as accounts;
            let home = std::path::Path::new(home);
            accounts::list_subscription_accounts()
                .into_iter()
                .find(|account| account.provider == "claude" && account.managed_home == home)
                .and_then(|account| {
                    accounts::identity_for_profile(&format!("claude/{}", account.label))
                })
        });
        let started = self
            .runtime
            .start_with_spec_for_account(
                binding_id.to_string(),
                legacy_kind,
                launch,
                cursor,
                account,
            )
            .await
            .map_err(driver_bridge_error)?;
        let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        let failure_driver = legacy_kind;
        let session_events = self.session_events.clone();
        let tapped_binding = binding_id.to_string();
        let generation = started.info.generation;
        let runtime = self.runtime.clone();
        tokio::spawn(async move {
            let mut events = started.events;
            while let Some(event) = events.recv().await {
                // A stopped process must not speak for the one that replaced it.
                if !runtime.is_current(&tapped_binding, generation).await {
                    break;
                }
                match event {
                    DriverRuntimeEvent::Normalized(event) => {
                        if let Some(tap) = session_events.get() {
                            let _ = tap.send((tapped_binding.clone(), event.clone()));
                        }
                        if events_tx.send(event).await.is_err() {
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
        // The generic supervisor has no vendor of its own; only Claude is registered.
        driver: DriverKind::Claude,
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
            rightkit_agent_coding_tools::process_resource_authority::require_process_resource_governor()
                .expect("driver process authority must be installed by host composition");
        Arc::new(
            rightkit_agent_coding_tools::process_resource_authority::GovernedProcessLaunchAdapter::new(
                rightkit_agent_coding_tools::process_launch::LocalProcessLaunchAdapter,
                governor,
                rightkit_agent_coding_tools::process_resource_authority::ResourceKind::ChildProcess,
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

/// `(request_id, subtype)` of a `control_request` from Claude Code that is not a permission
/// prompt (those are handled as server requests). `None` for every other line.
fn unsupported_claude_control_request(line: &str) -> Option<(String, String)> {
    let value: Value = serde_json::from_str(line).ok()?;
    if value.get("type").and_then(Value::as_str) != Some("control_request") {
        return None;
    }
    let subtype = value
        .get("request")
        .and_then(|request| request.get("subtype"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if subtype == "can_use_tool" {
        return None;
    }
    let request_id = value.get("request_id").and_then(Value::as_str)?;
    Some((request_id.to_string(), subtype.to_string()))
}

/// The channels and handles the stdout reader drives: where it sends driver events, the child's
/// stdin it answers on, the child it cancels on a protocol error, and the readiness signal.
struct StdoutReaderHandles {
    events: mpsc::Sender<DriverRuntimeEvent>,
    stdin: StdinHandle,
    child: Arc<Mutex<Box<dyn rightkit_sandbox::execution::ManagedChild>>>,
    ready: watch::Sender<bool>,
}

fn spawn_stdout_reader(
    kind: DriverKind,
    stdout: std::pin::Pin<Box<dyn AsyncRead + Send>>,
    initial_offset: u64,
    handles: StdoutReaderHandles,
    account: Option<ClaudeAccountIdentity>,
) {
    let StdoutReaderHandles {
        events,
        stdin,
        child,
        ready,
    } = handles;
    tokio::spawn(async move {
        let mut reader = BufReader::new(stdout);
        let mut buffer: Vec<u8> = Vec::new();
        let mut offset = initial_offset;
        loop {
            let line = match rightkit_sidecar::read_json_line_or_oversized_async_opts::<_, Value>(
                &mut reader,
                &mut buffer,
                MAX_VENDOR_LINE_BYTES,
                ReadOptions::default(),
            )
            .await
            {
                Ok(Some(Incoming::Frame(value))) => match serde_json::to_string(&value) {
                    Ok(line) => line,
                    Err(_) => {
                        reject_vendor_output(
                            kind,
                            offset,
                            "driver output is not valid JSON",
                            "driver output is not valid JSON",
                            &events,
                            &child,
                        )
                        .await;
                        break;
                    }
                },
                Ok(Some(Incoming::Oversized(_)))
                | Err(FrameError::TooLarge { .. })
                | Err(FrameError::Io(_)) => {
                    reject_vendor_output(
                        kind,
                        offset,
                        &format!("a line exceeded {MAX_VENDOR_LINE_BYTES} bytes"),
                        "driver output line too large",
                        &events,
                        &child,
                    )
                    .await;
                    break;
                }
                Err(_) => {
                    // Not valid JSON or not valid UTF-8; never include the line.
                    reject_vendor_output(
                        kind,
                        offset,
                        "driver output is not valid JSON",
                        "driver output is not valid JSON",
                        &events,
                        &child,
                    )
                    .await;
                    break;
                }
                Ok(None) => break,
            };
            offset = offset.saturating_add(1);
            // Events a single line implies beyond the last one (Claude `result` also yields
            // cost); they are delivered in order before the last.
            let mut leading: Vec<DriverRuntimeEvent> = Vec::new();
            let parsed = match kind {
                DriverKind::Codex => Err(DriverRuntimeError::Unsupported(DriverKind::Codex).to_string()),
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
                                DriverRuntimeEvent::Normalized(event)
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
                    Ok(None) if unsupported_claude_control_request(&line).is_some() => {
                        // Claude Code waits for an answer to every request it sends. One this
                        // driver does not implement gets the protocol's error answer, so the
                        // session goes on and nothing waits for ever.
                        let (request_id, subtype) =
                            unsupported_claude_control_request(&line).unwrap_or_default();
                        let answer = json!({
                            "type": "control_response",
                            "response": {
                                "subtype": "error",
                                "request_id": request_id,
                                "error": format!("{} does not support the '{subtype}' request", crate::host_identity::app())
                            }
                        });
                        match write_json(&stdin, &answer).await {
                            Ok(()) => Ok(None),
                            Err(error) => Err(error.to_string()),
                        }
                    }
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
                                DriverRuntimeEvent::Normalized(event)
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

/// Fail closed on a vendor output line the frame reader refused: a sanitized diagnostic, the
/// protocol error, then cancel the child. The line itself is never included.
async fn reject_vendor_output(
    kind: DriverKind,
    offset: u64,
    detail: &str,
    protocol_message: &str,
    events: &mpsc::Sender<DriverRuntimeEvent>,
    child: &Arc<Mutex<Box<dyn rightkit_sandbox::execution::ManagedChild>>>,
) {
    let diagnostic =
        format!("driver protocol rejected {kind:?} output at cursor {offset}: {detail}");
    if events
        .send(DriverRuntimeEvent::Diagnostic {
            stream: DiagnosticStream::Stderr,
            message: diagnostic,
        })
        .await
        .is_err()
    {
        return;
    }
    let _ = events
        .send(DriverRuntimeEvent::ProtocolError {
            message: protocol_message.to_string(),
        })
        .await;
    let mut child = child.lock().await;
    let _ = child.cancel().await;
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

/// Drop the registry entry only when it still belongs to this monitor's process: a late exit
/// report of a stopped process must not delete the entry of the one resume started after it.
async fn remove_if_generation(
    processes: &Arc<Mutex<HashMap<String, Arc<ManagedDriver>>>>,
    binding_id: &str,
    generation: u64,
) {
    let mut processes = processes.lock().await;
    if processes
        .get(binding_id)
        .is_some_and(|process| process.generation == generation)
    {
        processes.remove(binding_id);
    }
}

fn spawn_exit_monitor(
    binding_id: String,
    child: Arc<Mutex<Box<dyn rightkit_sandbox::execution::ManagedChild>>>,
    events: mpsc::Sender<DriverRuntimeEvent>,
    processes: Arc<Mutex<HashMap<String, Arc<ManagedDriver>>>>,
    generation: u64,
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
                    remove_if_generation(&processes, &binding_id, generation).await;
                }
                Err(error) => {
                    let _ = events
                        .send(DriverRuntimeEvent::ProtocolError {
                            message: format!("driver status failed: {error}"),
                        })
                        .await;
                    remove_if_generation(&processes, &binding_id, generation).await;
                }
            }
        }
    });
}
