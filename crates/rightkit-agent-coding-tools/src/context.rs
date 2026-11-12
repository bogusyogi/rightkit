//! Tool execution context — passed to every tool's `execute` method.

use rightkit_agent_engine_protocol::ArtifactTrustStore;
use rightkit_agent_policy::{ContentProvenance, PermissionMode, WorkspaceTrust};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// Secret received through an in-turn credential prompt. Debug output is
/// always redacted so it can safely cross ordinary error/log boundaries.
#[derive(Clone)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretString(<redacted>)")
    }
}

#[derive(Debug, Clone)]
pub struct CredentialPrompt {
    pub provider: String,
    pub reason: String,
}

/// Host callback for collecting one credential while its exact tool call is
/// paused. `None` means operator declined/cancelled.
pub trait CredentialRequester: Send + Sync {
    fn request(
        &self,
        tool_call_id: String,
        prompt: CredentialPrompt,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Option<SecretString>, String>> + Send>>;
}

/// Process-wide, session-keyed store for a Cloudflare credential refreshed
/// mid-session (`update_cloudflare_credential`). This exists alongside
/// `ToolContext::cloudflare_config` — which the engine clones fresh from its
/// own field into every dispatched call — because the engine's field lives
/// behind a per-turn lock the host cannot always reach without blocking on an
/// in-flight turn. This registry lets a refreshed token land for the NEXT
/// Cloudflare tool call in a session without waiting on that lock, and
/// without disturbing a call already in flight (it already holds its own
/// cloned/read snapshot of the credential).
///
/// Each session gets one `Arc<RwLock<crate::cloudflare::CloudflareConfig>>`
/// cell. Swapping the value is a single `write()` guard; readers never see a
/// torn value — an in-progress `read()` still observes the old value in full,
/// and any `read()` started after `store_cloudflare_credential` returns sees
/// the new value in full.
type CloudflareCredentialCell = Arc<RwLock<crate::cloudflare::CloudflareConfig>>;

fn cloudflare_credential_registry() -> &'static Mutex<HashMap<String, CloudflareCredentialCell>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, CloudflareCredentialCell>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Mid-session Cloudflare credential refresh. Swaps (or creates) the cell for
/// `session_id` so the next lookup via [`cloudflare_credential`] observes the
/// new token.
pub fn update_cloudflare_credential(session_id: &str, config: crate::cloudflare::CloudflareConfig) {
    let mut registry = cloudflare_credential_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match registry.get(session_id) {
        Some(cell) => {
            let mut guard = cell
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = config;
        }
        None => {
            registry.insert(session_id.to_string(), Arc::new(RwLock::new(config)));
        }
    }
}

/// Read the current mid-session-refreshed Cloudflare credential for
/// `session_id`, if `update_cloudflare_credential` has ever been called for
/// it. Falls back to `ToolContext::cloudflare_config` (the session-start
/// value) when absent.
pub fn cloudflare_credential(session_id: &str) -> Option<crate::cloudflare::CloudflareConfig> {
    let registry = cloudflare_credential_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    registry.get(session_id).map(|cell| {
        cell.read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    })
}

/// Drop the refreshed-credential cell for a session. Callers should invoke
/// this alongside other per-session cleanup (see
/// `retire_session_compaction_state` in the engine crate) so a long-running
/// daemon does not accumulate one entry per session forever.
pub fn retire_cloudflare_credential(session_id: &str) {
    cloudflare_credential_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id);
}

type FlyioCredentialCell = Arc<RwLock<crate::flyio::FlyioConfig>>;

fn flyio_credential_registry() -> &'static Mutex<HashMap<String, FlyioCredentialCell>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, FlyioCredentialCell>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Mid-session Fly.io credential refresh, mirroring
/// [`update_cloudflare_credential`].
pub fn update_flyio_credential(session_id: &str, config: crate::flyio::FlyioConfig) {
    let mut registry = flyio_credential_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match registry.get(session_id) {
        Some(cell) => {
            let mut guard = cell
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = config;
        }
        None => {
            registry.insert(session_id.to_string(), Arc::new(RwLock::new(config)));
        }
    }
}

