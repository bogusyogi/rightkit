//! Provider adapter, transport, and agent runtime traits.
//!
//! PR 1 of the architecture plan. The four traits in this file are
//! the cross-provider seams:
//!
//! - [`CliProviderAdapter`] — owns discovery, auth, model catalogue,
//!   launch-arg construction, and per-event normalization for one
//!   vendor CLI.
//! - [`CliTransport`] — owns the protocol-level session lifecycle
//!   for one transport kind (Claude stream JSON, Codex app-server,
//!   ACP, or resumable NDJSON). Independent of provider identity.
//! - [`AgentRuntimeBackend`] — the closed enum a the host app agent
//!   executor dispatches against. `CodeRightNative` keeps the
//!   existing spawned session; `AuthenticatedCli` is the new path.
//! - [`AgentRuntimeExecutor`] — the common execution contract used
//!   by both root orchestrators and child agents.
//!
//! Adapters compose a transport; transports do not know providers;
//! executors do not know transports. The ownership boundaries are
//! deliberate.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::driver_host::cli_runtime::ids::{
    CliAccountProfile, CliAuthMode, CliProviderCapabilities, CliProviderId, CliTransportKind,
    HarnessRequirements, QualificationProbe,
};
use crate::driver_host::contracts::NormalizedEvent;

pub fn native_profile_hash(profile: &CliAccountProfile) -> String {
    let mut hash = rightkit_fs::digest::Sha256Hasher::new();
    hash.update(profile.id.as_str().as_bytes());
    hash.update(b"\0");
    if let Some(fingerprint) = &profile.account_fingerprint {
        hash.update(fingerprint.as_bytes());
    }
    hash.finalize().chars().take(16).collect()
}

/// Concrete request issued by the executor when it asks an adapter to
/// build a launch specification for a binding. Identity is established
/// upstream; the adapter only translates it into vendor argv.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliLaunchRequest {
    pub provider_id: CliProviderId,
    pub executable: PathBuf,
    pub profile: CliAccountProfile,
    pub model_id: Option<String>,
    pub session_id_seed: String,
    pub mcp_server: CliMcpServerSpec,
    pub policy: String,
    pub resume: bool,
}

/// The single MCP server the binding is allowed to talk to. The
/// adapter is responsible for passing exactly one entry to the
/// provider; adding a second MCP server is a forbidden configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliMcpServerSpec {
    pub name: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Resolved launch specification returned by an adapter. The
/// supervisor consumes this directly; the adapter does not launch
/// processes itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliLaunchSpec {
    pub provider_id: CliProviderId,
    pub transport: CliTransportKind,
    pub executable: PathBuf,
    pub argv: Vec<String>,
    pub env_keep: Vec<String>,
    pub env_remove: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cwd: PathBuf,
    pub mcp_server: Option<CliMcpServerSpec>,
    pub profile_hash: String,
    pub session_id: String,
}

/// Discovery result for one provider on one host. Stable across calls
/// so the daemon can cache it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliDiscovery {
    pub installed: bool,
    pub executable: Option<PathBuf>,
    pub binary_version: Option<String>,
    pub executable_sha256: Option<String>,
    pub protocol_version: Option<String>,
    pub transport: CliTransportKind,
}

/// Authentication status for one profile. `auth_mode` is the
/// evidence-derived mode; the executor refuses to launch when it is
/// anything other than `NativeCliAccount`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliAuthenticationStatus {
    pub signed_in: bool,
    pub auth_mode: CliAuthMode,
    pub account_label: Option<String>,
    pub account_fingerprint: Option<String>,
    pub source_label: String,
    pub observed_at: String,
}

/// Catalog of models an adapter exposes for a profile. Adapters
/// obtain this by querying the vendor CLI; the host app never invents
/// model IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliModelCatalog {
    pub models: Vec<String>,
    pub source: String,
    pub fetched_at: String,
}

