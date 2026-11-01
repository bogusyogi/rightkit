//! Shared [`AgentRuntimeExecutor`] infrastructure.
//!
//! PR 4 of the architecture plan. Two concrete executors live here:
//!
//! - [`CodeRightNativeExecutor`] wraps the existing the host app-native
//!   session runtime. The root session is still launched by the
//!   daemon's existing native spawner (later PRs migrate that path);
//!   the executor is the new dispatch seam for child agents and is
//!   already wired so the daemon can route through it without
//!   changing root behavior.
//! - [`AuthenticatedCliExecutor`] wraps the authenticated-CLI path
//!   for Claude and Codex. It looks up the adapter in the
//!   [`crate::driver_host::cli_runtime::registry::CliAdapterRegistry`], calls
//!   `build_launch` to produce a [`crate::driver_host::cli_runtime::adapter::CliLaunchSpec`],
//!   and then drives the actual process through a
//!   [`CliDriverHostBridge`] that the daemon implements. Vendor
//!   events arrive as [`crate::driver_host::contracts::NormalizedEvent`] and are
//!   folded into [`AgentTurnResult`] by the executor.
//!
//! Both executors implement the same [`AgentRuntimeExecutor`] trait
//! so root sessions and child agents use the same dispatch surface.
//! Future PRs add ACP and ResumableNdjson transports; the executor
//! stays the same shape.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

#[cfg(test)]
use crate::driver_host::cli_runtime::adapter::AgentKind;
use crate::driver_host::cli_runtime::adapter::{
    AgentRuntimeBackend, AgentRuntimeError, AgentRuntimeExecutor, AgentRuntimeHandle,
    AgentTurnRequest, AgentTurnResult, AgentTurnStatus, CliLaunchSpec, ExecutorKind,
    OpenAgentRequest,
};
use crate::driver_host::cli_runtime::ids::CliProviderId;
#[cfg(test)]
use crate::driver_host::cli_runtime::ids::CliTransportKind;
use crate::driver_host::cli_runtime::recovery::BindingResumeGuard;
use crate::driver_host::cli_runtime::registry::CliAdapterRegistry;
#[cfg(test)]
use crate::driver_host::contracts::DriverKind;
use crate::driver_host::contracts::{DriverCursor, NormalizedEvent, NormalizedEventKind};

/// One resolved handoff brief that a parent runtime gives to a child.
/// PR 5 fills in the construction logic; PR 4 only lays down the
/// type so consumers can match exhaustively across PRs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliHandoffBrief {
    pub parent_agent_id: String,
    pub child_agent_id: String,
    pub task: String,
    pub done_when: Vec<String>,
    pub goal_summary: String,
    pub tool_ceiling: Vec<String>,
    pub mutation_policy: MutationPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationPolicy {
    ReadOnly,
    ScopedWrite,
    FullWrite,
}

/// Bounded structured result returned to a parent after a child
/// completes. The full transcript remains first-class in the agent
/// rail; the parent provider only sees this structured form to
/// avoid polluting its context window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResult {
    pub coderight_agent_id: String,
    pub provider_id: Option<CliProviderId>,
    pub vendor_session_id: Option<String>,
    pub status: AgentResultStatus,
    pub final_text: String,
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentResultStatus {
    Completed,
    Partial,
    Failed,
    Cancelled,
}

/// Errors raised by the bridge that connects the executor to the
/// daemon's process supervisor. The supervisor is the daemon's
/// existing `DriverHostRuntime`; the bridge exists so the executor
/// never depends on the daemon binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for BridgeError {}

impl BridgeError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// What the bridge returns after a successful `start`. The executor
/// reads from `events` to fold vendor output into an
/// [`AgentTurnResult`]. The events stream ends when the vendor
/// process exits or the bridge is shut down.
#[derive(Debug)]
pub struct StartedBridgeSession {
    pub vendor_session_id: String,
    pub model_id: Option<String>,
    pub events: mpsc::Receiver<NormalizedEvent>,
}

/// Daemon-owned launch inputs. Keeping them outside the executor prevents it
/// from fabricating a profile, MCP binary, or gateway lease for a provider.
#[derive(Debug, Clone)]
pub struct CliExecutorLaunchContext {
    pub executable: std::path::PathBuf,
    pub profile: crate::driver_host::cli_runtime::ids::CliAccountProfile,
    pub mcp_server: crate::driver_host::cli_runtime::adapter::CliMcpServerSpec,
    pub policy: String,
    /// Exact daemon-resolved context that earned harness qualification.
    pub attestation_inputs: crate::driver_host::cli_runtime::attestation::AttestationInputs,
}

pub trait CliExecutorLaunchContextProvider: Send + Sync {
    fn resolve(
        &self,
        provider_id: &CliProviderId,
        profile_id: &crate::driver_host::cli_runtime::ids::CliAccountProfileId,
        session_id: &str,
        agent_id: &str,
        binding_id: &str,
    ) -> Result<CliExecutorLaunchContext, AgentRuntimeError>;
}

/// The bridge that connects the [`AuthenticatedCliExecutor`] to the
/// daemon's actual process supervisor. The trait lives in the
/// `driver_host` crate so the executor can be wired in this crate
/// without depending on the daemon binary. The daemon implements
/// this trait by wrapping its existing `DriverHostRuntime`.
#[async_trait]
pub trait CliDriverHostBridge: Send + Sync {
    /// Start the vendor process from a [`CliLaunchSpec`] produced by
    /// the adapter. Returns the started session along with an event
    /// receiver that yields normalized events until the process exits
    /// or the binding is shut down.
    async fn start(
        &self,
        binding_id: &str,
        spec: CliLaunchSpec,
        cursor: DriverCursor,
    ) -> Result<StartedBridgeSession, BridgeError>;

    /// Send a prompt to an already-started session. The bridge is
    /// expected to dispatch the right JSON-RPC / stream-JSON message
    /// for the active transport.
    async fn prompt(
        &self,
        binding_id: &str,
        vendor_session_id: &str,
        text: &str,
    ) -> Result<(), BridgeError>;

    /// Interrupt the in-flight turn. The bridge may forward the
    /// interrupt to the vendor immediately; the executor does not
    /// need to wait for the events stream to drain.
    async fn interrupt(
        &self,
        binding_id: &str,
        vendor_session_id: &str,
        vendor_turn_id: Option<&str>,
    ) -> Result<(), BridgeError>;

    /// Shut down the process. Idempotent; safe to call more than once.
    async fn shutdown(&self, binding_id: &str) -> Result<(), BridgeError>;

    /// Resume an existing vendor session by its native session id.
    async fn resume(&self, binding_id: &str, vendor_session_id: &str) -> Result<(), BridgeError>;
}

