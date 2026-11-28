use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use rightkit_agent_coding_tools::{
    SpawnRuntimeKind, SubagentContinuity, SubagentSpawnOptions, SubagentSpawner, ToolRegistry,
    ToolResult,
};
use rightkit_agent_engine_protocol::ToolCall;
use rightkit_agents::driver_host::cli_runtime::AgentRuntimeExecutor;
use rightkit_agents::driver_host::{
    ClaudeLaunchOptions, DriverCursor, DriverKind, NormalizedEvent, NormalizedEventKind,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::api::{lock_recover, now_iso, now_millis, DaemonState, DriverToolRequest};
use crate::api_permissions::enforce_trusted_source_authority;

// ra-016 decomposition: `api_drivers.rs` stays the facade — it owns
// `driver_routes()`/`driver_capability_routes()`, the wire types, and every
// HTTP handler's public behavior. These private feature modules own one
// responsibility apiece (CLI profile/route resolution and driver/capability
// JSON Schemas); the glob re-exports below put every item back into this
// module's namespace so the rest of this file keeps calling them
// unqualified exactly as before the split. Route paths, methods, JSON
// schemas, status mapping, lease headers, and driver-host calls are
// unchanged — only the item's source file moved.
mod agent_runtime;
mod capability_gateway;
mod cli_profiles;
mod driver_lifecycle;
mod schemas;
pub(crate) use agent_runtime::*;
pub(crate) use capability_gateway::*;
pub(crate) use cli_profiles::*;
pub(crate) use driver_lifecycle::*;
pub(crate) use schemas::*;

static CAPABILITY_HEADER: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| crate::host_identity::header("capability-token"));
static DRIVER_INSTANCE_HEADER: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| crate::host_identity::header("driver-instance"));
const MAX_LEASE_TTL_SECONDS: u64 = 60 * 60;
const DEFAULT_LEASE_TTL_SECONDS: u64 = 15 * 60;
const SPECIAL_TOOLS: &[&str] = &[
    "memory_recall",
    "delegate_task",
    "delegate_parallel",
    "verify",
];
const READ_ONLY_SPECIAL_TOOLS: &[&str] = &["memory_recall"];
/// What the harness check offers the program in its own throwaway folder.
const HARNESS_CHECK_TOOLS: &[&str] = &["write_file", "read_file", "memory_recall"];
const DRIVER_BLOCKED_TOOLS: &[&str] = &[
    "spawn_subagent",
    "send_message",
    "team_create",
    "team_delete",
    "followup_subagent",
    "wait_for_subagents",
    "interrupt_subagent",
];
const DRIVER_CAPABILITIES: &[&str] = &[
    "memory_recall",
    "delegate_task",
    "delegate_parallel",
    "verify",
];
/// The variables of the MCP server's environment that carry a secret (the scoped gateway token
/// and the driver instance secret), under the host's names.
fn driver_mcp_secret_environment() -> [String; 2] {
    [
        rightkit_agent_config::env_name("GATEWAY_TOKEN"),
        rightkit_agent_config::env_name("DRIVER_INSTANCE_SECRET"),
    ]
}
pub(crate) fn driver_routes() -> Router<DaemonState> {
    Router::new()
        .route("/cli-providers", get(list_cli_providers))
        .route(
            "/cli-providers/{provider}/discovery",
            get(cli_provider_discovery),
        )
        .route(
            "/cli-providers/{provider}/auth-status",
            get(cli_provider_auth_status),
        )
        .route("/cli-providers/{provider}/models", get(cli_provider_models))
        .route(
            "/cli-providers/{provider}/attestation",
            get(cli_provider_attestation),
        )
        .route("/cli-providers/{provider}/login", post(cli_provider_login))
        .route(
            "/cli-providers/{provider}/logout",
            post(cli_provider_logout),
        )
        .route(
            "/cli-providers/{provider}/verify-harness",
            post(cli_provider_verify_harness),
        )
        .route("/drivers/{kind}/discovery", get(discover_driver))
        .route("/drivers/{kind}/models", get(list_driver_models))
        .route(
            "/sessions/{id}/cli-delegation-policy",
            get(get_cli_delegation_policy).put(set_cli_delegation_policy),
        )
        .route(
            "/sessions/{id}/driver-capabilities/leases",
            post(issue_capability_lease),
        )
        .route("/sessions/{id}/driver/start", post(start_driver))
        .route("/sessions/{id}/driver/prompt", post(prompt_driver))
        .route("/sessions/{id}/driver/interrupt", post(interrupt_driver))
        .route("/sessions/{id}/driver/resume", post(resume_driver))
        .route("/sessions/{id}/driver/stop", post(stop_driver))
        .route("/sessions/{id}/driver/status", get(driver_status))
        .route(
            "/sessions/{id}/agents/{agent_id}/runtime/open",
            post(open_agent_runtime),
        )
        .route(
            "/sessions/{id}/agents/{agent_id}/runtime/prompt",
            post(prompt_agent_runtime),
        )
        .route(
            "/sessions/{id}/agents/{agent_id}/runtime/interrupt",
            post(interrupt_agent_runtime),
        )
        .route(
            "/sessions/{id}/agents/{agent_id}/runtime/resume",
            post(resume_agent_runtime),
        )
        .route(
            "/sessions/{id}/agents/{agent_id}/runtime/stop",
            post(stop_agent_runtime),
        )
        .route(
            "/sessions/{id}/agents/{agent_id}/runtime/status",
            get(agent_runtime_status),
        )
}

