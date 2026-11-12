//! Daemon-side hook runner.
//!
//! Subscribes to a session's engine [`HookBus`] and runs **user-configured**
//! shell commands and HTTP webhooks on lifecycle events — the Claude-Code /
//! OpenHarness hook pattern. Configured via environment variables so the default
//! is a safe no-op:
//!
//! - `RIGHTKIT_HOOK_PRE_TOOL_USE`       — command run before each tool call
//! - `RIGHTKIT_HOOK_POST_TOOL_USE`      — command run after each tool call
//! - `RIGHTKIT_HOOK_USER_PROMPT_SUBMIT` — command run after each user prompt
//! - `RIGHTKIT_HOOK_NOTIFICATION`       — command run for assistant notifications
//! - `RIGHTKIT_HOOK_SUBAGENT_STOP`      — command run when a sub-agent stops
//! - `RIGHTKIT_HOOK_STOP_BLOCKING`      — command run by the engine before completion
//! - `RIGHTKIT_HOOK_PRE_COMMIT`         — blocking command run before a commit (WS-3 auditor gate)
//! - `RIGHTKIT_HOOK_PRE_TOOL_USE_URL`   — URL POSTed before each tool call
//! - `RIGHTKIT_HOOK_POST_TOOL_USE_URL`  — URL POSTed after each tool call
//! - `RIGHTKIT_HOOK_USER_PROMPT_SUBMIT_URL`
//! - `RIGHTKIT_HOOK_NOTIFICATION_URL`
//! - `RIGHTKIT_HOOK_SUBAGENT_STOP_URL`
//! - `RIGHTKIT_HOOK_PRE_COMMIT_URL`     — URL POSTed before a commit
//! - `RIGHTKIT_HOOK_PRE_TOOL_USE_MCP`   — qualified MCP tool invoked before each tool call
//! - `RIGHTKIT_HOOK_POST_TOOL_USE_MCP`  — qualified MCP tool invoked after each tool call
//! - `RIGHTKIT_HOOK_USER_PROMPT_SUBMIT_MCP`
//! - `RIGHTKIT_HOOK_NOTIFICATION_MCP`
//! - `RIGHTKIT_HOOK_SUBAGENT_STOP_MCP`
//! - `RIGHTKIT_HOOK_PRE_COMMIT_MCP`     — qualified MCP tool invoked before a commit
//!
//! The command runs through the platform shell with context in the environment:
//! `RIGHTKIT_HOOK_EVENT`, `RIGHTKIT_HOOK_TOOL`, `RIGHTKIT_HOOK_SESSION`,
//! `RIGHTKIT_HOOK_ROLE`, `RIGHTKIT_HOOK_SUCCESS`, `RIGHTKIT_HOOK_MESSAGE`, and
//! `RIGHTKIT_HOOK_IS_ERROR`. HTTP hooks receive the same lifecycle
//! data as JSON. `PreToolUse` command hooks are evaluated by the engine before
//! dispatch so exit code 2 can deny the call; the remaining daemon hook sinks
//! are fire-and-forget and never block the agent loop.

use rightkit_hooks::agent_hooks::registry::{HookAdmissionV1, HookRejectionReasonV1};
use rightkit_hooks::agent_hooks::{HookBus, HookEvent, HookRegistry};
use serde::Serialize;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::{response::Json, routing::get, Router};
use serde_json::{json, Map, Value};

include!("hooks_management.rs");

#[derive(Clone, Default)]
pub struct HookRunnerConfig {
    pub pre_tool_use: Option<String>,
    pub post_tool_use: Option<String>,
    pub user_prompt_submit: Option<String>,
    pub notification: Option<String>,
    pub subagent_stop: Option<String>,
    pub blocking_stop: Option<String>,
    /// Blocking command run before a commit (WS-3 auditor gate).
    pub pre_commit: Option<String>,
    pub pre_tool_use_url: Option<String>,
    pub post_tool_use_url: Option<String>,
    pub user_prompt_submit_url: Option<String>,
    pub notification_url: Option<String>,
    pub subagent_stop_url: Option<String>,
    /// URL POSTed before a commit (fire-and-forget notification).
    pub pre_commit_url: Option<String>,
    pub pre_tool_use_mcp_tool: Option<String>,
    pub post_tool_use_mcp_tool: Option<String>,
    pub user_prompt_submit_mcp_tool: Option<String>,
    pub notification_mcp_tool: Option<String>,
    pub subagent_stop_mcp_tool: Option<String>,
    /// Qualified MCP tool invoked before a commit (fire-and-forget notification).
    pub pre_commit_mcp_tool: Option<String>,
    pub mcp: Option<Arc<rightkit_mcp::agent_mcp::McpManager>>,
    /// Shared, daemon-wide revocation registry consulted by the runner per
    /// event. `None` = runner ignores revocations (back-compat default).
    pub revoked: Option<Arc<HookRegistry>>,
}