/// Bridge to the existing the host app-native session runtime. The
/// trait mirrors what `SubagentSpawner` already provides to the
/// `spawn_subagent` tool, but the executor needs its own copy so
/// the call graph stays in `driver_host` (the daemon binary
/// implements both). PR 5 closes the gap between the two by
/// collapsing the executor's child path into the spawner.
#[async_trait]
pub trait NativeConductor: Send + Sync {
    async fn run_native_turn(
        &self,
        session_id: &str,
        agent_id: &str,
        role: &str,
        prompt: &str,
    ) -> Result<String, NativeConductorError>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeConductorError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for NativeConductorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for NativeConductorError {}

impl NativeConductorError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Resident state for an opened agent. The executor stores the
/// bridge-side identifiers (binding id, vendor session id, model)
/// in addition to the executor-side identifiers (session id, agent
/// id) so the bridge can be re-invoked without re-deriving them.
#[derive(Debug, Clone)]
struct AgentState {
    session_id: String,
    agent_id: String,
    role: String,
    backend: AgentRuntimeBackend,
    #[allow(dead_code)]
    tool_ceiling: Vec<String>,
    #[allow(dead_code)]
    worktree_id: Option<String>,
    binding_id: Option<String>,
    vendor_session_id: Option<String>,
    vendor_turn_id: Option<String>,
    model_id: Option<String>,
    /// In-flight text accumulated from `AssistantDelta` events
    /// between the last call to `run_turn` and the terminal event.
    #[allow(dead_code)]
    last_partial_text: String,
}

/// The native executor. Wraps the existing the host app-native
/// session runtime. The bridge [`NativeConductor`] is consulted
/// for child agents; root agents get a no-op `run_turn` because
/// the root session is still launched by the daemon's existing
/// native path (the executor is the new dispatch seam, not a
/// replacement for the conductor surface).
pub struct HostNativeExecutor {
    handles: Arc<Mutex<HashMap<String, AgentState>>>,
    conductor: Option<Arc<dyn NativeConductor>>,
}

impl HostNativeExecutor {
    pub fn new() -> Self {
        Self {
            handles: Arc::new(Mutex::new(HashMap::new())),
            conductor: None,
        }
    }

    /// Bind the executor to a native conductor. PR 5 wires the
    /// daemon's `SubagentSpawner` here. Until then, `run_turn` for
    /// child agents returns a "no_conductor" error so the wrap is
    /// observable without breaking existing behavior.
    pub fn with_conductor(mut self, conductor: Arc<dyn NativeConductor>) -> Self {
        self.conductor = Some(conductor);
        self
    }
}

impl Default for HostNativeExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentRuntimeExecutor for HostNativeExecutor {
    fn kind(&self) -> ExecutorKind {
        ExecutorKind::Native
    }

    async fn open_agent(
        &self,
        request: OpenAgentRequest,
    ) -> Result<AgentRuntimeHandle, AgentRuntimeError> {
        if let AgentRuntimeBackend::AuthenticatedCli { .. } = &request.backend {
            return Err(AgentRuntimeError::new(
                "backend_mismatch",
                "CodeRightNativeExecutor only accepts CodeRightNative backends",
            ));
        }
        let handle = AgentRuntimeHandle {
            session_id: request.session_id,
            agent_id: request.agent_id,
            backend: request.backend,
            binding_id: None,
            vendor_session_id: None,
            vendor_turn_id: None,
            model_id: None,
        };
        let state = AgentState {
            session_id: handle.session_id.clone(),
            agent_id: handle.agent_id.clone(),
            role: request.role,
            backend: handle.backend.clone(),
            tool_ceiling: request.tool_ceiling,
            worktree_id: request.worktree_id,
            binding_id: None,
            vendor_session_id: None,
            vendor_turn_id: None,
            model_id: None,
            last_partial_text: String::new(),
        };
        let mut guard = self.handles.lock().map_err(|_| {
            AgentRuntimeError::new("lock_poisoned", "native executor handle map poisoned")
        })?;
        guard.insert(handle.agent_id.clone(), state);
        Ok(handle)
    }

    async fn run_turn(
        &self,
        handle: &AgentRuntimeHandle,
        request: AgentTurnRequest,
    ) -> Result<AgentTurnResult, AgentRuntimeError> {
        let state = {
            let guard = self.handles.lock().map_err(|_| {
                AgentRuntimeError::new("lock_poisoned", "native executor handle map poisoned")
            })?;
            guard
                .get(&handle.agent_id)
                .cloned()
                .ok_or_else(|| AgentRuntimeError::new("unknown_agent", "handle not registered"))?
        };
        if request.agent_kind != state.backend.agent_kind() {
            return Err(AgentRuntimeError::new(
                "agent_kind_mismatch",
                format!(
                    "request role {:?} does not match backend role {:?}",
                    request.agent_kind,
                    state.backend.agent_kind()
                ),
            ));
        }
        if request.agent_kind.is_root() {
            // Root native sessions are still launched by the daemon's
            // existing conductor surface. The executor is the dispatch
            // seam; the actual run lives elsewhere. Returning a
            // Completed result preserves root behavior unchanged.
            return Ok(AgentTurnResult {
                agent_id: handle.agent_id.clone(),
                status: AgentTurnStatus::Completed,
                final_text: String::new(),
                continuation_token: None,
                prompt_tokens: 0,
                completion_tokens: 0,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            });
        }
        let Some(conductor) = self.conductor.clone() else {
            return Err(AgentRuntimeError::new(
                "no_conductor",
                "HostNativeExecutor has no NativeConductor bound; \
                 child native spawning is wired in PR 5",
            ));
        };
        let result = conductor
            .run_native_turn(
                &state.session_id,
                &state.agent_id,
                &state.role,
                &request.prompt,
            )
            .await
            .map_err(|error| {
                AgentRuntimeError::new(
                    "native_turn_failed",
                    format!("native conductor failed: {error}"),
                )
            })?;
        Ok(AgentTurnResult {
            agent_id: handle.agent_id.clone(),
            status: AgentTurnStatus::Completed,
            final_text: result,
            continuation_token: None,
            prompt_tokens: 0,
            completion_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        })
    }

    async fn interrupt(&self, handle: &AgentRuntimeHandle) -> Result<(), AgentRuntimeError> {
        let _ = handle;
        Ok(())
    }

