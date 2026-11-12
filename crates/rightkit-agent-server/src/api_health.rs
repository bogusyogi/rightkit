use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::{Path as FsPath, PathBuf};

use crate::api::{lock_recover, now_iso, DaemonState};
use crate::api_permissions::enforce_trusted_source_authority;

pub(crate) fn health_routes() -> Router<DaemonState> {
    Router::new()
        .route("/health", get(health))
        .route("/health/diagnostics", get(health_diagnostics))
        .route("/health/readiness", get(health_readiness))
        .route("/machine/status", get(machine_status))
        .route("/machine/info", get(machine_info))
        .route("/health/machine/status", get(machine_status))
        .route("/health/machine/info", get(machine_info))
        .route(
            "/sandbox/settings",
            get(get_sandbox_settings).put(save_sandbox_settings),
        )
        .route("/diagnostics/bundle", post(create_diagnostics_bundle))
        .route(
            "/diagnostics/bundles/{id}",
            get(download_diagnostics_bundle),
        )
        .route("/diagnostics/denials", get(list_denials_handler))
}

/// The unauthenticated `GET /health` body is exactly the five fields of the
/// client API contract (R37). The machine posture (`port`, `databasePath`,
/// storage pool, provider-key posture) stays behind the bearer on
/// `/machine/status` and `/machine/info`.
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        ok: true,
        service: crate::host_identity::prefixed("daemon"),
        version: env!("CARGO_PKG_VERSION").to_string(),
        contract_version: rightkit_agent_engine_api_platform::health::CONTRACT_VERSION,
        protocol_version: rightkit_agent_engine_api_platform::health::PROTOCOL_VERSION,
    })
}

fn provider_key_storage_status(
    health: rightkit_llm::agent_providers::ProviderKeyStorageHealth,
) -> ProviderKeyStorageStatus {
    ProviderKeyStorageStatus {
        keychain_failure_observed: health.keychain_failure_observed,
    }
}

/// Runtime preflight (the `{app} doctor` checks, runtime subset) so the
/// onboarding UI can show ready/warning/blocked with fixes.
async fn health_diagnostics(
    Query(query): Query<WorkspaceQuery>,
) -> Json<crate::doctor::DiagnosticsReport> {
    let saved = query
        .workspace_root
        .as_deref()
        .and_then(|value| optional_workspace_root(Some(value)))
        .and_then(|root| load_sandbox_settings(&root).ok().flatten());
    let resolution = resolve_sandbox_settings(saved.as_ref());
    let source = match resolution.source {
        rightkit_agent_policy::SandboxPolicySource::Default => "default",
        rightkit_agent_policy::SandboxPolicySource::Operator => "RIGHTKIT_SANDBOX_TIER",
        rightkit_agent_policy::SandboxPolicySource::Workspace => {
            crate::host_identity::interned(format!(
                "{home}/sandbox-settings.json",
                home = crate::host_identity::home_dir()
            ))
        }
        rightkit_agent_policy::SandboxPolicySource::OperatorAndWorkspace => {
            crate::host_identity::interned(format!(
                "RIGHTKIT_SANDBOX_TIER + {home}/sandbox-settings.json",
                home = crate::host_identity::home_dir()
            ))
        }
    };
    let report = crate::doctor::gather_runtime_with_sandbox_tier(
        Some(resolution.effective.persisted_value().to_string()),
        source,
    );
    Json(report)
}

async fn get_sandbox_settings(
    Query(query): Query<WorkspaceQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let root = workspace_root_or_current(query.workspace_root.as_deref());
    let saved = load_sandbox_settings(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let resolution = resolve_sandbox_settings(saved.as_ref());
    Ok(Json(serde_json::json!({
        "workspaceRoot": root,
        "storePath": sandbox_settings_path(&root),
        "tier": resolution.effective.persisted_value(),
        "workspaceTier": resolution.workspace.map(|tier| tier.persisted_value()),
        "operatorTier": resolution.operator.map(|tier| tier.persisted_value()),
        "source": sandbox_policy_source(resolution.source),
        "operatorValid": resolution.operator_valid,
        "workspaceValid": resolution.workspace_valid,
        "allowedTiers": allowed_sandbox_tiers(),
    })))
}

async fn save_sandbox_settings(
    Query(query): Query<WorkspaceQuery>,
    headers: HeaderMap,
    Json(request): Json<SaveSandboxSettingsRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let root = workspace_root_or_current(query.workspace_root.as_deref());
    let tier = normalize_sandbox_tier(&request.tier).ok_or(StatusCode::BAD_REQUEST)?;
    let record = SandboxSettingsRecord {
        tier: tier.clone(),
        updated_at: now_iso(),
    };
    save_sandbox_settings_record(&root, &record).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let resolution = resolve_sandbox_settings(Some(&record));
    Ok(Json(serde_json::json!({
        "workspaceRoot": root,
        "storePath": sandbox_settings_path(&root),
        "tier": resolution.effective.persisted_value(),
        "workspaceTier": tier,
        "operatorTier": resolution.operator.map(|tier| tier.persisted_value()),
        "source": sandbox_policy_source(resolution.source),
        "operatorValid": resolution.operator_valid,
        "workspaceValid": resolution.workspace_valid,
        "allowedTiers": allowed_sandbox_tiers(),
    })))
}

/// Summary returned by `POST /diagnostics/bundle`. The bundle itself is the
/// RightKit (`rightkit-crash`) ZIP of redacted crash reports and summary, kept
/// under `<state root>/diagnostics/<bundleId>.zip`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiagnosticsBundleSummary {
    pub(crate) bundle_id: String,
    pub(crate) file_name: String,
    pub(crate) byte_length: u64,
    pub(crate) crash_reports: usize,
    pub(crate) native_artifacts: usize,
    pub(crate) log_events: usize,
    pub(crate) download_url: String,
}

