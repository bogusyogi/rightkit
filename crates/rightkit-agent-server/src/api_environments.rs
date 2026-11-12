// D4 — Environments: typed environment entry (Local / Worktree / SSH / WSL /
// configured remote) with root, branch, trust, setup state, latency, model /
// provider availability, and test-connection. Setup scripts are versioned,
// logged, synchronous, and rerunnable. Runs advertise `cancellable: false`;
// the cancellation route reports the terminal state without claiming that a
// background process exists.
//
// Standalone route + contract fragment for Wave 5 environment entry. Agent 4
// owns the central `api_router.rs` and central `api_contract` endpoint
// manifest; this module exposes builders and contract fragments the integration
// lane wires in. No central registration happens here.
//
// Honest capability modeling:
//   - `Local` and `Worktree` are runtime-supported (the daemon runs `git
//     worktree` directly on Local).
//   - `Ssh`, `Wsl`, and `ConfiguredRemote` are typed but flagged
//     `available: false` until the shell lane gains an executor that knows
//     how to dispatch commands across them. The UI surfaces this distinction
//     instead of pretending the runtime supports them.
//
// Authority + workspace trust:
//   - The trusted-source gate (`enforce_trusted_source_authority`) blocks
//     state-changing routes that come from an unverified origin.
//   - Every environment record carries its workspace trust label so the UI can
//     refuse to launch setup scripts against `locked` workspaces.

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::io::AsyncReadExt;

use crate::api::{lock_recover, parse_workspace_trust_label, workspace_trust_key, DaemonState};
use crate::api_permissions::enforce_trusted_source_authority;

pub fn environments_routes() -> Router<DaemonState> {
    Router::new()
        .route("/environments", get(list_environments_handler))
        .route("/environments/select", post(select_environment_handler))
        .route("/environments/{id}/test", post(test_environment_handler))
        .route("/environments/scripts", get(list_setup_scripts_handler))
        .route("/environments/scripts/run", post(run_setup_script_handler))
        .route(
            "/environments/scripts/runs/{runId}/cancel",
            post(cancel_setup_script_handler),
        )
        .route(
            "/environments/scripts/runs/{runId}/logs",
            get(get_setup_script_logs_handler),
        )
        .route(
            "/environments/scripts/runs/{runId}/rerun",
            post(rerun_setup_script_handler),
        )
}

// ─── Typed environment surface ───────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentKind {
    Local,
    Worktree,
    Ssh,
    Wsl,
    ConfiguredRemote,
}

