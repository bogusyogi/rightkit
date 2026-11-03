// D4 — Delivery (branch / worktree / stage / commit / push / PR / checks / failing logs).
//
// Standalone route + contract fragment for Wave 5 Git/PR/CI delivery. Agent 4
// owns the central `api_router.rs` and central `api_contract` endpoint manifest;
// this module exposes builders and contract fragments the integration lane
// wires in. No central registration happens here.
//
// The D4 design honors:
//   - visible commands and errors (every route returns the raw command, exit
//     code, stdout/stderr tail, and elapsed time);
//   - worktree-only writes outside the trusted root (`enforce_workspace_root`);
//   - authority-gated execution (`enforce_trusted_source_authority` for any
//     state-changing route);
//   - typed capabilities per host (kind, available, scope, auth, setup state,
//     latency, last error) so the UI can show honest "configured / unavailable".
//
// `BackendKind` runtime support is intentionally narrow — the daemon runs
// commands through `tokio::process::Command` and `git` CLI on the local
// machine only. Remote backends (SSH, WSL, configured remote) are typed but
// flagged `available: false` until an explicit executor is wired into the
// shell/process lane. The UI surfaces this distinction honestly.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::api::DaemonState;
use crate::api_permissions::enforce_trusted_source_authority;
use rightkit_llm::agent_providers::github::Forge;

/// Route builder for the delivery surface. Returns a router that Agent 4 may
/// `merge` into the central daemon router. Not invoked from this module.
pub fn delivery_routes() -> Router<DaemonState> {
    Router::new()
        .route("/delivery/status", get(get_delivery_status_handler))
        .route("/delivery/forges", get(list_forges_handler))
        .route("/delivery/branches", get(list_branches_handler))
        .route("/delivery/worktrees", get(list_worktrees_handler))
        .route(
            "/delivery/worktree",
            post(create_worktree_handler).delete(remove_worktree_handler),
        )
        .route(
            "/delivery/stage",
            post(stage_paths_handler).delete(unstage_paths_handler),
        )
        .route("/delivery/commit", post(commit_handler))
        .route("/delivery/push", post(push_handler))
        .route("/delivery/pull", post(pull_handler))
        .route("/delivery/pr", post(create_pr_handler))
        .route("/delivery/pr/{id}", get(get_pr_handler))
        .route("/delivery/pr/{id}/checks", get(list_pr_checks_handler))
        .route(
            "/delivery/pr/{id}/checks/{checkId}/rerun",
            post(rerun_check_handler),
        )
        .route(
            "/delivery/pr/{id}/checks/{checkId}/logs",
            get(get_check_logs_handler),
        )
        .route("/delivery/pr/{id}/threads", get(list_pr_threads_handler))
        .route("/delivery/command", post(run_command_handler))
        .route("/delivery/cancel", post(cancel_command_handler))
}

// ─── Typed capabilities & snapshot ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DeliveryCapabilityKind {
    Git,
    PullRequest,
    Checks,
    FailingLogs,
    BackgroundCommand,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryCapability {
    pub kind: DeliveryCapabilityKind,
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryAuthorityStatus {
    pub trusted: bool,
    pub source: String,
    pub workspace_root: String,
    pub trust_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeliverySnapshot {
    pub workspace_root: String,
    pub host_kind: DeliveryHostKind,
    pub host_available: bool,
    pub host_unavailable_reason: Option<String>,
    pub capabilities: Vec<DeliveryCapability>,
    pub authority: DeliveryAuthorityStatus,
    pub git_binary: Option<String>,
    pub gh_binary: Option<String>,
    pub git_version: Option<String>,
    pub gh_version: Option<String>,
    pub checked_at: String,
}

/// Honest `BackendKind`-equivalent: the runtime only supports `Local`.
/// `Worktree`, `Ssh`, `Wsl`, and `ConfiguredRemote` are typed but flagged
/// unavailable until a dedicated executor is wired into the shell lane.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum DeliveryHostKind {
    Local,
    Worktree,
    Ssh,
    Wsl,
    ConfiguredRemote,
}

impl DeliveryHostKind {
    pub fn is_runtime_supported(self) -> bool {
        // The daemon only executes commands on the local machine today.
        // Other kinds are surfaced so the UI can render them honestly as
        // "configured but not yet supported by the runtime" instead of
        // pretending they work.
        matches!(self, DeliveryHostKind::Local)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryHostKind::Local => "local",
            DeliveryHostKind::Worktree => "worktree",
            DeliveryHostKind::Ssh => "ssh",
            DeliveryHostKind::Wsl => "wsl",
            DeliveryHostKind::ConfiguredRemote => "configuredRemote",
        }
    }
}

// ─── Errors & command records ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CommandRecord {
    pub command_id: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub exit_code: Option<i32>,
    pub status: CommandStatus,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub host_kind: DeliveryHostKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum CommandStatus {
    Pending,
    Running,
    Exited,
    Killed,
    SpawnFailed,
    Cancelled,
    AuthorityDenied,
}