/// Read the current mid-session-refreshed Fly.io credential for `session_id`.
pub fn flyio_credential(session_id: &str) -> Option<crate::flyio::FlyioConfig> {
    let registry = flyio_credential_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    registry.get(session_id).map(|cell| {
        cell.read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    })
}

/// Drop the refreshed Fly.io credential cell for a session.
pub fn retire_flyio_credential(session_id: &str) {
    flyio_credential_registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(session_id);
}

/// Selector for the runtime that should host a sub-agent. PR 1 of the
/// multi-provider CLI runtime architecture: this enum mirrors
/// `host_driver_host::cli_runtime::AgentRuntimeBackend` so that
/// `tools` does not need to depend on `driver_host`. The spawner
/// implementation is responsible for translating this into the
/// driver's `AgentRuntimeBackend` (or rejecting it) at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SpawnRuntimeKind {
    /// A the host app-native spawned session (the existing path).
    HostNative {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_id: Option<String>,
        model_id: String,
    },
    /// An authenticated vendor-CLI session. `provider_id` is the
    /// string identifier declared by the built-in registry
    /// (`claude`, `codex`, `grok`, `cursor`, `cline`,
    /// `commandcode`). `account_profile_id` is `<provider>/<label>`
    /// and resolves to the vendor's managed profile directory.
    AuthenticatedCli {
        provider_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model_id: Option<String>,
        account_profile_id: String,
    },
}

impl SpawnRuntimeKind {
    /// `true` when the runtime is an authenticated vendor CLI.
    pub fn is_authenticated_cli(&self) -> bool {
        matches!(self, SpawnRuntimeKind::AuthenticatedCli { .. })
    }

    /// Parse the optional camelCase JSON object shape used by the
    /// `spawn_subagent`, `delegate_task`, and `delegate_parallel` tool
    /// schemas. Native routes retain legacy semantics when no provider is
    /// supplied, while a provider+model pair remains scoped to this call.
    ///
    /// `value` is whatever `args.get("runtime")` returned — pass
    /// `&Value::Null` when the field is absent to get `Ok(None)`.
    pub fn from_tool_args(value: &serde_json::Value) -> Result<Option<Self>, String> {
        if value.is_null() {
            return Ok(None);
        }
        let Some(obj) = value.as_object() else {
            return Err("spawn options: 'runtime' must be an object".to_string());
        };
        let kind = obj
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "spawn options: 'runtime.kind' is required".to_string())?;
        match kind {
            "authenticated_cli" => {
                let provider_id = obj
                    .get("providerId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        "spawn options: 'runtime.providerId' is required for authenticated_cli"
                            .to_string()
                    })?
                    .to_string();
                let model_id = obj
                    .get("modelId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let account_profile_id = obj
                    .get("accountProfileId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        "spawn options: 'runtime.accountProfileId' is required for authenticated_cli"
                            .to_string()
                    })?
                    .to_string();
                Ok(Some(SpawnRuntimeKind::AuthenticatedCli {
                    provider_id,
                    model_id,
                    account_profile_id,
                }))
            }
            "rightkit_native" => {
                let provider_id = obj
                    .get("providerId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string);
                let model_id = obj
                    .get("modelId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_default();
                if provider_id.is_some() && model_id.is_empty() {
                    return Err(
                        "spawn options: 'runtime.modelId' is required with 'runtime.providerId'"
                            .to_string(),
                    );
                }
                Ok(Some(SpawnRuntimeKind::HostNative {
                    provider_id,
                    model_id,
                }))
            }
            other => Err(format!(
                "spawn options: 'runtime.kind' must be 'authenticated_cli' or 'rightkit_native', got {other:?}"
            )),
        }
    }