/// Gateway routes authenticate with a scoped lease plus a per-process secret.
/// They deliberately remain outside the daemon-bearer router so vendor MCP
/// processes never receive the daemon master token.
pub(crate) fn driver_capability_routes() -> Router<DaemonState> {
    Router::new()
        .route(
            "/sessions/{id}/driver-capabilities/tools",
            get(list_capabilities),
        )
        .route(
            "/sessions/{id}/driver-capabilities/call",
            post(call_capability),
        )
}

async fn get_cli_delegation_policy(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let policy = lock_recover(&state.cli_delegation_policies, "cli_delegation_policies")
        .get(&session_id)
        .cloned()
        .unwrap_or_else(|| state.cli_delegation_policy.clone());
    Ok(Json(
        serde_json::to_value(policy).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    ))
}

async fn set_cli_delegation_policy(
    State(state): State<DaemonState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    Json(policy): Json<rightkit_agents::driver_host::cli_runtime::CliDelegationPolicy>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    if policy.max_depth == 0
        || policy.max_depth > 4
        || policy.max_parallel_children == 0
        || policy.allowed_child_providers.iter().any(|provider| {
            state.cli_registry.adapter(provider).is_none()
                || policy
                    .max_parallel_per_provider
                    .get(provider)
                    .is_some_and(|limit| *limit == 0 || *limit > policy.max_parallel_children)
        })
        || policy
            .default_child_provider
            .as_ref()
            .is_some_and(|provider| !policy.allowed_child_providers.contains(provider))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    lock_recover(&state.cli_delegation_policies, "cli_delegation_policies")
        .insert(session_id, policy.clone());
    Ok(Json(
        serde_json::to_value(policy).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    ))
}

/// Outer cap on provider discovery probes; a provider still silent after this is reported as
/// unavailable instead of holding the HTTP response open.
const CLI_PROVIDER_PROBE_CAP: std::time::Duration = std::time::Duration::from_secs(25);

/// Run `probe` for every provider concurrently against one shared deadline. A probe that has not
/// finished by `cap` is replaced by `on_timeout(provider)`; a probe returning `None` is skipped.
async fn probe_providers_with_cap<T, F, Fut>(
    provider_ids: Vec<String>,
    cap: std::time::Duration,
    probe: F,
    on_timeout: impl Fn(&str) -> Option<T>,
) -> Vec<T>
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + cap;
    let probes = provider_ids.into_iter().map(|id| {
        let fut = probe(id.clone());
        let on_timeout = &on_timeout;
        async move {
            match tokio::time::timeout_at(deadline, fut).await {
                Ok(entry) => entry,
                Err(_) => on_timeout(&id),
            }
        }
    });
    futures_util::future::join_all(probes)
        .await
        .into_iter()
        .flatten()
        .collect()
}

async fn list_cli_providers(State(state): State<DaemonState>) -> Json<Value> {
    let ids = state
        .cli_registry
        .provider_ids()
        .into_iter()
        .map(|id| id.as_str().to_string())
        .collect::<Vec<_>>();
    let providers = probe_providers_with_cap(
        ids,
        CLI_PROVIDER_PROBE_CAP,
        |id| {
            let state = state.clone();
            async move {
                cli_provider_entry(
                    &state,
                    rightkit_agents::driver_host::cli_runtime::CliProviderId::new(id),
                )
                .await
            }
        },
        |id| cli_provider_timed_out_entry(&state, id),
    )
    .await;
    Json(json!({"providers": providers}))
}