// ─── Inputs ───────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeliveryWorkspaceQuery {
    pub(crate) workspace_root: Option<String>,
    pub(crate) host: Option<String>,
    pub(crate) forge: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CreateWorktreeRequest {
    pub(crate) workspace_root: String,
    pub(crate) branch: String,
    pub(crate) base: Option<String>,
    pub(crate) path: Option<String>,
    #[serde(default)]
    pub(crate) force: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoveWorktreeRequest {
    pub(crate) workspace_root: String,
    pub(crate) path: String,
    #[serde(default)]
    pub(crate) force: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StagePathsRequest {
    workspace_root: String,
    paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitRequest {
    workspace_root: String,
    message: String,
    #[serde(default)]
    amend: bool,
    #[serde(default)]
    sign_off: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PushRequest {
    workspace_root: String,
    #[serde(default)]
    remote: Option<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    set_upstream: bool,
    #[serde(default)]
    force: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequestRequest {
    workspace_root: String,
    #[serde(default)]
    forge: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    head: Option<String>,
    #[serde(default)]
    draft: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunCommandRequest {
    workspace_root: String,
    command: String,
    args: Vec<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelCommandRequest {
    workspace_root: String,
    command_id: String,
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn list_forges_handler() -> Json<Value> {
    // Capability discovery must not serialize three process timeout windows.
    let (github, gitlab, bitbucket, azure) = tokio::join!(
        forge_capability(Forge::GitHub),
        forge_capability(Forge::GitLab),
        forge_capability(Forge::Bitbucket),
        forge_capability(Forge::AzureDevOps),
    );
    Json(json!({ "forges": [github, gitlab, bitbucket, azure] }))
}

async fn forge_capability(forge: Forge) -> Value {
    let (cli, version) = match forge.cli_binary() {
        Some(binary) => probe_binary(binary, &["--version"]).await,
        None => (None, None),
    };
    json!({
        "forge": forge.as_str(),
        "available": forge.supports_pr_ci() && cli.is_some(),
        "cli": cli,
        "version": version,
        "apiBase": forge.api_base(),
        "reason": if !forge.supports_pr_ci() {
            "forge is reserved; PR/CI adapter is not wired in this daemon lane"
        } else if cli.is_none() {
            "forge CLI is unavailable or its version probe failed"
        } else {
            "PR/CI command adapter is wired"
        }
    })
}

async fn get_delivery_status_handler(
    State(state): State<DaemonState>,
    Query(query): Query<DeliveryWorkspaceQuery>,
) -> Result<Json<DeliverySnapshot>, StatusCode> {
    let workspace_root = query
        .workspace_root
        .clone()
        .unwrap_or_else(|| state_default_workspace_root(&state));
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let host_kind = parse_host_kind(query.host.as_deref());
    let (git_binary, git_version) = probe_binary("git", &["--version"]).await;
    let (gh_binary, gh_version) = probe_binary("gh", &["--version"]).await;
    let (glab_binary, _glab_version) = probe_binary("glab", &["--version"]).await;
    let forge_cli_available = gh_binary.is_some() || glab_binary.is_some();

    let mut capabilities = Vec::new();
    capabilities.push(DeliveryCapability {
        kind: DeliveryCapabilityKind::Git,
        available: host_kind.is_runtime_supported() && git_binary.is_some(),
        reason: if !host_kind.is_runtime_supported() {
            Some(format!(
                "host '{}' is typed but not executed by the runtime; only Local runs commands today",
                host_kind.as_str()
            ))
        } else {
            git_binary
                .as_ref()
                .map(|_| "git binary located on PATH".to_string())
        },
    });
    capabilities.push(DeliveryCapability {
        kind: DeliveryCapabilityKind::PullRequest,
        available: host_kind.is_runtime_supported() && forge_cli_available,
        reason: if gh_binary.is_none() {
            Some("gh CLI not located on PATH; PRs cannot be opened without it".to_string())
        } else if !host_kind.is_runtime_supported() {
            Some(format!(
                "host '{}' is not runtime-supported for PR commands",
                host_kind.as_str()
            ))
        } else {
            Some("gh binary located on PATH".to_string())
        },
    });
    capabilities.push(DeliveryCapability {
        kind: DeliveryCapabilityKind::Checks,
        available: host_kind.is_runtime_supported() && forge_cli_available,
        reason: Some(
            "Checks surface the gh CLI run/check APIs; unavailable if gh is missing or host is non-local"
                .to_string(),
        ),
    });
    capabilities.push(DeliveryCapability {
        kind: DeliveryCapabilityKind::FailingLogs,
        available: host_kind.is_runtime_supported() && forge_cli_available,
        reason: Some(
            "Failing logs route through `gh run view --log-failed`; requires gh on a runtime-supported host"
                .to_string(),
        ),
    });
    capabilities.push(DeliveryCapability {
        kind: DeliveryCapabilityKind::BackgroundCommand,
        available: host_kind.is_runtime_supported(),
        reason: if host_kind.is_runtime_supported() {
            Some("Background commands route through the shell lane".to_string())
        } else {
            Some(format!(
                "host '{}' is not runtime-supported; background commands are refused",
                host_kind.as_str()
            ))
        },
    });

    let authority = DeliveryAuthorityStatus {
        trusted: true,
        source: "tauri-webview".to_string(),
        workspace_root: workspace_root.clone(),
        trust_label: "trusted".to_string(),
    };

    let snapshot = DeliverySnapshot {
        workspace_root,
        host_kind,
        host_available: host_kind.is_runtime_supported(),
        host_unavailable_reason: if host_kind.is_runtime_supported() {
            None
        } else {
            Some(format!(
                "Delivery host '{}' is typed but not yet supported by the runtime. The daemon runs commands on Local only.",
                host_kind.as_str()
            ))
        },
        capabilities,
        authority,
        git_binary,
        gh_binary,
        git_version,
        gh_version,
        checked_at: now_iso(),
    };
    Ok(Json(snapshot))
}

async fn list_branches_handler(
    State(state): State<DaemonState>,
    Query(query): Query<DeliveryWorkspaceQuery>,
) -> Result<Json<Value>, StatusCode> {
    let workspace_root = query
        .workspace_root
        .clone()
        .unwrap_or_else(|| state_default_workspace_root(&state));
    let host = parse_host_kind(query.host.as_deref());
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !host.is_runtime_supported() {
        return Err(StatusCode::NOT_IMPLEMENTED);
    }
    let result = run_git(&workspace_root, &["branch", "--all", "--no-color"]).await;
    Ok(Json(serde_json::to_value(&result).unwrap_or(Value::Null)))
}

pub(crate) async fn list_worktrees_handler(
    State(state): State<DaemonState>,
    Query(query): Query<DeliveryWorkspaceQuery>,
) -> Result<Json<Value>, StatusCode> {
    let workspace_root = query
        .workspace_root
        .clone()
        .unwrap_or_else(|| state_default_workspace_root(&state));
    let host = parse_host_kind(query.host.as_deref());
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !host.is_runtime_supported() {
        return Err(StatusCode::NOT_IMPLEMENTED);
    }
    let result = run_git(&workspace_root, &["worktree", "list", "--porcelain"]).await;
    Ok(Json(serde_json::to_value(&result).unwrap_or(Value::Null)))
}

pub(crate) async fn create_worktree_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<CreateWorktreeRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !safe_git_atom(&req.branch) || req.base.as_deref().is_some_and(|base| !safe_git_atom(base)) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let path = req.path.clone().unwrap_or_else(|| {
        std::env::temp_dir()
            .join(format!(
                "coderight-wt-{}",
                sanitize_branch_segment(&req.branch)
            ))
            .to_string_lossy()
            .into_owned()
    });
    if path.trim().is_empty() || path.starts_with('-') {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut args: Vec<String> = vec!["worktree".to_string(), "add".to_string()];
    if req.force {
        args.push("--force".to_string());
    }
    args.push(path.clone());
    args.push("-b".to_string());
    args.push(req.branch.clone());
    if let Some(base) = req.base.clone() {
        args.push(base);
    }
    let result = run_worktree_git(&req.workspace_root, &args, &path, true).await?;
    Ok(Json(serde_json::json!({
        "command": result.command,
        "exitCode": result.exit_code,
        "stdout": result.stdout_tail,
        "stderr": result.stderr_tail,
        "path": path,
        "branch": req.branch,
        "status": result.status,
    })))
}

pub(crate) async fn remove_worktree_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<RemoveWorktreeRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req.path.trim().is_empty() || req.path.starts_with('-') {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut args: Vec<String> = vec!["worktree".to_string(), "remove".to_string()];
    if req.force {
        args.push("--force".to_string());
    }
    args.push(req.path.clone());
    let result = run_worktree_git(&req.workspace_root, &args, &req.path, false).await?;
    Ok(Json(serde_json::json!({
        "command": result.command,
        "exitCode": result.exit_code,
        "stdout": result.stdout_tail,
        "stderr": result.stderr_tail,
        "path": req.path,
        "status": result.status,
    })))
}

async fn stage_paths_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<StagePathsRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req.paths.is_empty() || req.paths.iter().any(|path| !safe_repo_path(path)) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut args: Vec<String> = vec!["add".to_string(), "--".to_string()];
    for p in &req.paths {
        args.push(p.clone());
    }
    let result = run_git(&req.workspace_root, &args).await;
    Ok(Json(serde_json::json!({
        "command": result.command,
        "exitCode": result.exit_code,
        "stdout": result.stdout_tail,
        "stderr": result.stderr_tail,
        "paths": req.paths,
        "status": result.status,
    })))
}

async fn unstage_paths_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<StagePathsRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req.paths.is_empty() || req.paths.iter().any(|path| !safe_repo_path(path)) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut args: Vec<String> = vec!["reset".to_string(), "HEAD".to_string(), "--".to_string()];
    for p in &req.paths {
        args.push(p.clone());
    }
    let result = run_git(&req.workspace_root, &args).await;
    Ok(Json(serde_json::json!({
        "command": result.command,
        "exitCode": result.exit_code,
        "stdout": result.stdout_tail,
        "stderr": result.stderr_tail,
        "paths": req.paths,
        "status": result.status,
    })))
}

async fn commit_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<CommitRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req.message.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut args: Vec<String> = vec!["commit".to_string(), "-m".to_string(), req.message.clone()];
    if req.amend {
        args.push("--amend".to_string());
    }
    if req.sign_off {
        args.push("--signoff".to_string());
    }
    let result = run_git(&req.workspace_root, &args).await;
    Ok(Json(serde_json::json!({
        "command": result.command,
        "exitCode": result.exit_code,
        "stdout": result.stdout_tail,
        "stderr": result.stderr_tail,
        "message": req.message,
        "status": result.status,
    })))
}

async fn push_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<PushRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if req
        .remote
        .as_deref()
        .is_some_and(|remote| !safe_git_atom(remote))
        || req
            .branch
            .as_deref()
            .is_some_and(|branch| !safe_git_atom(branch))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut args: Vec<String> = vec!["push".to_string()];
    if req.force {
        args.push("--force-with-lease".to_string());
    }
    if let Some(remote) = req.remote.clone() {
        args.push(remote);
    } else {
        args.push("origin".to_string());
    }
    if let Some(branch) = req.branch.clone() {
        if req.set_upstream {
            args.push("--set-upstream".to_string());
        }
        args.push(branch);
    } else if req.set_upstream {
        args.push("--set-upstream".to_string());
    }
    let result = run_git(&req.workspace_root, &args).await;
    Ok(Json(serde_json::json!({
        "command": result.command,
        "exitCode": result.exit_code,
        "stdout": result.stdout_tail,
        "stderr": result.stderr_tail,
        "status": result.status,
    })))
}

async fn pull_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let _ = state;
    let workspace_root = req
        .get("workspaceRoot")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let result = run_git(&workspace_root, &["pull", "--ff-only"]).await;
    Ok(Json(serde_json::json!({
        "command": result.command,
        "exitCode": result.exit_code,
        "stdout": result.stdout_tail,
        "stderr": result.stderr_tail,
        "status": result.status,
    })))
}

async fn create_pr_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<PullRequestRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let forge = parse_delivery_forge(req.forge.as_deref())?;
    ensure_pr_ci_forge(forge)?;
    let mut args: Vec<String> = match forge {
        Forge::GitHub => vec!["pr".to_string(), "create".to_string()],
        Forge::GitLab => vec!["mr".to_string(), "create".to_string()],
        Forge::Bitbucket | Forge::AzureDevOps => unreachable!("unsupported forge rejected above"),
    };
    if req.draft {
        args.push("--draft".to_string());
    }
    if let Some(base) = req.base.clone() {
        args.push(
            if forge == Forge::GitHub {
                "--base"
            } else {
                "--target-branch"
            }
            .to_string(),
        );
        args.push(base);
    }
    if let Some(head) = req.head.clone() {
        args.push(
            if forge == Forge::GitHub {
                "--head"
            } else {
                "--source-branch"
            }
            .to_string(),
        );
        args.push(head);
    }
    args.push("--title".to_string());
    args.push(
        req.title
            .clone()
            .unwrap_or_else(|| "(untitled)".to_string()),
    );
    if let Some(body) = req.body.clone() {
        args.push(
            if forge == Forge::GitHub {
                "--body"
            } else {
                "--description"
            }
            .to_string(),
        );
        args.push(body);
    }
    let result = run_forge(&req.workspace_root, forge, &args).await;
    Ok(Json(serde_json::json!({
        "forge": forge.as_str(),
        "command": result.command,
        "exitCode": result.exit_code,
        "stdout": result.stdout_tail,
        "stderr": result.stderr_tail,
        "status": result.status,
    })))
}

async fn get_pr_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Query(query): Query<DeliveryWorkspaceQuery>,
) -> Result<Json<Value>, StatusCode> {
    let _ = state;
    let workspace_root = query
        .workspace_root
        .clone()
        .unwrap_or_else(|| ".".to_string());
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let forge = parse_delivery_forge(query.forge.as_deref())?;
    ensure_pr_ci_forge(forge)?;
    let args = if forge == Forge::GitHub {
        vec!["pr", "view", id.as_str(), "--json", "number,title,state,url,author,headRefName,baseRefName,isDraft,mergeable,additions,deletions,changedFiles"]
    } else {
        vec!["mr", "view", id.as_str(), "--output", "json"]
    };
    let result = run_forge(&workspace_root, forge, &args).await;
    Ok(Json(serde_json::json!({
        "forge": forge.as_str(),
        "id": id,
        "command": result.command,
        "exitCode": result.exit_code,
        "stdout": result.stdout_tail,
        "stderr": result.stderr_tail,
        "status": result.status,
    })))
}

async fn list_pr_checks_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Query(query): Query<DeliveryWorkspaceQuery>,
) -> Result<Json<Value>, StatusCode> {
    let _ = state;
    let workspace_root = query
        .workspace_root
        .clone()
        .unwrap_or_else(|| ".".to_string());
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let forge = parse_delivery_forge(query.forge.as_deref())?;
    ensure_pr_ci_forge(forge)?;
    let args = if forge == Forge::GitHub {
        vec![
            "pr",
            "checks",
            id.as_str(),
            "--json",
            "bucket,name,state,link,startedAt,completedAt",
        ]
    } else {
        vec!["ci", "status", "--mr", id.as_str(), "--output", "json"]
    };
    let result = run_forge(&workspace_root, forge, &args).await;
    Ok(Json(serde_json::json!({
        "forge": forge.as_str(),
        "id": id,
        "command": result.command,
        "exitCode": result.exit_code,
        "stdout": result.stdout_tail,
        "stderr": result.stderr_tail,
        "status": result.status,
    })))
}