    /// Return an explicit native provider/model pair scoped to this call.
    pub fn native_provider_model(&self) -> Option<(String, String)> {
        let Self::HostNative {
            provider_id: Some(provider_id),
            model_id,
        } = self
        else {
            return None;
        };
        let provider_id = provider_id.trim();
        let model_id = model_id.trim();
        (!provider_id.is_empty() && !model_id.is_empty())
            .then(|| (provider_id.to_string(), model_id.to_string()))
    }
}

impl Default for SpawnRuntimeKind {
    fn default() -> Self {
        SpawnRuntimeKind::HostNative {
            provider_id: None,
            model_id: String::new(),
        }
    }
}

/// Callback for spawning a sub-agent from within a tool.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubagentSpawnOptions {
    pub worktree: Option<SubagentWorktreeOptions>,
    pub max_iterations: Option<u32>,
    /// Internal authority override for trusted orchestrators. The public
    /// spawn_subagent tool never accepts this from model-controlled JSON.
    pub permission_mode: Option<PermissionMode>,
    /// Optional tool allow/deny profile supplied by a trusted orchestrator.
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    /// Per-child ceiling in millionths of a US dollar, avoiding floats in the
    /// shared spawn contract while retaining sub-cent precision.
    pub budget_microusd: Option<u64>,
    /// Optional runtime selection. When `None` the spawner falls
    /// back to the legacy the host app-native session, preserving the
    /// existing behavior for every current call site. When `Some`,
    /// the spawner must validate the runtime against the session's
    /// `CliDelegationPolicy` and translate the value into a typed
    /// `AgentRuntimeBackend`.
    pub runtime: Option<SpawnRuntimeKind>,
    /// Continuity preference for the child. `New` always creates a
    /// fresh child; `ContinueAgent` re-attaches to an existing
    /// child by id (used for follow-ups and retries).
    pub continuity: Option<SubagentContinuity>,
    /// Opt-in escape hatch from this product's default of zero visible
    /// console windows: when `true`, the spawn opens a real, visible
    /// `host app attach <session>` window (the actual interactive TUI, not
    /// a raw log dump) so an operator can watch this one subagent work.
    /// Defaults to `false` via `#[derive(Default)]` — a spawn is never
    /// visible unless explicitly requested. See
    /// `crate::visible_subagent_window`.
    pub visible: bool,
    /// Caller-allocated session id for the child, when the caller must know
    /// the child's identity *before* the spawn future resolves.
    ///
    /// A `visible` spawn needs this: the watch window attaches to the child's
    /// session, but the host normally mints that id inside the spawn future,
    /// where no caller can observe it in time. When `Some`, the host MUST use
    /// this id for the child instead of minting one; when `None` it mints as
    /// before. Ignored by hosts that do not create sessions.
    pub child_session_id: Option<String>,
}

/// Continuity preference for a sub-agent spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubagentContinuity {
    New,
    ContinueAgent { continue_agent_id: String },
}

impl Default for SubagentContinuity {
    fn default() -> Self {
        SubagentContinuity::New
    }
}