    async fn resume(
        &self,
        handle: &AgentRuntimeHandle,
    ) -> Result<AgentRuntimeHandle, AgentRuntimeError> {
        Ok(handle.clone())
    }
}

/// The authenticated-CLI executor. Looks up the provider's adapter
/// in the [`CliAdapterRegistry`], produces a [`CliLaunchSpec`]
/// through `build_launch`, and drives the vendor process via the
/// [`CliDriverHostBridge`]. Vendor events are folded into
/// [`AgentTurnResult`] by [`fold_turn_result`].
pub struct AuthenticatedCliExecutor {
    registry: Arc<CliAdapterRegistry>,
    bridge: Arc<dyn CliDriverHostBridge>,
    handles: Arc<Mutex<HashMap<String, AgentState>>>,
    /// Live event streams for every open binding. The daemon's
    /// `DriverHostRuntime` runs an event reader per binding; the
    /// executor reads from the bridge's channel and translates
    /// events into per-turn results.
    sessions: Arc<Mutex<HashMap<String, mpsc::Receiver<NormalizedEvent>>>>,
    traces: Arc<Mutex<HashMap<String, Vec<NormalizedEvent>>>>,
    active_turn_ids: Arc<Mutex<HashMap<String, String>>>,
    /// Optional daemon-owned guard that reloads and validates the persisted
    /// binding before any vendor-native resume is attempted.
    resume_guard: Option<Arc<dyn BindingResumeGuard>>,
    launch_contexts: Option<Arc<dyn CliExecutorLaunchContextProvider>>,
}

impl AuthenticatedCliExecutor {
    pub fn new(registry: Arc<CliAdapterRegistry>, bridge: Arc<dyn CliDriverHostBridge>) -> Self {
        Self {
            registry,
            bridge,
            handles: Arc::new(Mutex::new(HashMap::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            traces: Arc::new(Mutex::new(HashMap::new())),
            active_turn_ids: Arc::new(Mutex::new(HashMap::new())),
            resume_guard: None,
            launch_contexts: None,
        }
    }

    /// Attach the daemon recovery boundary. The guard re-loads the binding
    /// and enforces process generation, executable digest, context epoch,
    /// schema, and protocol invariants before `resume()` reaches the bridge.
    pub fn with_resume_guard(mut self, guard: Arc<dyn BindingResumeGuard>) -> Self {
        self.resume_guard = Some(guard);
        self
    }

    pub fn with_launch_context_provider(
        mut self,
        provider: Arc<dyn CliExecutorLaunchContextProvider>,
    ) -> Self {
        self.launch_contexts = Some(provider);
        self
    }

    pub async fn shutdown_agent(&self, agent_id: &str) -> Result<(), AgentRuntimeError> {
        let state = self
            .handles
            .lock()
            .map_err(|_| {
                AgentRuntimeError::new("lock_poisoned", "cli executor handle map poisoned")
            })?
            .remove(agent_id)
            .ok_or_else(|| AgentRuntimeError::new("unknown_agent", "handle not registered"))?;
        let binding_id = state
            .binding_id
            .ok_or_else(|| AgentRuntimeError::new("not_open", "agent was not opened"))?;
        self.sessions
            .lock()
            .map_err(|_| {
                AgentRuntimeError::new("lock_poisoned", "cli executor session map poisoned")
            })?
            .remove(&binding_id);
        self.bridge.shutdown(&binding_id).await.map_err(|error| {
            AgentRuntimeError::new(
                "bridge_shutdown_failed",
                format!("bridge shutdown failed: {error}"),
            )
        })
    }

    pub fn handle_for(&self, agent_id: &str) -> Result<AgentRuntimeHandle, AgentRuntimeError> {
        let handles = self.handles.lock().map_err(|_| {
            AgentRuntimeError::new("lock_poisoned", "cli executor handle map poisoned")
        })?;
        let state = handles
            .get(agent_id)
            .ok_or_else(|| AgentRuntimeError::new("unknown_agent", "handle not registered"))?;
        Ok(AgentRuntimeHandle {
            session_id: state.session_id.clone(),
            agent_id: state.agent_id.clone(),
            backend: state.backend.clone(),
            binding_id: state.binding_id.clone(),
            vendor_session_id: state.vendor_session_id.clone(),
            vendor_turn_id: state.vendor_turn_id.clone(),
            model_id: state.model_id.clone(),
        })
    }

    pub fn trace_for(&self, binding_id: &str) -> Result<Vec<NormalizedEvent>, AgentRuntimeError> {
        self.traces
            .lock()
            .map_err(|_| AgentRuntimeError::new("lock_poisoned", "trace map poisoned"))?
            .get(binding_id)
            .cloned()
            .ok_or_else(|| AgentRuntimeError::new("trace_missing", "binding has no turn trace"))
    }

    fn provider_id_for_backend(
        backend: &AgentRuntimeBackend,
    ) -> Result<&CliProviderId, AgentRuntimeError> {
        match backend {
            AgentRuntimeBackend::HostNative { .. } => Err(AgentRuntimeError::new(
                "backend_mismatch",
                "AuthenticatedCliExecutor only accepts AuthenticatedCli backends",
            )),
            AgentRuntimeBackend::AuthenticatedCli { provider_id, .. } => Ok(provider_id),
        }
    }

    async fn build_launch_spec(
        &self,
        backend: &AgentRuntimeBackend,
        session_id: &str,
        agent_id: &str,
        binding_id: &str,
        resume_session_id: Option<&str>,
    ) -> Result<CliLaunchSpec, AgentRuntimeError> {
        let provider_id = Self::provider_id_for_backend(backend)?;
        let adapter = self.registry.adapter(provider_id).ok_or_else(|| {
            AgentRuntimeError::new(
                "adapter_not_registered",
                format!("no adapter registered for provider {provider_id}"),
            )
        })?;
        let (model_id, account_profile_id) = match backend {
            AgentRuntimeBackend::AuthenticatedCli {
                model_id,
                account_profile_id,
                ..
            } => (model_id.clone(), account_profile_id.clone()),
            AgentRuntimeBackend::HostNative { .. } => {
                unreachable!("provider_id_for_backend already rejected this")
            }
        };
        let contexts = self.launch_contexts.as_ref().ok_or_else(|| {
            AgentRuntimeError::new(
                "launch_context_unavailable",
                "daemon must provide managed profile, scoped Agent MCP, and run policy",
            )
        })?;
        let context = contexts.resolve(
            provider_id,
            &account_profile_id,
            session_id,
            agent_id,
            binding_id,
        )?;
        if context.profile.provider_id != *provider_id || context.profile.id != account_profile_id {
            return Err(AgentRuntimeError::new(
                "launch_context_mismatch",
                "daemon launch context does not match requested provider/profile",
            ));
        }
        if !context.profile.signed_in
            || context.profile.auth_mode
                != crate::driver_host::cli_runtime::ids::CliAuthMode::NativeCliAccount
        {
            return Err(AgentRuntimeError::new(
                "native_cli_login_required",
                "provider must have a verified native CLI account profile",
            ));
        }
        if context.mcp_server.name != "rightkit-agent" {
            return Err(AgentRuntimeError::new(
                "launch_context_mcp",
                "launch context must expose exactly Agent MCP",
            ));
        }
        let request = crate::driver_host::cli_runtime::adapter::CliLaunchRequest {
            provider_id: provider_id.clone(),
            executable: context.executable,
            profile: context.profile,
            model_id: model_id.clone(),
            session_id_seed: resume_session_id.unwrap_or(session_id).to_string(),
            mcp_server: context.mcp_server,
            policy: context.policy,
            resume: resume_session_id.is_some(),
        };
        let mut spec = adapter.build_launch(&request).await.map_err(|error| {
            AgentRuntimeError::new(
                "build_launch_failed",
                format!("adapter {} failed to build launch: {}", provider_id, error),
            )
        })?;
        if let Some(model_id) = model_id {
            spec.env
                .push(("RIGHTKIT_INTERNAL_MODEL_ID".to_string(), model_id));
        }
        let mut current = context.attestation_inputs;
        let discovery = adapter.discover(&request.profile).await.map_err(|error| {
            AgentRuntimeError::new("launch_discovery_failed", error.to_string())
        })?;
        let digest =
            crate::driver_host::cli_runtime::recovery::compute_executable_digest(&spec.executable)
                .map_err(|error| {
                    AgentRuntimeError::new("executable_digest_failed", error.to_string())
                })?;
        current.executable_sha256 = digest;
        let policy_hash = rightkit_fs::digest::sha256_bytes(request.policy.as_bytes());
        let protocol = discovery
            .protocol_version
            .unwrap_or_else(|| adapter.transport_kind().label().to_string());
        if discovery.binary_version.as_deref() != Some(current.binary_version.as_str())
            || protocol != current.protocol_version
            || spec.profile_hash != current.profile_hash
            || policy_hash != current.policy_hash
            || current.operating_system != std::env::consts::OS
            || current.architecture != std::env::consts::ARCH
            || current.isolation_mode != adapter.capabilities().isolation_mode
            || !self.registry.is_ready_for_inputs(&current)
        {
            return Err(AgentRuntimeError::new(
                "harness_verification_required",
                "current launch context lacks a matching fresh harness attestation",
            ));
        }
        Ok(spec)
    }

    pub async fn restart_agent(
        &self,
        handle: &AgentRuntimeHandle,
    ) -> Result<AgentRuntimeHandle, AgentRuntimeError> {
        let state = self
            .handles
            .lock()
            .map_err(|_| {
                AgentRuntimeError::new("lock_poisoned", "cli executor handle map poisoned")
            })?
            .get(&handle.agent_id)
            .cloned()
            .ok_or_else(|| AgentRuntimeError::new("unknown_agent", "handle not registered"))?;
        let binding_id = state
            .binding_id
            .clone()
            .ok_or_else(|| AgentRuntimeError::new("not_open", "agent was not opened"))?;
        let vendor_session_id = state
            .vendor_session_id
            .clone()
            .ok_or_else(|| AgentRuntimeError::new("no_vendor_session", "session id missing"))?;
        if let Some(guard) = &self.resume_guard {
            guard
                .validate_resume(&binding_id)
                .map_err(|error| AgentRuntimeError::new(error.code(), error.to_string()))?;
        }
        let spec = self
            .build_launch_spec(
                &state.backend,
                &state.session_id,
                &state.agent_id,
                &binding_id,
                Some(&vendor_session_id),
            )
            .await?;
        let provider_id = Self::provider_id_for_backend(&state.backend)?;
        let digest =
            crate::driver_host::cli_runtime::recovery::compute_executable_digest(&spec.executable)
                .map_err(|error| {
                    AgentRuntimeError::new("executable_digest_failed", error.to_string())
                })?;
        if !self.registry.is_ready(provider_id, &digest) {
            return Err(AgentRuntimeError::new(
                "harness_verification_required",
                "current binary lacks a matching fresh harness attestation",
            ));
        }
        self.bridge
            .shutdown(&binding_id)
            .await
            .map_err(|error| AgentRuntimeError::new("bridge_shutdown_failed", error.to_string()))?;
        let started = self
            .bridge
            .start(&binding_id, spec, DriverCursor::default())
            .await
            .map_err(|error| AgentRuntimeError::new("bridge_restart_failed", error.to_string()))?;
        self.sessions
            .lock()
            .map_err(|_| {
                AgentRuntimeError::new("lock_poisoned", "cli executor session map poisoned")
            })?
            .insert(binding_id.clone(), started.events);
        let mut handles = self.handles.lock().map_err(|_| {
            AgentRuntimeError::new("lock_poisoned", "cli executor handle map poisoned")
        })?;
        let current = handles
            .get_mut(&handle.agent_id)
            .ok_or_else(|| AgentRuntimeError::new("unknown_agent", "handle not registered"))?;
        current.vendor_session_id = Some(started.vendor_session_id);
        current.vendor_turn_id = None;
        current.model_id = started.model_id;
        drop(handles);
        self.handle_for(&handle.agent_id)
    }

    pub async fn recover_agent(
        &self,
        request: OpenAgentRequest,
        vendor_session_id: &str,
    ) -> Result<AgentRuntimeHandle, AgentRuntimeError> {
        let provider_id = Self::provider_id_for_backend(&request.backend)?;
        let binding_id = format!("binding-{}", request.agent_id);
        if let Some(guard) = &self.resume_guard {
            guard
                .validate_resume(&binding_id)
                .map_err(|error| AgentRuntimeError::new(error.code(), error.to_string()))?;
        }
        let spec = self
            .build_launch_spec(
                &request.backend,
                &request.session_id,
                &request.agent_id,
                &binding_id,
                Some(vendor_session_id),
            )
            .await?;
        let digest =
            crate::driver_host::cli_runtime::recovery::compute_executable_digest(&spec.executable)
                .map_err(|error| {
                    AgentRuntimeError::new("executable_digest_failed", error.to_string())
                })?;
        if !self.registry.is_ready(provider_id, &digest) {
            return Err(AgentRuntimeError::new(
                "harness_verification_required",
                "current binary lacks a matching fresh harness attestation",
            ));
        }
        let started = self
            .bridge
            .start(&binding_id, spec, DriverCursor::default())
            .await
            .map_err(|error| AgentRuntimeError::new("bridge_recover_failed", error.to_string()))?;
        if started.vendor_session_id != vendor_session_id {
            let _ = self.bridge.shutdown(&binding_id).await;
            return Err(AgentRuntimeError::new(
                "vendor_session_mismatch",
                "provider resumed a different native session",
            ));
        }
        let handle = AgentRuntimeHandle {
            session_id: request.session_id,
            agent_id: request.agent_id,
            backend: request.backend,
            binding_id: Some(binding_id.clone()),
            vendor_session_id: Some(started.vendor_session_id.clone()),
            vendor_turn_id: None,
            model_id: started.model_id.clone(),
        };
        self.handles
            .lock()
            .map_err(|_| {
                AgentRuntimeError::new("lock_poisoned", "cli executor handle map poisoned")
            })?
            .insert(
                handle.agent_id.clone(),
                AgentState {
                    session_id: handle.session_id.clone(),
                    agent_id: handle.agent_id.clone(),
                    role: request.role,
                    backend: handle.backend.clone(),
                    tool_ceiling: request.tool_ceiling,
                    worktree_id: request.worktree_id,
                    binding_id: Some(binding_id.clone()),
                    vendor_session_id: handle.vendor_session_id.clone(),
                    vendor_turn_id: None,
                    model_id: handle.model_id.clone(),
                    last_partial_text: String::new(),
                },
            );
        self.sessions
            .lock()
            .map_err(|_| {
                AgentRuntimeError::new("lock_poisoned", "cli executor session map poisoned")
            })?
            .insert(binding_id, started.events);
        Ok(handle)
    }
}

#[async_trait]
impl AgentRuntimeExecutor for AuthenticatedCliExecutor {
    async fn open_agent(
        &self,
        request: OpenAgentRequest,
    ) -> Result<AgentRuntimeHandle, AgentRuntimeError> {
        let provider_id = Self::provider_id_for_backend(&request.backend)?;
        let adapter = self.registry.adapter(provider_id).ok_or_else(|| {
            AgentRuntimeError::new(
                "adapter_not_registered",
                format!("no adapter registered for provider {provider_id}"),
            )
        })?;
        let transport = self
            .registry
            .transport(adapter.transport_kind())
            .ok_or_else(|| {
                AgentRuntimeError::new(
                    "transport_not_registered",
                    format!("provider {provider_id} transport is not registered"),
                )
            })?;
        if transport.kind() != adapter.transport_kind() {
            return Err(AgentRuntimeError::new(
                "transport_mismatch",
                "registry transport does not match adapter transport",
            ));
        }
        let binding_id = format!("binding-{}", request.agent_id);
        let spec = self
            .build_launch_spec(
                &request.backend,
                &request.session_id,
                &request.agent_id,
                &binding_id,
                None,
            )
            .await?;
        let digest =
            crate::driver_host::cli_runtime::recovery::compute_executable_digest(&spec.executable)
                .map_err(|error| {
                    AgentRuntimeError::new("executable_digest_failed", error.to_string())
                })?;
        if !self.registry.is_ready(provider_id, &digest) {
            return Err(AgentRuntimeError::new(
                "harness_verification_required",
                "current binary lacks a matching fresh harness attestation",
            ));
        }
        let started = self
            .bridge
            .start(&binding_id, spec, DriverCursor::default())
            .await
            .map_err(|error| {
                AgentRuntimeError::new(
                    "bridge_start_failed",
                    format!("bridge failed to start {provider_id}: {error}"),
                )
            })?;
        let handle = AgentRuntimeHandle {
            session_id: request.session_id,
            agent_id: request.agent_id,
            backend: request.backend,
            binding_id: Some(binding_id.clone()),
            vendor_session_id: Some(started.vendor_session_id.clone()),
            vendor_turn_id: None,
            model_id: started.model_id.clone(),
        };
        let state = AgentState {
            session_id: handle.session_id.clone(),
            agent_id: handle.agent_id.clone(),
            role: request.role,
            backend: handle.backend.clone(),
            tool_ceiling: request.tool_ceiling,
            worktree_id: request.worktree_id,
            binding_id: Some(binding_id.clone()),
            vendor_session_id: Some(started.vendor_session_id),
            vendor_turn_id: None,
            model_id: started.model_id,
            last_partial_text: String::new(),
        };
        {
            let mut guard = self.handles.lock().map_err(|_| {
                AgentRuntimeError::new("lock_poisoned", "cli executor handle map poisoned")
            })?;
            guard.insert(handle.agent_id.clone(), state);
        }
        {
            let mut guard = self.sessions.lock().map_err(|_| {
                AgentRuntimeError::new("lock_poisoned", "cli executor session map poisoned")
            })?;
            guard.insert(binding_id, started.events);
        }
        Ok(handle)
    }

    async fn run_turn(
        &self,
        handle: &AgentRuntimeHandle,
        request: AgentTurnRequest,
    ) -> Result<AgentTurnResult, AgentRuntimeError> {
        let state = {
            let guard = self.handles.lock().map_err(|_| {
                AgentRuntimeError::new("lock_poisoned", "cli executor handle map poisoned")
            })?;
            guard
                .get(&handle.agent_id)
                .cloned()
                .ok_or_else(|| AgentRuntimeError::new("unknown_agent", "handle not registered"))?
        };
        if request.agent_kind != state.backend.agent_kind() {
            return Err(AgentRuntimeError::new(
                "agent_kind_mismatch",
                format!(
                    "request role {:?} does not match backend role {:?}",
                    request.agent_kind,
                    state.backend.agent_kind()
                ),
            ));
        }
        let binding_id = state.binding_id.clone().ok_or_else(|| {
            AgentRuntimeError::new("not_open", "agent was not successfully opened")
        })?;
        let vendor_session_id = state.vendor_session_id.clone().ok_or_else(|| {
            AgentRuntimeError::new("no_vendor_session", "no vendor session id recorded")
        })?;
        self.bridge
            .prompt(&binding_id, &vendor_session_id, &request.prompt)
            .await
            .map_err(|error| {
                AgentRuntimeError::new(
                    "bridge_prompt_failed",
                    format!("bridge prompt failed: {error}"),
                )
            })?;
        let mut receiver = {
            let mut guard = self.sessions.lock().map_err(|_| {
                AgentRuntimeError::new("lock_poisoned", "cli executor session map poisoned")
            })?;
            guard
                .remove(&binding_id)
                .ok_or_else(|| AgentRuntimeError::new("session_closed", "no live event stream"))?
        };
        let mut trace = Vec::new();
        let result = fold_turn_result_with_identity(
            &mut receiver,
            Some(&mut trace),
            Some((self.active_turn_ids.clone(), binding_id.clone())),
        )
        .await;
        self.sessions
            .lock()
            .map_err(|_| {
                AgentRuntimeError::new("lock_poisoned", "cli executor session map poisoned")
            })?
            .insert(binding_id, receiver);
        self.traces
            .lock()
            .map_err(|_| AgentRuntimeError::new("lock_poisoned", "trace map poisoned"))?
            .insert(state.binding_id.clone().expect("open binding"), trace);
        self.active_turn_ids
            .lock()
            .map_err(|_| AgentRuntimeError::new("lock_poisoned", "turn id map poisoned"))?
            .remove(state.binding_id.as_deref().expect("open binding"));
        let result = result?;
        if result.vendor_session_id.is_some() || result.vendor_turn_id.is_some() {
            let mut handles = self.handles.lock().map_err(|_| {
                AgentRuntimeError::new("lock_poisoned", "cli executor handle map poisoned")
            })?;
            let current = handles
                .get_mut(&handle.agent_id)
                .ok_or_else(|| AgentRuntimeError::new("unknown_agent", "handle not registered"))?;
            if let Some(vendor_session_id) = result.vendor_session_id {
                current.vendor_session_id = Some(vendor_session_id);
            }
            current.vendor_turn_id = result.vendor_turn_id;
        }
        Ok(AgentTurnResult {
            agent_id: handle.agent_id.clone(),
            status: result.status,
            final_text: result.final_text,
            continuation_token: None,
            prompt_tokens: result.prompt_tokens,
            completion_tokens: result.completion_tokens,
            cache_read_tokens: result.cache_read_tokens,
            cache_creation_tokens: result.cache_creation_tokens,
        })
    }

    async fn interrupt(&self, handle: &AgentRuntimeHandle) -> Result<(), AgentRuntimeError> {
        let state = {
            let guard = self.handles.lock().map_err(|_| {
                AgentRuntimeError::new("lock_poisoned", "cli executor handle map poisoned")
            })?;
            guard
                .get(&handle.agent_id)
                .cloned()
                .ok_or_else(|| AgentRuntimeError::new("unknown_agent", "handle not registered"))?
        };
        let binding_id = state.binding_id.clone().ok_or_else(|| {
            AgentRuntimeError::new("not_open", "agent was not successfully opened")
        })?;
        let vendor_session_id = state.vendor_session_id.clone().ok_or_else(|| {
            AgentRuntimeError::new("no_vendor_session", "no vendor session id recorded")
        })?;
        let vendor_turn_id = self
            .active_turn_ids
            .lock()
            .map_err(|_| AgentRuntimeError::new("lock_poisoned", "turn id map poisoned"))?
            .get(&binding_id)
            .cloned()
            .or(state.vendor_turn_id.clone());
        self.bridge
            .interrupt(&binding_id, &vendor_session_id, vendor_turn_id.as_deref())
            .await
            .map_err(|error| {
                AgentRuntimeError::new(
                    "bridge_interrupt_failed",
                    format!("bridge interrupt failed: {error}"),
                )
            })
    }

    async fn resume(
        &self,
        handle: &AgentRuntimeHandle,
    ) -> Result<AgentRuntimeHandle, AgentRuntimeError> {
        let state = {
            let guard = self.handles.lock().map_err(|_| {
                AgentRuntimeError::new("lock_poisoned", "cli executor handle map poisoned")
            })?;
            guard
                .get(&handle.agent_id)
                .cloned()
                .ok_or_else(|| AgentRuntimeError::new("unknown_agent", "handle not registered"))?
        };
        let binding_id = state.binding_id.clone().ok_or_else(|| {
            AgentRuntimeError::new("not_open", "agent was not successfully opened")
        })?;
        if let Some(guard) = &self.resume_guard {
            guard
                .validate_resume(&binding_id)
                .map_err(|error| AgentRuntimeError::new(error.code(), error.to_string()))?;
        }
        let vendor_session_id = state.vendor_session_id.clone().ok_or_else(|| {
            AgentRuntimeError::new("no_vendor_session", "no vendor session id recorded")
        })?;
        self.bridge
            .resume(&binding_id, &vendor_session_id)
            .await
            .map_err(|error| {
                AgentRuntimeError::new(
                    "bridge_resume_failed",
                    format!("bridge resume failed: {error}"),
                )
            })?;
        Ok(handle.clone())
    }
}

/// Fold a stream of normalized vendor events into a terminal
/// [`AgentTurnResult`]. Reads events until a terminal event
/// (`TurnCompleted`, `TurnFailed`, `TurnInterrupted`) is observed,
/// or the event stream closes. Accumulates `AssistantDelta` and
/// `AssistantCompleted` text for the caller's `final_text`.
pub async fn fold_turn_result(
    events: &mut mpsc::Receiver<NormalizedEvent>,
) -> Result<(AgentTurnStatus, String), AgentRuntimeError> {
    let result = fold_turn_result_with_identity(events, None, None).await?;
    Ok((result.status, result.final_text))
}

struct FoldedTurn {
    status: AgentTurnStatus,
    final_text: String,
    vendor_session_id: Option<String>,
    vendor_turn_id: Option<String>,
    prompt_tokens: u32,
    completion_tokens: u32,
    cache_read_tokens: u32,
    cache_creation_tokens: u32,
}

#[allow(clippy::type_complexity)]
async fn fold_turn_result_with_identity(
    events: &mut mpsc::Receiver<NormalizedEvent>,
    mut trace: Option<&mut Vec<NormalizedEvent>>,
    turn_sink: Option<(Arc<Mutex<HashMap<String, String>>>, String)>,
) -> Result<FoldedTurn, AgentRuntimeError> {
    let mut final_text = String::new();
    let mut vendor_session_id = None;
    let mut vendor_turn_id = None;
    let mut prompt_tokens = 0;
    let mut completion_tokens = 0;
    let mut cache_read_tokens = 0;
    let mut cache_creation_tokens = 0;
    while let Some(event) = events.recv().await {
        if let Some(trace) = trace.as_deref_mut() {
            trace.push(event.clone());
        }
        match event.kind {
            NormalizedEventKind::AssistantDelta { text } => {
                final_text.push_str(&text);
            }
            NormalizedEventKind::AssistantCompleted { text } => {
                if !text.is_empty() {
                    final_text = text;
                }
            }
            NormalizedEventKind::TurnCompleted => {
                return Ok(FoldedTurn {
                    status: AgentTurnStatus::Completed,
                    final_text,
                    vendor_session_id,
                    vendor_turn_id,
                    prompt_tokens,
                    completion_tokens,
                    cache_read_tokens,
                    cache_creation_tokens,
                });
            }
            NormalizedEventKind::TurnCompletedWithUsage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                cache_read_tokens: cache_read,
                cache_creation_tokens: cache_creation,
            } => {
                return Ok(FoldedTurn {
                    status: AgentTurnStatus::Completed,
                    final_text,
                    vendor_session_id,
                    vendor_turn_id,
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                    cache_read_tokens: cache_read,
                    cache_creation_tokens: cache_creation,
                });
            }
            NormalizedEventKind::TurnInterrupted => {
                return Ok(FoldedTurn {
                    status: AgentTurnStatus::Interrupted,
                    final_text,
                    vendor_session_id,
                    vendor_turn_id,
                    prompt_tokens,
                    completion_tokens,
                    cache_read_tokens,
                    cache_creation_tokens,
                });
            }
            NormalizedEventKind::TurnFailed { message } => {
                let combined = if final_text.is_empty() {
                    message
                } else {
                    format!("{final_text}\n\n{message}")
                };
                return Ok(FoldedTurn {
                    status: AgentTurnStatus::Failed,
                    final_text: combined,
                    vendor_session_id,
                    vendor_turn_id,
                    prompt_tokens,
                    completion_tokens,
                    cache_read_tokens,
                    cache_creation_tokens,
                });
            }
            NormalizedEventKind::SessionStarted {
                vendor_session_id: observed_session_id,
                ..
            } => {
                vendor_session_id = Some(observed_session_id);
            }
            NormalizedEventKind::TurnStarted {
                vendor_turn_id: observed_turn_id,
            } => {
                if let Some((turn_ids, binding_id)) = &turn_sink {
                    turn_ids
                        .lock()
                        .map_err(|_| {
                            AgentRuntimeError::new("lock_poisoned", "turn id map poisoned")
                        })?
                        .insert(binding_id.clone(), observed_turn_id.clone());
                }
                vendor_turn_id = Some(observed_turn_id);
            }
            NormalizedEventKind::ToolCallStarted { .. }
            | NormalizedEventKind::ToolResult { .. }
            | NormalizedEventKind::ApprovalRequested { .. }
            | NormalizedEventKind::RateLimit { .. }
            | NormalizedEventKind::TurnCostReported { .. }
            | NormalizedEventKind::GatewayReady => {
                // Tool/approval/gateway events are surfaced via the
                // agent rail, not the executor's turn result.
            }
        }
    }
    // Stream closed without a terminal event — treat as a failure so
    // callers do not silently assume success.
    Ok(FoldedTurn {
        status: AgentTurnStatus::Failed,
        final_text: if final_text.is_empty() {
            "vendor event stream closed without a terminal result".to_string()
        } else {
            format!("{final_text}\n\nvendor event stream closed without a terminal result")
        },
        vendor_session_id,
        vendor_turn_id,
        prompt_tokens,
        completion_tokens,
        cache_read_tokens,
        cache_creation_tokens,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::cli_runtime::ids::{CliAccountProfileId, HarnessRequirements};
    use crate::driver_host::contracts::NormalizedEventKind;

    fn open_native_root() -> (HostNativeExecutor, AgentRuntimeHandle) {
        let exec = HostNativeExecutor::new();
        let backend = AgentRuntimeBackend::HostNative {
            agent_kind: AgentKind::Root,
            model_id: "n/a".into(),
        };
        let handle = futures::executor::block_on(exec.open_agent(OpenAgentRequest {
            session_id: "s".into(),
            agent_id: "a".into(),
            role: "coder".into(),
            parent_agent_id: None,
            backend,
            tool_ceiling: vec![],
            worktree_id: None,
        }))
        .unwrap();
        (exec, handle)
    }

    #[test]
    fn native_root_run_turn_returns_completed() {
        let (exec, handle) = open_native_root();
        let result = futures::executor::block_on(exec.run_turn(
            &handle,
            AgentTurnRequest {
                agent_kind: AgentKind::Root,
                prompt: "noop".into(),
                continuation_token: None,
            },
        ))
        .unwrap();
        assert_eq!(result.status, AgentTurnStatus::Completed);
        assert!(result.final_text.is_empty());
    }

    #[test]
    fn open_agent_rejects_cli_backend_for_native_executor() {
        let exec = HostNativeExecutor::new();
        let backend = AgentRuntimeBackend::AuthenticatedCli {
            agent_kind: AgentKind::Root,
            provider_id: CliProviderId::new("claude"),
            model_id: None,
            account_profile_id: CliAccountProfileId::new("claude/default"),
        };
        let error = futures::executor::block_on(exec.open_agent(OpenAgentRequest {
            session_id: "s".into(),
            agent_id: "a".into(),
            role: "coder".into(),
            parent_agent_id: None,
            backend,
            tool_ceiling: vec![],
            worktree_id: None,
        }))
        .unwrap_err();
        assert_eq!(error.code, "backend_mismatch");
    }

    #[test]
    fn native_child_run_turn_errors_without_conductor() {
        let exec = HostNativeExecutor::new();
        let backend = AgentRuntimeBackend::HostNative {
            agent_kind: AgentKind::Child,
            model_id: "n/a".into(),
        };
        let handle = futures::executor::block_on(exec.open_agent(OpenAgentRequest {
            session_id: "s".into(),
            agent_id: "a".into(),
            role: "challenger".into(),
            parent_agent_id: Some("parent".into()),
            backend,
            tool_ceiling: vec![],
            worktree_id: None,
        }))
        .unwrap();
        let error = futures::executor::block_on(exec.run_turn(
            &handle,
            AgentTurnRequest {
                agent_kind: AgentKind::Child,
                prompt: "do it".into(),
                continuation_token: None,
            },
        ))
        .unwrap_err();
        assert_eq!(error.code, "no_conductor");
    }

    #[test]
    fn native_child_run_turn_delegates_to_conductor() {
        use std::sync::Mutex;

        struct FixedConductor {
            captured: Mutex<Vec<(String, String, String, String)>>,
        }

        #[async_trait]
        impl NativeConductor for FixedConductor {
            async fn run_native_turn(
                &self,
                session_id: &str,
                agent_id: &str,
                role: &str,
                prompt: &str,
            ) -> Result<String, NativeConductorError> {
                self.captured.lock().unwrap().push((
                    session_id.to_string(),
                    agent_id.to_string(),
                    role.to_string(),
                    prompt.to_string(),
                ));
                Ok("conductor answer".to_string())
            }
        }

        let conductor = Arc::new(FixedConductor {
            captured: Mutex::new(Vec::new()),
        });
        let exec = HostNativeExecutor::new().with_conductor(conductor.clone());
        let backend = AgentRuntimeBackend::HostNative {
            agent_kind: AgentKind::Child,
            model_id: "n/a".into(),
        };
        let handle = futures::executor::block_on(exec.open_agent(OpenAgentRequest {
            session_id: "s".into(),
            agent_id: "a".into(),
            role: "challenger".into(),
            parent_agent_id: Some("parent".into()),
            backend,
            tool_ceiling: vec![],
            worktree_id: None,
        }))
        .unwrap();
        let result = futures::executor::block_on(exec.run_turn(
            &handle,
            AgentTurnRequest {
                agent_kind: AgentKind::Child,
                prompt: "do it".into(),
                continuation_token: None,
            },
        ))
        .unwrap();
        assert_eq!(result.status, AgentTurnStatus::Completed);
        assert_eq!(result.final_text, "conductor answer");
        let captured = conductor.captured.lock().unwrap().clone();
        assert_eq!(
            captured,
            vec![(
                "s".to_string(),
                "a".to_string(),
                "challenger".to_string(),
                "do it".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn fold_turn_result_returns_completed_with_delta_text() {
        let (tx, mut rx) = mpsc::channel(8);
        tx.send(NormalizedEvent {
            event_key: "k1".into(),
            driver: DriverKind::Claude,
            source_offset: 1,
            vendor_method: "stream_event".into(),
            kind: NormalizedEventKind::AssistantDelta {
                text: "hello ".into(),
            },
        })
        .await
        .unwrap();
        tx.send(NormalizedEvent {
            event_key: "k2".into(),
            driver: DriverKind::Claude,
            source_offset: 2,
            vendor_method: "stream_event".into(),
            kind: NormalizedEventKind::AssistantDelta {
                text: "world".into(),
            },
        })
        .await
        .unwrap();
        tx.send(NormalizedEvent {
            event_key: "k3".into(),
            driver: DriverKind::Claude,
            source_offset: 3,
            vendor_method: "result".into(),
            kind: NormalizedEventKind::TurnCompleted,
        })
        .await
        .unwrap();
        drop(tx);
        let (status, text) = fold_turn_result(&mut rx).await.unwrap();
        assert_eq!(status, AgentTurnStatus::Completed);
        assert_eq!(text, "hello world");
    }

    #[tokio::test]
    async fn fold_turn_result_returns_failed_when_event_stream_closes_without_terminal() {
        let (tx, mut rx) = mpsc::channel(2);
        tx.send(NormalizedEvent {
            event_key: "k1".into(),
            driver: DriverKind::Codex,
            source_offset: 1,
            vendor_method: "item/agentMessage/delta".into(),
            kind: NormalizedEventKind::AssistantDelta {
                text: "partial".into(),
            },
        })
        .await
        .unwrap();
        drop(tx);
        let (status, text) = fold_turn_result(&mut rx).await.unwrap();
        assert_eq!(status, AgentTurnStatus::Failed);
        assert!(text.contains("partial"));
        assert!(text.contains("closed without a terminal result"));
    }

    #[tokio::test]
    async fn fold_turn_result_returns_failed_when_turn_failed() {
        let (tx, mut rx) = mpsc::channel(2);
        tx.send(NormalizedEvent {
            event_key: "k1".into(),
            driver: DriverKind::Codex,
            source_offset: 1,
            vendor_method: "turn/completed".into(),
            kind: NormalizedEventKind::TurnFailed {
                message: "boom".into(),
            },
        })
        .await
        .unwrap();
        drop(tx);
        let (status, text) = fold_turn_result(&mut rx).await.unwrap();
        assert_eq!(status, AgentTurnStatus::Failed);
        assert!(text.contains("boom"));
    }

    #[test]
    fn agent_kind_default_is_root() {
        assert_eq!(AgentKind::default(), AgentKind::Root);
    }

    #[test]
    fn agent_kind_predicates_match() {
        assert!(AgentKind::Root.is_root());
        assert!(!AgentKind::Root.is_child());
        assert!(AgentKind::Child.is_child());
        assert!(!AgentKind::Child.is_root());
    }

    #[test]
    fn backend_agent_kind_returns_carried_value() {
        let native = AgentRuntimeBackend::HostNative {
            agent_kind: AgentKind::Child,
            model_id: "m".into(),
        };
        assert_eq!(native.agent_kind(), AgentKind::Child);
        let cli = AgentRuntimeBackend::AuthenticatedCli {
            agent_kind: AgentKind::Root,
            provider_id: CliProviderId::new("claude"),
            model_id: None,
            account_profile_id: CliAccountProfileId::new("claude/default"),
        };
        assert_eq!(cli.agent_kind(), AgentKind::Root);
    }

    #[test]
    fn native_executor_reports_native_kind() {
        let executor = HostNativeExecutor::new();
        assert_eq!(executor.kind(), ExecutorKind::Native);
    }

    #[test]
    fn registry_has_no_unregistered_cli_provider() {
        let registry = Arc::new(CliAdapterRegistry::new());
        assert!(registry.adapter(&CliProviderId::new("claude")).is_none());
    }

    #[test]
    fn registry_retains_registered_cli_provider() {
        use crate::driver_host::cli_runtime::providers::claude::ClaudeAdapter;
        use crate::driver_host::cli_runtime::providers::codex::CodexAdapter;

        let registry = Arc::new(CliAdapterRegistry::new());
        let _ = registry.register_adapter(Arc::new(ClaudeAdapter::new())
            as Arc<dyn crate::driver_host::cli_runtime::adapter::CliProviderAdapter>);
        let _ = registry.register_adapter(Arc::new(CodexAdapter::new())
            as Arc<dyn crate::driver_host::cli_runtime::adapter::CliProviderAdapter>);

        assert!(registry.adapter(&CliProviderId::new("claude")).is_some());
    }

    #[test]
    fn registry_rejects_inconsistent_acp_provider() {
        let registry = Arc::new(CliAdapterRegistry::new());
        // Register a hand-rolled adapter with a non-Claude/Codex
        // transport; for PR 4 those are still pending.
        struct AcpAdapter;
        #[async_trait]
        impl crate::driver_host::cli_runtime::adapter::CliProviderAdapter for AcpAdapter {
            fn id(&self) -> &CliProviderId {
                static ID: std::sync::OnceLock<CliProviderId> = std::sync::OnceLock::new();
                ID.get_or_init(|| CliProviderId::new("grok"))
            }
            fn display_name(&self) -> &'static str {
                "Grok"
            }
            fn transport_kind(&self) -> CliTransportKind {
                CliTransportKind::Acp
            }
            async fn discover(
                &self,
                _profile: &crate::driver_host::cli_runtime::ids::CliAccountProfile,
            ) -> Result<
                crate::driver_host::cli_runtime::adapter::CliDiscovery,
                crate::driver_host::cli_runtime::adapter::CliAdapterError,
            > {
                unimplemented!()
            }
            async fn authentication_status(
                &self,
                _profile: &crate::driver_host::cli_runtime::ids::CliAccountProfile,
            ) -> Result<
                crate::driver_host::cli_runtime::adapter::CliAuthenticationStatus,
                crate::driver_host::cli_runtime::adapter::CliAdapterError,
            > {
                unimplemented!()
            }
            async fn list_models(
                &self,
                _profile: &crate::driver_host::cli_runtime::ids::CliAccountProfile,
            ) -> Result<
                crate::driver_host::cli_runtime::adapter::CliModelCatalog,
                crate::driver_host::cli_runtime::adapter::CliAdapterError,
            > {
                unimplemented!()
            }
            async fn build_launch(
                &self,
                _request: &crate::driver_host::cli_runtime::adapter::CliLaunchRequest,
            ) -> Result<
                crate::driver_host::cli_runtime::adapter::CliLaunchSpec,
                crate::driver_host::cli_runtime::adapter::CliAdapterError,
            > {
                unimplemented!()
            }
            fn capabilities(
                &self,
            ) -> crate::driver_host::cli_runtime::ids::CliProviderCapabilities {
                crate::driver_host::cli_runtime::ids::CliProviderCapabilities::default()
            }
            fn harness_requirements(&self) -> HarnessRequirements {
                HarnessRequirements::default()
            }
            fn qualification_suite(
                &self,
            ) -> &'static [crate::driver_host::cli_runtime::ids::QualificationProbe] {
                &[]
            }
            fn normalize_provider_event(
                &self,
                _event: serde_json::Value,
            ) -> Result<
                Option<crate::driver_host::contracts::NormalizedEvent>,
                crate::driver_host::cli_runtime::adapter::CliAdapterError,
            > {
                Ok(None)
            }
        }
        let error = registry
            .register_adapter(Arc::new(AcpAdapter)
                as Arc<dyn crate::driver_host::cli_runtime::adapter::CliProviderAdapter>)
            .expect_err("inconsistent default requirements must be rejected");
        assert!(matches!(
            error,
            crate::driver_host::cli_runtime::registry::RegistryError::InconsistentRequirements(_)
        ));
    }
}