async fn rerun_check_handler(
    State(state): State<DaemonState>,
    Path((_id, check_id)): Path<(String, String)>,
    Query(query): Query<DeliveryWorkspaceQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let _ = state;
    let workspace_root = query
        .workspace_root
        .clone()
        .unwrap_or_else(|| ".".to_string());
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let forge = parse_delivery_forge(query.forge.as_deref())?;
    ensure_pr_ci_forge(forge)?;
    let args = if forge == Forge::GitHub {
        // `{id}` identifies the PR, not an owner/repository slug. Let gh
        // resolve its repository from the already-validated workspace.
        vec!["checks", "rerun", check_id.as_str()]
    } else {
        vec!["ci", "retry", check_id.as_str()]
    };
    let result = run_forge(&workspace_root, forge, &args).await;
    Ok(Json(serde_json::json!({
        "forge": forge.as_str(),
        "command": result.command,
        "exitCode": result.exit_code,
        "stdout": result.stdout_tail,
        "stderr": result.stderr_tail,
        "status": result.status,
    })))
}

async fn get_check_logs_handler(
    State(state): State<DaemonState>,
    Path((_id, check_id)): Path<(String, String)>,
    Query(query): Query<DeliveryWorkspaceQuery>,
) -> Result<Json<Value>, StatusCode> {
    let _ = state;
    let workspace_root = query
        .workspace_root
        .clone()
        .unwrap_or_else(|| ".".to_string());
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let forge = parse_delivery_forge(query.forge.as_deref())?;
    ensure_pr_ci_forge(forge)?;
    let args = if forge == Forge::GitHub {
        vec!["run", "view", check_id.as_str(), "--log-failed"]
    } else {
        vec!["ci", "trace", check_id.as_str()]
    };
    let result = run_forge(&workspace_root, forge, &args).await;
    Ok(Json(serde_json::json!({
        "forge": forge.as_str(),
        "command": result.command,
        "exitCode": result.exit_code,
        "stdout": result.stdout_tail,
        "stderr": result.stderr_tail,
        "status": result.status,
    })))
}