/// Body of `GET /diagnostics/bundles/{id}`: the ZIP, base64-encoded in JSON.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiagnosticsBundleDownload {
    pub(crate) file_name: String,
    pub(crate) content_type: &'static str,
    pub(crate) content_base64: String,
}

fn diagnostics_bundle_dir() -> Option<PathBuf> {
    rightkit_agent_config::state_root().map(|root| root.join("diagnostics"))
}

fn safe_bundle_id(id: &str) -> Option<&str> {
    let ok = !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    ok.then_some(id)
}

async fn create_diagnostics_bundle(
    State(_state): State<DaemonState>,
    headers: HeaderMap,
) -> Result<Json<DiagnosticsBundleSummary>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let dir = diagnostics_bundle_dir().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let bundle_id = format!("bundle-{nanos}-{}", std::process::id());
    let destination = dir.join(format!("{bundle_id}.zip"));
    let crash_dir = rightkit_agent_config::state_root()
        .map(|root| root.join("crash"))
        .unwrap_or_else(|| rightkit_crash::fallback_crash_dir("RightKit"));
    let options = rightkit_crash::SupportBundleOptions {
        app_name: "RightKit".to_string(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        build: env!("CARGO_PKG_VERSION").to_string(),
        crash_dir,
        log_sources: Vec::new(),
        settings: json!({ "surface": "daemon" }),
        policy: rightkit_logs::CollectionPolicy::default(),
        include_native_dumps: false,
        destination: destination.clone(),
    };
    let manifest =
        tokio::task::spawn_blocking(move || rightkit_crash::build_support_bundle(&options))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let byte_length = std::fs::metadata(&destination)
        .map(|meta| meta.len())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(DiagnosticsBundleSummary {
        file_name: format!("{bundle_id}.zip"),
        download_url: format!("/diagnostics/bundles/{bundle_id}"),
        bundle_id,
        byte_length,
        crash_reports: manifest.crash_reports,
        native_artifacts: manifest.native_artifacts,
        log_events: manifest.log_events,
    }))
}

