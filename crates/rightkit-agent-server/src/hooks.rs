//! Daemon-side hook runner.
//!
//! Subscribes to a session's engine [`HookBus`] and runs the **user-configured** hooks that the
//! engine's own chain does not run: URL and MCP-tool hooks for every published event, and command
//! hooks for `PostToolUse` / `PostToolUseFailure` (the engine runs every other event's command
//! hooks itself, through `HookBus::run_event_hooks`). The default is a safe no-op.
//!
//! One configuration surface (tools rule 58): the `[hooks]` key of `config.toml`, the project
//! hook file `hooks.json` in the state folder (trusted workspaces only) and the JSON-form environment
//! variable `<PREFIX>_HOOKS_JSON`. The per-event single-command variables no longer exist.
//! Entries look like `{"matcher":"shell","command":"audit.sh"}`, `{"type":"url","command":"https://..."}`
//! or `{"type":"mcp","command":"mcp__server__tool"}`.
//!
//! A command runs through the platform shell with context in the environment:
//! `<PREFIX>_HOOK_EVENT`, `<PREFIX>_HOOK_TOOL`, `<PREFIX>_HOOK_SESSION`,
//! `<PREFIX>_HOOK_ROLE`, `<PREFIX>_HOOK_SUCCESS`, `<PREFIX>_HOOK_MESSAGE`, and
//! `<PREFIX>_HOOK_IS_ERROR`. HTTP hooks receive the same lifecycle data as JSON. `PreToolUse`
//! command hooks are evaluated by the engine before dispatch so exit code 2 can deny the call;
//! the daemon hook sinks are fire-and-forget and never block the agent loop.

use rightkit_hooks::agent_hooks::registry::{HookAdmissionV1, HookRejectionReasonV1};
use rightkit_hooks::agent_hooks::{
    ConfiguredHooks, HookBus, HookEvent, HookKind, HookRegistry, IdentifiedHook,
};
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::{response::Json, routing::get, Router};
use serde_json::{json, Map, Value};

include!("hooks_management.rs");

/// The hooks a daemon-side runner serves, identified, plus its shared services.
#[derive(Clone, Default)]
pub struct HookRunnerConfig {
    pub hooks: Vec<IdentifiedHook>,
    pub mcp: Option<Arc<rightkit_mcp::agent_mcp::McpManager>>,
    /// Shared, daemon-wide revocation registry consulted by the runner per
    /// event. `None` = runner ignores revocations.
    pub revoked: Option<Arc<HookRegistry>>,
    /// The session workspace: command hooks start there and, in safe mode,
    /// are confined to it. `None` falls back to the daemon's cwd.
    pub workspace_root: Option<std::path::PathBuf>,
}

impl HookRunnerConfig {
    /// Every hook `bus` serves (config key, project file, JSON environment variable). An invalid
    /// surface yields no hooks here; the engine's chain reports it and fails `PreToolUse` closed.
    fn from_bus(bus: &HookBus) -> Self {
        Self {
            hooks: bus.inventory().unwrap_or_default(),
            mcp: None,
            revoked: None,
            workspace_root: None,
        }
    }

    /// Whether the runner has anything to do: a URL or MCP hook anywhere, or a command hook on
    /// the events whose command hooks the engine does not run.
    fn is_empty(&self) -> bool {
        !self.hooks.iter().any(runner_serves)
    }

    fn needs_mcp(&self) -> bool {
        self.hooks
            .iter()
            .any(|hook| hook.spec.kind == HookKind::Mcp)
    }
}

/// Whether the daemon runner (not the engine chain) serves `hook`.
fn runner_serves(hook: &IdentifiedHook) -> bool {
    match hook.spec.kind {
        HookKind::Url | HookKind::Mcp => true,
        // A `PostToolUse` hook that declares `rewrite` is awaited by the engine before the result is
        // stored (`HookBus::rewrite_post_tool_result`), so the runner must not run it a second time.
        HookKind::Command => {
            matches!(hook.event.as_str(), "PostToolUse" | "PostToolUseFailure")
                && !(hook.spec.rewrite && hook.event == "PostToolUse")
        }
    }
}

/// Names the host to the hook crate: the prefix of the variables hooks read and receive and the
/// folder of the project hook file. The hook crate keeps the first choice (a start-up path may
/// have made it already, with the same names), so this is safe to call on every bus build.
fn init_hook_conventions() {
    rightkit_hooks::agent_hooks::set_env_prefix(rightkit_agent_config::env_prefix());
    rightkit_hooks::agent_hooks::set_state_dir_name(rightkit_agent_config::state_dir_name());
}