impl SubagentContinuity {
    /// Parse the optional camelCase JSON object shape used by the
    /// `spawn_subagent`, `delegate_task`, and `delegate_parallel` tool
    /// schemas. Returns `Ok(None)` when `value` is `null`/`None`/missing,
    /// `Ok(Some(SubagentContinuity::New))` for `{kind: "new"}`, and
    /// `Ok(Some(SubagentContinuity::ContinueAgent { .. }))` for
    /// `{kind: "continue_agent", continueAgentId: "..."}`.
    pub fn from_tool_args(value: &serde_json::Value) -> Result<Option<Self>, String> {
        if value.is_null() {
            return Ok(None);
        }
        let Some(obj) = value.as_object() else {
            return Err("spawn options: 'continuity' must be an object".to_string());
        };
        let kind = obj
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "spawn options: 'continuity.kind' is required".to_string())?;
        match kind {
            "new" => Ok(Some(SubagentContinuity::New)),
            "continue_agent" => {
                let continue_agent_id = obj
                    .get("continueAgentId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        "spawn options: 'continuity.continueAgentId' is required for continue_agent"
                            .to_string()
                    })?
                    .to_string();
                Ok(Some(SubagentContinuity::ContinueAgent {
                    continue_agent_id,
                }))
            }
            other => Err(format!(
                "spawn options: 'continuity.kind' must be 'new' or 'continue_agent', got {other:?}"
            )),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubagentWorktreeOptions {
    pub branch: Option<String>,
    pub path: Option<String>,
    pub cleanup: bool,
}

pub trait SubagentSpawner: Send + Sync {
    fn spawn(
        &self,
        role: &str,
        task: &str,
        parent_session_id: &str,
        options: SubagentSpawnOptions,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>;
}

/// Terminal state reported for one supervised child.  This lives beside the
/// callback boundary rather than a particular host implementation so tool
/// callers can verify a wait covers every requested child before reporting a
/// terminal aggregate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentTerminalStatus {
    Completed,
    Failed,
    Cancelled,
}

/// One terminal result returned by [`SubagentSupervisor::wait_for`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentWaitOutcome {
    /// Canonical `agent` or `parent/agent` path requested by the tool.
    pub agent_path: String,
    pub status: SubagentTerminalStatus,
}