impl EnvironmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EnvironmentKind::Local => "local",
            EnvironmentKind::Worktree => "worktree",
            EnvironmentKind::Ssh => "ssh",
            EnvironmentKind::Wsl => "wsl",
            EnvironmentKind::ConfiguredRemote => "configuredRemote",
        }
    }

    /// Honest runtime support: only Local and Worktree are executed today.
    /// Other kinds are typed so the UI can render them as configured-but-
    /// unavailable until a dedicated executor is wired in.
    pub fn is_runtime_supported(self) -> bool {
        matches!(self, EnvironmentKind::Local | EnvironmentKind::Worktree)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentSetupState {
    Ready,
    Pending,
    Running,
    Failed,
    Cancelled,
    Unsupported,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentTrustLevel {
    Trusted,
    Sandboxed,
    Locked,
    Untrusted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentProviderStatus {
    pub provider: String,
    pub available: bool,
    pub default_model: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRecord {
    pub id: String,
    pub label: String,
    pub kind: EnvironmentKind,
    pub workspace_root: String,
    pub branch: Option<String>,
    pub trust: EnvironmentTrustLevel,
    pub setup_state: EnvironmentSetupState,
    pub setup_error: Option<String>,
    pub last_latency_ms: Option<u64>,
    pub last_tested_at: Option<String>,
    pub runtime_supported: bool,
    pub runtime_unavailable_reason: Option<String>,
    pub providers: Vec<EnvironmentProviderStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentListResponse {
    pub selected_id: Option<String>,
    pub environments: Vec<EnvironmentRecord>,
    pub generated_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentWorkspaceQuery {
    workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SelectEnvironmentRequest {
    workspace_root: String,
    environment_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TestEnvironmentRequest {
    workspace_root: String,
    environment_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentTestResult {
    pub environment_id: String,
    pub reachable: bool,
    pub latency_ms: Option<u64>,
    pub tested_at: String,
    pub error: Option<String>,
    pub command: String,
    pub exit_code: Option<i32>,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

// ─── Setup scripts ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupScriptStep {
    pub id: String,
    pub label: String,
    pub command: String,
    pub args: Vec<String>,
    pub working_dir: Option<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupScript {
    pub id: String,
    pub version: u32,
    pub label: String,
    pub environment_kind: EnvironmentKind,
    pub project_policy: String,
    pub steps: Vec<SetupScriptStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupScriptListResponse {
    pub workspace_root: String,
    pub scripts: Vec<SetupScript>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetupScriptListQuery {
    workspace_root: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunSetupScriptRequest {
    workspace_root: String,
    script_id: String,
    #[serde(default)]
    version: Option<u32>,
    #[serde(default)]
    parameters: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SetupScriptRunState {
    Pending,
    Running,
    Cancelled,
    Completed,
    Failed,
    TimedOut,
    Unsupported,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupScriptRunLogLine {
    pub ts: String,
    pub stream: String,
    pub line: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupScriptRun {
    pub run_id: String,
    pub script_id: String,
    pub script_version: u32,
    pub workspace_root: String,
    pub environment_kind: EnvironmentKind,
    pub state: SetupScriptRunState,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub exit_code: Option<i32>,
    pub log_path: String,
    pub log_tail: Vec<SetupScriptRunLogLine>,
    pub cancellable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupScriptLogsResponse {
    pub run: SetupScriptRun,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunSetupScriptPathParams {
    run_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelRerunSetupScriptRequest {
    workspace_root: String,
    script_id: String,
}

// ─── Storage ──────────────────────────────────────────────────────────────────

fn environments_dir(workspace_root: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(workspace_root)
        .join(crate::host_identity::home_dir())
        .join("environments")
}

fn environment_selection_path(workspace_root: &str) -> std::path::PathBuf {
    environments_dir(workspace_root).join("selected.json")
}

fn setup_scripts_path(workspace_root: &str) -> std::path::PathBuf {
    environments_dir(workspace_root).join("setup-scripts.json")
}

fn setup_run_dir(workspace_root: &str) -> std::path::PathBuf {
    environments_dir(workspace_root).join("runs")
}

fn setup_run_log_path(workspace_root: &str, run_id: &str) -> std::path::PathBuf {
    setup_run_dir(workspace_root).join(format!("{run_id}.log"))
}

fn setup_run_state_path(workspace_root: &str, run_id: &str) -> std::path::PathBuf {
    setup_run_dir(workspace_root).join(format!("{run_id}.json"))
}

fn load_selection(workspace_root: &str) -> Option<String> {
    let path = environment_selection_path(workspace_root);
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()?
        .get("environmentId")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn save_selection(workspace_root: &str, environment_id: &str) -> Result<(), String> {
    let dir = environments_dir(workspace_root);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("failed to create environments dir '{}': {e}", dir.display()))?;
    let path = environment_selection_path(workspace_root);
    let body = serde_json::json!({
        "environmentId": environment_id,
        "savedAt": now_iso(),
    });
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&body).unwrap_or_default(),
    )
    .map_err(|e| format!("failed to write selection '{}': {e}", path.display()))
}

fn load_setup_scripts(workspace_root: &str) -> Result<Vec<SetupScript>, String> {
    let path = setup_scripts_path(workspace_root);
    if !path.exists() {
        return Ok(default_setup_scripts());
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read scripts '{}': {e}", path.display()))?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| format!("failed to parse scripts '{}': {e}", path.display()))?;
    let arr = parsed
        .get("scripts")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut scripts = Vec::new();
    for entry in arr {
        if let Ok(script) = serde_json::from_value::<SetupScript>(entry) {
            scripts.push(script);
        }
    }
    Ok(scripts)
}

fn default_setup_scripts() -> Vec<SetupScript> {
    vec![
        SetupScript {
            id: "local.bootstrap".to_string(),
            version: 1,
            label: "Local bootstrap".to_string(),
            environment_kind: EnvironmentKind::Local,
            project_policy: "default".to_string(),
            steps: vec![SetupScriptStep {
                id: "git-status".to_string(),
                label: "Confirm a clean git working tree".to_string(),
                command: "git".to_string(),
                args: vec!["status".to_string(), "--porcelain".to_string()],
                working_dir: None,
                timeout_ms: Some(15_000),
            }],
        },
        SetupScript {
            id: "worktree.bootstrap".to_string(),
            version: 1,
            label: "Worktree bootstrap".to_string(),
            environment_kind: EnvironmentKind::Worktree,
            project_policy: "default".to_string(),
            steps: vec![SetupScriptStep {
                id: "worktree-list".to_string(),
                label: "Confirm worktrees registered".to_string(),
                command: "git".to_string(),
                args: vec!["worktree".to_string(), "list".to_string()],
                working_dir: None,
                timeout_ms: Some(15_000),
            }],
        },
        SetupScript {
            id: "ssh.bootstrap".to_string(),
            version: 1,
            label: "SSH bootstrap".to_string(),
            environment_kind: EnvironmentKind::Ssh,
            project_policy: "default".to_string(),
            steps: vec![SetupScriptStep {
                id: "ssh-probe".to_string(),
                label: "Probe SSH host (not runtime-supported today)".to_string(),
                command: "true".to_string(),
                args: vec![],
                working_dir: None,
                timeout_ms: Some(5_000),
            }],
        },
        SetupScript {
            id: "wsl.bootstrap".to_string(),
            version: 1,
            label: "WSL bootstrap".to_string(),
            environment_kind: EnvironmentKind::Wsl,
            project_policy: "default".to_string(),
            steps: vec![SetupScriptStep {
                id: "wsl-probe".to_string(),
                label: "Probe WSL distro (not runtime-supported today)".to_string(),
                command: "true".to_string(),
                args: vec![],
                working_dir: None,
                timeout_ms: Some(5_000),
            }],
        },
        SetupScript {
            id: "configuredRemote.bootstrap".to_string(),
            version: 1,
            label: "Configured remote bootstrap".to_string(),
            environment_kind: EnvironmentKind::ConfiguredRemote,
            project_policy: "default".to_string(),
            steps: vec![SetupScriptStep {
                id: "remote-probe".to_string(),
                label: "Probe remote (not runtime-supported today)".to_string(),
                command: "true".to_string(),
                args: vec![],
                working_dir: None,
                timeout_ms: Some(5_000),
            }],
        },
    ]
}

// ─── Handlers ─────────────────────────────────────────────────────────────────

async fn list_environments_handler(
    State(state): State<DaemonState>,
    Query(query): Query<EnvironmentWorkspaceQuery>,
) -> Result<Json<EnvironmentListResponse>, StatusCode> {
    let workspace_root = query
        .workspace_root
        .clone()
        .unwrap_or_else(|| state_default_workspace_root(&state));
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let trust = state_workspace_trust(&state, &workspace_root);
    let environments = default_environments(&workspace_root, trust);
    let selected_id = load_selection(&workspace_root);
    Ok(Json(EnvironmentListResponse {
        selected_id,
        environments,
        generated_at: now_iso(),
    }))
}

async fn select_environment_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<SelectEnvironmentRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if matches!(
        state_workspace_trust(&state, &req.workspace_root),
        EnvironmentTrustLevel::Locked | EnvironmentTrustLevel::Untrusted
    ) {
        return Err(StatusCode::FORBIDDEN);
    }
    if !default_environments(
        &req.workspace_root,
        state_workspace_trust(&state, &req.workspace_root),
    )
    .iter()
    .any(|environment| environment.id == req.environment_id)
    {
        return Err(StatusCode::NOT_FOUND);
    }
    save_selection(&req.workspace_root, &req.environment_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "environmentId": req.environment_id,
        "saved": true,
        "savedAt": now_iso(),
    })))
}

async fn test_environment_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<TestEnvironmentRequest>,
) -> Result<Json<EnvironmentTestResult>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let kind = env_kind_from_id(&req.environment_id);
    let trust = state_workspace_trust(&state, &req.workspace_root);
    if matches!(trust, EnvironmentTrustLevel::Locked) {
        return Ok(Json(EnvironmentTestResult {
            environment_id: req.environment_id,
            reachable: false,
            latency_ms: None,
            tested_at: now_iso(),
            error: Some("workspace trust is locked; refusing to test".to_string()),
            command: format!("(blocked) {}", kind.as_str()),
            exit_code: None,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
        }));
    }
    if !kind.is_runtime_supported() {
        return Ok(Json(EnvironmentTestResult {
            environment_id: req.environment_id,
            reachable: false,
            latency_ms: None,
            tested_at: now_iso(),
            error: Some(format!(
                "environment kind '{}' is typed but not runtime-supported today",
                kind.as_str()
            )),
            command: format!("(unavailable) {}", kind.as_str()),
            exit_code: None,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
        }));
    }
    let started = std::time::Instant::now();
    let command = match kind {
        EnvironmentKind::Local => "git status --porcelain".to_string(),
        EnvironmentKind::Worktree => "git worktree list --porcelain".to_string(),
        _ => "(unavailable)".to_string(),
    };
    let output = match kind {
        EnvironmentKind::Local => {
            Some(run_managed_process("git", &["status", "--porcelain"], &req.workspace_root).await)
        }
        EnvironmentKind::Worktree => Some(
            run_managed_process(
                "git",
                &["worktree", "list", "--porcelain"],
                &req.workspace_root,
            )
            .await,
        ),
        _ => None,
    };
    let latency_ms = started.elapsed().as_millis() as u64;
    match output {
        Some(Ok(out)) if out.success => Ok(Json(EnvironmentTestResult {
            environment_id: req.environment_id,
            reachable: true,
            latency_ms: Some(latency_ms),
            tested_at: now_iso(),
            error: None,
            command,
            exit_code: out.code,
            stdout_tail: truncate(&out.stdout, 2_000),
            stderr_tail: truncate(&out.stderr, 2_000),
        })),
        Some(Ok(out)) => Ok(Json(EnvironmentTestResult {
            environment_id: req.environment_id,
            reachable: false,
            latency_ms: Some(latency_ms),
            tested_at: now_iso(),
            error: Some(format!(
                "command exited with status {}",
                out.code.unwrap_or(-1)
            )),
            command,
            exit_code: out.code,
            stdout_tail: truncate(&out.stdout, 2_000),
            stderr_tail: truncate(&out.stderr, 2_000),
        })),
        Some(Err(e)) => Ok(Json(EnvironmentTestResult {
            environment_id: req.environment_id,
            reachable: false,
            latency_ms: Some(latency_ms),
            tested_at: now_iso(),
            error: Some(format!("spawn failed: {e}")),
            command,
            exit_code: None,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
        })),
        None => Ok(Json(EnvironmentTestResult {
            environment_id: req.environment_id,
            reachable: false,
            latency_ms: None,
            tested_at: now_iso(),
            error: Some("no executor registered for this environment kind".to_string()),
            command,
            exit_code: None,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
        })),
    }
}

async fn list_setup_scripts_handler(
    State(state): State<DaemonState>,
    Query(query): Query<SetupScriptListQuery>,
) -> Result<Json<SetupScriptListResponse>, StatusCode> {
    let _ = state;
    if !enforce_workspace_root(&query.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let scripts =
        load_setup_scripts(&query.workspace_root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(SetupScriptListResponse {
        workspace_root: query.workspace_root,
        scripts,
    }))
}

async fn run_setup_script_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(req): Json<RunSetupScriptRequest>,
) -> Result<Json<SetupScriptRun>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let trust = state_workspace_trust(&state, &req.workspace_root);
    if matches!(
        trust,
        EnvironmentTrustLevel::Locked
            | EnvironmentTrustLevel::Untrusted
            | EnvironmentTrustLevel::Sandboxed
    ) {
        return Err(StatusCode::FORBIDDEN);
    }
    let scripts =
        load_setup_scripts(&req.workspace_root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let script = scripts
        .iter()
        .find(|s| s.id == req.script_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;
    if req.version.is_some_and(|version| version != script.version) {
        return Err(StatusCode::CONFLICT);
    }
    if !script.environment_kind.is_runtime_supported() {
        return Ok(Json(unsupported_run(&req, &script)));
    }
    let run_id = format!("setup-run-{}", rightkit_agent_engine_protocol::uuid_v7());
    let started_at = now_iso();
    let dir = setup_run_dir(&req.workspace_root);
    std::fs::create_dir_all(&dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let log_path = setup_run_log_path(&req.workspace_root, &run_id);
    std::fs::write(&log_path, "").map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let state_path = setup_run_state_path(&req.workspace_root, &run_id);
    let mut log_tail: Vec<SetupScriptRunLogLine> = Vec::new();
    let mut last_exit: Option<i32> = None;
    let mut final_state = SetupScriptRunState::Completed;
    for step in &script.steps {
        log_tail.push(SetupScriptRunLogLine {
            ts: now_iso(),
            stream: "system".to_string(),
            line: format!(
                "step {} → {} {}",
                step.id,
                step.command,
                step.args.join(" ")
            ),
        });
        let cwd = step
            .working_dir
            .clone()
            .unwrap_or_else(|| req.workspace_root.clone());
        if !path_is_within_workspace(&req.workspace_root, &cwd) {
            return Err(StatusCode::FORBIDDEN);
        }
        let step_run = run_managed_process(
            &step.command,
            &step.args.iter().map(String::as_str).collect::<Vec<_>>(),
            &cwd,
        )
        .await;
        match step_run {
            Ok(out) => {
                last_exit = out.code;
                let stdout = out.stdout;
                let stderr = out.stderr;
                if !stdout.is_empty() {
                    log_tail.push(SetupScriptRunLogLine {
                        ts: now_iso(),
                        stream: "stdout".to_string(),
                        line: truncate(&stdout, 4_000),
                    });
                }
                if !stderr.is_empty() {
                    log_tail.push(SetupScriptRunLogLine {
                        ts: now_iso(),
                        stream: "stderr".to_string(),
                        line: truncate(&stderr, 4_000),
                    });
                }
                if !out.success {
                    final_state = SetupScriptRunState::Failed;
                    log_tail.push(SetupScriptRunLogLine {
                        ts: now_iso(),
                        stream: "system".to_string(),
                        line: format!(
                            "step {} exited with status {}",
                            step.id,
                            out.code.unwrap_or(-1)
                        ),
                    });
                    break;
                }
            }
            Err(e) => {
                final_state = SetupScriptRunState::Failed;
                log_tail.push(SetupScriptRunLogLine {
                    ts: now_iso(),
                    stream: "stderr".to_string(),
                    line: format!("step {} spawn failed: {e}", step.id),
                });
                break;
            }
        }
    }
    let finished_at = now_iso();
    persist_log_tail(&log_path, &log_tail).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let run = SetupScriptRun {
        run_id: run_id.clone(),
        script_id: script.id.clone(),
        script_version: script.version,
        workspace_root: req.workspace_root.clone(),
        environment_kind: script.environment_kind,
        state: final_state,
        started_at,
        finished_at: Some(finished_at),
        exit_code: last_exit,
        log_path: log_path.to_string_lossy().into_owned(),
        log_tail,
        cancellable: false,
    };
    let state_json = serde_json::to_string_pretty(&run).unwrap_or_default();
    let _ = crate::store_write::write_store_file(&state_path, state_json.as_bytes());
    Ok(Json(run))
}

async fn cancel_setup_script_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(params): Path<RunSetupScriptPathParams>,
    Json(req): Json<CancelRerunSetupScriptRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let state_path = setup_run_state_path(&req.workspace_root, &params.run_id);
    let raw = std::fs::read_to_string(&state_path).map_err(|_| StatusCode::NOT_FOUND)?;
    let mut run: SetupScriptRun =
        serde_json::from_str(&raw).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if run.script_id != req.script_id || run.workspace_root != req.workspace_root {
        return Err(StatusCode::CONFLICT);
    }
    if matches!(
        run.state,
        SetupScriptRunState::Completed
            | SetupScriptRunState::Failed
            | SetupScriptRunState::Cancelled
            | SetupScriptRunState::TimedOut
            | SetupScriptRunState::Unsupported
    ) {
        return Ok(Json(serde_json::json!({
            "runId": run.run_id,
            "cancelled": false,
            "state": run.state,
            "note": "run already terminated; nothing to cancel"
        })));
    }
    run.state = SetupScriptRunState::Cancelled;
    run.finished_at = Some(now_iso());
    let body = serde_json::to_string_pretty(&run).unwrap_or_default();
    crate::store_write::write_store_file(&state_path, body.as_bytes())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "runId": run.run_id,
        "cancelled": true,
        "state": run.state
    })))
}

async fn get_setup_script_logs_handler(
    State(state): State<DaemonState>,
    Path(params): Path<RunSetupScriptPathParams>,
    Query(query): Query<EnvironmentWorkspaceQuery>,
) -> Result<Json<SetupScriptLogsResponse>, StatusCode> {
    let _ = state;
    let workspace_root = query
        .workspace_root
        .clone()
        .ok_or(StatusCode::BAD_REQUEST)?;
    if !enforce_workspace_root(&workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let state_path = setup_run_state_path(&workspace_root, &params.run_id);
    let raw = std::fs::read_to_string(&state_path).map_err(|_| StatusCode::NOT_FOUND)?;
    let run: SetupScriptRun =
        serde_json::from_str(&raw).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(SetupScriptLogsResponse { run }))
}

async fn rerun_setup_script_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(params): Path<RunSetupScriptPathParams>,
    Json(req): Json<RunSetupScriptRequest>,
) -> Result<Json<SetupScriptRun>, StatusCode> {
    enforce_trusted_source_authority(&headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let _ = state;
    if !enforce_workspace_root(&req.workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let original_path = setup_run_state_path(&req.workspace_root, &params.run_id);
    let original_raw =
        std::fs::read_to_string(&original_path).map_err(|_| StatusCode::NOT_FOUND)?;
    let original: SetupScriptRun =
        serde_json::from_str(&original_raw).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if original.script_id != req.script_id || original.workspace_root != req.workspace_root {
        return Err(StatusCode::CONFLICT);
    }
    let scripts =
        load_setup_scripts(&req.workspace_root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let script = scripts
        .iter()
        .find(|s| s.id == req.script_id)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)?;
    if !script.environment_kind.is_runtime_supported() {
        return Ok(Json(unsupported_run(&req, &script)));
    }
    // Re-running uses the same handler with a fresh run-id so the prior
    // run's state file remains on disk for history. The handler is
    // recursively safe; no separate dispatch.
    let inner = RunSetupScriptRequest {
        workspace_root: req.workspace_root,
        script_id: script.id.clone(),
        version: Some(script.version),
        parameters: req.parameters,
    };
    let original_run_id = params.run_id;
    let response = run_setup_script_handler(State(state), headers, Json(inner)).await;
    if let Ok(Json(mut run)) = response {
        run.log_path = format!("{} (rerun of {})", run.log_path, original_run_id);
        return Ok(Json(run));
    }
    Err(StatusCode::INTERNAL_SERVER_ERROR)
}

// ─── OpenAPI fragment ─────────────────────────────────────────────────────────

pub(crate) fn environments_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "EnvironmentKind".to_string(),
        json!({"type": "string", "enum": ["local", "worktree", "ssh", "wsl", "configuredRemote"]}),
    );
    schemas.insert(
        "EnvironmentSetupState".to_string(),
        json!({"type": "string", "enum": ["ready", "pending", "running", "failed", "cancelled", "unsupported"]}),
    );
    schemas.insert(
        "EnvironmentTrustLevel".to_string(),
        json!({"type": "string", "enum": ["trusted", "sandboxed", "locked", "untrusted"]}),
    );
    schemas.insert(
        "EnvironmentProviderStatus".to_string(),
        json!({
            "type": "object",
            "required": ["provider", "available"],
            "properties": {
                "provider": {"type": "string"},
                "available": {"type": "boolean"},
                "defaultModel": {"type": ["string", "null"]},
                "reason": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "EnvironmentRecord".to_string(),
        json!({
            "type": "object",
            "required": ["id", "label", "kind", "workspaceRoot", "trust", "setupState", "runtimeSupported", "providers"],
            "properties": {
                "id": {"type": "string"},
                "label": {"type": "string"},
                "kind": {"$ref": "#/components/schemas/EnvironmentKind"},
                "workspaceRoot": {"type": "string"},
                "branch": {"type": ["string", "null"]},
                "trust": {"$ref": "#/components/schemas/EnvironmentTrustLevel"},
                "setupState": {"$ref": "#/components/schemas/EnvironmentSetupState"},
                "setupError": {"type": ["string", "null"]},
                "lastLatencyMs": {"type": ["integer", "null"], "minimum": 0},
                "lastTestedAt": {"type": ["string", "null"]},
                "runtimeSupported": {"type": "boolean"},
                "runtimeUnavailableReason": {"type": ["string", "null"]},
                "providers": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/EnvironmentProviderStatus"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "EnvironmentListResponse".to_string(),
        json!({
            "type": "object",
            "required": ["environments", "generatedAt"],
            "properties": {
                "selectedId": {"type": ["string", "null"]},
                "environments": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/EnvironmentRecord"}
                },
                "generatedAt": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "EnvironmentTestResult".to_string(),
        json!({
            "type": "object",
            "required": ["environmentId", "reachable", "testedAt", "command"],
            "properties": {
                "environmentId": {"type": "string"},
                "reachable": {"type": "boolean"},
                "latencyMs": {"type": ["integer", "null"], "minimum": 0},
                "testedAt": {"type": "string"},
                "error": {"type": ["string", "null"]},
                "command": {"type": "string"},
                "exitCode": {"type": ["integer", "null"]},
                "stdoutTail": {"type": "string"},
                "stderrTail": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SetupScriptStep".to_string(),
        json!({
            "type": "object",
            "required": ["id", "label", "command", "args"],
            "properties": {
                "id": {"type": "string"},
                "label": {"type": "string"},
                "command": {"type": "string"},
                "args": {"type": "array", "items": {"type": "string"}},
                "workingDir": {"type": ["string", "null"]},
                "timeoutMs": {"type": ["integer", "null"], "minimum": 0}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SetupScript".to_string(),
        json!({
            "type": "object",
            "required": ["id", "version", "label", "environmentKind", "projectPolicy", "steps"],
            "properties": {
                "id": {"type": "string"},
                "version": {"type": "integer", "minimum": 1},
                "label": {"type": "string"},
                "environmentKind": {"$ref": "#/components/schemas/EnvironmentKind"},
                "projectPolicy": {"type": "string"},
                "steps": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SetupScriptStep"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SetupScriptListResponse".to_string(),
        json!({
            "type": "object",
            "required": ["workspaceRoot", "scripts"],
            "properties": {
                "workspaceRoot": {"type": "string"},
                "scripts": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SetupScript"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SetupScriptRunState".to_string(),
        json!({"type": "string", "enum": ["pending", "running", "cancelled", "completed", "failed", "timedOut", "unsupported"]}),
    );
    schemas.insert(
        "SetupScriptRunLogLine".to_string(),
        json!({
            "type": "object",
            "required": ["ts", "stream", "line"],
            "properties": {
                "ts": {"type": "string"},
                "stream": {"type": "string"},
                "line": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SetupScriptRun".to_string(),
        json!({
            "type": "object",
            "required": ["runId", "scriptId", "scriptVersion", "workspaceRoot", "environmentKind", "state", "startedAt", "logPath", "logTail", "cancellable"],
            "properties": {
                "runId": {"type": "string"},
                "scriptId": {"type": "string"},
                "scriptVersion": {"type": "integer", "minimum": 1},
                "workspaceRoot": {"type": "string"},
                "environmentKind": {"$ref": "#/components/schemas/EnvironmentKind"},
                "state": {"$ref": "#/components/schemas/SetupScriptRunState"},
                "startedAt": {"type": "string"},
                "finishedAt": {"type": ["string", "null"]},
                "exitCode": {"type": ["integer", "null"]},
                "logPath": {"type": "string"},
                "logTail": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/SetupScriptRunLogLine"}
                },
                "cancellable": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "SetupScriptLogsResponse".to_string(),
        json!({
            "type": "object",
            "required": ["run"],
            "properties": {
                "run": {"$ref": "#/components/schemas/SetupScriptRun"}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

pub(crate) fn environments_openapi_request_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("post", "/environments/select") | ("post", "/environments/{id}/test") => Some(json!({
            "type": "object", "required": ["workspaceRoot", "environmentId"],
            "properties": {"workspaceRoot": {"type": "string"}, "environmentId": {"type": "string"}},
            "additionalProperties": false
        })),
        ("post", "/environments/scripts/run")
        | ("post", "/environments/scripts/runs/{runId}/rerun") => Some(json!({
            "type": "object", "required": ["workspaceRoot", "scriptId"],
            "properties": {"workspaceRoot": {"type": "string"}, "scriptId": {"type": "string"}, "version": {"type": ["integer", "null"], "minimum": 1}, "parameters": {"type": "object", "additionalProperties": {"type": "string"}}},
            "additionalProperties": false
        })),
        ("post", "/environments/scripts/runs/{runId}/cancel") => Some(json!({
            "type": "object", "required": ["workspaceRoot", "scriptId"],
            "properties": {"workspaceRoot": {"type": "string"}, "scriptId": {"type": "string"}},
            "additionalProperties": false
        })),
        _ => None,
    }
}

pub(crate) fn environments_openapi_response_schema(method: &str, path: &str) -> Option<Value> {
    match (method, path) {
        ("get", "/environments") => {
            Some(json!({"$ref": "#/components/schemas/EnvironmentListResponse"}))
        }
        ("post", "/environments/{id}/test") => {
            Some(json!({"$ref": "#/components/schemas/EnvironmentTestResult"}))
        }
        ("get", "/environments/scripts") => {
            Some(json!({"$ref": "#/components/schemas/SetupScriptListResponse"}))
        }
        ("post", "/environments/scripts/run")
        | ("post", "/environments/scripts/runs/{runId}/rerun") => {
            Some(json!({"$ref": "#/components/schemas/SetupScriptRun"}))
        }
        ("get", "/environments/scripts/runs/{runId}/logs") => {
            Some(json!({"$ref": "#/components/schemas/SetupScriptLogsResponse"}))
        }
        _ if path.starts_with("/environments/") => {
            Some(json!({"type": "object", "additionalProperties": true}))
        }
        _ => None,
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

pub(crate) struct ManagedProcessOutput {
    pub(crate) success: bool,
    pub(crate) code: Option<i32>,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

pub(crate) async fn run_managed_process(
    program: &str,
    args: &[&str],
    workspace_root: &str,
) -> Result<ManagedProcessOutput, String> {
    run_managed_process_with_timeout(
        program,
        args,
        workspace_root,
        std::time::Duration::from_secs(60),
    )
    .await
}

pub(crate) async fn run_managed_process_with_timeout(
    program: &str,
    args: &[&str],
    workspace_root: &str,
    timeout: std::time::Duration,
) -> Result<ManagedProcessOutput, String> {
    use rightkit_sandbox::execution::{CommandLaunchSpec, ProcessLaunchContext};
    let adapter = crate::main_runtime::governed_child_process_launcher()
        .map_err(|error| format!("managed process launch policy unavailable: {error}"))?;
    let mut child = adapter
        .spawn(CommandLaunchSpec {
            program: program.to_string(),
            argv: args.iter().map(|arg| (*arg).to_string()).collect(),
            cwd: workspace_root.to_string(),
            context: ProcessLaunchContext {
                workspace_root: workspace_root.to_string(),
                sandbox_profile: Some("workspace".to_string()),
                sandbox_required: true,
                network_required: false,
            },
            env: Vec::new(),
        })
        .await
        .map_err(|error| format!("managed process launch failed: {error:?}"))?;
    // No caller of this output helper supplies stdin. Close it before reading
    // so a confined command cannot wait for input from an idle pipe writer.
    drop(child.take_stdin());
    let mut stdout = child
        .take_stdout()
        .ok_or_else(|| "managed process did not provide stdout".to_string())?;
    let mut stderr = child
        .take_stderr()
        .ok_or_else(|| "managed process did not provide stderr".to_string())?;
    let output = async {
        tokio::join!(
            async {
                let mut bytes = Vec::new();
                let result = stdout.read_to_end(&mut bytes).await;
                (result, bytes)
            },
            async {
                let mut bytes = Vec::new();
                let result = stderr.read_to_end(&mut bytes).await;
                (result, bytes)
            },
            child.wait()
        )
    };
    let (stdout, stderr, code) = match tokio::time::timeout(timeout, output).await {
        Ok(output) => output,
        Err(_) => {
            let _ = child.cancel().await;
            let _ = child.wait().await;
            tracing::warn!(program, "managed process output timed out; child reaped");
            return Err(format!("managed process output timed out: {program}"));
        }
    };
    let code = code.map_err(|error| format!("managed process wait failed: {error:?}"))?;
    let success = code == 0 && stdout.0.is_ok() && stderr.0.is_ok();
    let stdout = String::from_utf8_lossy(&stdout.1).into_owned();
    let stderr = String::from_utf8_lossy(&stderr.1).into_owned();
    Ok(ManagedProcessOutput {
        success,
        code: Some(code),
        stdout,
        stderr,
    })
}

fn state_default_workspace_root(state: &DaemonState) -> String {
    let _ = state;
    ".".to_string()
}

fn state_workspace_trust(state: &DaemonState, workspace_root: &str) -> EnvironmentTrustLevel {
    if let Some(session) = state
        .list_sessions()
        .into_iter()
        .find(|session| session.workspace_root == workspace_root)
    {
        return match session.workspace_trust {
            rightkit_agent_policy::WorkspaceTrust::Trusted => EnvironmentTrustLevel::Trusted,
            rightkit_agent_policy::WorkspaceTrust::Sandboxed => EnvironmentTrustLevel::Sandboxed,
            rightkit_agent_policy::WorkspaceTrust::Locked => EnvironmentTrustLevel::Locked,
            rightkit_agent_policy::WorkspaceTrust::Untrusted => EnvironmentTrustLevel::Untrusted,
        };
    }
    let key = workspace_trust_key(workspace_root);
    let trust = lock_recover(&state.workspace_trust_store, "state.workspace_trust_store")
        .projects
        .get(&key)
        .and_then(|record| parse_workspace_trust_label(&record.trust))
        .unwrap_or(rightkit_agent_policy::WorkspaceTrust::Untrusted);
    match trust {
        rightkit_agent_policy::WorkspaceTrust::Trusted => EnvironmentTrustLevel::Trusted,
        rightkit_agent_policy::WorkspaceTrust::Sandboxed => EnvironmentTrustLevel::Sandboxed,
        rightkit_agent_policy::WorkspaceTrust::Locked => EnvironmentTrustLevel::Locked,
        rightkit_agent_policy::WorkspaceTrust::Untrusted => EnvironmentTrustLevel::Untrusted,
    }
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

fn path_is_within_workspace(workspace_root: &str, candidate: &str) -> bool {
    let Ok(root) = std::path::Path::new(workspace_root).canonicalize() else {
        return false;
    };
    let candidate_path = std::path::Path::new(candidate);
    let candidate = if candidate_path.is_absolute() {
        candidate_path.to_path_buf()
    } else {
        root.join(candidate_path)
    };
    candidate
        .canonicalize()
        .is_ok_and(|canonical| canonical.starts_with(root))
}

fn env_kind_from_id(id: &str) -> EnvironmentKind {
    let lower = id.to_ascii_lowercase();
    if lower.contains("ssh") {
        EnvironmentKind::Ssh
    } else if lower.contains("wsl") {
        EnvironmentKind::Wsl
    } else if lower.contains("remote") || lower.contains("cloud") {
        EnvironmentKind::ConfiguredRemote
    } else if lower.contains("worktree") {
        EnvironmentKind::Worktree
    } else {
        EnvironmentKind::Local
    }
}

fn default_environments(
    workspace_root: &str,
    trust: EnvironmentTrustLevel,
) -> Vec<EnvironmentRecord> {
    vec![
        EnvironmentRecord {
            id: "local".to_string(),
            label: "Local machine".to_string(),
            kind: EnvironmentKind::Local,
            workspace_root: workspace_root.to_string(),
            branch: None,
            trust,
            setup_state: EnvironmentSetupState::Ready,
            setup_error: None,
            last_latency_ms: None,
            last_tested_at: None,
            runtime_supported: true,
            runtime_unavailable_reason: None,
            providers: vec![
                EnvironmentProviderStatus {
                    provider: "claude".to_string(),
                    available: true,
                    default_model: Some("claude-sonnet-5".to_string()),
                    reason: None,
                },
                EnvironmentProviderStatus {
                    provider: "openai".to_string(),
                    available: true,
                    default_model: Some("gpt-5".to_string()),
                    reason: None,
                },
            ],
        },
        EnvironmentRecord {
            id: "worktree".to_string(),
            label: "Git worktree".to_string(),
            kind: EnvironmentKind::Worktree,
            workspace_root: workspace_root.to_string(),
            branch: None,
            trust,
            setup_state: EnvironmentSetupState::Ready,
            setup_error: None,
            last_latency_ms: None,
            last_tested_at: None,
            runtime_supported: true,
            runtime_unavailable_reason: None,
            providers: vec![EnvironmentProviderStatus {
                provider: "claude".to_string(),
                available: true,
                default_model: Some("claude-sonnet-5".to_string()),
                reason: None,
            }],
        },
        EnvironmentRecord {
            id: "ssh".to_string(),
            label: "SSH host".to_string(),
            kind: EnvironmentKind::Ssh,
            workspace_root: workspace_root.to_string(),
            branch: None,
            trust,
            setup_state: EnvironmentSetupState::Unsupported,
            setup_error: Some(
                "SSH executor not wired into the shell lane; type is recorded but the runtime cannot dispatch"
                    .to_string(),
            ),
            last_latency_ms: None,
            last_tested_at: None,
            runtime_supported: false,
            runtime_unavailable_reason: Some(
                "SSH environments are typed for the UI but not yet runtime-supported; selecting this entry leaves the actual run on Local"
                    .to_string(),
            ),
            providers: Vec::new(),
        },
        EnvironmentRecord {
            id: "wsl".to_string(),
            label: "WSL distro".to_string(),
            kind: EnvironmentKind::Wsl,
            workspace_root: workspace_root.to_string(),
            branch: None,
            trust,
            setup_state: EnvironmentSetupState::Unsupported,
            setup_error: Some(
                "WSL executor not wired into the shell lane; type is recorded but the runtime cannot dispatch"
                    .to_string(),
            ),
            last_latency_ms: None,
            last_tested_at: None,
            runtime_supported: false,
            runtime_unavailable_reason: Some(
                "WSL environments are typed for the UI but not yet runtime-supported".to_string(),
            ),
            providers: Vec::new(),
        },
        EnvironmentRecord {
            id: "configuredRemote".to_string(),
            label: "Configured remote".to_string(),
            kind: EnvironmentKind::ConfiguredRemote,
            workspace_root: workspace_root.to_string(),
            branch: None,
            trust,
            setup_state: EnvironmentSetupState::Unsupported,
            setup_error: Some(
                "Configured-remote executor not wired into the shell lane; type is recorded but the runtime cannot dispatch"
                    .to_string(),
            ),
            last_latency_ms: None,
            last_tested_at: None,
            runtime_supported: false,
            runtime_unavailable_reason: Some(
                "Configured-remote environments are typed for the UI but not yet runtime-supported"
                    .to_string(),
            ),
            providers: Vec::new(),
        },
    ]
}

fn unsupported_run(req: &RunSetupScriptRequest, script: &SetupScript) -> SetupScriptRun {
    SetupScriptRun {
        run_id: format!("setup-run-{}", now_iso()),
        script_id: script.id.clone(),
        script_version: script.version,
        workspace_root: req.workspace_root.clone(),
        environment_kind: script.environment_kind,
        state: SetupScriptRunState::Unsupported,
        started_at: now_iso(),
        finished_at: Some(now_iso()),
        exit_code: None,
        log_path: "(not-executed)".to_string(),
        log_tail: vec![SetupScriptRunLogLine {
            ts: now_iso(),
            stream: "system".to_string(),
            line: format!(
                "environment kind '{}' is typed but not runtime-supported; refusing to run '{}'",
                script.environment_kind.as_str(),
                script.id
            ),
        }],
        cancellable: false,
    }
}

fn persist_log_tail(
    path: &std::path::Path,
    lines: &[SetupScriptRunLogLine],
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut body = String::new();
    for line in lines {
        body.push_str(&format!("{}\t{}\t{}\n", line.ts, line.stream, line.line));
    }
    std::fs::write(path, body)
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