/// Read the hooks of one session's workspace into `bus` (tools rule 58): the `[hooks]` key
/// (user layer always; the workspace layer only when trusted) and the project hook file (trusted
/// workspaces only). An unreadable or invalid project file, or one whose bytes changed since it
/// was pinned at approval (`project_hook_pin_error`), is carried as an error so `PreToolUse`
/// fails closed rather than dropping the hooks.
pub(crate) fn configure_session_hooks(bus: &HookBus, workspace_root: &Path) {
    init_hook_conventions();
    let trusted = rightkit_agent_config::is_workspace_trusted(workspace_root);
    let config =
        rightkit_agent_config::ConfigLoader::new().load_files_for_workspace(workspace_root);
    let mut hooks = ConfiguredHooks::new(config.hooks.as_document(), None);
    let pin_error = if trusted {
        project_hook_pin_error(workspace_root)
    } else {
        None
    };
    if let Some(error) = pin_error {
        hooks = hooks.with_error(error);
    } else {
        match rightkit_hooks::agent_hooks::chain::load_project_hook_file(workspace_root, trusted) {
            Ok(project) => hooks = ConfiguredHooks::new(config.hooks.as_document(), project),
            Err(error) => hooks = hooks.with_error(error),
        }
    }
    bus.set_configured_hooks(hooks);
    // Events the engine fires through the chain are not on the bus, so the chain hands their `url`
    // hooks to the same bounded fire-and-forget POST the runner uses.
    bus.set_url_sink(Arc::new(|url: String, body: Value| {
        post_http_hook(url, body)
    }));
    // The engine runs command hooks of every event but `PostToolUse*` through this bus, so the
    // bus must honour `/hooks/revoke` by the same stable key the list, test and runner paths use
    // (audit 2026-11-05 round 2, finding 3). The registry is read on every chain run, so a
    // revocation made after the session started applies to its next event.
    let scope = workspace_scope(workspace_root);
    bus.set_revocation_filter(Arc::new(move |hook: &IdentifiedHook| {
        shared_registry().is_revoked(&hook_revocation_key(hook, &scope))
    }));
}

/// POL-13: refuse a project hook file whose bytes changed since it was approved. A missing or
/// unreadable file is not this function's concern: `load_project_hook_file` reports it. The
/// first time a trusted workspace's hook file is seen it is pinned; trusting the workspace
/// again clears the pin.
fn project_hook_pin_error(workspace_root: &Path) -> Option<String> {
    let file = workspace_root
        .join(rightkit_hooks::agent_hooks::state_dir_name())
        .join("hooks.json");
    let bytes = std::fs::read(&file).ok()?;
    let store = rightkit_agent_config::definition_pins::pin_store_path("hook-sets.json")?;
    let scope = rightkit_agent_config::workspace_trust_key(&workspace_scope(workspace_root));
    let digest = rightkit_agent_config::definition_pins::digest_of_bytes(&bytes);
    match rightkit_agent_config::definition_pins::check_or_pin(&store, &scope, "hooks.json", &digest) {
        rightkit_agent_config::definition_pins::PinCheck::Changed { pinned } => Some(format!(
            "the project hook file {} changed since it was approved (approved sha256 {}, now {}); none of its hooks ran. Review the file, then approve it again with `{} trust` (or the trust control of the app), which pins the current file.",
            file.display(),
            &pinned[..pinned.len().min(12)],
            &digest[..digest.len().min(12)],
            crate::host_identity::app()
        )),
        _ => None,
    }
}

/// A throwaway bus carrying the daemon-wide view of the hooks (the user config, then the JSON
/// environment variable) for the status, list and test routes.
fn daemon_wide_bus() -> HookBus {
    let bus = HookBus::new(1);
    let workspace = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    configure_session_hooks(&bus, &workspace);
    bus
}

pub(crate) fn hook_routes() -> Router<crate::api::DaemonState> {
    Router::new()
        .route("/hooks/status", get(get_hook_status))
        .merge(hook_management_routes())
}