impl HookRunnerConfig {
    fn from_env() -> Self {
        Self {
            pre_tool_use: env_value("RIGHTKIT_HOOK_PRE_TOOL_USE"),
            post_tool_use: env_value("RIGHTKIT_HOOK_POST_TOOL_USE"),
            user_prompt_submit: env_value("RIGHTKIT_HOOK_USER_PROMPT_SUBMIT"),
            notification: env_value("RIGHTKIT_HOOK_NOTIFICATION"),
            subagent_stop: env_value("RIGHTKIT_HOOK_SUBAGENT_STOP"),
            blocking_stop: env_value("RIGHTKIT_HOOK_STOP_BLOCKING"),
            pre_commit: env_value("RIGHTKIT_HOOK_PRE_COMMIT"),
            pre_tool_use_url: env_value("RIGHTKIT_HOOK_PRE_TOOL_USE_URL"),
            post_tool_use_url: env_value("RIGHTKIT_HOOK_POST_TOOL_USE_URL"),
            user_prompt_submit_url: env_value("RIGHTKIT_HOOK_USER_PROMPT_SUBMIT_URL"),
            notification_url: env_value("RIGHTKIT_HOOK_NOTIFICATION_URL"),
            subagent_stop_url: env_value("RIGHTKIT_HOOK_SUBAGENT_STOP_URL"),
            pre_commit_url: env_value("RIGHTKIT_HOOK_PRE_COMMIT_URL"),
            pre_tool_use_mcp_tool: env_value("RIGHTKIT_HOOK_PRE_TOOL_USE_MCP"),
            post_tool_use_mcp_tool: env_value("RIGHTKIT_HOOK_POST_TOOL_USE_MCP"),
            user_prompt_submit_mcp_tool: env_value("RIGHTKIT_HOOK_USER_PROMPT_SUBMIT_MCP"),
            notification_mcp_tool: env_value("RIGHTKIT_HOOK_NOTIFICATION_MCP"),
            subagent_stop_mcp_tool: env_value("RIGHTKIT_HOOK_SUBAGENT_STOP_MCP"),
            pre_commit_mcp_tool: env_value("RIGHTKIT_HOOK_PRE_COMMIT_MCP"),
            mcp: None,
            revoked: None,
        }
    }

    fn is_empty(&self) -> bool {
        self.pre_tool_use.is_none()
            && self.post_tool_use.is_none()
            && self.user_prompt_submit.is_none()
            && self.notification.is_none()
            && self.subagent_stop.is_none()
            && self.blocking_stop.is_none()
            && self.pre_commit.is_none()
            && self.pre_tool_use_url.is_none()
            && self.post_tool_use_url.is_none()
            && self.user_prompt_submit_url.is_none()
            && self.notification_url.is_none()
            && self.subagent_stop_url.is_none()
            && self.pre_commit_url.is_none()
            && self.pre_tool_use_mcp_tool.is_none()
            && self.post_tool_use_mcp_tool.is_none()
            && self.user_prompt_submit_mcp_tool.is_none()
            && self.notification_mcp_tool.is_none()
            && self.subagent_stop_mcp_tool.is_none()
            && self.pre_commit_mcp_tool.is_none()
    }