async fn list_pr_threads_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Query(query): Query<DeliveryWorkspaceQuery>,
) -> Result<Json<Value>, StatusCode> {
    let _ = state;
    let workspace_root = query
        .workspace_root
        .clone()
        .unwrap_or_else(|| ".".to_string());
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let forge = parse_delivery_forge(query.forge.as_deref())?;
    ensure_pr_ci_forge(forge)?;
    let args = if forge == Forge::GitHub {
        vec![
            "api".to_string(),
            format!("repos/:owner/:repo/pulls/{id}/comments"),
        ]
    } else {
        vec![
            "api".to_string(),
            format!("projects/:id/merge_requests/{id}/discussions"),
        ]
    };
    let result = run_forge(&workspace_root, forge, &args).await;
    Ok(Json(serde_json::json!({
        "forge": forge.as_str(),
        "id": id,
        "command": result.command,
        "exitCode": result.exit_code,
        "stdout": result.stdout_tail,
        "stderr": result.stderr_tail,
        "status": result.status,
    })))
}

async fn run_command_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<RunCommandRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let result = run_local_command(
        &req.command,
        &req.args,
        &req.workspace_root,
        req.timeout_ms.unwrap_or(120_000).clamp(100, 600_000),
    )
    .await;
    Ok(Json(serde_json::to_value(&result).unwrap_or(Value::Null)))
}