/// Entry for a provider whose probes did not answer within the cap: same shape as an
/// installed-but-unreadable provider (no discovery, no auth, no models).
fn cli_provider_timed_out_entry(state: &DaemonState, id: &str) -> Option<Value> {
    let provider_id = rightkit_agents::driver_host::cli_runtime::CliProviderId::new(id.to_string());
    let adapter = state.cli_registry.adapter(&provider_id)?;
    Some(json!({
        "providerId": id, "displayName": adapter.display_name(),
        "installed": false, "executable": null, "binaryVersion": null, "executableSha256": null,
        "transport": adapter.transport_kind().label(),
        "auth": {"mode":"unknown","signedIn":false,"detectedSignedIn":false,"launchReady":false,"source":"managed_cli","state":"needs_auth","accountLabel":null},
        "harness": {"eligibility": "unavailable", "reason": "probe timed out"},
        "models": {"available": false, "source": null, "items": []},
        "unavailable": true, "timedOut": true,
        "repairActions": ["verify_harness"],
    }))
}

async fn cli_provider_entry(
    state: &DaemonState,
    provider_id: rightkit_agents::driver_host::cli_runtime::CliProviderId,
) -> Option<Value> {
    {
        let adapter = state.cli_registry.adapter(&provider_id)?;
        let profile = match persisted_cli_profile(state, provider_id.clone()).await {
            Ok(profile) => profile,
            Err(_) => return None,
        };
        let discovery = adapter.discover(&profile).await.ok();
        let auth = adapter.authentication_status(&profile).await.ok();
        let models = {
            adapter
                .list_models(&profile)
                .await
                .ok()
                .map(|catalog| json!({"source": catalog.source, "items": catalog.models}))
        };
        let signed_in = auth.as_ref().is_some_and(|value| value.signed_in);
        let harness =
            cli_harness_snapshot(state, &provider_id, discovery.as_ref(), signed_in).await;
        let qualified = harness.get("eligibility").and_then(Value::as_str) == Some("eligible");
        let launch_ready = signed_in && qualified;
        let auth_wire = json!({"mode":auth.as_ref().map(|v| v.auth_mode.label()).unwrap_or("unknown"),"signedIn":launch_ready,"detectedSignedIn":signed_in,"launchReady":launch_ready,"source":"managed_cli","state":if launch_ready{"configured"}else{"needs_auth"},"accountLabel":auth.as_ref().and_then(|v|v.account_label.clone())});
        Some(json!({
            "providerId": provider_id.as_str(), "displayName": adapter.display_name(),
            "installed": discovery.as_ref().is_some_and(|value| value.installed),
            "executable": discovery.as_ref().and_then(|value| value.executable.as_ref()).map(|value| value.to_string_lossy().to_string()),
            "binaryVersion": discovery.as_ref().and_then(|value| value.binary_version.as_ref()),
            "executableSha256": discovery.as_ref().and_then(|value| value.executable_sha256.as_ref()),
            "transport": adapter.transport_kind().label(),
            "auth": auth_wire,
            "harness": harness,
            "models": {"available": models.is_some(),"source":models.as_ref().and_then(|value| value.get("source").cloned()),"items":models.and_then(|value| value.get("items").cloned()).unwrap_or_else(|| json!([]))},
            "repairActions": if !qualified { json!(["verify_harness"]) } else { json!(["native_login"]) },
        }))
    }
}