/// Reason a native login attempt failed. Distinguishing "user
/// cancelled the browser flow" from "the binary refused to launch"
/// matters because the repair action is different.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliAdapterError {
    pub code: String,
    pub message: String,
    pub provider_id: CliProviderId,
}

impl std::fmt::Display for CliAdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {} ({})", self.provider_id, self.message, self.code)
    }
}

impl std::error::Error for CliAdapterError {}

impl CliAdapterError {
    pub fn new(
        provider_id: CliProviderId,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            provider_id,
        }
    }
}

/// Errors raised by a transport implementation. Kept distinct from
/// `CliAdapterError` so adapter code and protocol code do not share
/// error types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliRuntimeError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for CliRuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for CliRuntimeError {}

impl CliRuntimeError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Minimal UI handle an adapter receives during a native login.
/// Adapters drive the browser/device flow themselves; this trait is
/// how they surface a "follow this URL" or "press enter to confirm"
/// instruction to the desktop shell.
#[async_trait]
pub trait NativeLoginUi: Send + Sync {
    async fn present_browser_url(&self, url: &str) -> Result<(), CliAdapterError>;
    async fn present_cli_prompt(&self, message: &str) -> Result<(), CliAdapterError>;
    async fn report_progress(&self, message: &str) -> Result<(), CliAdapterError>;
}

/// The provider adapter. One implementation per vendor CLI. Owns
/// discovery, auth, model catalog, launch-arg construction, and
/// per-event normalization. Does **not** own the transport.
#[async_trait]
pub trait CliProviderAdapter: Send + Sync {
    fn id(&self) -> &CliProviderId;
    fn display_name(&self) -> &'static str;
    fn transport_kind(&self) -> CliTransportKind;

    async fn discover(&self, profile: &CliAccountProfile) -> Result<CliDiscovery, CliAdapterError>;

    async fn authentication_status(
        &self,
        profile: &CliAccountProfile,
    ) -> Result<CliAuthenticationStatus, CliAdapterError>;

    async fn begin_native_login(
        &self,
        profile: &CliAccountProfile,
        ui: &dyn NativeLoginUi,
    ) -> Result<(), CliAdapterError>;

    async fn list_models(
        &self,
        profile: &CliAccountProfile,
    ) -> Result<CliModelCatalog, CliAdapterError>;

    async fn build_launch(
        &self,
        request: &CliLaunchRequest,
    ) -> Result<CliLaunchSpec, CliAdapterError>;

    fn capabilities(&self) -> CliProviderCapabilities;
    fn harness_requirements(&self) -> HarnessRequirements;
    fn qualification_suite(&self) -> &'static [QualificationProbe];

    fn normalize_provider_event(
        &self,
        event: serde_json::Value,
    ) -> Result<Option<NormalizedEvent>, CliAdapterError>;
}

/// The transport. One implementation per transport kind. Multiple
/// providers may share a transport; ACP in particular is shared
/// across Grok, Cursor, and Cline.
#[async_trait]
pub trait CliTransport: Send + Sync {
    fn kind(&self) -> CliTransportKind;

    async fn start(&self, launch: &CliLaunchSpec) -> Result<StartedCliProcess, CliRuntimeError>;

    async fn create_session(
        &self,
        process: &StartedCliProcess,
        request: CreateVendorSession,
    ) -> Result<VendorSessionHandle, CliRuntimeError>;

    async fn load_session(
        &self,
        process: &StartedCliProcess,
        request: LoadVendorSession,
    ) -> Result<VendorSessionHandle, CliRuntimeError>;

    async fn prompt(
        &self,
        session: &VendorSessionHandle,
        prompt: CliPrompt,
    ) -> Result<VendorTurnHandle, CliRuntimeError>;

    /// Read one raw vendor frame after `prompt`. The daemon passes it through
    /// the selected adapter's normalizer before persistence, so transports do
    /// not need provider-specific event types.
    async fn read_event(
        &self,
        session: &VendorSessionHandle,
    ) -> Result<Option<serde_json::Value>, CliRuntimeError> {
        session.process.inner.read_event()
    }