    fn needs_mcp(&self) -> bool {
        self.pre_tool_use_mcp_tool.is_some()
            || self.post_tool_use_mcp_tool.is_some()
            || self.user_prompt_submit_mcp_tool.is_some()
            || self.notification_mcp_tool.is_some()
            || self.subagent_stop_mcp_tool.is_some()
            || self.pre_commit_mcp_tool.is_some()
    }
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
            "required": ["event", "kind", "envVar", "configured"],
            "properties": {
                "event": {"type": "string"},
                "kind": {"type": "string", "enum": ["command", "url", "mcp"]},
                "envVar": {"type": "string"},
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
    Json(hook_status_from_config(&HookRunnerConfig::from_env()))
}

pub(crate) fn current_hook_status_value() -> Value {
    serde_json::to_value(hook_status_from_config(&HookRunnerConfig::from_env()))
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

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HookStatusEntry {
    event: &'static str,
    kind: &'static str,
    env_var: &'static str,
    configured: bool,
}

fn hook_status_from_config(config: &HookRunnerConfig) -> HookStatus {
    let hooks = vec![
        hook_entry(
            "PreToolUse",
            "command",
            "RIGHTKIT_HOOK_PRE_TOOL_USE",
            config.pre_tool_use.is_some(),
        ),
        hook_entry(
            "PostToolUse",
            "command",
            "RIGHTKIT_HOOK_POST_TOOL_USE",
            config.post_tool_use.is_some(),
        ),
        hook_entry(
            "UserPromptSubmit",
            "command",
            "RIGHTKIT_HOOK_USER_PROMPT_SUBMIT",
            config.user_prompt_submit.is_some(),
        ),
        hook_entry(
            "Notification",
            "command",
            "RIGHTKIT_HOOK_NOTIFICATION",
            config.notification.is_some(),
        ),
        hook_entry(
            "SubagentStop",
            "command",
            "RIGHTKIT_HOOK_SUBAGENT_STOP",
            config.subagent_stop.is_some(),
        ),
        hook_entry(
            "Stop",
            "command",
            "RIGHTKIT_HOOK_STOP_BLOCKING",
            config.blocking_stop.is_some(),
        ),
        hook_entry(
            "PreCommit",
            "command",
            "RIGHTKIT_HOOK_PRE_COMMIT",
            config.pre_commit.is_some(),
        ),
        hook_entry(
            "PreToolUse",
            "url",
            "RIGHTKIT_HOOK_PRE_TOOL_USE_URL",
            config.pre_tool_use_url.is_some(),
        ),
        hook_entry(
            "PostToolUse",
            "url",
            "RIGHTKIT_HOOK_POST_TOOL_USE_URL",
            config.post_tool_use_url.is_some(),
        ),
        hook_entry(
            "UserPromptSubmit",
            "url",
            "RIGHTKIT_HOOK_USER_PROMPT_SUBMIT_URL",
            config.user_prompt_submit_url.is_some(),
        ),
        hook_entry(
            "Notification",
            "url",
            "RIGHTKIT_HOOK_NOTIFICATION_URL",
            config.notification_url.is_some(),
        ),
        hook_entry(
            "SubagentStop",
            "url",
            "RIGHTKIT_HOOK_SUBAGENT_STOP_URL",
            config.subagent_stop_url.is_some(),
        ),
        hook_entry(
            "PreCommit",
            "url",
            "RIGHTKIT_HOOK_PRE_COMMIT_URL",
            config.pre_commit_url.is_some(),
        ),
        hook_entry(
            "PreToolUse",
            "mcp",
            "RIGHTKIT_HOOK_PRE_TOOL_USE_MCP",
            config.pre_tool_use_mcp_tool.is_some(),
        ),
        hook_entry(
            "PostToolUse",
            "mcp",
            "RIGHTKIT_HOOK_POST_TOOL_USE_MCP",
            config.post_tool_use_mcp_tool.is_some(),
        ),
        hook_entry(
            "UserPromptSubmit",
            "mcp",
            "RIGHTKIT_HOOK_USER_PROMPT_SUBMIT_MCP",
            config.user_prompt_submit_mcp_tool.is_some(),
        ),
        hook_entry(
            "Notification",
            "mcp",
            "RIGHTKIT_HOOK_NOTIFICATION_MCP",
            config.notification_mcp_tool.is_some(),
        ),
        hook_entry(
            "SubagentStop",
            "mcp",
            "RIGHTKIT_HOOK_SUBAGENT_STOP_MCP",
            config.subagent_stop_mcp_tool.is_some(),
        ),
        hook_entry(
            "PreCommit",
            "mcp",
            "RIGHTKIT_HOOK_PRE_COMMIT_MCP",
            config.pre_commit_mcp_tool.is_some(),
        ),
    ];
    let total_configured = hooks.iter().filter(|hook| hook.configured).count();
    HookStatus {
        configured: total_configured > 0,
        total_configured,
        command_configured: count_kind(&hooks, "command"),
        url_configured: count_kind(&hooks, "url"),
        mcp_configured: count_kind(&hooks, "mcp"),
        hooks,
    }
}

fn hook_entry(
    event: &'static str,
    kind: &'static str,
    env_var: &'static str,
    configured: bool,
) -> HookStatusEntry {
    HookStatusEntry {
        event,
        kind,
        env_var,
        configured,
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

/// Spawn a background task that runs configured hook commands for `bus`'s
/// events. No-op (and no task) when no hooks are configured.
#[allow(dead_code)]
pub fn spawn_hook_runner(bus: &HookBus) {
    spawn_hook_runner_with_config(bus, HookRunnerConfig::from_env());
}

/// `confine` is the session's sandbox requirement (`--safe-mode`). Operator
/// configured hook commands and hook MCP servers are operator-authored, not
/// model-authored: outside safe mode they run unconfined (a typed, recorded
/// opt-out on the launch context), so a hook script and its log may live
/// anywhere the operator can reach. In safe mode every hook launch is
/// AppContainer/Seatbelt/Landlock-confined and fails closed.
pub fn spawn_hook_runner_for_workspace(bus: &HookBus, workspace_root: &Path, confine: bool) {
    let mut config = HookRunnerConfig::from_env();
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
            match rightkit_agent_tools::coding_tools::process_resource_authority::require_process_resource_governor() {
                Ok(governor) => governor,
                Err(error) => {
                    tracing::warn!(error = %error, "MCP hook transport setup denied: process resource authority unavailable");
                    return;
                }
            };
        let launcher: Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter> = Arc::new(
            rightkit_agent_tools::coding_tools::process_resource_authority::GovernedProcessLaunchAdapter::new(
                rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter,
                governor,
                rightkit_agent_tools::coding_tools::process_resource_authority::ResourceKind::Mcp,
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

/// Confined by default: callers that have not declared a session sandbox
/// requirement keep the strict behavior.
pub fn spawn_hook_runner_with_config(bus: &HookBus, config: HookRunnerConfig) {
    spawn_hook_runner_with_config_confined(bus, config, true);
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
    let mut sub = bus.subscribe();
    tokio::spawn(async move {
        while let Some(ev) = sub.recv().await {
            let reg_ref = registry.as_deref();
            let is_revoked = |key: &str| -> bool { reg_ref.is_some_and(|r| r.is_revoked(key)) };
            match &ev {
                HookEvent::PreToolUse { session_id, tool } => {
                    let payload = hook_payload("PreToolUse", session_id, Some(tool), None);
                    if let Some(url) = &config.pre_tool_use_url {
                        if !is_revoked("RIGHTKIT_HOOK_PRE_TOOL_USE_URL") {
                            post_http_hook(url.clone(), payload.clone());
                        }
                    }
                    if let (Some(manager), Some(tool_name)) =
                        (&config.mcp, &config.pre_tool_use_mcp_tool)
                    {
                        if !is_revoked("RIGHTKIT_HOOK_PRE_TOOL_USE_MCP") {
                            invoke_mcp_hook(
                                manager.clone(),
                                tool_name.clone(),
                                payload,
                                session_id.clone(),
                            );
                        }
                    }
                }
                HookEvent::PostToolUse {
                    session_id,
                    tool,
                    is_error,
                } => {
                    if let Some(cmd) = &config.post_tool_use {
                        if !is_revoked("RIGHTKIT_HOOK_POST_TOOL_USE") {
                            run(
                                cmd,
                                &hook_payload(
                                    "PostToolUse",
                                    session_id,
                                    Some(tool),
                                    Some(*is_error),
                                ),
                                confine,
                            )
                            .await;
                        }
                    }
                    let payload =
                        hook_payload("PostToolUse", session_id, Some(tool), Some(*is_error));
                    if let Some(url) = &config.post_tool_use_url {
                        if !is_revoked("RIGHTKIT_HOOK_POST_TOOL_USE_URL") {
                            post_http_hook(url.clone(), payload.clone());
                        }
                    }
                    if let (Some(manager), Some(tool_name)) =
                        (&config.mcp, &config.post_tool_use_mcp_tool)
                    {
                        if !is_revoked("RIGHTKIT_HOOK_POST_TOOL_USE_MCP") {
                            invoke_mcp_hook(
                                manager.clone(),
                                tool_name.clone(),
                                payload,
                                session_id.clone(),
                            );
                        }
                    }
                }
                HookEvent::UserPromptSubmit { session_id } => {
                    let payload = hook_payload("UserPromptSubmit", session_id, None, None);
                    if let Some(cmd) = &config.user_prompt_submit {
                        if !is_revoked("RIGHTKIT_HOOK_USER_PROMPT_SUBMIT") {
                            run(cmd, &payload, confine).await;
                        }
                    }
                    if let Some(url) = &config.user_prompt_submit_url {
                        if !is_revoked("RIGHTKIT_HOOK_USER_PROMPT_SUBMIT_URL") {
                            post_http_hook(url.clone(), payload.clone());
                        }
                    }
                    if let (Some(manager), Some(tool_name)) =
                        (&config.mcp, &config.user_prompt_submit_mcp_tool)
                    {
                        if !is_revoked("RIGHTKIT_HOOK_USER_PROMPT_SUBMIT_MCP") {
                            invoke_mcp_hook(
                                manager.clone(),
                                tool_name.clone(),
                                payload,
                                session_id.clone(),
                            );
                        }
                    }
                }
                HookEvent::Notification {
                    session_id,
                    message,
                } => {
                    let payload =
                        hook_payload("Notification", session_id, None, None).with_message(message);
                    if let Some(cmd) = &config.notification {
                        if !is_revoked("RIGHTKIT_HOOK_NOTIFICATION") {
                            run(cmd, &payload, confine).await;
                        }
                    }
                    if let Some(url) = &config.notification_url {
                        if !is_revoked("RIGHTKIT_HOOK_NOTIFICATION_URL") {
                            post_http_hook(url.clone(), payload.clone());
                        }
                    }
                    if let (Some(manager), Some(tool_name)) =
                        (&config.mcp, &config.notification_mcp_tool)
                    {
                        if !is_revoked("RIGHTKIT_HOOK_NOTIFICATION_MCP") {
                            invoke_mcp_hook(
                                manager.clone(),
                                tool_name.clone(),
                                payload,
                                session_id.clone(),
                            );
                        }
                    }
                }
                HookEvent::SubagentStop {
                    session_id,
                    role,
                    success,
                } => {
                    let payload = hook_payload("SubagentStop", session_id, None, None)
                        .with_subagent(role, *success);
                    if let Some(cmd) = &config.subagent_stop {
                        if !is_revoked("RIGHTKIT_HOOK_SUBAGENT_STOP") {
                            run(cmd, &payload, confine).await;
                        }
                    }
                    if let Some(url) = &config.subagent_stop_url {
                        if !is_revoked("RIGHTKIT_HOOK_SUBAGENT_STOP_URL") {
                            post_http_hook(url.clone(), payload.clone());
                        }
                    }
                    if let (Some(manager), Some(tool_name)) =
                        (&config.mcp, &config.subagent_stop_mcp_tool)
                    {
                        if !is_revoked("RIGHTKIT_HOOK_SUBAGENT_STOP_MCP") {
                            invoke_mcp_hook(
                                manager.clone(),
                                tool_name.clone(),
                                payload,
                                session_id.clone(),
                            );
                        }
                    }
                }
                HookEvent::PreCommit { session_id, diff } => {
                    let payload = hook_payload("PreCommit", session_id, None, None).with_diff(diff);
                    if let Some(cmd) = &config.pre_commit {
                        if !is_revoked("RIGHTKIT_HOOK_PRE_COMMIT") {
                            run(cmd, &payload, confine).await;
                        }
                    }
                    if let Some(url) = &config.pre_commit_url {
                        if !is_revoked("RIGHTKIT_HOOK_PRE_COMMIT_URL") {
                            post_http_hook(url.clone(), payload.clone());
                        }
                    }
                    if let (Some(manager), Some(tool_name)) =
                        (&config.mcp, &config.pre_commit_mcp_tool)
                    {
                        if !is_revoked("RIGHTKIT_HOOK_PRE_COMMIT_MCP") {
                            invoke_mcp_hook(
                                manager.clone(),
                                tool_name.clone(),
                                payload,
                                session_id.clone(),
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    });
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.trim().is_empty())
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

fn post_http_hook(url: String, payload: HttpHookPayload) {
    tokio::spawn(async move {
        // Fire-and-forget: the hook URL is operator-configured, so its host alone may
        // resolve privately; the call is bounded to 5s and any failure is ignored.
        let Ok(client) = rightkit_agent_daemon::http_client::endpoint_client(&url, Duration::from_secs(5))
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

async fn run(cmd: &str, payload: &HttpHookPayload, confine: bool) {
    #[cfg(windows)]
    let (sh, flag) = ("cmd", "/C");
    #[cfg(not(windows))]
    let (sh, flag) = ("sh", "-c");

    let mut env = vec![
        ("RIGHTKIT_HOOK_EVENT".into(), payload.event.clone()),
        (
            "RIGHTKIT_HOOK_TOOL".into(),
            payload.tool.clone().unwrap_or_default(),
        ),
        ("RIGHTKIT_HOOK_SESSION".into(), payload.session_id.clone()),
        (
            "RIGHTKIT_HOOK_ROLE".into(),
            payload.role.clone().unwrap_or_default(),
        ),
        (
            "RIGHTKIT_HOOK_SUCCESS".into(),
            payload
                .success
                .map(|v| if v { "1" } else { "0" })
                .unwrap_or("")
                .into(),
        ),
        (
            "RIGHTKIT_HOOK_MESSAGE".into(),
            payload.message.clone().unwrap_or_default(),
        ),
    ];
    if let Some(e) = payload.is_error {
        env.push((
            "RIGHTKIT_HOOK_IS_ERROR".into(),
            if e { "1" } else { "0" }.into(),
        ));
    }
    if let Some(diff) = &payload.diff {
        env.push(("RIGHTKIT_HOOK_DIFF".into(), diff.clone()));
    }
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program: sh.into(),
        argv: vec![flag.into(), cmd.into()],
        cwd: ".".into(),
        context: rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: std::env::current_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."))
                .to_string_lossy()
                .into(),
            sandbox_profile: Some("hook".into()),
            sandbox_required: confine,
            network_required: false,
        },
        env,
    };
    let governor =
        match rightkit_agent_tools::coding_tools::process_resource_authority::require_process_resource_governor() {
            Ok(governor) => governor,
            Err(error) => {
                tracing::warn!(hook_event = %payload.event, error = %error, "hook launch denied: process resource authority unavailable");
                return;
            }
        };
    let launcher = rightkit_agent_tools::coding_tools::process_resource_authority::GovernedProcessLaunchAdapter::new(
        rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter,
        governor,
        rightkit_agent_tools::coding_tools::process_resource_authority::ResourceKind::ChildProcess,
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
    // waiting so a verbose hook cannot block itself on a full pipe.
    let stdout = child.take_stdout().map(|mut stream| {
        tokio::spawn(async move {
            let mut bytes = Vec::new();
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut bytes).await;
        })
    });
    let stderr = child.take_stderr().map(|mut stream| {
        tokio::spawn(async move {
            let mut bytes = Vec::new();
            let _ = tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut bytes).await;
        })
    });
    match tokio::time::timeout(Duration::from_secs(30), child.wait()).await {
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
            tracing::warn!(hook_event = %payload.event, "hook timed out and was cancelled");
            // H8: cancelled before a result was observed — reject.
            record_hook_admission(
                &payload.event,
                HookAdmissionV1::from_execution(false, false, Vec::new()),
            );
        }
    }
    if let Some(reader) = stdout {
        let _ = reader.await;
    }
    if let Some(reader) = stderr {
        let _ = reader.await;
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