async fn download_diagnostics_bundle(
    State(_state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<DiagnosticsBundleDownload>, StatusCode> {
    let id = safe_bundle_id(&id).ok_or(StatusCode::NOT_FOUND)?;
    let dir = diagnostics_bundle_dir().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let bytes = tokio::fs::read(dir.join(format!("{id}.zip")))
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    Ok(Json(DiagnosticsBundleDownload {
        file_name: format!("{id}.zip"),
        content_type: "application/zip",
        content_base64: rightkit_crypto::base64_encode(&bytes),
    }))
}

#[derive(Debug, Deserialize)]
struct DenialsQuery {
    #[serde(default, alias = "sessionId", alias = "session_id")]
    session: Option<String>,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
}

async fn list_denials_handler(
    State(state): State<DaemonState>,
    Query(query): Query<DenialsQuery>,
) -> Json<Value> {
    let limit = query.limit.unwrap_or(50).min(500);
    let offset = query.offset.unwrap_or(0);
    let (denials, total) = {
        let guard = lock_recover(&state.denial_tracker, "denial_tracker");
        let (slice, total) = guard.query(
            query.session.as_deref(),
            query.tool.as_deref(),
            offset,
            limit,
        );
        let owned: Vec<rightkit_agent_policy::DenialRecord> = slice.into_iter().cloned().collect();
        (owned, total)
    };
    Json(json!({
        "denials": denials,
        "total": total,
        "limit": limit,
        "offset": offset,
    }))
}

include!("api_health/types.rs");

include!("api_health/openapi.rs");

include!("api_health/sandbox.rs");

include!("api_health/readiness.rs");

// ── FM-118 machine status/info runtime surface ──────────────────────────────
// Added inside api_health.rs only. Health, diagnostics, readiness, sandbox,
// bundle and denial routes are preserved verbatim above. The machine surface
// reuses the daemon's live health state (bound port, database path, storage
// pool, provider-key posture) and keeps a typed unavailable contract so
// callers can distinguish "daemon not initialized" from a normal 200
// degraded response. It does not touch desktop/iOS, Membrane/CM-001,
// §17/manifests, or Cargo.

pub(crate) const MACHINE_STATUS_SCHEMA: &str =
    rightkit_agent_engine_api_platform::openapi::MACHINE_STATUS_SCHEMA;

/// Stable machine-facing lifecycle class. `Unreachable` is emitted by a
/// client that cannot transport-probe a daemon; the daemon itself can emit
/// the other three classes because an unreachable listener has no HTTP body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MachineStatus {
    Healthy,
    Stopped,
    Unreachable,
    Degraded,
}

impl MachineStatus {
    fn is_healthy(self) -> bool {
        matches!(self, Self::Healthy)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MachineStatusResponse {
    pub(crate) schema_version: String,
    pub(crate) status: MachineStatus,
    pub(crate) ok: bool,
    pub(crate) service: String,
    pub(crate) port: u16,
    pub(crate) database_path: String,
    pub(crate) utc_now: String,
    pub(crate) degraded: bool,
    pub(crate) storage_available: bool,
    pub(crate) storage_read_pool_idle: Option<usize>,
    pub(crate) provider_key_storage: ProviderKeyStorageStatus,
    pub(crate) version: String,
    /// Human-readable reasons the daemon is degraded beyond storage/provider-key posture.
    #[serde(default)]
    pub(crate) degraded_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MachineInfoResponse {
    pub(crate) service: String,
    pub(crate) version: String,
    pub(crate) os: String,
    pub(crate) arch: String,
    pub(crate) pid: u32,
    pub(crate) hostname: Option<String>,
    pub(crate) database_path: String,
    pub(crate) port: u16,
    pub(crate) utc_now: String,
    pub(crate) storage_available: bool,
}

fn machine_status_for_runtime(
    initialized: bool,
    storage_available: bool,
    provider_degraded: bool,
) -> MachineStatus {
    if !initialized {
        MachineStatus::Stopped
    } else if storage_available && !provider_degraded {
        MachineStatus::Healthy
    } else {
        MachineStatus::Degraded
    }
}

fn machine_unavailable_response(
    status: MachineStatus,
    detail: impl Into<String>,
) -> (StatusCode, Json<Value>) {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "schemaVersion": MACHINE_STATUS_SCHEMA,
            "status": status,
            "ok": false,
            "code": "unavailable",
            "error": detail.into(),
            "service": crate::host_identity::prefixed("daemon")
        })),
    )
}

async fn machine_status(State(state): State<DaemonState>) -> impl IntoResponse {
    if state.bound_port == 0 && state.database_path.is_empty() {
        return machine_unavailable_response(
            MachineStatus::Stopped,
            "machine status unavailable: daemon not yet initialized",
        )
        .into_response();
    }
    let provider_health = rightkit_llm::agent_providers::provider_key_storage_health();
    let provider_key_storage = provider_key_storage_status(provider_health);
    let storage_read_pool_idle = state
        .storage
        .as_ref()
        .and_then(|storage| storage.read_pool())
        .map(|pool| pool.idle_len());
    let storage_available = state.storage.is_some();
    let permission_rules_error = crate::api_stores::refresh_permission_rule_store(
        &state.permission_rule_store_path,
        &state.permission_rule_store,
    );
    let degraded_reasons: Vec<String> = permission_rules_error
        .iter()
        .map(|error| error.degraded_reason())
        .collect();
    let status = machine_status_for_runtime(
        true,
        storage_available,
        provider_key_storage.keychain_failure_observed || !degraded_reasons.is_empty(),
    );
    let payload = MachineStatusResponse {
        schema_version: MACHINE_STATUS_SCHEMA.to_string(),
        status,
        ok: status.is_healthy(),
        service: crate::host_identity::prefixed("daemon"),
        port: state.bound_port,
        database_path: state.database_path.clone(),
        utc_now: now_iso(),
        degraded: matches!(status, MachineStatus::Degraded),
        storage_available,
        storage_read_pool_idle,
        provider_key_storage,
        version: env!("CARGO_PKG_VERSION").to_string(),
        degraded_reasons,
    };
    (StatusCode::OK, Json(json!(payload))).into_response()
}

async fn machine_info(State(state): State<DaemonState>) -> impl IntoResponse {
    if state.bound_port == 0 && state.database_path.is_empty() {
        return machine_unavailable_response(
            MachineStatus::Stopped,
            "machine info unavailable: daemon not yet initialized",
        )
        .into_response();
    }
    let hostname = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .and_then(|v| {
            let t = v.trim().to_string();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        });
    let payload = MachineInfoResponse {
        service: crate::host_identity::prefixed("daemon"),
        version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        pid: std::process::id(),
        hostname,
        database_path: state.database_path.clone(),
        port: state.bound_port,
        utc_now: now_iso(),
        storage_available: state.storage.is_some(),
    };
    (StatusCode::OK, Json(json!(payload))).into_response()
}