    async fn interrupt(&self, turn: &VendorTurnHandle) -> Result<(), CliRuntimeError>;

    async fn close(&self, session: &VendorSessionHandle) -> Result<(), CliRuntimeError>;
}

/// Process handle returned by a transport's `start`. The supervisor
/// owns its lifetime; the transport only reads and writes to it.
pub struct StartedCliProcess {
    pub pid: u32,
    pub generation: u64,
    pub inner: Box<dyn CliProcessIo + Send + Sync>,
}

impl std::fmt::Debug for StartedCliProcess {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StartedCliProcess")
            .field("pid", &self.pid)
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

/// Process I/O interface. Each transport implements this for its
/// concrete protocol (NDJSON framing, JSON-RPC, ACP message codec).
///
/// Implementations must be cloneable through [`CliProcessIo::clone_box`]
/// so the session layer can attach a [`VendorSessionHandle`] to a
/// process while keeping the supervisor as the sole owner of the
/// underlying I/O state. The default blanket impl derives `Clone`
/// when the implementer is `Clone`; transports that wrap non-`Clone`
/// state must provide their own.
pub trait CliProcessIo: Send + Sync {
    fn write_line(&self, line: &str) -> Result<(), CliRuntimeError>;
    fn read_event(&self) -> Result<Option<serde_json::Value>, CliRuntimeError>;
    fn shutdown(&self) -> Result<(), CliRuntimeError>;
    /// Clone this I/O handle into a new boxed instance. The default
    /// impl works for types that derive `Clone`; transport-specific
    /// implementations override only when they wrap non-`Clone` state
    /// (e.g. a `tokio::ChildStdin`).
    fn clone_box(&self) -> Box<dyn CliProcessIo>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateVendorSession {
    pub model_id: Option<String>,
    pub mcp_server: Option<CliMcpServerSpec>,
    pub system_policy: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadVendorSession {
    pub vendor_session_id: String,
    pub model_id: Option<String>,
    pub mcp_server: Option<CliMcpServerSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliPrompt {
    pub text: String,
    pub tool_ceiling: Vec<String>,
    pub model_id: Option<String>,
}

#[derive(Debug)]
pub struct VendorSessionHandle {
    pub provider_id: CliProviderId,
    pub transport: CliTransportKind,
    pub vendor_session_id: String,
    pub model_id: Option<String>,
    pub process: StartedCliProcess,
}

#[derive(Debug)]
pub struct VendorTurnHandle {
    pub vendor_turn_id: Option<String>,
    pub session: VendorSessionHandle,
}

/// The closed backend enum used by both root orchestrators and child
/// agents. Root-vs-child is a logical role, not a backend variant —
/// the role lives in [`AgentKind`] and is attached to every backend
/// value so the executor can dispatch correctly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentRuntimeBackend {
    HostNative {
        agent_kind: AgentKind,
        model_id: String,
    },
    AuthenticatedCli {
        agent_kind: AgentKind,
        provider_id: CliProviderId,
        model_id: Option<String>,
        account_profile_id: crate::driver_host::cli_runtime::ids::CliAccountProfileId,
    },
}

impl AgentRuntimeBackend {
    pub fn provider_id(&self) -> Option<&CliProviderId> {
        match self {
            AgentRuntimeBackend::HostNative { .. } => None,
            AgentRuntimeBackend::AuthenticatedCli { provider_id, .. } => Some(provider_id),
        }
    }

    /// The agent kind carried by this backend. Root vs child is a
    /// logical role on the logical agent, not a backend variant.
    pub fn agent_kind(&self) -> AgentKind {
        match self {
            AgentRuntimeBackend::HostNative { agent_kind, .. } => *agent_kind,
            AgentRuntimeBackend::AuthenticatedCli { agent_kind, .. } => *agent_kind,
        }
    }
}

/// Logical role of a the host app agent. Same backend type may act as a
/// root user-facing session or as a child agent delegated to by a
/// parent. The role is attached to the backend and turn request so
/// the executor can enforce per-role policy (e.g. children carry
/// handoff briefs and bounded budgets; roots do not).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    /// The user-facing session for the the host app run. Roots own the
    /// conductor surface and do not carry a the host app handoff brief.
    #[default]
    Root,
    /// An agent spawned by a parent. Children carry delegation
    /// metadata, restricted tool ceilings, and bounded budgets.
    Child,
}

impl AgentKind {
    /// True for the user-facing root session of a the host app run.
    pub fn is_root(self) -> bool {
        matches!(self, AgentKind::Root)
    }