pub(crate) fn hooks_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.extend(hook_management_openapi_components());
    schemas.insert(
        "HookStatusEntry".to_string(),
        json!({
            "type": "object",
            "required": ["id", "event", "kind", "origin", "configured"],
            "properties": {
                "id": {"type": "string"},
                "event": {"type": "string"},
                "kind": {"type": "string", "enum": ["command", "url", "mcp"]},
                "origin": {"type": "string", "enum": ["config", "project", "env-json"]},
                "configured": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "HookStatus".to_string(),
        json!({
            "type": "object",
            "required": ["configured", "totalConfigured", "commandConfigured", "urlConfigured", "mcpConfigured", "hooks"],
            "properties": {
                "configured": {"type": "boolean"},
                "totalConfigured": {"type": "integer", "minimum": 0},
                "commandConfigured": {"type": "integer", "minimum": 0},
                "urlConfigured": {"type": "integer", "minimum": 0},
                "mcpConfigured": {"type": "integer", "minimum": 0},
                "hooks": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/HookStatusEntry"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas
}

pub(crate) fn hook_status_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/HookStatus"})
}

async fn get_hook_status() -> Json<HookStatus> {
    Json(hook_status_from_bus(&daemon_wide_bus()))
}

pub(crate) fn current_hook_status_value() -> Value {
    serde_json::to_value(hook_status_from_bus(&daemon_wide_bus()))
        .unwrap_or_else(|_| json!({"configured": false, "hooks": []}))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HookStatus {
    configured: bool,
    total_configured: usize,
    command_configured: usize,
    url_configured: usize,
    mcp_configured: usize,
    hooks: Vec<HookStatusEntry>,
}

/// One configured hook. Values (commands, URLs, tool names) are never part of the status.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HookStatusEntry {
    id: String,
    event: String,
    kind: &'static str,
    origin: &'static str,
    configured: bool,
}

fn hook_status_from_bus(bus: &HookBus) -> HookStatus {
    let hooks: Vec<HookStatusEntry> = bus
        .inventory()
        .unwrap_or_default()
        .into_iter()
        .map(|hook| HookStatusEntry {
            id: hook.id,
            event: hook.event,
            kind: hook.spec.kind.as_str(),
            origin: hook.origin,
            configured: true,
        })
        .collect();
    HookStatus {
        configured: !hooks.is_empty(),
        total_configured: hooks.len(),
        command_configured: count_kind(&hooks, "command"),
        url_configured: count_kind(&hooks, "url"),
        mcp_configured: count_kind(&hooks, "mcp"),
        hooks,
    }
}

fn count_kind(hooks: &[HookStatusEntry], kind: &str) -> usize {
    hooks
        .iter()
        .filter(|hook| hook.kind == kind && hook.configured)
        .count()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HttpHookPayload {
    event: String,
    session_id: String,
    tool: Option<String>,
    is_error: Option<bool>,
    role: Option<String>,
    success: Option<bool>,
    message: Option<String>,
    /// Staged diff content for PreCommit hooks.
    #[serde(skip_serializing_if = "Option::is_none")]
    diff: Option<String>,
}

/// `confine` is the session's sandbox requirement (`--safe-mode`). Operator
/// configured hook commands and hook MCP servers are operator-authored, not
/// model-authored: outside safe mode they run unconfined (a typed, recorded
/// opt-out on the launch context), so a hook script and its log may live
/// anywhere the operator can reach. In safe mode every hook launch is
/// AppContainer/Seatbelt/Landlock-confined and fails closed.
pub fn spawn_hook_runner_for_workspace(bus: &HookBus, workspace_root: &Path, confine: bool) {
    // The caller has already installed the session's hooks with `configure_session_hooks`.
    let mut config = HookRunnerConfig::from_bus(bus);
    config.workspace_root = Some(workspace_root.to_path_buf());
    // Honor the operator's hook revocations via the daemon-wide registry.
    config.revoked = Some(shared_registry());
    if config.is_empty() {
        return;
    }
    if !config.needs_mcp() {
        spawn_hook_runner_with_config_confined(bus, config, confine);
        return;
    }
    let bus = bus.clone();
    let workspace_root = workspace_root.to_path_buf();
    tokio::spawn(async move {
        let mut config = config;
        let governor =
            match rightkit_agent_coding_tools::process_resource_authority::require_process_resource_governor() {
                Ok(governor) => governor,
                Err(error) => {
                    tracing::warn!(error = %error, "MCP hook transport setup denied: process resource authority unavailable");
                    return;
                }
            };
        let launcher: Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter> = Arc::new(
            rightkit_agent_coding_tools::process_resource_authority::GovernedProcessLaunchAdapter::new(
                rightkit_agent_coding_tools::shell::NativeCommandSandboxAdapter,
                governor,
                rightkit_agent_coding_tools::process_resource_authority::ResourceKind::Mcp,
                true,
            ),
        );
        config.mcp = Some(Arc::new(
            rightkit_mcp::agent_mcp::config::load_with_process_launcher(
                Some(&workspace_root),
                launcher,
                rightkit_sandbox::execution::ProcessLaunchContext {
                    workspace_root: workspace_root.to_string_lossy().into_owned(),
                    sandbox_profile: Some("mcp-hook".to_string()),
                    sandbox_required: confine,
                    network_required: false,
                },
            )
            .await,
        ));
        config.revoked = Some(shared_registry());
        spawn_hook_runner_with_config_confined(&bus, config, confine);
    });
}

pub fn spawn_hook_runner_with_config_confined(
    bus: &HookBus,
    config: HookRunnerConfig,
    confine: bool,
) {
    if config.is_empty() {
        return;
    }

    let registry = config.revoked.clone();
    let workspace_root = config.workspace_root.clone().unwrap_or_else(|| {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    });
    // M4: revocation is keyed on each hook's stable identity in this workspace, not its list
    // position, so a reordered hook file can neither revive a revoked hook nor block another.
    let scope = workspace_scope(&workspace_root);
    let revocation_keys: Vec<String> = config
        .hooks
        .iter()
        .map(|hook| hook_revocation_key(hook, &scope))
        .collect();
    let mut sub = bus.subscribe();
    tokio::spawn(async move {
        while let Some(ev) = sub.recv().await {
            let reg_ref = registry.as_deref();
            let is_revoked = |key: &str| -> bool { reg_ref.is_some_and(|r| r.is_revoked(key)) };
            let session_id = ev.session_id().to_string();
            let (payload, tool, failed) = match &ev {
                HookEvent::PreToolUse { session_id, tool } => (
                    hook_payload("PreToolUse", session_id, Some(tool), None),
                    Some(tool.as_str()),
                    false,
                ),
                HookEvent::PostToolUse {
                    session_id,
                    tool,
                    is_error,
                } => (
                    hook_payload("PostToolUse", session_id, Some(tool), Some(*is_error)),
                    Some(tool.as_str()),
                    *is_error,
                ),
                HookEvent::UserPromptSubmit { session_id } => (
                    hook_payload("UserPromptSubmit", session_id, None, None),
                    None,
                    false,
                ),
                HookEvent::Notification {
                    session_id,
                    message,
                } => (
                    hook_payload("Notification", session_id, None, None).with_message(message),
                    None,
                    false,
                ),
                HookEvent::SubagentStop {
                    session_id,
                    role,
                    success,
                } => (
                    hook_payload("SubagentStop", session_id, None, None)
                        .with_subagent(role, *success),
                    None,
                    false,
                ),
                HookEvent::PreCommit { session_id, diff } => (
                    hook_payload("PreCommit", session_id, None, None).with_diff(diff),
                    None,
                    false,
                ),
                other => (
                    hook_payload(other.name(), other.session_id(), None, None),
                    None,
                    false,
                ),
            };
            // A failed tool call fires `PostToolUseFailure` after its `PostToolUse` (tools rule 58).
            let mut names = vec![ev.name()];
            if failed {
                names.push("PostToolUseFailure");
            }
            for name in names {
                let mut payload = payload.clone();
                payload.event = name.to_string();
                for (hook, key) in config
                    .hooks
                    .iter()
                    .zip(&revocation_keys)
                    .filter(|(hook, _)| hook.event == name)
                {
                    if !runner_serves(hook) || is_revoked(key.as_str()) || !hook.spec.matches(tool)
                    {
                        continue;
                    }
                    match hook.spec.kind {
                        HookKind::Command => {
                            run(
                                &hook.spec.command,
                                hook.spec.timeout,
                                &payload,
                                &workspace_root,
                                confine,
                            )
                            .await;
                        }
                        HookKind::Url => post_http_hook(hook.spec.command.clone(), payload.clone()),
                        HookKind::Mcp => {
                            if let Some(manager) = &config.mcp {
                                invoke_mcp_hook(
                                    manager.clone(),
                                    hook.spec.command.clone(),
                                    payload.clone(),
                                    session_id.clone(),
                                );
                            }
                        }
                    }
                }
            }
        }
    });
}

fn hook_payload(
    event: &str,
    session: &str,
    tool: Option<&str>,
    is_error: Option<bool>,
) -> HttpHookPayload {
    HttpHookPayload {
        event: event.to_string(),
        session_id: session.to_string(),
        tool: tool.map(str::to_string),
        is_error,
        role: None,
        success: None,
        message: None,
        diff: None,
    }
}

impl HttpHookPayload {
    fn with_message(mut self, message: &str) -> Self {
        self.message = Some(message.to_string());
        self
    }

    fn with_subagent(mut self, role: &str, success: bool) -> Self {
        self.role = Some(role.to_string());
        self.success = Some(success);
        self
    }

    fn with_diff(mut self, diff: &str) -> Self {
        self.diff = Some(diff.to_string());
        self
    }
}

fn post_http_hook(url: String, payload: impl Serialize + Send + 'static) {
    tokio::spawn(async move {
        // Fire-and-forget: the hook URL is operator-configured, so its host alone may
        // resolve privately; the call is bounded to 5s and any failure is ignored.
        let Ok(client) =
            rightkit_agent_daemon::http_client::endpoint_client(&url, Duration::from_secs(5))
        else {
            return;
        };
        let Ok(request) = rightkit_http::Request::post(url).json(&payload) else {
            return;
        };
        let _ = client.send_any(&request).await;
    });
}

fn invoke_mcp_hook(
    manager: Arc<rightkit_mcp::agent_mcp::McpManager>,
    tool_name: String,
    payload: HttpHookPayload,
    session_id: String,
) {
    tokio::spawn(async move {
        let args = serde_json::to_value(payload).unwrap_or_else(|_| serde_json::json!({}));
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            manager.invoke(&tool_name, args, Some(&session_id)),
        )
        .await;
    });
}

async fn run(
    cmd: &str,
    timeout: Duration,
    payload: &HttpHookPayload,
    workspace_root: &Path,
    confine: bool,
) {
    let mut env = vec![
        (
            rightkit_agent_config::env_name("HOOK_EVENT"),
            payload.event.clone(),
        ),
        (
            rightkit_agent_config::env_name("HOOK_TOOL"),
            payload.tool.clone().unwrap_or_default(),
        ),
        (
            rightkit_agent_config::env_name("HOOK_SESSION"),
            payload.session_id.clone(),
        ),
        (
            rightkit_agent_config::env_name("HOOK_ROLE"),
            payload.role.clone().unwrap_or_default(),
        ),
        (
            rightkit_agent_config::env_name("HOOK_SUCCESS"),
            payload
                .success
                .map(|v| if v { "1" } else { "0" })
                .unwrap_or("")
                .into(),
        ),
        (
            rightkit_agent_config::env_name("HOOK_MESSAGE"),
            payload.message.clone().unwrap_or_default(),
        ),
    ];
    if let Some(e) = payload.is_error {
        env.push((
            rightkit_agent_config::env_name("HOOK_IS_ERROR"),
            if e { "1" } else { "0" }.into(),
        ));
    }
    if let Some(diff) = &payload.diff {
        env.push((rightkit_agent_config::env_name("HOOK_DIFF"), diff.clone()));
    }
    let spec = hook_command_spec(cmd, env, workspace_root, confine);
    let governor =
        match rightkit_agent_coding_tools::process_resource_authority::require_process_resource_governor() {
            Ok(governor) => governor,
            Err(error) => {
                tracing::warn!(hook_event = %payload.event, error = %error, "hook launch denied: process resource authority unavailable");
                return;
            }
        };
    let launcher =
        rightkit_agent_coding_tools::process_resource_authority::GovernedProcessLaunchAdapter::new(
            rightkit_agent_coding_tools::shell::NativeCommandSandboxAdapter,
            governor,
            rightkit_agent_coding_tools::process_resource_authority::ResourceKind::ChildProcess,
            true,
        );
    let Ok(mut child) =
        rightkit_sandbox::execution::ProcessLaunchAdapter::spawn(&launcher, spec).await
    else {
        // A hook cannot bypass native launch policy; unavailable launch is
        // recorded as no execution rather than falling back to raw spawn.
        // H8: the event fired but nothing actually ran, so admission must
        // reject it — never manufacture evidence from the attempt alone.
        record_hook_admission(
            &payload.event,
            HookAdmissionV1::from_execution(false, false, Vec::new()),
        );
        return;
    };
    // Native adapters intentionally return managed pipes. Drain both before
    // waiting so a verbose hook cannot block itself on a full pipe. Nothing reads
    // hook output, so it is discarded as it arrives: the daemon holds one chunk,
    // never the whole stream (audit M6).
    let stdout = child.take_stdout().map(drain_discarding);
    let stderr = child.take_stderr().map(drain_discarding);
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(code)) => {
            tracing::debug!(hook_event = %payload.event, exit_code = code, "hook completed");
            // H8: the child was spawned AND produced a confirmed exit code —
            // this is genuine host execution, so the hook is admitted.
            record_hook_admission(
                &payload.event,
                HookAdmissionV1::from_execution(true, false, vec![cmd.to_string()]),
            );
        }
        Ok(Err(error)) => {
            tracing::warn!(hook_event = %payload.event, error = %error, "hook wait failed");
            // H8: spawn happened but no confirmed result came back — reject,
            // do not admit a half-observed process as evidence.
            record_hook_admission(
                &payload.event,
                HookAdmissionV1::from_execution(false, false, Vec::new()),
            );
        }
        Err(_) => {
            let _ = child.cancel().await;
            tracing::warn!(
                hook_event = %payload.event,
                timeout_secs = timeout.as_secs(),
                "hook timed out and was cancelled"
            );
            // H8: cancelled before a result was observed — reject.
            record_hook_admission(
                &payload.event,
                HookAdmissionV1::from_execution(false, false, Vec::new()),
            );
        }
    }
    // A backgrounded grandchild (`watcher &`) can hold the pipes open long after the
    // hook itself exited. Give the readers a short grace, then drop them, so the
    // serial runner moves on to the next hook (audit M6).
    finish_readers(stdout.into_iter().chain(stderr), HOOK_OUTPUT_GRACE).await;
}

/// How long the runner keeps draining a hook's pipes after the hook exited.
const HOOK_OUTPUT_GRACE: Duration = Duration::from_secs(2);
const HOOK_DRAIN_CHUNK_BYTES: usize = 8 * 1024;

/// Read `stream` to EOF in fixed chunks and discard them. Returns the byte count.
fn drain_discarding<R>(mut stream: R) -> tokio::task::JoinHandle<u64>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut chunk = vec![0u8; HOOK_DRAIN_CHUNK_BYTES];
        let mut total = 0u64;
        loop {
            match tokio::io::AsyncReadExt::read(&mut stream, &mut chunk).await {
                Ok(0) | Err(_) => return total,
                Ok(read) => total = total.saturating_add(read as u64),
            }
        }
    })
}