async fn cli_provider_discovery(
    State(state): State<DaemonState>,
    Path(provider): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let provider_id = rightkit_agents::driver_host::cli_runtime::CliProviderId::new(provider);
    let adapter = state
        .cli_registry
        .adapter(&provider_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let profile = persisted_cli_profile(&state, provider_id).await?;
    let value = tokio::time::timeout(CLI_PROVIDER_PROBE_CAP, adapter.discover(&profile))
        .await
        .map_err(|_| StatusCode::GATEWAY_TIMEOUT)?
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(Json(
        serde_json::to_value(value).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    ))
}

async fn cli_provider_auth_status(
    State(state): State<DaemonState>,
    Path(provider): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let provider_id = rightkit_agents::driver_host::cli_runtime::CliProviderId::new(provider);
    let adapter = state
        .cli_registry
        .adapter(&provider_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let mut profile = persisted_cli_profile(&state, provider_id).await?;
    let value = adapter
        .authentication_status(&profile)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    profile.signed_in = value.signed_in;
    profile.auth_mode = value.auth_mode;
    profile.account_fingerprint = value.account_fingerprint.clone();
    profile.last_verified_at = Some(value.observed_at.clone());
    persist_cli_profile(&state, &profile).await?;
    Ok(Json(
        serde_json::to_value(value).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    ))
}

async fn cli_provider_models(
    State(state): State<DaemonState>,
    Path(provider): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let provider_id = rightkit_agents::driver_host::cli_runtime::CliProviderId::new(provider);
    let adapter = state
        .cli_registry
        .adapter(&provider_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let profile = persisted_cli_profile(&state, provider_id.clone()).await?;
    let value = adapter
        .list_models(&profile)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    Ok(Json(
        serde_json::to_value(value).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    ))
}

async fn cli_provider_attestation(
    State(state): State<DaemonState>,
    Path(provider): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let provider_id = rightkit_agents::driver_host::cli_runtime::CliProviderId::new(provider);
    if state.cli_registry.adapter(&provider_id).is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let row = storage
        .lock()
        .await
        .latest_cli_harness_attestation(provider_id.as_str())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(cli_attestation_json(&row)))
}

fn cli_attestation_json(row: &rightkit_agent_engine_storage::CliHarnessAttestationRow) -> Value {
    let probes = serde_json::from_str::<Value>(&row.evidence_json)
        .ok()
        .and_then(|value| value.get("probes").cloned())
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    let passed_probes = probes
        .iter()
        .filter(|probe| probe.get("passed").and_then(Value::as_bool) == Some(true))
        .filter_map(|probe| probe.get("probeId").or_else(|| probe.get("probe_id")))
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    let failed_probes = probes
        .iter()
        .filter(|probe| probe.get("passed").and_then(Value::as_bool) == Some(false))
        .filter_map(|probe| probe.get("probeId").or_else(|| probe.get("probe_id")))
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    json!({
        "id": row.id,
        "providerId": row.provider_id,
        "executableSha256": row.executable_sha256,
        "binaryVersion": row.binary_version,
        "protocolVersion": row.protocol_version,
        "profileHash": row.profile_hash,
        "policyHash": row.policy_hash,
        "toolSchemaHash": row.tool_schema_hash,
        "operatingSystem": row.operating_system,
        "architecture": row.architecture,
        "isolationMode": row.isolation_mode,
        "passedProbes": passed_probes,
        "failedProbes": failed_probes,
        "verifiedAt": row.verified_at,
    })
}

async fn cli_harness_snapshot(
    state: &DaemonState,
    provider_id: &rightkit_agents::driver_host::cli_runtime::CliProviderId,
    discovery: Option<&rightkit_agents::driver_host::cli_runtime::CliDiscovery>,
    signed_in: bool,
) -> Value {
    let Some(storage) = state.storage.as_ref() else {
        return json!({
            "eligibility": "harness_verification_required",
            "attestationId": Value::Null,
            "isolationMode": "unknown",
        });
    };
    let latest = storage
        .lock()
        .await
        .latest_cli_harness_attestation(provider_id.as_str())
        .ok()
        .flatten();
    let attestation_id = latest.as_ref().map(|row| row.id.clone());
    let isolation_mode = latest
        .as_ref()
        .map(|row| row.isolation_mode.clone())
        .unwrap_or_else(|| "unknown".to_string());
    let ready = discovery
        .and_then(|value| value.executable_sha256.as_deref())
        .is_some_and(|digest| state.cli_registry.is_ready(provider_id, digest));
    let eligibility = if !signed_in {
        "login_required"
    } else if ready {
        "ready"
    } else if latest.as_ref().is_some_and(|row| row.result != "passed") {
        "unqualified"
    } else {
        "harness_verification_required"
    };
    json!({
        "eligibility": eligibility,
        "attestationId": attestation_id,
        "isolationMode": isolation_mode,
    })
}

/// The login route answers with a typed body so the person sees why a sign-in failed (providers
/// R13, D3): the vendor's own stderr summary on a failed or timed-out login, never a bare status.
fn login_error_response(
    status: StatusCode,
    code: &str,
    message: String,
) -> (StatusCode, Json<Value>) {
    (
        status,
        Json(json!({"error": {"code": code, "message": message}})),
    )
}

async fn cli_provider_login(
    State(state): State<DaemonState>,
    Path(provider): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    use rightkit_agents::driver_host::cli_runtime::LoginError;
    let bare = |status: StatusCode| {
        login_error_response(status, "cli_provider_login_failed", status.to_string())
    };
    let provider_id = rightkit_agents::driver_host::cli_runtime::CliProviderId::new(provider);
    if state.cli_registry.adapter(&provider_id).is_none() {
        return Err(bare(StatusCode::NOT_FOUND));
    }
    let login_provider = provider_id.clone();
    let login_result = tokio::task::spawn_blocking(move || {
        rightkit_agents::driver_host::cli_runtime::login(&login_provider, "default")
    })
    .await
    .map_err(|_| bare(StatusCode::INTERNAL_SERVER_ERROR))?;
    if let Err(error) = login_result {
        let (status, code) = match &error {
            LoginError::NotRegistered(_) => (StatusCode::NOT_FOUND, "login_not_available"),
            LoginError::TimedOut { .. } => (StatusCode::GATEWAY_TIMEOUT, "login_timed_out"),
            _ => (StatusCode::BAD_GATEWAY, "login_failed"),
        };
        return Err(login_error_response(status, code, error.to_string()));
    }
    let mut profile = persisted_cli_profile(&state, provider_id.clone())
        .await
        .map_err(bare)?;
    let status = rightkit_agents::driver_host::cli_runtime::login_status(&provider_id, "default")
        .map_err(|_| bare(StatusCode::BAD_GATEWAY))?;
    profile.signed_in = status.signed_in;
    profile.auth_mode = status.auth_mode;
    profile.account_fingerprint = status.account_fingerprint;
    profile.last_verified_at = Some(status.observed_at);
    persist_cli_profile(&state, &profile).await.map_err(bare)?;
    Ok(Json(
        json!({"providerId": provider_id.as_str(), "profile": profile}),
    ))
}

async fn cli_provider_logout(
    State(state): State<DaemonState>,
    Path(provider): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let provider_id = rightkit_agents::driver_host::cli_runtime::CliProviderId::new(provider);
    if state.cli_registry.adapter(&provider_id).is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let logout_provider = provider_id.clone();
    tokio::task::spawn_blocking(move || {
        rightkit_agents::driver_host::cli_runtime::logout(&logout_provider, "default")
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let profile_id = default_cli_profile(provider_id).id.as_str().to_string();
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    storage
        .lock()
        .await
        .delete_cli_account_profile(&profile_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({"loggedOut": true})))
}

struct DaemonQualificationObserver {
    storage: crate::api::SharedStorage,
    baselines: tokio::sync::Mutex<HashMap<String, HashSet<String>>>,
}

#[async_trait::async_trait]
impl rightkit_agents::driver_host::cli_runtime::QualificationObserver
    for DaemonQualificationObserver
{
    async fn begin_probe(
        &self,
        binding_id: &str,
        probe: &rightkit_agents::driver_host::cli_runtime::QualificationProbe,
    ) -> Result<(), String> {
        let keys = self
            .storage
            .lock()
            .await
            .list_driver_events(binding_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|event| event.event_key)
            .collect();
        self.baselines
            .lock()
            .await
            .insert(format!("{binding_id}:{}", probe.id), keys);
        Ok(())
    }

    async fn finish_probe(
        &self,
        binding_id: &str,
        probe: &rightkit_agents::driver_host::cli_runtime::QualificationProbe,
        events: &[NormalizedEvent],
    ) -> Result<rightkit_agents::driver_host::cli_runtime::QualificationEvidence, String> {
        let baseline = self
            .baselines
            .lock()
            .await
            .remove(&format!("{binding_id}:{}", probe.id))
            .unwrap_or_default();
        let rows = self
            .storage
            .lock()
            .await
            .list_driver_events(binding_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|event| !baseline.contains(&event.event_key))
            .collect::<Vec<_>>();
        let durable_call_ids = rows
            .iter()
            .filter_map(|event| event.event_key.rsplit(':').next())
            .collect::<HashSet<_>>();
        let durable_effect_ids = rows
            .iter()
            .filter(|event| event.status == "processed")
            .map(|event| event.event_key.clone())
            .collect::<Vec<_>>();
        let mut native_denial_event_ids = Vec::new();
        let mut native_effect_observed = false;
        let mut gateway_ready = false;
        for event in events {
            match &event.kind {
                NormalizedEventKind::GatewayReady => gateway_ready = true,
                NormalizedEventKind::ToolResult {
                    call_id, is_error, ..
                } if !durable_call_ids.contains(call_id.as_str()) => {
                    if *is_error {
                        native_denial_event_ids.push(event.event_key.clone());
                    } else {
                        native_effect_observed = true;
                    }
                }
                NormalizedEventKind::TurnFailed { message }
                    if ["denied", "forbidden", "permission", "blocked"]
                        .iter()
                        .any(|needle| message.to_ascii_lowercase().contains(needle)) =>
                {
                    native_denial_event_ids.push(event.event_key.clone());
                }
                _ => {}
            }
        }
        let host_mcp_discovered = gateway_ready || !rows.is_empty();
        // Probe ids carry the provider's name (`claude_mcp_discovery`). This compared against an
        // older fixed id that no probe has, so finding the tool server was never enough and the
        // discovery check also demanded a completed effect.
        let expected_host_effect_observed = if probe.id.ends_with("mcp_discovery") {
            host_mcp_discovered
        } else {
            !durable_effect_ids.is_empty()
        };
        Ok(
            rightkit_agents::driver_host::cli_runtime::QualificationEvidence {
                native_effect_observed,
                native_denial_observed: !native_denial_event_ids.is_empty(),
                host_mcp_discovered,
                expected_host_effect_observed,
                native_denial_event_ids,
                durable_effect_ids,
                detail: None,
            },
        )
    }
}

async fn cli_provider_verify_harness(
    State(state): State<DaemonState>,
    Path(provider): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let provider_id = rightkit_agents::driver_host::cli_runtime::CliProviderId::new(provider);
    let adapter = state
        .cli_registry
        .adapter(&provider_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let profile = launchable_cli_profile(&state, provider_id.clone()).await?;
    let resolved = resolve_cli_launch_route(&state, &provider_id, profile, None)
        .await
        .map_err(|_| StatusCode::CONFLICT)?;
    let profile = resolved.launch_profile.clone();
    let discovery = adapter
        .discover(&profile)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let executable = discovery.executable.clone().ok_or(StatusCode::CONFLICT)?;
    let workspace = std::env::temp_dir().join(format!(
        "{app}-qualification-{}",
        crate::startup::generate_token(),
        app = crate::host_identity::app()
    ));
    tokio::fs::create_dir(&workspace)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let session_id = format!("qualification-{}", crate::startup::generate_token());
    let binding_id = format!("binding-{session_id}");
    let agent_id = format!("agent-{session_id}");
    // The check's folder is the engine's own: made a moment ago, empty, removed afterwards. The
    // session is trusted in that folder only and may write files there without a person, so the
    // "governed write" check is a real write through the host and the check does not depend on
    // Membrane being installed (memory recall was the only tool it used to offer).
    let live = state.create_session_with_options(
        &session_id,
        Some("qualification"),
        &workspace.to_string_lossy(),
        "CLI harness qualification",
        None,
        crate::api::SessionRuntimeOptions {
            trust_workspace: true,
            permission_mode: Some(rightkit_agent_policy::PermissionMode::AcceptEdits),
            allowed_tools: HARNESS_CHECK_TOOLS
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
            ..Default::default()
        },
    );
    *lock_recover(&live.driver_kind, "session.driver_kind") = provider_id.as_str().to_string();
    let lease_id = format!("lease-{}", crate::startup::generate_token());
    let nonce = crate::startup::generate_token();
    let capability_token = format!(
        "{lease_id}.{nonce}.{}{}",
        crate::startup::generate_token(),
        crate::startup::generate_token()
    );
    let process_instance_secret = format!(
        "{}{}",
        crate::startup::generate_token(),
        crate::startup::generate_token()
    );
    let current_exe = std::env::current_exe().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mcp_server = rightkit_agents::driver_host::cli_runtime::CliMcpServerSpec {
        name: rightkit_agents::driver_host::host_names::mcp_server_name(),
        command: current_exe,
        args: vec![
            "mcp-server".to_string(),
            "--session-id".to_string(),
            session_id.clone(),
        ],
        env: vec![
            (
                rightkit_agent_config::env_name("DAEMON_URL"),
                format!("http://127.0.0.1:{}", state.bound_port),
            ),
            (
                rightkit_agent_config::env_name("GATEWAY_TOKEN"),
                capability_token.clone(),
            ),
            (
                rightkit_agent_config::env_name("DRIVER_INSTANCE_SECRET"),
                process_instance_secret.clone(),
            ),
        ],
    };
    let policy = rightkit_agents::driver_host::run_scoped_policy();
    let profile_hash = resolved.effective_profile_hash.clone();
    let policy_hash = rightkit_fs::digest::sha256_bytes(policy.as_bytes());
    let tool_schema_hash =
        gateway_tool_schema_hash().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let protocol_version = discovery
        .protocol_version
        .clone()
        .unwrap_or_else(|| adapter.transport_kind().label().to_string());
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let created_at = unix_key(0);
    let binding = rightkit_agent_engine_storage::DriverBindingRow {
        id: binding_id.clone(),
        driver_kind: provider_id.as_str().to_string(),
        session_id: session_id.clone(),
        vendor_session_id: None,
        vendor_thread_id: None,
        vendor_turn_id: None,
        binary_version: discovery.binary_version.clone().unwrap_or_default(),
        schema_hash: tool_schema_hash.clone(),
        status: "qualifying".to_string(),
        last_event_key: None,
        last_event_cursor: None,
        host_agent_id: Some(agent_id.clone()),
        parent_agent_id: String::new(),
        provider_id: Some(provider_id.as_str().to_string()),
        transport_kind: Some(adapter.transport_kind().label().to_string()),
        account_profile_id: Some(profile.id.as_str().to_string()),
        auth_mode: Some(profile.auth_mode.label().to_string()),
        account_fingerprint: profile.account_fingerprint.clone(),
        model_id: None,
        executable_path: Some(executable.to_string_lossy().into_owned()),
        executable_sha256: discovery.executable_sha256.clone(),
        protocol_version: Some(protocol_version.clone()),
        profile_hash: Some(profile_hash.clone()),
        policy_hash: Some(policy_hash.clone()),
        tool_schema_hash: Some(tool_schema_hash.clone()),
        harness_attestation_id: None,
        isolation_mode: Some(adapter.capabilities().isolation_mode.label().to_string()),
        resume_strategy: None,
        context_epoch: 0,
        process_generation: 1,
        created_at: created_at.clone(),
        updated_at: created_at.clone(),
    };
    let lease = rightkit_agent_engine_storage::SessionCapabilityLeaseRow {
        id: lease_id,
        binding_id: binding_id.clone(),
        token_hash: token_hash(&capability_token),
        nonce,
        session_id: session_id.clone(),
        canonical_workspace: workspace.to_string_lossy().into_owned(),
        tool_ceiling_json: serde_json::to_string(&effective_ceiling(
            &live,
            &HARNESS_CHECK_TOOLS
                .iter()
                .map(|name| (*name).to_string())
                .collect::<Vec<_>>(),
        )?)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        created_at: created_at.clone(),
        expires_at: unix_key(MAX_LEASE_TTL_SECONDS),
        revoked_at: None,
        process_instance_hash: Some(token_hash(&process_instance_secret)),
    };
    {
        let store = storage.lock().await;
        store
            .upsert_driver_binding(&binding)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        store
            .upsert_session_capability_lease(&lease)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    state
        .qualification_bindings
        .write()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .insert(binding_id.clone());
    let observer = DaemonQualificationObserver {
        storage: storage.clone(),
        baselines: tokio::sync::Mutex::new(HashMap::new()),
    };
    let config = rightkit_agents::driver_host::cli_runtime::QualificationConfig {
        probes: adapter.qualification_suite().to_vec(),
        ..rightkit_agents::driver_host::cli_runtime::QualificationConfig::default()
    };
    let inputs = rightkit_agents::driver_host::cli_runtime::QualificationInputs {
        provider_id: provider_id.clone(),
        executable,
        protocol_version,
        profile_hash,
        policy_hash,
        tool_schema_hash,
        operating_system: std::env::consts::OS.to_string(),
        architecture: std::env::consts::ARCH.to_string(),
        isolation_mode: adapter.capabilities().isolation_mode,
        context: rightkit_agents::driver_host::cli_runtime::QualificationContext {
            binding_id: binding_id.clone(),
            agent_id,
            profile,
            mcp_server,
            policy,
            isolated_workspace: workspace.clone(),
            session_id: session_id.clone(),
            model_id: None,
        },
    };
    // The check reads the program's output with blocking calls. Run it on a blocking thread:
    // on a server worker it stopped the engine from answering any request while it waited, and
    // the program's own tool server (which calls back into this engine) could never get through,
    // so every check that needs a tool call ran out its time.
    let registry = state.cli_registry.clone();
    let runtime = tokio::runtime::Handle::current();
    let probes = config.probes.clone();
    let run = tokio::task::spawn_blocking(move || {
        runtime.block_on(
            rightkit_agents::driver_host::cli_runtime::run_qualification(
                &registry, &inputs, &config, &observer,
            ),
        )
    })
    .await
    .unwrap_or_else(|error| Err(std::io::Error::other(error.to_string())));
    if let Ok(mut bindings) = state.qualification_bindings.write() {
        bindings.remove(&binding_id);
    }
    lock_recover(&state.sessions, "sessions").remove(&session_id);
    {
        let store = storage.lock().await;
        let _ = store.revoke_session_capability_lease(&lease.id, &unix_key(0));
        let _ = store.delete_driver_binding(&binding_id);
        let _ = store.delete_session(&session_id);
    }
    let _ = tokio::fs::remove_dir_all(&workspace).await;
    let outcome = run.map_err(|error| {
        tracing::warn!(provider = %provider_id, %error, "CLI qualification failed");
        StatusCode::BAD_GATEWAY
    })?;
    let failed_mandatory = outcome.mandatory_failures(&probes);
    let evidence_json = json!({
        "probes": outcome.results.iter().map(|result| json!({
            "probeId": result.probe_id,
            "polarity": result.polarity,
            "passed": result.passed,
            "detail": result.detail,
        })).collect::<Vec<_>>()
    });
    let attestation = outcome.into_attestation(std::env::consts::OS, std::env::consts::ARCH);
    let row = rightkit_agent_engine_storage::CliHarnessAttestationRow {
        id: attestation.id.clone(),
        provider_id: attestation.provider_id.as_str().to_string(),
        executable_sha256: attestation.executable_sha256.clone(),
        binary_version: attestation.binary_version.clone(),
        protocol_version: attestation.protocol_version.clone(),
        profile_hash: attestation.profile_hash.clone(),
        policy_hash: attestation.policy_hash.clone(),
        tool_schema_hash: attestation.tool_schema_hash.clone(),
        operating_system: attestation.operating_system.clone(),
        architecture: attestation.architecture.clone(),
        isolation_mode: attestation.isolation_mode.label().to_string(),
        result: if failed_mandatory.is_empty() {
            "passed"
        } else {
            "failed"
        }
        .to_string(),
        evidence_json: serde_json::to_string(&evidence_json)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        verified_at: attestation.verified_at.clone(),
    };
    storage
        .lock()
        .await
        .upsert_cli_harness_attestation(&row)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(json!({
        "providerId": provider_id.as_str(),
        "attestationId": attestation.id,
        "passed": failed_mandatory.is_empty(),
        "failedMandatoryProbes": failed_mandatory,
    })))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IssueLeaseRequest {
    driver_kind: String,
    /// Stable logical agent identity. Legacy callers omit it; their root
    /// session id remains the compatibility identity.
    agent_id: Option<String>,
    binary_version: String,
    schema_hash: String,
    #[serde(default)]
    tools: Vec<String>,
    ttl_seconds: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CapabilityCallRequest {
    call_id: Value,
    name: String,
    #[serde(default = "empty_object")]
    arguments: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StartDriverRequest {
    #[serde(default)]
    binding_id: String,
    executable: Option<String>,
    /// A prior process died after claiming durable work. Resuming requires an
    /// explicit acknowledgement that those effects must not be retried.
    #[serde(default)]
    recover_claimed: bool,
    /// Caller-selected model id, e.g. `"o3"` (Codex) or `"sonnet"` (Claude). Validated by
    /// `CodexLaunchOptions`/`ClaudeLaunchOptions::launch_spec` before it reaches an argv.
    model: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OpenAgentRuntimeRequest {
    runtime: OpenAgentRuntimeSelection,
    role: String,
    parent_agent_id: Option<String>,
    worktree_id: Option<String>,
    #[serde(default)]
    tool_ceiling: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OpenAgentRuntimeSelection {
    kind: String,
    provider_id: String,
    model_id: Option<String>,
    account_profile_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DriverPromptRequest {
    #[serde(default)]
    binding_id: String,
    #[serde(default)]
    vendor_session_id: String,
    text: String,
    request_label: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DriverInterruptRequest {
    #[serde(default)]
    binding_id: String,
    #[serde(default)]
    vendor_session_id: String,
    vendor_turn_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DriverBindingRequest {
    #[serde(default)]
    binding_id: String,
    vendor_session_id: Option<String>,
}

fn empty_object() -> Value {
    json!({})
}

fn canonical_workspace(workspace: &str) -> Result<PathBuf, StatusCode> {
    std::fs::canonicalize(workspace).map_err(|_| StatusCode::BAD_REQUEST)
}

fn token_hash(token: &str) -> String {
    rightkit_crypto::hex_encode(&rightkit_crypto::sha256(token.as_bytes()))
}

fn unix_key(offset_seconds: u64) -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
        .saturating_add(offset_seconds);
    format!("{seconds:020}")
}

fn tool_result_json(result: ToolResult) -> Value {
    json!({
        "content": [{"type": "text", "text": result.content}],
        "isError": result.is_error,
        "_meta": {
            "mutationCount": result.mutations.len(),
            "processId": result.process_id,
            "mediaType": result.media_type,
        }
    })
}