    /// True for a delegated child agent.
    pub fn is_child(self) -> bool {
        matches!(self, AgentKind::Child)
    }
}

/// Errors raised by an agent runtime executor. Kept separate from
/// transport and adapter errors so the executor can wrap them
/// without confusing callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRuntimeError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for AgentRuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AgentRuntimeError {}

impl AgentRuntimeError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenAgentRequest {
    pub session_id: String,
    pub agent_id: String,
    pub role: String,
    pub parent_agent_id: Option<String>,
    pub backend: AgentRuntimeBackend,
    pub tool_ceiling: Vec<String>,
    pub worktree_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTurnRequest {
    /// The role this turn is acting under. The executor uses this to
    /// decide whether to require a handoff brief (child) or treat the
    /// prompt as a direct user turn (root).
    pub agent_kind: AgentKind,
    pub prompt: String,
    pub continuation_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTurnResult {
    pub agent_id: String,
    pub status: AgentTurnStatus,
    pub final_text: String,
    pub continuation_token: Option<String>,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTurnStatus {
    Completed,
    Failed,
    Interrupted,
    WaitingForApproval,
    WaitingForChild,
}

#[derive(Debug, Clone)]
pub struct AgentRuntimeHandle {
    pub session_id: String,
    pub agent_id: String,
    pub backend: AgentRuntimeBackend,
    pub binding_id: Option<String>,
    pub vendor_session_id: Option<String>,
    pub vendor_turn_id: Option<String>,
    pub model_id: Option<String>,
}

/// Common execution contract. Both `CodeRightNativeExecutor` and
/// `AuthenticatedCliExecutor` implement this. The executor owns the
/// per-agent lifecycle; transport and adapter are pulled in only when
/// the backend needs them.
#[async_trait]
pub trait AgentRuntimeExecutor: Send + Sync {
    /// Selector the dispatcher uses to decide which backend the
    /// executor drives. The default is [`ExecutorKind::Pending`] so a
    /// stub does not accidentally claim a real backend.
    fn kind(&self) -> ExecutorKind {
        ExecutorKind::Pending
    }

    async fn open_agent(
        &self,
        request: OpenAgentRequest,
    ) -> Result<AgentRuntimeHandle, AgentRuntimeError>;

    async fn run_turn(
        &self,
        handle: &AgentRuntimeHandle,
        request: AgentTurnRequest,
    ) -> Result<AgentTurnResult, AgentRuntimeError>;

    async fn interrupt(&self, handle: &AgentRuntimeHandle) -> Result<(), AgentRuntimeError>;

    async fn resume(
        &self,
        handle: &AgentRuntimeHandle,
    ) -> Result<AgentRuntimeHandle, AgentRuntimeError>;
}

/// Selector returned by [`AgentRuntimeExecutor::kind`]. Used by the
/// registry to report which backend an executor actually drives. The
/// `Pending` variant marks a stub executor — wiring for it lands in
/// a later PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorKind {
    /// The executor drives the the host app-native session runtime.
    Native,
    /// The executor drives an authenticated vendor CLI through the
    /// CLI host bridge.
    AuthenticatedCli,
    /// The executor is a placeholder for a backend whose concrete
    /// implementation is not yet wired.
    Pending,
}