/// Wait at most `grace` (in total) for the pipe readers, then abort the rest, which closes the
/// daemon's read ends.
async fn finish_readers(
    readers: impl IntoIterator<Item = tokio::task::JoinHandle<u64>>,
    grace: Duration,
) {
    let deadline = tokio::time::Instant::now() + grace;
    for mut reader in readers {
        if tokio::time::timeout_at(deadline, &mut reader)
            .await
            .is_err()
        {
            reader.abort();
        }
    }
}

/// The scope a hook's revocation identity lives in: the session workspace, canonicalised when
/// it exists so `/ws` and `/ws/.` name the same scope on every OS.
pub(crate) fn workspace_scope(workspace_root: &Path) -> String {
    std::fs::canonicalize(workspace_root)
        .unwrap_or_else(|_| workspace_root.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Prefix of a stable hook revocation key.
const HOOK_KEY_PREFIX: &str = "hook:";

/// Stable revocation identity of `hook` (audit 2026-11-05 M4): `hook:<event>:<origin>:<digest>`,
/// where the digest covers what the hook does (kind, matcher, command), not where it sits in the
/// list. A project hook belongs to one workspace, so its digest also covers `scope`; a hook from
/// the operator's own config or `<PREFIX>_HOOKS_JSON` is the same hook in every workspace and a
/// revocation of it holds everywhere. The timeout is left out, so retuning it does not un-revoke.
pub(crate) fn hook_revocation_key(hook: &IdentifiedHook, scope: &str) -> String {
    let workspace = if hook.origin == "project" { scope } else { "" };
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    for part in [
        workspace,
        hook.event.as_str(),
        hook.origin,
        hook.spec.kind.as_str(),
        hook.spec.matcher.as_deref().unwrap_or(""),
        hook.spec.command.as_str(),
    ] {
        hasher.update(part.as_bytes());
        hasher.update(&[0]);
    }
    let digest: String = hasher.finalize_bytes()[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{HOOK_KEY_PREFIX}{}:{}:{digest}", hook.event, hook.origin)
}

/// Launch spec for a daemon command hook: the platform shell, started in the
/// session workspace, whose root is also the sandbox root in safe mode.
fn hook_command_spec(
    cmd: &str,
    env: Vec<(String, String)>,
    workspace_root: &Path,
    confine: bool,
) -> rightkit_sandbox::execution::CommandLaunchSpec {
    #[cfg(windows)]
    let (sh, flag) = ("cmd", "/C");
    #[cfg(not(windows))]
    let (sh, flag) = ("sh", "-c");
    let root = workspace_root.to_string_lossy().into_owned();
    rightkit_sandbox::execution::CommandLaunchSpec {
        program: sh.into(),
        argv: vec![flag.into(), cmd.into()],
        cwd: root.clone(),
        context: rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: root,
            sandbox_profile: Some("hook".into()),
            sandbox_required: confine,
            network_required: false,
        },
        env,
    }
}

/// H8 admission logging: the single place `run()`'s daemon-side command
/// hooks report their admission decision. Delegates all branching to
/// [`HookAdmissionV1::from_execution`] (the one hook-admission authority
/// shared with `rightkit_hooks::agent_hooks::registry`'s dry-run path) — this function
/// only records the outcome, it never re-derives one.
fn record_hook_admission(event: &str, admission: HookAdmissionV1) {
    match admission {
        HookAdmissionV1::Admitted { invoked } => {
            tracing::debug!(hook_event = %event, invoked = ?invoked, "H8 hook admission: admitted");
        }
        HookAdmissionV1::Rejected { reason } => {
            let reason_label = match reason {
                HookRejectionReasonV1::Revoked => "revoked",
                HookRejectionReasonV1::NoRealExecution => "no_real_execution",
            };
            tracing::debug!(
                hook_event = %event,
                reason = reason_label,
                "H8 hook admission: rejected — hook event alone did not manufacture evidence"
            );
        }
    }
}

#[cfg(test)]
mod hook_runner_workspace_tests {
    use super::*;

    #[test]
    fn command_hook_runs_in_session_workspace() {
        let workspace = std::env::temp_dir().join("hook-ws");
        let spec = hook_command_spec("pwd > marker", Vec::new(), &workspace, false);
        let expected = workspace.to_string_lossy().into_owned();
        assert_eq!(spec.cwd, expected);
        assert_eq!(spec.context.workspace_root, expected);
        assert!(!spec.context.sandbox_required);
        assert_eq!(spec.argv.last().map(String::as_str), Some("pwd > marker"));
    }

    #[test]
    fn safe_mode_confines_command_hook_to_session_workspace() {
        let workspace = std::env::temp_dir().join("hook-safe-ws");
        let spec = hook_command_spec("./scripts/post.sh", Vec::new(), &workspace, true);
        assert!(spec.context.sandbox_required);
        assert_eq!(spec.context.workspace_root, workspace.to_string_lossy());
        assert_ne!(
            Some(spec.context.workspace_root.clone()),
            std::env::current_dir()
                .ok()
                .map(|cwd| cwd.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn runner_config_for_workspace_carries_the_workspace() {
        let config = HookRunnerConfig {
            workspace_root: Some(std::path::PathBuf::from("ws")),
            ..HookRunnerConfig::default()
        };
        assert_eq!(config.workspace_root.as_deref(), Some(Path::new("ws")));
    }
}

#[cfg(test)]
mod hook_audit_m4_m6_tests {
    use super::*;
    use rightkit_hooks::agent_hooks::HookSpec;

    fn hook(id: &str, origin: &'static str, command: &str) -> IdentifiedHook {
        IdentifiedHook {
            id: id.to_string(),
            event: "PostToolUse".to_string(),
            origin,
            spec: HookSpec::new(command),
        }
    }

    fn bus_with_project(commands: &[&str]) -> HookBus {
        let entries: Vec<Value> = commands.iter().map(|c| json!({"command": c})).collect();
        let bus = HookBus::new(1);
        bus.set_configured_hooks(ConfiguredHooks::new(
            None,
            Some(json!({"PostToolUse": entries})),
        ));
        bus
    }

    #[test]
    fn revocation_key_ignores_list_position() {
        let a = hook_revocation_key(&hook("PostToolUse#1", "project", "post-diff.sh"), "/ws");
        let b = hook_revocation_key(&hook("PostToolUse#2", "project", "post-diff.sh"), "/ws");
        assert_eq!(a, b);
        assert!(a.starts_with(HOOK_KEY_PREFIX));
        let other = hook_revocation_key(&hook("PostToolUse#1", "project", "harmless.sh"), "/ws");
        assert_ne!(a, other);
    }

    #[test]
    fn project_revocation_is_workspace_scoped() {
        let project = hook("PostToolUse#0", "project", "post-diff.sh");
        assert_ne!(
            hook_revocation_key(&project, "/ws-a"),
            hook_revocation_key(&project, "/ws-b")
        );
        // The operator's own hook is the same hook everywhere.
        let user = hook("PostToolUse#0", "config", "audit.sh");
        assert_eq!(
            hook_revocation_key(&user, "/ws-a"),
            hook_revocation_key(&user, "/ws-b")
        );
    }

    #[test]
    fn prepending_a_hook_does_not_revive_a_revoked_one() {
        let registry = HookRegistry::default();
        let before = bus_with_project(&["true", "post-diff.sh"]);
        let key = resolve_revocation_key("PostToolUse#1", &before, "/ws");
        let response = apply_revocation(&registry, "PostToolUse#1".into(), key, false);
        assert!(response.applied && response.revoked);

        // The project prepends a harmless hook; the revoked command moves to #2.
        let after = bus_with_project(&["echo hi", "true", "post-diff.sh"]);
        let (revoked, live) = split_revoked(after.inventory().unwrap(), &registry, "/ws");
        let revoked_commands: Vec<&str> = revoked.iter().map(|h| h.spec.command.as_str()).collect();
        assert_eq!(revoked_commands, vec!["post-diff.sh"]);
        assert!(live.iter().all(|h| h.spec.command != "post-diff.sh"));
        assert!(live.iter().any(|h| h.spec.command == "true"));
    }

    #[test]
    fn unknown_positional_id_is_never_stored() {
        let registry = HookRegistry::default();
        let bus = bus_with_project(&["true"]);
        let key = resolve_revocation_key("PostToolUse#9", &bus, "/ws");
        let response = apply_revocation(&registry, "PostToolUse#9".into(), key, false);
        assert!(!response.applied && !response.revoked);
        assert!(registry.snapshot().is_empty());
    }

    #[test]
    fn stable_key_round_trips_through_revoke_and_restore() {
        let registry = HookRegistry::default();
        let bus = bus_with_project(&["true"]);
        let key = resolve_revocation_key("PostToolUse#0", &bus, "/ws").unwrap();
        assert_eq!(resolve_revocation_key(&key, &bus, "/ws"), Some(key.clone()));
        assert!(apply_revocation(&registry, key.clone(), Some(key.clone()), false).revoked);
        assert!(!apply_revocation(&registry, key.clone(), Some(key), true).revoked);
    }

    #[tokio::test]
    async fn drain_discards_output_without_retaining_it() {
        let (reader, mut writer) = tokio::io::duplex(64 * 1024);
        let handle = drain_discarding(reader);
        let chunk = vec![b'x'; 1024 * 1024];
        for _ in 0..8 {
            tokio::io::AsyncWriteExt::write_all(&mut writer, &chunk)
                .await
                .unwrap();
        }
        drop(writer);
        assert_eq!(handle.await.unwrap(), 8 * 1024 * 1024);
    }

    #[tokio::test]
    async fn pipe_held_by_a_background_child_does_not_block_the_runner() {
        // The writer stays open, as a backgrounded grandchild holding stdout would.
        let (reader, _held_open) = tokio::io::duplex(1024);
        let handle = drain_discarding(reader);
        let started = tokio::time::Instant::now();
        finish_readers(Some(handle), Duration::from_millis(100)).await;
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}

#[cfg(test)]
mod hook_bus_revocation_tests {
    use super::*;

    #[test]
    fn a_revoked_engine_run_hook_leaves_the_session_bus() {
        let workspace = tempfile::tempdir().unwrap();
        let bus = HookBus::new(1);
        configure_session_hooks(&bus, workspace.path());
        // A command unique to this test, so the daemon-wide registry other tests share is
        // untouched by anything else.
        let command = "exit 2 # round2-finding3-pretooluse";
        bus.set_configured_hooks(ConfiguredHooks::new(
            None,
            Some(json!({"PreToolUse": [{"command": command}]})),
        ));
        assert_eq!(bus.specs_for("PreToolUse").unwrap().len(), 1);

        let scope = workspace_scope(workspace.path());
        let key = resolve_revocation_key("PreToolUse#0", &bus, &scope);
        let registry = shared_registry();
        let response = apply_revocation(&registry, "PreToolUse#0".into(), key.clone(), false);
        assert!(response.revoked);
        assert!(
            bus.specs_for("PreToolUse").unwrap().is_empty(),
            "the engine chain must not run a hook /hooks/revoke reported revoked"
        );

        apply_revocation(&registry, "PreToolUse#0".into(), key, true);
        assert_eq!(bus.specs_for("PreToolUse").unwrap().len(), 1);
    }
}