/// Callback for live child lifecycle control.  Unlike [`SubagentSpawner`],
/// which returns a child's final response for legacy inline delegation, this
/// boundary preserves child identity so wait & interrupt can target an exact
/// subtree through a host-owned supervisor.
pub trait SubagentSupervisor: Send + Sync {
    /// Wait for every requested child.  A successful return must contain one
    /// terminal outcome for each requested path; the tool verifies that
    /// contract before exposing an aggregate result to a model.
    fn wait_for(
        &self,
        parent_session_id: &str,
        agent_paths: Vec<String>,
        timeout_ms: Option<u64>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Vec<SubagentWaitOutcome>, String>> + Send>>;

    /// Interrupt one exact child subtree.  The host must not broaden this to
    /// siblings or its parent session.
    fn interrupt_subtree(
        &self,
        parent_session_id: &str,
        agent_path: String,
        reason: Option<String>,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
}

/// Callback for asking the operator a structured question from within a tool.
/// `questions` is the raw `questions` array; the returned value is the answers
/// payload (shape decided by the driver — typically `{header: answer}`).
pub trait UserQuestioner: Send + Sync {
    /// Headless callers retain sandbox errors without opening a grant prompt.
    fn supports_sandbox_grants(&self) -> bool {
        true
    }

    fn ask(
        &self,
        questions: serde_json::Value,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send>>;
}

/// Callback for the `exit_plan_mode` tool: present `plan` to the operator and
/// resolve to `true` when approved (the driver also leaves Plan mode), `false`
/// if rejected.
pub trait PlanExit: Send + Sync {
    fn present(
        &self,
        plan: String,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<bool, String>> + Send>>;
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MessageDispatch {
    pub from_session_id: String,
    pub target_session_id: Option<String>,
    pub channel: String,
    pub message: String,
}

/// Callback for delivering `send_message` payloads into a live runtime instead
/// of only writing a filesystem drop file.
pub trait MessageDispatcher: Send + Sync {
    fn dispatch(
        &self,
        message: MessageDispatch,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>;
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TeamDefinition {
    pub name: String,
    pub description: Option<String>,
    pub members: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_workers: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<f64>,
}

/// Callback for registering/deleting team definitions in a live coordinator.
pub trait TeamDirectory: Send + Sync {
    fn create_team(
        &self,
        team: TeamDefinition,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>;

    fn delete_team(
        &self,
        name: String,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionResourceLimits {
    pub cpu: Option<u32>,
    pub memory_mb: Option<u32>,
    pub disk_mb: Option<u32>,
}

/// Everything a tool needs to know about its execution environment.
pub struct ToolContext {
    pub session_id: String,
    /// Exact active tool call identity used to bind credential prompts.
    pub tool_call_id: String,
    pub workspace_root: PathBuf,
    pub permission_mode: PermissionMode,
    /// Session mode before exact tool approval bypasses tool-level checks.
    /// Tool consent must not implicitly authorize network or wider isolation.
    sandbox_permission_mode: Option<PermissionMode>,
    pub workspace_trust: WorkspaceTrust,
    /// Highest-risk content origin currently able to influence this call.
    pub content_provenance: ContentProvenance,
    /// True only when the operator approved this exact durable tool call.
    pub durable_write_approved: bool,
    /// Configured path deny lists (`deny-read-globs` / `deny-write-globs`
    /// layered over the always-on defaults). `None` still enforces defaults.
    pub path_deny: Option<Arc<rightkit_agent_policy::DenyGlobSet>>,
    /// Single-invocation sandbox grant, attached only by EFF-001 after explicit approval.
    #[cfg(feature = "host")]
    pub sandbox_retry_grant: Option<crate::sandbox::SandboxGrant>,
    /// Optional trust store for artifact-related tools.
    pub trust_store: Option<Arc<Mutex<dyn ArtifactTrustStore + Send>>>,
    /// Optional GitHub config for the github tool.
    pub github_token: Option<String>,
    /// Optional web search config.
    pub web_search_config: Option<crate::web_search::WebSearchConfig>,
    /// Per-session image-provider credentials supplied directly by a host.
    /// They are deliberately separate from text-provider credentials: an
    /// image route can only select a key for its own provider slug.
    pub image_provider_keys: HashMap<String, String>,
    /// Optional Cloudflare account credential for native deployment tools.
    pub cloudflare_config: Option<crate::cloudflare::CloudflareConfig>,
    /// Optional user-connected cloud agent endpoint credential (ENG-62),
    /// scoped to this session's `Cloud` execution target only (ENG-65). See
    /// `crate::cloud_endpoint::target_credential` for the session+target
    /// keyed registry a mid-session refresh should update instead of this
    /// field, mirroring `cloudflare_credential` alongside `cloudflare_config`.
    pub cloud_endpoint_config: Option<crate::cloud_endpoint::CloudEndpointConfig>,
    /// Session-start Fly.io credential; `crate::context::flyio_credential`
    /// supersedes it after a mid-session refresh.
    pub flyio_config: Option<crate::flyio::FlyioConfig>,
    /// Optional sub-agent spawner (only present when the orchestration layer is wired in).
    pub spawner: Option<Arc<dyn SubagentSpawner>>,
    /// Optional live supervisor for typed child wait & subtree interruption.
    pub subagent_supervisor: Option<Arc<dyn SubagentSupervisor>>,
    /// Optional operator-question callback (present when a UI/driver can answer).
    pub questioner: Option<Arc<dyn UserQuestioner>>,
    /// Optional host-owned mid-turn credential prompt.
    pub credential_requester: Option<Arc<dyn CredentialRequester>>,
    /// Optional registry of background shells, shared across a session's tool calls.
    #[cfg(feature = "host")]
    pub background_shells: Option<crate::background_shell::BgStore>,
    /// Session-owned read receipts and per-path workspace mutation locks.
    pub file_service: Arc<crate::file_service::FileService>,
    /// Optional registry of browser sessions, shared across a session's tool calls.
    #[cfg(feature = "host")]
    pub browser_sessions: Option<crate::browser::BrowserStore>,
    /// Optional registry of durable/background task records, shared across a session's tool calls.
    pub task_store: Option<crate::task_tools::TaskStore>,
    /// Optional selected execution backend for process-spawning tools.
    pub execution_backend: Option<String>,
    /// Optional selected execution workspace id for backend-routed process-spawning tools.
    pub execution_workspace_id: Option<String>,
    /// Optional materialized execution workspace directory for backend-routed process tools.
    pub execution_workspace_path: Option<PathBuf>,
    /// Optional selected network profile for process-spawning tools.
    pub execution_network_profile: Option<String>,
    /// Optional resource caps selected for backend-routed process tools.
    pub execution_resource_limits: Option<ExecutionResourceLimits>,
    /// Optional plan-exit callback for the `exit_plan_mode` tool.
    pub plan_exit: Option<Arc<dyn PlanExit>>,
    /// Optional live message dispatcher for the `send_message` tool.
    pub message_dispatcher: Option<Arc<dyn MessageDispatcher>>,
    /// Optional live team directory for team tools.
    pub team_directory: Option<Arc<dyn TeamDirectory>>,
    /// Optional MCP manager for the dynamic MCP tools.
    pub mcp: Option<Arc<rightkit_mcp::agent_mcp::McpManager>>,
    /// Active user/channel surface for MCP server channel policy checks.
    pub mcp_channel: String,
    /// Active toolset profile id, when the session was created under a named toolset.
    pub toolset: Option<String>,
    /// Optional cancellation watch for in-flight tool invocation cancellation.
    /// When the sender sets `true`, the tool should abort its current operation.
    pub cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    /// Live output sink for long-running process tools (shell/exec). Default
    /// is a no-op; the engine installs a coalescing sink per tool call.
    pub output_notifier: rightkit_agent_engine_protocol::ToolOutputNotifier,
    /// Optional directory containing content-addressed large tool results.
    pub tool_result_dir: Option<PathBuf>,
    /// Optional skill directories for `invoke_skill`, ordered by precedence.
    pub skill_dirs: Vec<PathBuf>,
    /// Optional execution backend registry for backend-aware tool routing.
    #[cfg(feature = "host")]
    pub execution_backend_registry:
        Option<Arc<rightkit_sandbox::execution::ExecutionBackendRegistry>>,
    /// Host-owned process launch boundary for Git and other managed effects.
    #[cfg(feature = "host")]
    pub process_launch_adapter: Option<Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>>,
    /// Policy context carried into host-owned process launches.
    #[cfg(feature = "host")]
    pub process_launch_context: rightkit_sandbox::execution::ProcessLaunchContext,
}

impl ToolContext {
    pub fn new(
        session_id: impl Into<String>,
        workspace_root: impl Into<PathBuf>,
        permission_mode: PermissionMode,
        workspace_trust: WorkspaceTrust,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            tool_call_id: String::new(),
            workspace_root: workspace_root.into(),
            permission_mode,
            sandbox_permission_mode: None,
            workspace_trust,
            content_provenance: ContentProvenance::session(),
            durable_write_approved: false,
            path_deny: None,
            #[cfg(feature = "host")]
            sandbox_retry_grant: None,
            trust_store: None,
            github_token: None,
            web_search_config: None,
            image_provider_keys: HashMap::new(),
            cloudflare_config: None,
            cloud_endpoint_config: None,
            flyio_config: None,
            spawner: None,
            subagent_supervisor: None,
            questioner: None,
            credential_requester: None,
            #[cfg(feature = "host")]
            background_shells: None,
            file_service: crate::file_service::FileService::new(),
            #[cfg(feature = "host")]
            browser_sessions: None,
            task_store: None,
            execution_backend: None,
            execution_workspace_id: None,
            execution_workspace_path: None,
            execution_network_profile: None,
            execution_resource_limits: None,
            plan_exit: None,
            message_dispatcher: None,
            team_directory: None,
            mcp: None,
            mcp_channel: "desktop".to_string(),
            toolset: None,
            cancel_rx: None,
            output_notifier: rightkit_agent_engine_protocol::ToolOutputNotifier::default(),
            tool_result_dir: None,
            skill_dirs: Vec::new(),
            #[cfg(feature = "host")]
            execution_backend_registry: None,
            #[cfg(feature = "host")]
            process_launch_adapter: None,
            #[cfg(feature = "host")]
            process_launch_context: rightkit_sandbox::execution::ProcessLaunchContext {
                workspace_root: String::new(),
                sandbox_profile: None,
                sandbox_required: false,
                network_required: false,
            },
        }
    }

    pub fn with_trust_store(mut self, store: Arc<Mutex<dyn ArtifactTrustStore + Send>>) -> Self {
        self.trust_store = Some(store);
        self
    }

    pub fn with_content_provenance(mut self, provenance: ContentProvenance) -> Self {
        self.content_provenance = provenance;
        self
    }

    pub fn with_durable_write_approval(mut self, approved: bool) -> Self {
        self.durable_write_approved = approved;
        self
    }

    pub fn with_path_deny(mut self, deny: Option<Arc<rightkit_agent_policy::DenyGlobSet>>) -> Self {
        self.path_deny = deny;
        self
    }

    pub fn with_sandbox_permission_mode(mut self, mode: PermissionMode) -> Self {
        self.sandbox_permission_mode = Some(mode);
        self
    }

    pub fn sandbox_permission_mode(&self) -> PermissionMode {
        self.sandbox_permission_mode.unwrap_or(self.permission_mode)
    }

    pub fn with_github_token(mut self, token: impl Into<String>) -> Self {
        self.github_token = Some(token.into());
        self
    }

    pub fn with_web_search_config(mut self, config: crate::web_search::WebSearchConfig) -> Self {
        self.web_search_config = Some(config);
        self
    }

    /// Supply one image-provider key for this execution context. The key is
    /// never serialized into tool input/output; it is read only by the
    /// dedicated image transport for an exactly matching provider route.
    pub fn with_image_provider_key(
        mut self,
        provider: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        let provider = provider.into();
        let api_key = api_key.into();
        if !provider.trim().is_empty() && !api_key.trim().is_empty() {
            self.image_provider_keys.insert(provider, api_key);
        }
        self
    }

    pub fn with_cloudflare_config(mut self, config: crate::cloudflare::CloudflareConfig) -> Self {
        self.cloudflare_config = Some(config);
        self
    }

    pub fn with_cloud_endpoint_config(
        mut self,
        config: crate::cloud_endpoint::CloudEndpointConfig,
    ) -> Self {
        self.cloud_endpoint_config = Some(config);
        self
    }

    pub fn with_spawner(mut self, spawner: Arc<dyn SubagentSpawner>) -> Self {
        self.spawner = Some(spawner);
        self
    }

    /// Attach a host-owned child supervisor.  It is separate from the legacy
    /// spawner because completed inline spawns do not retain a controllable
    /// child identity.
    pub fn with_subagent_supervisor(mut self, supervisor: Arc<dyn SubagentSupervisor>) -> Self {
        self.subagent_supervisor = Some(supervisor);
        self
    }

    pub fn with_questioner(mut self, questioner: Arc<dyn UserQuestioner>) -> Self {
        self.questioner = Some(questioner);
        self
    }

    pub fn with_tool_call_id(mut self, tool_call_id: impl Into<String>) -> Self {
        self.tool_call_id = tool_call_id.into();
        self
    }

    pub fn with_credential_requester(mut self, requester: Arc<dyn CredentialRequester>) -> Self {
        self.credential_requester = Some(requester);
        self
    }

    #[cfg(feature = "host")]
    pub fn with_background_shells(mut self, store: crate::background_shell::BgStore) -> Self {
        self.background_shells = Some(store);
        self
    }

    pub fn with_file_service(mut self, service: Arc<crate::file_service::FileService>) -> Self {
        self.file_service = service;
        self
    }

    #[cfg(feature = "host")]
    pub fn with_browser_sessions(mut self, store: crate::browser::BrowserStore) -> Self {
        self.browser_sessions = Some(store);
        self
    }

    pub fn with_task_store(mut self, store: crate::task_tools::TaskStore) -> Self {
        self.task_store = Some(store);
        self
    }

    pub fn with_execution_backend(mut self, backend: impl Into<String>) -> Self {
        let backend = backend.into();
        self.execution_backend = (!backend.trim().is_empty()).then(|| backend.trim().to_string());
        self
    }

    pub fn with_execution_workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        let workspace_id = workspace_id.into();
        self.execution_workspace_id =
            (!workspace_id.trim().is_empty()).then(|| workspace_id.trim().to_string());
        self
    }

    pub fn with_execution_workspace_path(mut self, workspace_path: impl Into<PathBuf>) -> Self {
        let workspace_path = workspace_path.into();
        self.execution_workspace_path =
            (!workspace_path.as_os_str().is_empty()).then_some(workspace_path);
        self
    }

    pub fn with_execution_network_profile(mut self, profile: impl Into<String>) -> Self {
        let profile = profile.into();
        self.execution_network_profile =
            (!profile.trim().is_empty()).then(|| profile.trim().to_string());
        self
    }

    pub fn with_execution_resource_limits(mut self, limits: ExecutionResourceLimits) -> Self {
        self.execution_resource_limits =
            (limits.cpu.is_some() || limits.memory_mb.is_some() || limits.disk_mb.is_some())
                .then_some(limits);
        self
    }

    #[cfg(feature = "host")]
    pub fn with_process_launch(
        mut self,
        adapter: Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
        context: rightkit_sandbox::execution::ProcessLaunchContext,
    ) -> Self {
        self.process_launch_adapter = Some(adapter);
        self.process_launch_context = context;
        self
    }

    pub fn nonlocal_execution_backend_error(&self, surface: &str) -> Option<String> {
        let backend = self
            .execution_backend
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        if backend.eq_ignore_ascii_case("local") {
            return None;
        }
        Some(format!(
            "{surface} executionBackend '{backend}' is not available for command execution; only 'local' is currently supported"
        ))
    }

    pub fn unenforced_network_profile_error(&self, surface: &str) -> Option<String> {
        let profile = self
            .execution_network_profile
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        if profile.eq_ignore_ascii_case("local-host") {
            return None;
        }
        Some(format!(
            "{surface} networkProfile '{profile}' is not enforced by local host execution; refusing to run until backend routing applies the selected profile"
        ))
    }

    pub fn with_plan_exit(mut self, plan_exit: Arc<dyn PlanExit>) -> Self {
        self.plan_exit = Some(plan_exit);
        self
    }

    pub fn with_message_dispatcher(mut self, dispatcher: Arc<dyn MessageDispatcher>) -> Self {
        self.message_dispatcher = Some(dispatcher);
        self
    }

    pub fn with_team_directory(mut self, directory: Arc<dyn TeamDirectory>) -> Self {
        self.team_directory = Some(directory);
        self
    }

    pub fn with_mcp(mut self, mcp: Arc<rightkit_mcp::agent_mcp::McpManager>) -> Self {
        self.mcp = Some(mcp);
        self
    }

    pub fn with_mcp_channel(mut self, channel: impl Into<String>) -> Self {
        let channel = channel.into();
        let channel = channel.trim();
        if !channel.is_empty() {
            self.mcp_channel = channel.to_ascii_lowercase();
        }
        self
    }

    pub fn with_toolset(mut self, toolset: impl Into<String>) -> Self {
        let toolset = toolset.into();
        self.toolset = (!toolset.trim().is_empty()).then(|| toolset.trim().to_string());
        self
    }

    pub fn with_tool_result_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.tool_result_dir = Some(dir.into());
        self
    }

    pub fn with_cancel_rx(mut self, rx: tokio::sync::watch::Receiver<bool>) -> Self {
        self.cancel_rx = Some(rx);
        self
    }

    pub fn with_output_notifier(
        mut self,
        notifier: rightkit_agent_engine_protocol::ToolOutputNotifier,
    ) -> Self {
        self.output_notifier = notifier;
        self
    }

    pub fn with_skill_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.skill_dirs = dirs;
        self
    }

    #[cfg(feature = "host")]
    pub fn with_execution_backend_registry(
        mut self,
        registry: Arc<rightkit_sandbox::execution::ExecutionBackendRegistry>,
    ) -> Self {
        self.execution_backend_registry = Some(registry);
        self
    }
}