async fn cancel_command_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<CancelCommandRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    // Cancellation is delegated to the shell lane; without that wired in we
    // surface a typed refusal the UI can show honestly.
    Ok(Json(serde_json::json!({
        "cancelled": false,
        "commandId": req.command_id,
        "status": CommandStatus::Cancelled,
        "note": "Background command cancellation is delegated to the shell lane; not wired in this lane."
    })))
}

// ─── OpenAPI component fragment ──────────────────────────────────────────────

pub(crate) fn delivery_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "DeliveryForge".to_string(),
        json!({"type": "string", "enum": ["github", "gitlab", "bitbucket", "azure_devops"]}),
    );
    schemas.insert(
        "DeliveryForgeCapability".to_string(),
        json!({
            "type": "object",
            "required": ["forge", "available", "apiBase"],
            "properties": {
                "forge": {"$ref": "#/components/schemas/DeliveryForge"},
                "available": {"type": "boolean"},
                "cli": {"type": ["string", "null"]},
                "version": {"type": ["string", "null"]},
                "apiBase": {"type": "string"},
                "reason": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "DeliveryCapabilityKind".to_string(),
        json!({"type": "string", "enum": ["git", "pullRequest", "checks", "failingLogs", "backgroundCommand"]}),
    );
    schemas.insert(
        "DeliveryHostKind".to_string(),
        json!({"type": "string", "enum": ["local", "worktree", "ssh", "wsl", "configuredRemote"]}),
    );
    schemas.insert(
        "DeliveryCapability".to_string(),
        json!({
            "type": "object",
            "required": ["kind", "available"],
            "properties": {
                "kind": {"$ref": "#/components/schemas/DeliveryCapabilityKind"},
                "available": {"type": "boolean"},
                "reason": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "DeliveryAuthorityStatus".to_string(),
        json!({
            "type": "object",
            "required": ["trusted", "source", "workspaceRoot", "trustLabel"],
            "properties": {
                "trusted": {"type": "boolean"},
                "source": {"type": "string"},
                "workspaceRoot": {"type": "string"},
                "trustLabel": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "DeliverySnapshot".to_string(),
        json!({
            "type": "object",
            "required": [
                "workspaceRoot",
                "hostKind",
                "hostAvailable",
                "capabilities",
                "authority",
                "checkedAt"
            ],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "hostKind": {"$ref": "#/components/schemas/DeliveryHostKind"},
                "hostAvailable": {"type": "boolean"},
                "hostUnavailableReason": {"type": ["string", "null"]},
                "capabilities": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/DeliveryCapability"}
                },
                "authority": {"$ref": "#/components/schemas/DeliveryAuthorityStatus"},
                "gitBinary": {"type": ["string", "null"]},
                "ghBinary": {"type": ["string", "null"]},
                "gitVersion": {"type": ["string", "null"]},
                "ghVersion": {"type": ["string", "null"]},
                "checkedAt": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CommandStatus".to_string(),
        json!({
            "type": "string",
            "enum": ["pending", "running", "exited", "killed", "spawnFailed", "cancelled", "authorityDenied"]
        }),
    );
    schemas.insert(
        "CommandRecord".to_string(),
        json!({
            "type": "object",
            "required": ["commandId", "command", "args", "cwd", "startedAt", "status", "hostKind"],
            "properties": {
                "commandId": {"type": "string"},
                "command": {"type": "string"},
                "args": {"type": "array", "items": {"type": "string"}},
                "cwd": {"type": "string"},
                "startedAt": {"type": "string"},
                "finishedAt": {"type": ["string", "null"]},
                "exitCode": {"type": ["integer", "null"]},
                "status": {"$ref": "#/components/schemas/CommandStatus"},
                "stdoutTail": {"type": "string"},
                "stderrTail": {"type": "string"},
                "hostKind": {"$ref": "#/components/schemas/DeliveryHostKind"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CommandFailure".to_string(),
        json!({
            "type": "object",
            "required": ["code", "message"],
            "properties": {
                "code": {"type": "string"},
                "message": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

pub(crate) fn delivery_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("post", "/delivery/worktree") => Some(json!({
            "type": "object", "required": ["workspaceRoot", "branch"],
            "properties": {
                "workspaceRoot": {"type": "string"}, "branch": {"type": "string"},
                "base": {"type": ["string", "null"]}, "path": {"type": ["string", "null"]},
                "force": {"type": "boolean"}
            }, "additionalProperties": false
        })),
        ("delete", "/delivery/worktree") => Some(json!({
            "type": "object", "required": ["workspaceRoot", "path"],
            "properties": {"workspaceRoot": {"type": "string"}, "path": {"type": "string"}, "force": {"type": "boolean"}},
            "additionalProperties": false
        })),
        ("post", "/delivery/stage") | ("delete", "/delivery/stage") => Some(json!({
            "type": "object", "required": ["workspaceRoot", "paths"],
            "properties": {"workspaceRoot": {"type": "string"}, "paths": {"type": "array", "items": {"type": "string"}}},
            "additionalProperties": false
        })),
        ("post", "/delivery/commit") => Some(json!({
            "type": "object", "required": ["workspaceRoot", "message"],
            "properties": {"workspaceRoot": {"type": "string"}, "message": {"type": "string"}, "amend": {"type": "boolean"}, "signOff": {"type": "boolean"}},
            "additionalProperties": false
        })),
        ("post", "/delivery/push") => Some(json!({
            "type": "object", "required": ["workspaceRoot"],
            "properties": {"workspaceRoot": {"type": "string"}, "remote": {"type": ["string", "null"]}, "branch": {"type": ["string", "null"]}, "setUpstream": {"type": "boolean"}, "force": {"type": "boolean"}},
            "additionalProperties": false
        })),
        ("post", "/delivery/pull") => Some(json!({
            "type": "object", "required": ["workspaceRoot"],
            "properties": {"workspaceRoot": {"type": "string"}, "remote": {"type": ["string", "null"]}, "branch": {"type": ["string", "null"]}},
            "additionalProperties": false
        })),
        ("post", "/delivery/pr") => Some(json!({
            "type": "object", "required": ["workspaceRoot"],
            "properties": {"workspaceRoot": {"type": "string"}, "forge": {"$ref": "#/components/schemas/DeliveryForge"}, "title": {"type": ["string", "null"]}, "body": {"type": ["string", "null"]}, "base": {"type": ["string", "null"]}, "head": {"type": ["string", "null"]}, "draft": {"type": "boolean"}},
            "additionalProperties": false
        })),
        ("post", "/delivery/command") => Some(json!({
            "type": "object", "required": ["workspaceRoot", "command", "args"],
            "properties": {"workspaceRoot": {"type": "string"}, "command": {"type": "string"}, "args": {"type": "array", "items": {"type": "string"}}, "timeoutMs": {"type": ["integer", "null"], "minimum": 0}},
            "additionalProperties": false
        })),
        ("post", "/delivery/cancel") => Some(json!({
            "type": "object", "required": ["workspaceRoot", "commandId"],
            "properties": {"workspaceRoot": {"type": "string"}, "commandId": {"type": "string"}},
            "additionalProperties": false
        })),
        _ => None,
    }
}

pub(crate) fn delivery_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/delivery/status") => {
            Some(json!({"$ref": "#/components/schemas/DeliverySnapshot"}))
        }
        ("get", "/delivery/forges") => Some(json!({
            "type": "object",
            "required": ["forges"],
            "properties": {
                "forges": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/DeliveryForgeCapability"}
                }
            },
            "additionalProperties": false
        })),
        _ if path.starts_with("/delivery/") => {
            Some(json!({"type": "object", "additionalProperties": true}))
        }
        _ => None,
    }
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

fn parse_host_kind(input: Option<&str>) -> DeliveryHostKind {
    match input.unwrap_or("local").to_ascii_lowercase().as_str() {
        "worktree" => DeliveryHostKind::Worktree,
        "ssh" => DeliveryHostKind::Ssh,
        "wsl" => DeliveryHostKind::Wsl,
        "configured-remote" | "configuredremote" => DeliveryHostKind::ConfiguredRemote,
        _ => DeliveryHostKind::Local,
    }
}

fn state_default_workspace_root(state: &DaemonState) -> String {
    let _ = state;
    ".".to_string()
}

fn enforce_workspace_root(root: &str) -> bool {
    let path = std::path::Path::new(root);
    !root.trim().is_empty()
        && path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        && path
            .canonicalize()
            .is_ok_and(|canonical| canonical.is_dir())
}

fn safe_git_atom(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && !value.starts_with('-')
        && !value.chars().any(char::is_whitespace)
        && !value.contains('\0')
}

fn safe_repo_path(value: &str) -> bool {
    let trimmed = value.trim();
    let path = std::path::Path::new(trimmed);
    !trimmed.is_empty()
        // Treat both slash styles as rooted before delegating to the host
        // Path implementation. Windows otherwise treats `/absolute` as a
        // drive-relative path, allowing a Unix-shaped absolute input through.
        && !trimmed.starts_with('/')
        && !trimmed.starts_with('\\')
        && !path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
}

fn sanitize_branch_segment(value: &str) -> String {
    let mut sanitized = String::with_capacity(value.len());
    let mut previous_was_dash = false;
    for character in value.chars() {
        let mapped = if character.is_ascii_alphanumeric()
            || character == '-'
            || character == '_'
            || character == '.'
        {
            character
        } else {
            '-'
        };
        if mapped == '-' && previous_was_dash {
            continue;
        }
        previous_was_dash = mapped == '-';
        sanitized.push(mapped);
    }
    sanitized
}

async fn probe_binary(bin: &str, args: &[&str]) -> (Option<String>, Option<String>) {
    // Version probes need no workspace. Confine them to a tiny private
    // directory: using the daemon's cwd made every probe lease the whole
    // working tree to the sandbox profile.
    let root = std::env::temp_dir().join("coderight-probe");
    let _ = std::fs::create_dir_all(&root);
    let result = run_local_command(bin, args, &root.to_string_lossy(), 2_000).await;
    if matches!(result.status, CommandStatus::Exited) && result.exit_code == Some(0) {
        (
            Some(bin.to_string()),
            Some(result.stdout_tail.trim().to_string()),
        )
    } else {
        (None, None)
    }
}

/// The authenticated worktree request grants its exact destination, never
/// its parent directory. Keep this ACL lease until the confined Git exits.
async fn run_worktree_git(
    workspace_root: &str,
    args: &[String],
    path: &str,
    create: bool,
) -> Result<CommandRecord, StatusCode> {
    #[cfg(windows)]
    {
        let root = workspace_root.to_string();
        let args = args.to_vec();
        let requested = std::path::PathBuf::from(path);
        let target = if requested.is_absolute() {
            requested
        } else {
            std::path::Path::new(workspace_root).join(requested)
        };
        let runtime = tokio::runtime::Handle::current();
        // AppContainerProfile owns a native SID; keep it on this worker
        // rather than carrying a non-Send handle across an async suspension.
        return tokio::task::spawn_blocking(move || {
            if target.parent().is_none()
                || target
                    .components()
                    .any(|part| part == std::path::Component::ParentDir)
                || rightkit_agent_tools::coding_tools::sandbox::path_is_protected_secret(&target)
            {
                return Err(StatusCode::BAD_REQUEST);
            }
            let created = create && !target.exists();
            if created {
                std::fs::create_dir_all(&target).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            }
            let profile =
                rightkit_agent_tools::coding_tools::sandbox::AppContainerProfile::for_workspace(
                    std::path::Path::new(&root),
                )
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            profile
                .lease_extra_access(&target, true)
                .map_err(|_| StatusCode::FORBIDDEN)?;
            let result = runtime.block_on(run_git(&root, &args));
            drop(profile);
            if created && result.exit_code != Some(0) {
                // Remove only an empty directory created for this request.
                let _ = std::fs::remove_dir(&target);
            }
            Ok(result)
        })
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    #[cfg(not(windows))]
    {
        let _ = (path, create);
        Ok(run_git(workspace_root, args).await)
    }
}

async fn run_git<S: AsRef<str>>(workspace_root: &str, args: &[S]) -> CommandRecord {
    let string_args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    run_local_command("git", &string_args, workspace_root, 60_000).await
}

async fn run_forge<S: AsRef<str>>(workspace_root: &str, forge: Forge, args: &[S]) -> CommandRecord {
    let string_args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    let Some(binary) = forge.cli_binary() else {
        return CommandRecord {
            command_id: format!("forge-unsupported-{}", now_iso()),
            command: forge.as_str().to_string(),
            args: string_args.iter().map(|arg| (*arg).to_string()).collect(),
            cwd: workspace_root.to_string(),
            started_at: now_iso(),
            finished_at: Some(now_iso()),
            exit_code: None,
            status: CommandStatus::SpawnFailed,
            stdout_tail: String::new(),
            stderr_tail: format!("forge '{}' has no daemon CLI adapter", forge),
            host_kind: DeliveryHostKind::Local,
        };
    };
    run_local_command(binary, &string_args, workspace_root, 60_000).await
}

fn parse_delivery_forge(input: Option<&str>) -> Result<Forge, StatusCode> {
    match input {
        None | Some("") => Ok(Forge::GitHub),
        Some(value) => Forge::parse(value).map_err(|_| StatusCode::BAD_REQUEST),
    }
}

fn ensure_pr_ci_forge(forge: Forge) -> Result<(), StatusCode> {
    if forge.supports_pr_ci() {
        Ok(())
    } else {
        Err(StatusCode::NOT_IMPLEMENTED)
    }
}

#[derive(Debug)]
pub(crate) enum TimedCommandError {
    Spawn(std::io::Error),
    TimedOut,
}

async fn run_local_command<S: AsRef<str>>(
    command: &str,
    args: &[S],
    workspace_root: &str,
    timeout_ms: u64,
) -> CommandRecord {
    let command_id = format!("delivery-cmd-{}", now_iso());
    let started_at = now_iso();
    let argv: Vec<String> = args.iter().map(|s| s.as_ref().to_string()).collect();
    let launch_context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: workspace_root.to_string(),
        sandbox_profile: Some("delivery".to_string()),
        sandbox_required: true,
        network_required: false,
    };
    let adapter = match crate::main_runtime::governed_child_process_launcher() {
        Ok(adapter) => adapter,
        Err(error) => {
            return CommandRecord {
                command_id,
                command: command.to_string(),
                args: argv,
                cwd: workspace_root.to_string(),
                started_at,
                finished_at: Some(now_iso()),
                exit_code: None,
                status: CommandStatus::SpawnFailed,
                stdout_tail: String::new(),
                stderr_tail: format!("launch policy rejected: {error}"),
                host_kind: DeliveryHostKind::Local,
            }
        }
    };
    let result = managed_output_with_timeout(
        adapter.as_ref(),
        rightkit_sandbox::execution::CommandLaunchSpec {
            program: command.to_string(),
            argv,
            cwd: workspace_root.to_string(),
            context: launch_context,
            env: Vec::new(),
        },
        std::time::Duration::from_millis(timeout_ms),
    )
    .await;
    let finished_at = now_iso();
    match result {
        Ok(out) if out.status.success() => CommandRecord {
            command_id,
            command: command.to_string(),
            args: args.iter().map(|s| s.as_ref().to_string()).collect(),
            cwd: workspace_root.to_string(),
            started_at,
            finished_at: Some(finished_at),
            exit_code: out.status.code(),
            status: CommandStatus::Exited,
            stdout_tail: truncate(&String::from_utf8_lossy(&out.stdout), 4_000),
            stderr_tail: truncate(&String::from_utf8_lossy(&out.stderr), 4_000),
            host_kind: DeliveryHostKind::Local,
        },
        Ok(out) => CommandRecord {
            command_id,
            command: command.to_string(),
            args: args.iter().map(|s| s.as_ref().to_string()).collect(),
            cwd: workspace_root.to_string(),
            started_at,
            finished_at: Some(finished_at),
            exit_code: out.status.code(),
            status: CommandStatus::Exited,
            stdout_tail: truncate(&String::from_utf8_lossy(&out.stdout), 4_000),
            stderr_tail: truncate(&String::from_utf8_lossy(&out.stderr), 4_000),
            host_kind: DeliveryHostKind::Local,
        },
        Err(TimedCommandError::Spawn(e)) => CommandRecord {
            command_id,
            command: command.to_string(),
            args: args.iter().map(|s| s.as_ref().to_string()).collect(),
            cwd: workspace_root.to_string(),
            started_at,
            finished_at: Some(finished_at),
            exit_code: None,
            status: CommandStatus::SpawnFailed,
            stdout_tail: String::new(),
            stderr_tail: truncate(&format!("spawn failed: {e}"), 4_000),
            host_kind: DeliveryHostKind::Local,
        },
        Err(TimedCommandError::TimedOut) => CommandRecord {
            command_id,
            command: command.to_string(),
            args: args.iter().map(|s| s.as_ref().to_string()).collect(),
            cwd: workspace_root.to_string(),
            started_at,
            finished_at: Some(finished_at),
            exit_code: None,
            status: CommandStatus::Killed,
            stdout_tail: String::new(),
            stderr_tail: truncate(&format!("command timed out after {timeout_ms}ms"), 4_000),
            host_kind: DeliveryHostKind::Local,
        },
    }
}

async fn managed_output_with_timeout(
    adapter: &dyn rightkit_sandbox::execution::ProcessLaunchAdapter,
    spec: rightkit_sandbox::execution::CommandLaunchSpec,
    timeout: std::time::Duration,
) -> Result<std::process::Output, TimedCommandError> {
    use tokio::io::AsyncReadExt;
    let mut child = adapter
        .spawn(spec)
        .await
        .map_err(|_| TimedCommandError::Spawn(std::io::Error::other("managed launch failed")))?;
    // These commands never supply input; do not leave a pipe writer live.
    drop(child.take_stdin());
    let mut stdout = child
        .take_stdout()
        .ok_or_else(|| TimedCommandError::Spawn(std::io::Error::other("stdout unavailable")))?;
    let mut stderr = child
        .take_stderr()
        .ok_or_else(|| TimedCommandError::Spawn(std::io::Error::other("stderr unavailable")))?;
    let read = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (_, _, code) = tokio::join!(
            stdout.read_to_end(&mut out),
            stderr.read_to_end(&mut err),
            child.wait()
        );
        let code =
            code.map_err(|_| TimedCommandError::Spawn(std::io::Error::other("wait failed")))?;
        Ok::<_, TimedCommandError>((out, err, code))
    };
    match tokio::time::timeout(timeout, read).await {
        Ok(Ok((stdout, stderr, code))) => {
            #[cfg(unix)]
            use std::os::unix::process::ExitStatusExt;
            #[cfg(windows)]
            use std::os::windows::process::ExitStatusExt;
            Ok(std::process::Output {
                #[cfg(unix)]
                status: std::process::ExitStatus::from_raw(code.max(0) << 8),
                #[cfg(windows)]
                status: std::process::ExitStatus::from_raw(code.max(0) as u32),
                stdout,
                stderr,
            })
        }
        Ok(Err(error)) => Err(error),
        Err(_) => {
            let _ = child.cancel().await;
            let _ = child.wait().await;
            Err(TimedCommandError::TimedOut)
        }
    }
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let head: String = value.chars().take(max.saturating_sub(16)).collect();
    format!("{head}...[truncated]")
}

fn now_iso() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    format!("unix:{secs}")
}
