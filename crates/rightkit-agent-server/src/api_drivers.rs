use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use rightkit_agent_engine_protocol::ToolCall;
use rightkit_agent_tools::coding_tools::{
    SpawnRuntimeKind, SubagentContinuity, SubagentSpawnOptions, SubagentSpawner, ToolRegistry,
    ToolResult,
};
use rightkit_agents::driver_host::cli_runtime::AgentRuntimeExecutor;
use rightkit_agents::driver_host::{
    ClaudeLaunchOptions, CodexLaunchOptions, DriverCursor, DriverKind, NormalizedEvent,
    NormalizedEventKind,
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
// responsibility apiece (CLI profile/route resolution, the Codex model
// catalogue protocol, and driver/capability JSON Schemas); the glob
// re-exports below put every item back into this module's namespace so the
// rest of this file, and its `#[cfg(test)]` module, keep calling them
// unqualified exactly as before the split. Route paths, methods, JSON
// schemas, status mapping, lease headers, and driver-host calls are
// unchanged — only the item's source file moved.
mod agent_runtime;
mod capability_gateway;
mod cli_profiles;
mod driver_lifecycle;
mod model_catalog;
mod schemas;
pub(crate) use agent_runtime::*;
pub(crate) use capability_gateway::*;
pub(crate) use cli_profiles::*;
pub(crate) use driver_lifecycle::*;
pub(crate) use model_catalog::*;
pub(crate) use schemas::*;

const CAPABILITY_HEADER: &str = "x-coderight-capability-token";
const DRIVER_INSTANCE_HEADER: &str = "x-coderight-driver-instance";
const MAX_LEASE_TTL_SECONDS: u64 = 60 * 60;
const DEFAULT_LEASE_TTL_SECONDS: u64 = 15 * 60;
const SPECIAL_TOOLS: &[&str] = &[
    "memory_recall",
    "delegate_task",
    "delegate_parallel",
    "verify",
];
const READ_ONLY_SPECIAL_TOOLS: &[&str] = &["memory_recall"];
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
/// Codex features whose vendor-native effect paths must never be available to
/// a driver-hosted session. The capability gateway is the sole effect path.
const CODEX_DISABLED_VENDOR_FEATURES: &[&str] = &[
    "apps",
    "plugins",
    "computer_use",
    "browser_use",
    "browser_use_external",
    "browser_use_full_cdp_access",
    "in_app_browser",
    "image_generation",
    "code_mode_host",
    "shell_tool",
    "shell_snapshot",
    "unified_exec",
    "multi_agent",
    "multi_agent_v2",
    "external_agent_memory_import",
];
const DRIVER_MCP_SECRET_ENVIRONMENT: &[&str] =
    &["RIGHTKIT_GATEWAY_TOKEN", "RIGHTKIT_DRIVER_INSTANCE_SECRET"];
const DRIVER_MCP_FORWARDED_ENVIRONMENT: &[&str] = &[
    "RIGHTKIT_DAEMON_URL",
    "RIGHTKIT_GATEWAY_TOKEN",
    "RIGHTKIT_DRIVER_INSTANCE_SECRET",
];
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
            "/cli-runtime/matrix-cell",
            post(run_cli_runtime_matrix_cell),
        )
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
        let state = state;
        let Some(adapter) = state.cli_registry.adapter(&provider_id) else {
            return None;
        };
        let profile = match persisted_cli_profile(state, provider_id.clone()).await {
            Ok(profile) => profile,
            Err(_) => return None,
        };
        let discovery = adapter.discover(&profile).await.ok();
        let auth = adapter.authentication_status(&profile).await.ok();
        let models = if provider_id.as_str() == "codex" {
            query_codex_model_catalogue_with_profile("codex", &profile)
                .await
                .ok()
                .map(|items| {
                    json!({
                        "source": "codex-app-server",
                        "items": items
                            .into_iter()
                            .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_string))
                            .collect::<Vec<_>>(),
                    })
                })
        } else {
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
    if provider_id.as_str() == "codex" {
        let models = query_codex_model_catalogue_with_profile("codex", &profile)
            .await
            .map_err(|_| StatusCode::BAD_GATEWAY)?;
        return Ok(Json(json!({
            "models": models,
            "source": "codex-app-server",
            "fetchedAt": now_iso(),
        })));
    }
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

async fn cli_provider_login(
    State(state): State<DaemonState>,
    Path(provider): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let provider_id = rightkit_agents::driver_host::cli_runtime::CliProviderId::new(provider);
    if state.cli_registry.adapter(&provider_id).is_none() {
        return Err(StatusCode::NOT_FOUND);
    }
    let login_provider = provider_id.clone();
    tokio::task::spawn_blocking(move || {
        rightkit_agents::driver_host::cli_runtime::login(&login_provider, "default")
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let mut profile = persisted_cli_profile(&state, provider_id.clone()).await?;
    let status = rightkit_agents::driver_host::cli_runtime::login_status(&provider_id, "default")
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    profile.signed_in = status.signed_in;
    profile.auth_mode = status.auth_mode;
    profile.account_fingerprint = status.account_fingerprint;
    profile.last_verified_at = Some(status.observed_at);
    persist_cli_profile(&state, &profile).await?;
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
        let coderight_mcp_discovered = gateway_ready || !rows.is_empty();
        let expected_coderight_effect_observed = if probe.id == "pos.coderight_mcp_discovery" {
            coderight_mcp_discovered
        } else {
            !durable_effect_ids.is_empty()
        };
        Ok(
            rightkit_agents::driver_host::cli_runtime::QualificationEvidence {
                native_effect_observed,
                native_denial_observed: !native_denial_event_ids.is_empty(),
                coderight_mcp_discovered,
                expected_coderight_effect_observed,
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
        "coderight-qualification-{}",
        crate::startup::generate_token()
    ));
    tokio::fs::create_dir(&workspace)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let session_id = format!("qualification-{}", crate::startup::generate_token());
    let binding_id = format!("binding-{session_id}");
    let agent_id = format!("agent-{session_id}");
    let live = state.create_session(
        &session_id,
        Some("qualification"),
        &workspace.to_string_lossy(),
        "CLI harness qualification",
        None,
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
        name: "coderight".to_string(),
        command: current_exe,
        args: vec![
            "mcp-server".to_string(),
            "--session-id".to_string(),
            session_id.clone(),
        ],
        env: vec![
            (
                "RIGHTKIT_DAEMON_URL".to_string(),
                format!("http://127.0.0.1:{}", state.bound_port),
            ),
            (
                "RIGHTKIT_GATEWAY_TOKEN".to_string(),
                capability_token.clone(),
            ),
            (
                "RIGHTKIT_DRIVER_INSTANCE_SECRET".to_string(),
                process_instance_secret.clone(),
            ),
        ],
    };
    let policy = rightkit_agents::driver_host::RUN_SCOPED_POLICY.to_string();
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
        coderight_agent_id: Some(agent_id.clone()),
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
        tool_ceiling_json: serde_json::to_string(&effective_ceiling(&live, &[])?)
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
    let run = rightkit_agents::driver_host::cli_runtime::run_qualification(
        &state.cli_registry,
        &rightkit_agents::driver_host::cli_runtime::QualificationInputs {
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
        },
        &config,
        &observer,
    )
    .await;
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
    let failed_mandatory = outcome.mandatory_failures(&config.probes);
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
struct CliRuntimeMatrixCellRequest {
    provider: String,
    scenario: String,
    workspace: String,
    child_provider: Option<String>,
}

/// Installed-matrix control surface. Every passing receipt comes from a real
/// authenticated executor turn plus daemon-owned normalized events/effects.
async fn run_cli_runtime_matrix_cell(
    State(state): State<DaemonState>,
    Json(body): Json<CliRuntimeMatrixCellRequest>,
) -> Json<Value> {
    let provider_id =
        rightkit_agents::driver_host::cli_runtime::CliProviderId::new(body.provider.clone());
    let Some(adapter) = state.cli_registry.adapter(&provider_id) else {
        return Json(matrix_cell_receipt(
            &body,
            false,
            "provider_not_registered",
            json!([]),
        ));
    };
    if !std::path::Path::new(&body.workspace).is_dir() {
        return Json(matrix_cell_receipt(
            &body,
            false,
            "workspace_not_found",
            json!([]),
        ));
    }
    let profile = rightkit_agents::driver_host::cli_runtime::CliAccountProfile {
        id: rightkit_agents::driver_host::cli_runtime::CliAccountProfileId::new(format!(
            "{}/default",
            body.provider
        )),
        provider_id: provider_id.clone(),
        label: "default".to_string(),
        managed_home: rightkit_agents::driver_host::cli_runtime::profile::managed_home(
            &provider_id,
            "default",
        ),
        auth_mode: rightkit_agents::driver_host::cli_runtime::CliAuthMode::Unknown,
        signed_in: false,
        account_fingerprint: None,
        last_verified_at: None,
    };
    let discovery = match adapter.discover(&profile).await {
        Ok(value) => value,
        Err(error) => {
            return Json(matrix_cell_receipt(
                &body,
                false,
                &error.to_string(),
                json!([]),
            ))
        }
    };
    if !discovery.installed {
        return Json(matrix_cell_receipt(
            &body,
            false,
            "provider_binary_not_installed",
            json!([]),
        ));
    }
    let session_id = format!("matrix-{}", crate::startup::generate_token());
    let agent_id = format!("matrix-agent-{}", crate::startup::generate_token());
    let role = if body.scenario == "child" {
        "child"
    } else {
        "root"
    };
    let parent_agent_id = (body.scenario == "child").then(|| "matrix-parent".to_string());
    let live = state.create_session(
        &session_id,
        Some(role),
        &body.workspace,
        "CLI runtime matrix",
        None,
    );
    *lock_recover(&live.driver_kind, "session.driver_kind") = body.provider.clone();
    let open = open_agent_runtime(
        State(state.clone()),
        Path((session_id.clone(), agent_id.clone())),
        HeaderMap::new(),
        Json(OpenAgentRuntimeRequest {
            runtime: OpenAgentRuntimeSelection {
                kind: "authenticated_cli".to_string(),
                provider_id: body.provider.clone(),
                model_id: None,
                account_profile_id: format!("{}/default", body.provider),
            },
            role: role.to_string(),
            parent_agent_id,
            worktree_id: Some(body.workspace.clone()),
            tool_ceiling: Vec::new(),
        }),
    )
    .await;
    let open = match open {
        Ok(Json(value)) => value,
        Err(status) => {
            lock_recover(&state.sessions, "sessions").remove(&session_id);
            return Json(matrix_cell_receipt(
                &body,
                false,
                &format!("runtime_open_failed:{}", status.as_u16()),
                json!([]),
            ));
        }
    };
    let binding_id = open
        .get("bindingId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let prompt = matrix_scenario_prompt(&body);
    // CRA-08 response-loss proof state is collected before vendor termination
    // and consumed by the post-kill assertions below.
    let mut response_request_accepted = false;
    let mut response_call_id: Option<String> = None;
    let mut response_pre_kill_terminal = false;
    let mut response_kill_succeeded = false;
    let turn = if body.scenario == "response_loss" {
        // Submit request, then terminate vendor process before response.
        match state.cli_executor.as_ref() {
            Some(executor) => match executor.handle_for(&agent_id) {
                Ok(handle) => {
                    let running_executor = executor.clone();
                    let running_handle = handle.clone();
                    let task = tokio::spawn(async move {
                        running_executor
                            .run_turn(
                                &running_handle,
                                rightkit_agents::driver_host::cli_runtime::AgentTurnRequest {
                                    agent_kind: running_handle.backend.agent_kind(),
                                    prompt,
                                    continuation_token: None,
                                },
                            )
                            .await
                    });
                    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
                    while tokio::time::Instant::now() < deadline {
                        if let Ok(trace) = executor.trace_for(&binding_id) {
                            response_pre_kill_terminal = trace.iter().any(|event| {
                                matches!(
                                    event.kind,
                                    NormalizedEventKind::TurnCompleted
                                        | NormalizedEventKind::TurnCompletedWithUsage { .. }
                                        | NormalizedEventKind::TurnFailed { .. }
                                        | NormalizedEventKind::TurnInterrupted
                                )
                            });
                            response_call_id = trace.iter().find_map(|event| match &event.kind {
                                NormalizedEventKind::ToolResult {
                                    call_id,
                                    is_error: false,
                                    ..
                                } => Some(call_id.clone()),
                                _ => None,
                            });
                            response_request_accepted =
                                response_call_id.is_some() && !response_pre_kill_terminal;
                        }
                        if response_request_accepted {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                    response_kill_succeeded = executor.shutdown_agent(&agent_id).await.is_ok();
                    let mut task = task;
                    let outcome = tokio::time::timeout(Duration::from_secs(5), &mut task).await;
                    if outcome.is_err() {
                        task.abort();
                        let _ = task.await;
                    }
                    Err(StatusCode::GATEWAY_TIMEOUT)
                }
                Err(_) => Err(StatusCode::BAD_GATEWAY),
            },
            None => Err(StatusCode::SERVICE_UNAVAILABLE),
        }
    } else if body.scenario == "interrupt" {
        match state.cli_executor.as_ref() {
            Some(executor) => match executor.handle_for(&agent_id) {
                Ok(handle) => {
                    let running_executor = executor.clone();
                    let running_handle = handle.clone();
                    let task = tokio::spawn(async move {
                        running_executor
                            .run_turn(
                                &running_handle,
                                rightkit_agents::driver_host::cli_runtime::AgentTurnRequest {
                                    agent_kind: running_handle.backend.agent_kind(),
                                    prompt,
                                    continuation_token: None,
                                },
                            )
                            .await
                    });
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    let interrupt = executor.interrupt(&handle).await;
                    match tokio::time::timeout(Duration::from_secs(15), task).await {
                        Ok(Ok(Ok(result))) if interrupt.is_ok() => Ok(Json(json!({
                            "status": result.status,
                            "finalText": result.final_text,
                        }))),
                        _ => Err(StatusCode::BAD_GATEWAY),
                    }
                }
                Err(_) => Err(StatusCode::CONFLICT),
            },
            None => Err(StatusCode::SERVICE_UNAVAILABLE),
        }
    } else {
        prompt_agent_runtime(
            State(state.clone()),
            Path((session_id.clone(), agent_id.clone())),
            HeaderMap::new(),
            Json(DriverPromptRequest {
                binding_id: String::new(),
                vendor_session_id: String::new(),
                text: prompt,
                request_label: Some(format!("matrix:{}", body.scenario)),
            }),
        )
        .await
    };
    let mut events = state
        .cli_executor
        .as_ref()
        .and_then(|executor| executor.trace_for(&binding_id).ok())
        .unwrap_or_default();
    let mut control_events = Vec::new();
    let mut continuity_proved = false;
    let mut binary_invalidation_proved = false;
    let mut exact_once_proved = false;
    let mut login_expiry_proved = false;
    let mut fault_proved = false;
    let mut fault_disposition: Option<String> = None;
    if matches!(body.scenario.as_str(), "process_kill" | "app_restart") {
        if let Some(executor) = state.cli_executor.as_ref() {
            if let Ok(before) = executor.handle_for(&agent_id) {
                let before_id = before.vendor_session_id.clone();
                let restarted =
                    if body.scenario == "app_restart" {
                        let vendor_session_id = before_id.clone().unwrap_or_default();
                        match executor.shutdown_agent(&agent_id).await {
                            Ok(()) => executor
                                .recover_agent(
                                    rightkit_agents::driver_host::cli_runtime::OpenAgentRequest {
                                        session_id: session_id.clone(),
                                        agent_id: agent_id.clone(),
                                        role: role.to_string(),
                                        parent_agent_id: None,
                                        backend: before.backend.clone(),
                                        tool_ceiling: effective_ceiling(&live, &[])
                                            .unwrap_or_default(),
                                        worktree_id: Some(body.workspace.clone()),
                                    },
                                    &vendor_session_id,
                                )
                                .await,
                            Err(error) => Err(error),
                        }
                    } else {
                        executor.restart_agent(&before).await
                    };
                if let Ok(restarted) = restarted {
                    if let Some(storage) = state.storage.as_ref() {
                        let persisted = {
                            let store = storage.lock().await;
                            store.get_driver_binding(&binding_id)
                        };
                        if let Ok(Some(mut binding)) = persisted {
                            binding.process_generation =
                                binding.process_generation.saturating_add(1);
                            binding.status = "resuming".to_string();
                            binding.updated_at = unix_key(0);
                            if storage.lock().await.upsert_driver_binding(&binding).is_ok() {
                                state
                                    .cli_recovery_live_generations
                                    .set(&binding_id, binding.process_generation);
                            }
                        }
                    }
                    let follow_up = prompt_agent_runtime(
                        State(state.clone()),
                        Path((session_id.clone(), agent_id.clone())),
                        HeaderMap::new(),
                        Json(DriverPromptRequest {
                            binding_id: String::new(),
                            vendor_session_id: String::new(),
                            text: "Continue the same native provider session with one read-only RightKit MCP action.".to_string(),
                            request_label: Some(format!("matrix:{}:resume", body.scenario)),
                        }),
                    )
                    .await;
                    let after = executor.handle_for(&agent_id).ok();
                    continuity_proved = follow_up.is_ok()
                        && before_id.is_some()
                        && before_id
                            == after
                                .as_ref()
                                .and_then(|value| value.vendor_session_id.clone())
                        && restarted.vendor_session_id == before_id;
                    if let Ok(second_trace) = executor.trace_for(&binding_id) {
                        events.extend(second_trace);
                    }
                    if continuity_proved {
                        control_events.push(json!({
                            "kind": "continuity_proved",
                            "vendorSessionId": before_id,
                            "mode": body.scenario,
                        }));
                    }
                }
            }
        }
    } else if body.scenario == "binary_update" {
        if let (Some(executor), Some(storage)) =
            (state.cli_executor.as_ref(), state.storage.as_ref())
        {
            if let Ok(handle) = executor.handle_for(&agent_id) {
                let original = storage
                    .lock()
                    .await
                    .get_driver_binding(&binding_id)
                    .ok()
                    .flatten();
                if let Some(mut drifted) = original.clone() {
                    drifted.executable_sha256 = Some("0".repeat(64));
                    if storage.lock().await.upsert_driver_binding(&drifted).is_ok() {
                        binary_invalidation_proved = executor.restart_agent(&handle).await.is_err();
                    }
                    if let Some(original) = original {
                        let _ = storage.lock().await.upsert_driver_binding(&original);
                    }
                    if binary_invalidation_proved {
                        control_events.push(json!({
                            "kind": "binary_update_invalidated",
                            "bindingId": binding_id,
                        }));
                    }
                }
            }
        }
    } else if body.scenario == "exact_once" {
        if let Some((capability, instance)) = state
            .cli_launch_contexts
            .as_ref()
            .and_then(|contexts| contexts.gateway_credentials(&binding_id))
        {
            let mut headers = HeaderMap::new();
            if let (Ok(capability), Ok(instance)) = (
                capability.parse::<axum::http::HeaderValue>(),
                instance.parse::<axum::http::HeaderValue>(),
            ) {
                headers.insert(CAPABILITY_HEADER, capability);
                headers.insert(DRIVER_INSTANCE_HEADER, instance);
                let request = CapabilityCallRequest {
                    call_id: json!("matrix-exact-once"),
                    name: "memory_recall".to_string(),
                    arguments: json!({"query": "matrix exact-once probe", "limit": 1}),
                };
                let first = call_capability(
                    State(state.clone()),
                    Path(session_id.clone()),
                    headers.clone(),
                    Json(request),
                )
                .await;
                let second = call_capability(
                    State(state.clone()),
                    Path(session_id.clone()),
                    headers,
                    Json(CapabilityCallRequest {
                        call_id: json!("matrix-exact-once"),
                        name: "memory_recall".to_string(),
                        arguments: json!({"query": "matrix exact-once probe", "limit": 1}),
                    }),
                )
                .await;
                exact_once_proved =
                    matches!((&first, &second), (Ok(Json(a)), Ok(Json(b))) if a == b);
                if exact_once_proved {
                    control_events.push(json!({
                        "kind": "exact_once_proved",
                        "callId": "matrix-exact-once",
                    }));
                }
            }
        }
    } else if body.scenario == "login_expiry" {
        if let (Some(executor), Some(storage)) =
            (state.cli_executor.as_ref(), state.storage.as_ref())
        {
            let profile_id = format!("{}/default", body.provider);
            let original = {
                let store = storage.lock().await;
                store.get_cli_account_profile(&profile_id).ok().flatten()
            };
            if let (Some(mut expired), Ok(handle)) =
                (original.clone(), executor.handle_for(&agent_id))
            {
                expired.signed_in = false;
                expired.updated_at = unix_key(0);
                if storage
                    .lock()
                    .await
                    .upsert_cli_account_profile(&expired)
                    .is_ok()
                {
                    login_expiry_proved = executor.restart_agent(&handle).await.is_err();
                }
                if let Some(original) = original {
                    let _ = storage.lock().await.upsert_cli_account_profile(&original);
                }
                if login_expiry_proved {
                    control_events.extend([
                        json!({"kind": "login_required", "providerId": body.provider}),
                        json!({"kind": "turn_failed", "reason": "native_login_expired"}),
                    ]);
                }
            }
        }
    } else if matches!(
        body.scenario.as_str(),
        "revoked_authority" | "wrong_scope" | "ambiguous_evidence" | "stale_evidence"
    ) {
        // Drive gateway admission with the real lease credentials created by
        // open_agent_runtime. The probe must observe a typed refusal and prove
        // that no capability effect was admitted.
        if let (Some((capability, instance)), Some(storage)) = (
            state
                .cli_launch_contexts
                .as_ref()
                .and_then(|contexts| contexts.gateway_credentials(&binding_id)),
            state.storage.as_ref(),
        ) {
            let lease = storage
                .lock()
                .await
                .get_latest_session_capability_lease_for_binding(&binding_id)
                .ok()
                .flatten();
            let before = lease.as_ref().map(|row| row.revoked_at.clone());
            let mut headers = HeaderMap::new();
            if let (Ok(capability), Ok(instance)) = (
                capability.parse::<axum::http::HeaderValue>(),
                instance.parse::<axum::http::HeaderValue>(),
            ) {
                headers.insert(CAPABILITY_HEADER, capability);
                headers.insert(DRIVER_INSTANCE_HEADER, instance);
                if body.scenario == "revoked_authority" {
                    if let Some(lease) = lease.as_ref() {
                        let _ = storage
                            .lock()
                            .await
                            .revoke_session_capability_lease(&lease.id, &unix_key(0));
                    }
                    let refused = call_capability(
                        State(state.clone()),
                        Path(session_id.clone()),
                        headers,
                        Json(CapabilityCallRequest {
                            call_id: json!("cra08-revoked-authority"),
                            name: "memory_recall".into(),
                            arguments: json!({"query":"CRA08 revoked authority"}),
                        }),
                    )
                    .await
                    .is_err();
                    fault_proved =
                        refused && before.as_ref().and_then(|value| value.as_ref()).is_none();
                    fault_disposition = Some("authority_revoked".into());
                } else if body.scenario == "wrong_scope" {
                    let refused = call_capability(
                        State(state.clone()),
                        Path(format!("{session_id}-wrong-scope")),
                        headers,
                        Json(CapabilityCallRequest {
                            call_id: json!("cra08-wrong-scope"),
                            name: "memory_recall".into(),
                            arguments: json!({"query":"CRA08 wrong scope"}),
                        }),
                    )
                    .await
                    .is_err();
                    fault_proved = refused;
                    fault_disposition = Some("scope_mismatch".into());
                } else if body.scenario == "ambiguous_evidence" {
                    let original = storage
                        .lock()
                        .await
                        .get_driver_binding(&binding_id)
                        .ok()
                        .flatten();
                    let ambiguous = original.clone().map(|mut binding| {
                        binding.profile_hash = Some("conflicting-profile-evidence".into());
                        binding
                    });
                    let written = if let Some(binding) = ambiguous.as_ref() {
                        storage.lock().await.upsert_driver_binding(binding).is_ok()
                    } else {
                        false
                    };
                    let refused = if written {
                        if let Some(executor) = state.cli_executor.as_ref() {
                            if let Ok(handle) = executor.handle_for(&agent_id) {
                                // The conflicting profile hash is rejected
                                // by the existing restart identity check.
                                executor.restart_agent(&handle).await.is_err()
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if let Some(original) = original {
                        let _ = storage.lock().await.upsert_driver_binding(&original);
                    }
                    fault_proved = refused;
                    fault_disposition = Some("evidence_conflict".into());
                } else {
                    let stale_rejected = if let Some(executor) = state.cli_executor.as_ref() {
                        if let Ok(handle) = executor.handle_for(&agent_id) {
                            let original = storage
                                .lock()
                                .await
                                .get_driver_binding(&binding_id)
                                .ok()
                                .flatten();
                            if let Some(mut stale) = original.clone() {
                                stale.executable_sha256 = Some("0".repeat(64));
                                let written =
                                    storage.lock().await.upsert_driver_binding(&stale).is_ok();
                                let rejected = if written {
                                    executor.restart_agent(&handle).await.err().is_some_and(
                                        |error| {
                                            let text = error.to_string().to_ascii_lowercase();
                                            text.contains("binary")
                                                || text.contains("executable")
                                                || text.contains("digest")
                                        },
                                    )
                                } else {
                                    false
                                };
                                if let Some(original) = original {
                                    let _ = storage.lock().await.upsert_driver_binding(&original);
                                }
                                rejected
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    fault_proved = stale_rejected;
                    fault_disposition = Some("evidence_stale".into());
                }
            }
        }
    } else if body.scenario == "response_loss" {
        // A transport that closes before terminal output is surfaced by the
        // executor as an error; a successful turn cannot be accepted as loss.
        fault_proved = turn.is_err();
        fault_disposition = Some("response_lost".into());
    } else if body.scenario == "resource_exhaustion" {
        // Saturate the single process-backed capacity slot through the
        // composition-owned governor, reject the next allocation, then prove
        // release permits recovery. This is a typed admission error path.
        if let Some(executor) = state.cli_executor.as_ref() {
            let _ = executor.shutdown_agent(&agent_id).await;
        }
        if let Ok(governor) =
            rightkit_agent_tools::coding_tools::process_resource_authority::require_process_resource_governor()
        {
            let scope =
                rightkit_agent_engine_runtime_resources::ServiceScope::Workspace(body.workspace.clone());
            let mut permits = Vec::new();
            let mut rejected = false;
            for _ in 0..governor
                .limits()
                .daemon_background
                .max(governor.limits().daemon_children)
                .saturating_add(2)
            {
                match rightkit_agent_tools::coding_tools::process_resource_authority::reserve_with_governor(
                    &governor,
                    rightkit_agent_tools::coding_tools::process_resource_authority::ResourceKind::BackgroundProcess,
                    scope.clone(),
                    true,
                ) {
                    Ok(permit) => permits.push(permit),
                    Err(error) => {
                        rejected = error.kind == rightkit_agent_tools::coding_tools::process_resource_authority::ResourceKind::ExternalExecution
                            || error.kind == rightkit_agent_tools::coding_tools::process_resource_authority::ResourceKind::BackgroundProcess;
                        break;
                    }
                }
            }
            let filled_capacity = !permits.is_empty() && rejected;
            drop(permits);
            let recovered = rightkit_agent_tools::coding_tools::process_resource_authority::reserve_with_governor(
                &governor,
                rightkit_agent_tools::coding_tools::process_resource_authority::ResourceKind::BackgroundProcess,
                scope,
                true,
            )
            .is_ok();
            fault_proved = filled_capacity && recovered;
        }
        fault_disposition = Some("resource_limit_exhausted".into());
    }
    if fault_disposition.is_none() {
        fault_disposition = match body.scenario.as_str() {
            "binary_update" if binary_invalidation_proved => Some("evidence_stale".into()),
            "process_kill" if continuity_proved => Some("crash_recovered".into()),
            "app_restart" if continuity_proved => Some("state_recovered".into()),
            "interrupt"
                if events
                    .iter()
                    .any(|event| matches!(event.kind, NormalizedEventKind::TurnInterrupted)) =>
            {
                Some("cancelled".into())
            }
            _ => None,
        };
    }
    let effects = match state.storage.as_ref() {
        Some(storage) => storage
            .lock()
            .await
            .list_driver_events(&binding_id)
            .unwrap_or_default(),
        None => Vec::new(),
    };
    if body.scenario == "response_loss" {
        let accepted_effect = response_call_id.as_ref().is_some_and(|call_id| {
            effects
                .iter()
                .any(|effect| effect.status == "processed" && effect.payload_json.contains(call_id))
        });
        let no_completion = !events.iter().any(|event| {
            matches!(
                event.kind,
                NormalizedEventKind::TurnCompleted
                    | NormalizedEventKind::TurnCompletedWithUsage { .. }
            )
        });
        fault_proved = response_request_accepted
            && response_kill_succeeded
            && !response_pre_kill_terminal
            && turn.is_err()
            && accepted_effect
            && no_completion;
    }
    let current = state
        .cli_executor
        .as_ref()
        .and_then(|executor| executor.handle_for(&agent_id).ok());
    let vendor_session_id = current
        .as_ref()
        .and_then(|handle| handle.vendor_session_id.clone())
        .or_else(|| {
            open.get("vendorSessionId")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    let (child_agent_id, child_vendor_session_id) = matrix_child_identity(&effects);
    let passed = (body.scenario == "response_loss" || turn.is_ok())
        && match body.scenario.as_str() {
            "process_kill" | "app_restart" => continuity_proved,
            "binary_update" => binary_invalidation_proved,
            "exact_once" => exact_once_proved,
            "login_expiry" => login_expiry_proved,
            "revoked_authority" | "wrong_scope" | "ambiguous_evidence" | "stale_evidence" => {
                fault_proved
            }
            "response_loss" | "resource_exhaustion" => fault_proved,
            "cross_provider" => {
                matrix_scenario_evidence(&body.scenario, &events, &effects)
                    && child_agent_id.is_some()
                    && child_vendor_session_id.is_some()
                    && child_vendor_session_id != vendor_session_id
            }
            _ => matrix_scenario_evidence(&body.scenario, &events, &effects),
        };
    let stop_status = stop_agent_runtime(
        State(state.clone()),
        Path((session_id.clone(), agent_id.clone())),
        HeaderMap::new(),
        Json(DriverBindingRequest {
            binding_id: String::new(),
            vendor_session_id: None,
        }),
    )
    .await
    .err();
    if let Some(status) = stop_status {
        tracing::warn!(
            session_id = %session_id,
            agent_id = %agent_id,
            status = status.as_u16(),
            "driver matrix: agent runtime stop failed; the driver process may still be running"
        );
    }
    lock_recover(&state.sessions, "sessions").remove(&session_id);
    Json(json!({
        "provider": body.provider,
        "childProvider": body.child_provider,
        "scenario": body.scenario,
        "passed": passed,
        "stopped": stop_status.is_none(),
        "stopError": stop_status.map(|status| format!("agent runtime stop failed with HTTP {}", status.as_u16())),
        "bindingId": binding_id,
        "vendorSessionId": vendor_session_id,
        "parentVendorSessionId": vendor_session_id,
        "childAgentId": child_agent_id,
        "childVendorSessionId": child_vendor_session_id,
        "events": events.into_iter().map(|event| serde_json::to_value(event).unwrap_or_else(|_| json!({"kind":"serialization_failed"}))).chain(control_events).collect::<Vec<_>>(),
        "effects": effects.iter().map(|effect| json!({
            "id": effect.event_key,
            "kind": effect.event_kind,
            "status": effect.status,
            "result": effect.result_json.as_deref().and_then(|value| serde_json::from_str::<Value>(value).ok()),
        })).collect::<Vec<_>>(),
        "diagnostic": if passed { Value::Null } else { json!("required live scenario evidence was not observed") },
        "faultDisposition": fault_disposition,
    }))
}

fn matrix_child_identity(
    effects: &[rightkit_agent_engine_storage::DriverEventInboxRow],
) -> (Option<String>, Option<String>) {
    effects
        .iter()
        .rev()
        .filter(|effect| {
            effect.event_kind == "tool.call.delegate_task" && effect.status == "processed"
        })
        .filter_map(|effect| effect.result_json.as_deref())
        .filter_map(|raw| serde_json::from_str::<Value>(raw).ok())
        .filter_map(|result| {
            result
                .pointer("/content/0/text")
                .and_then(Value::as_str)
                .and_then(|text| serde_json::from_str::<Value>(text).ok())
        })
        .find_map(|child| {
            let agent_id = child.get("agentId")?.as_str()?.to_string();
            let vendor_session_id = child.get("vendorSessionId")?.as_str()?.to_string();
            Some((Some(agent_id), Some(vendor_session_id)))
        })
        .unwrap_or((None, None))
}

fn matrix_scenario_prompt(body: &CliRuntimeMatrixCellRequest) -> String {
    match body.scenario.as_str() {
        "cross_provider" => format!(
            "Use RightKit delegate_task exactly once with providerHint={} for a bounded child task, wait for its structured result, then summarize it.",
            body.child_provider.as_deref().unwrap_or("none")
        ),
        "approval" => "Use RightKit MCP to request one governed write that requires approval, then report its tool result.".to_string(),
        "mutation" => "Use RightKit MCP to write a small matrix marker inside the bound workspace, then read it back.".to_string(),
        "exact_once" => "Issue the same RightKit MCP governed write twice with one stable call identity, then report both results.".to_string(),
        "interrupt" => "Begin a multi-step read-only RightKit MCP task and keep the turn active until interrupted.".to_string(),
        "process_kill" | "app_restart" => "Perform a read-only RightKit MCP turn, preserve native session identity, and report continuity after resume.".to_string(),
        "login_expiry" => "Perform one provider turn and surface native login expiry as a typed failure without falling back to API credentials.".to_string(),
        "stale_evidence" => "Attempt one turn after the qualified executable identity changes; refuse stale evidence and report the typed stale disposition without applying effects.".to_string(),
        "ambiguous_evidence" => "Present conflicting evidence for one stable capability identity; refuse the conflict without selecting a candidate or applying effects.".to_string(),
        "revoked_authority" => "Attempt one RightKit capability call after its lease is revoked; report typed authority revocation with no effect.".to_string(),
        "wrong_scope" => "Attempt one RightKit capability call through a different session scope; report typed scope mismatch with no effect.".to_string(),
        "response_loss" => "Run one bounded read-only turn whose response is unavailable; report typed loss without fabricating completion.".to_string(),
        "resource_exhaustion" => "Run one bounded turn at its resource ceiling; report typed resource exhaustion without a partial effect.".to_string(),
        "binary_update" => "Report current executable identity and refuse continuity if it differs from the qualified digest.".to_string(),
        "no_session_leakage" => "Use only the managed profile and RightKit MCP; report any unrelated environment variable as a failure.".to_string(),
        _ => "Complete one bounded read-only turn through RightKit MCP and return a terminal result.".to_string(),
    }
}

fn matrix_scenario_evidence(
    scenario: &str,
    events: &[NormalizedEvent],
    effects: &[rightkit_agent_engine_storage::DriverEventInboxRow],
) -> bool {
    let terminal = events.iter().any(|event| {
        matches!(
            event.kind,
            NormalizedEventKind::TurnCompleted
                | NormalizedEventKind::TurnInterrupted
                | NormalizedEventKind::TurnFailed { .. }
        )
    });
    let processed = effects.iter().any(|effect| effect.status == "processed");
    match scenario {
        "root" | "child" => terminal,
        "cross_provider" => terminal
            && effects.iter().any(|effect| {
                effect.event_kind == "tool.call.delegate_task" && effect.status == "processed"
            }),
        "approval" => {
            terminal
                && processed
                && events.iter().any(|event| {
                    matches!(event.kind, NormalizedEventKind::ApprovalRequested { .. })
                })
        }
        "mutation" | "exact_once" => terminal && processed,
        "interrupt" => events
            .iter()
            .any(|event| matches!(event.kind, NormalizedEventKind::TurnInterrupted)),
        "login_expiry" => events.iter().any(|event| {
            matches!(event.kind, NormalizedEventKind::TurnFailed { .. })
        }),
        "stale_evidence" | "ambiguous_evidence" | "revoked_authority" | "wrong_scope"
        | "response_loss" | "resource_exhaustion" => false,
        "no_session_leakage" => terminal && !events.iter().any(|event| {
            matches!(&event.kind, NormalizedEventKind::TurnFailed { message } if message.to_ascii_lowercase().contains("environment"))
        }),
        "process_kill" | "app_restart" | "binary_update" => false,
        _ => terminal,
    }
}

fn matrix_cell_receipt(
    body: &CliRuntimeMatrixCellRequest,
    passed: bool,
    diagnostic: &str,
    events: Value,
) -> Value {
    json!({
        "provider": body.provider,
        "childProvider": body.child_provider,
        "scenario": body.scenario,
        "passed": passed,
        "bindingId": Value::Null,
        "vendorSessionId": Value::Null,
        "parentVendorSessionId": Value::Null,
        "childVendorSessionId": Value::Null,
        "events": events,
        "effects": [],
        "diagnostic": diagnostic,
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn provider_probe_that_never_completes_yields_timed_out_entry_within_cap() {
        let cap = std::time::Duration::from_millis(100);
        let started = std::time::Instant::now();
        let entries = probe_providers_with_cap(
            vec!["fast".to_string(), "hung".to_string(), "absent".to_string()],
            cap,
            |id| async move {
                match id.as_str() {
                    "fast" => Some(json!({"providerId": "fast", "installed": true})),
                    "hung" => std::future::pending::<Option<Value>>().await,
                    _ => None,
                }
            },
            |id| Some(json!({"providerId": id, "unavailable": true, "timedOut": true})),
        )
        .await;
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(entries.len(), 2, "absent provider is skipped: {entries:?}");
        assert_eq!(entries[0]["installed"], true);
        assert_eq!(entries[1]["providerId"], "hung");
        assert_eq!(entries[1]["timedOut"], true);
    }

    #[tokio::test]
    async fn can_use_tool_request_routes_through_approval_and_denies_without_a_live_session() {
        let state = crate::api::DaemonState::new("test-token");
        let params = json!({
            "tool_name": "mcp__coderight__write_file",
            "input": {"path": "a.txt"},
            "tool_use_id": "toolu_1",
        });
        // The approval function is what the event consumer calls; with no live session (the
        // same outcome as a stop or timeout) it must deny, never allow.
        let answer = crate::api_sessions::approvals::answer_claude_can_use_tool(
            &state,
            "missing-session",
            &params,
        )
        .await;
        assert_eq!(answer["behavior"], "deny");
        // The wrapper then hands that answer to the driver host; with no running process the
        // write is refused and logged, and the call returns instead of hanging.
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            super::driver_lifecycle::answer_can_use_tool_request(
                &state,
                "no-binding",
                "missing-session",
                json!("req-1"),
                &params,
            ),
        )
        .await
        .expect("can_use_tool wrapper must not hang");
    }

    fn route_binding(
        profile_hash: &str,
        model_id: Option<&str>,
    ) -> rightkit_agent_engine_storage::DriverBindingRow {
        rightkit_agent_engine_storage::DriverBindingRow {
            id: "binding-route".into(),
            driver_kind: "codex".into(),
            session_id: "session".into(),
            vendor_session_id: None,
            vendor_thread_id: None,
            vendor_turn_id: None,
            binary_version: "1".into(),
            schema_hash: "schema".into(),
            status: "connected".into(),
            last_event_key: None,
            last_event_cursor: None,
            coderight_agent_id: Some("agent".into()),
            parent_agent_id: String::new(),
            provider_id: Some("codex".into()),
            transport_kind: Some("app_server".into()),
            account_profile_id: Some("codex/default".into()),
            auth_mode: Some("native_cli_account".into()),
            account_fingerprint: None,
            model_id: model_id.map(str::to_string),
            executable_path: None,
            executable_sha256: None,
            protocol_version: None,
            profile_hash: Some(profile_hash.into()),
            policy_hash: None,
            tool_schema_hash: None,
            harness_attestation_id: None,
            isolation_mode: None,
            resume_strategy: None,
            context_epoch: 0,
            process_generation: 1,
            created_at: "0".into(),
            updated_at: "0".into(),
        }
    }

    fn resolved_route(profile_hash: &str, model_id: Option<&str>) -> ResolvedCliLaunchRoute {
        ResolvedCliLaunchRoute {
            launch_profile: default_cli_profile(
                rightkit_agents::driver_host::cli_runtime::CliProviderId::new("codex"),
            ),
            native_profile_hash: "native".into(),
            effective_profile_hash: profile_hash.into(),
            model_id: model_id.map(str::to_string),
        }
    }

    #[test]
    fn root_open_native_requires_profile_metadata() {
        let provider = rightkit_agents::driver_host::cli_runtime::CliProviderId::new("codex");
        assert!(missing_default_profile_is_allowed(
            CliRouteMode::Native,
            &provider,
            "codex/default",
        ));
    }

    #[test]
    fn native_missing_profile_is_rejected() {
        let provider = rightkit_agents::driver_host::cli_runtime::CliProviderId::new("claude");
        assert!(!missing_default_profile_is_allowed(
            CliRouteMode::Native,
            &provider,
            "claude/default",
        ));
        assert!(matches!(
            parse_cli_route_mode("native"),
            Ok(CliRouteMode::Native)
        ));
    }

    #[test]
    fn recovery_refuses_route_or_model_tuple_drift_before_lease_rotation() {
        let binding = route_binding("effective-a", Some("model-a"));
        assert!(binding_route_tuple_matches(
            &binding,
            &resolved_route("effective-a", Some("model-a")),
        ));
        assert!(!binding_route_tuple_matches(
            &binding,
            &resolved_route("effective-b", Some("model-a")),
        ));
        assert!(!binding_route_tuple_matches(
            &binding,
            &resolved_route("effective-a", Some("model-b")),
        ));
    }

    #[test]
    fn schema_drift_is_detected_for_protocol_derived_kinds() {
        let current = protocol_schema_hash("claude", "2.1.220");
        assert_eq!(
            binding_schema_drifted("claude", "2.1.220", &current),
            Some(false),
            "a binding recorded under this build must not read as drifted"
        );
        assert_eq!(
            binding_schema_drifted("claude", "2.1.220", "sha256:from-an-older-protocol"),
            Some(true),
            "a binding recorded under a protocol this build no longer emits must be flagged"
        );
    }

    #[test]
    fn schema_drift_is_unknown_rather_than_false_for_probe_derived_kinds() {
        // Codex records a hash probed from the binary, so the protocol-derived
        // hash never matches it. Reporting `false` here would claim a healthy
        // schema we never checked; reporting `true` would flag every healthy
        // codex binding. Unknown is the only honest answer.
        assert_eq!(
            binding_schema_drifted("codex", "codex-cli 1.0.0", "sha256:probed-from-binary"),
            None
        );
    }

    #[test]
    fn token_hash_is_stable_and_does_not_contain_plaintext() {
        let token = "lease.nonce.secretsecretsecretsecretsecretsecretsecretsecretsecretsecret";
        let hash = token_hash(token);
        assert_eq!(hash.len(), 64);
        assert!(!hash.contains("secret"));
        assert_eq!(hash, token_hash(token));
    }

    #[test]
    fn driver_mcp_credentials_exclude_daemon_master_token() {
        assert!(!DRIVER_MCP_SECRET_ENVIRONMENT.contains(&"RIGHTKIT_DAEMON_TOKEN"));
        assert!(!DRIVER_MCP_FORWARDED_ENVIRONMENT.contains(&"RIGHTKIT_DAEMON_TOKEN"));
        assert_eq!(
            DRIVER_MCP_SECRET_ENVIRONMENT,
            ["RIGHTKIT_GATEWAY_TOKEN", "RIGHTKIT_DRIVER_INSTANCE_SECRET",]
        );
    }

    #[test]
    fn flat_delegate_hints_build_authenticated_cli_runtime() {
        let options = build_delegate_spawn_options(
            &json!({
                "role": "researcher",
                "task": "compare providers",
                "providerHint": "codex",
                "modelHint": "gpt-5"
            }),
            "delegate_task",
        )
        .unwrap();
        assert_eq!(
            options.runtime,
            Some(SpawnRuntimeKind::AuthenticatedCli {
                provider_id: "codex".into(),
                model_id: Some("gpt-5".into()),
                account_profile_id: "codex/default".into(),
            })
        );
    }

    #[test]
    fn delegation_schemas_advertise_flat_provider_and_model_hints() {
        let registry = ToolRegistry::build_default();
        for name in ["delegate_task", "delegate_parallel"] {
            let schema = capability_schema(&registry, name).unwrap();
            let item = if name == "delegate_task" {
                &schema["inputSchema"]
            } else {
                &schema["inputSchema"]["properties"]["tasks"]["items"]
            };
            assert_eq!(item["properties"]["providerHint"]["type"], "string");
            assert_eq!(item["properties"]["modelHint"]["type"], "string");
        }
    }

    #[test]
    fn unix_keys_preserve_expiry_order() {
        assert!(unix_key(60) > unix_key(0));
    }

    #[test]
    fn codex_auth_status_requires_an_affirmative_logged_in_state() {
        assert!(codex_login_status_is_authenticated(
            true,
            "Logged in using ChatGPT",
            ""
        ));
        assert!(codex_login_status_is_authenticated(
            true,
            "",
            "Authenticated through an API key"
        ));
        assert!(!codex_login_status_is_authenticated(
            true,
            "Not logged in",
            ""
        ));
        assert!(!codex_login_status_is_authenticated(
            false,
            "Logged in using ChatGPT",
            ""
        ));
    }

    #[test]
    fn schema_hash_ignores_json_object_key_order() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(
            first.path().join("schema.json"),
            r#"{"b":2,"a":{"y":1,"x":0}}"#,
        )
        .unwrap();
        std::fs::write(
            second.path().join("schema.json"),
            r#"{"a":{"x":0,"y":1},"b":2}"#,
        )
        .unwrap();
        assert_eq!(
            hash_schema_directory(first.path()).unwrap(),
            hash_schema_directory(second.path()).unwrap()
        );
    }

    #[test]
    fn driver_contract_schemas_expose_lifecycle_auth_and_discovery() {
        let discovery = driver_discovery_response_schema();
        assert_eq!(
            discovery["properties"]["driverKind"]["enum"],
            json!(["codex", "claude"])
        );
        assert_eq!(
            discovery["properties"]["capabilities"]["properties"]["requiresLease"]["const"],
            true
        );
        let status = driver_status_response_schema();
        assert_eq!(
            status["required"],
            json!([
                "sessionId",
                "driverKind",
                "authRequired",
                "lifecycle",
                "drivers"
            ])
        );
        assert_eq!(
            status["properties"]["lifecycle"]["enum"],
            json!(["native", "ready", "authentication_required", "not_resumed"])
        );
        assert_eq!(driver_lifecycle("unknown"), "failed");
        assert_eq!(driver_lifecycle("recovery_required"), "recovery_required");
        assert!(driver_diagnostic("recovery_required").is_some());
    }

    /// Builds a `DaemonState` backed by an in-memory storage instance
    /// containing the given session rows, none of which are resumed into the
    /// in-memory session map. Mirrors `LiveSession` restored from Recents:
    /// present in storage, absent from `state.sessions`.
    fn state_with_storage_only_sessions(
        rows: &[rightkit_agent_engine_storage::SessionRow],
    ) -> DaemonState {
        let store = rightkit_agent_engine_storage::Storage::open_in_memory().unwrap();
        for row in rows {
            store.create_session(row).unwrap();
        }
        let storage: crate::api::SharedStorage =
            std::sync::Arc::new(tokio::sync::Mutex::new(store)).into();
        DaemonState::new("").with_storage(storage)
    }

    fn dormant_session_row(
        id: &str,
        driver_kind_json: &str,
    ) -> rightkit_agent_engine_storage::SessionRow {
        rightkit_agent_engine_storage::SessionRow {
            id: id.to_string(),
            title: "Restored from Recents".to_string(),
            workspace_root: "/tmp/dormant-workspace".to_string(),
            created_at: "2026-08-01T00:00:00Z".to_string(),
            updated_at: "2026-08-01T00:00:00Z".to_string(),
            parent_session_id: None,
            role: None,
            status: "dormant".to_string(),
            config_json: json!({ "driverKind": driver_kind_json }).to_string(),
            tags: String::new(),
            recovery_marker: None,
        }
    }

    #[tokio::test]
    async fn driver_status_reports_calm_state_for_storage_only_session_instead_of_404() {
        let state =
            state_with_storage_only_sessions(&[dormant_session_row("dormant-session", "codex")]);

        let response = driver_status(State(state), Path("dormant-session".to_string()))
            .await
            .expect("a session that exists in storage must not 404 from a status poll");
        let body = response.0;

        assert_eq!(body["sessionId"], json!("dormant-session"));
        assert_eq!(body["driverKind"], json!("codex"));
        assert_eq!(body["authRequired"], json!(false));
        assert_eq!(body["lifecycle"], json!("not_resumed"));
        assert_eq!(body["drivers"], json!([]));
    }

    #[tokio::test]
    async fn driver_status_404s_only_when_unknown_to_memory_and_storage() {
        let state = state_with_storage_only_sessions(&[]);

        let error = driver_status(State(state), Path("never-existed".to_string()))
            .await
            .expect_err("a session absent from both memory and storage is genuinely missing");

        assert_eq!(error, StatusCode::NOT_FOUND);
    }

    #[test]
    fn gateway_annotations_mark_only_memory_recall_read_only() {
        let registry = ToolRegistry::build_default();
        let memory = capability_schema(&registry, "memory_recall").unwrap();
        let verify = capability_schema(&registry, "verify").unwrap();
        let read_file = capability_schema(&registry, "read_file").unwrap();

        assert_eq!(memory["annotations"]["readOnlyHint"], json!(true));
        assert_eq!(memory["annotations"]["idempotentHint"], json!(true));
        for tool in [&verify, &read_file] {
            assert_eq!(tool["annotations"]["readOnlyHint"], json!(false));
            assert_eq!(tool["annotations"]["destructiveHint"], json!(false));
            assert_eq!(tool["annotations"]["idempotentHint"], json!(false));
            assert_eq!(tool["annotations"]["openWorldHint"], json!(false));
        }
    }

    #[test]
    fn safe_driver_ceiling_never_auto_adds_execution_specials() {
        let tools = effective_ceiling_for(true, &[], &[], &[]).unwrap();

        assert!(tools.iter().any(|tool| tool == "read_file"));
        assert!(tools.iter().any(|tool| tool == "memory_recall"));
        for forbidden in ["delegate_task", "delegate_parallel", "verify"] {
            assert!(!tools.iter().any(|tool| tool == forbidden));
        }
    }

    #[test]
    fn safe_driver_ceiling_intersects_allowed_tools_but_keeps_memory_recall() {
        let allowed = vec!["read_file".to_string()];
        let tools = effective_ceiling_for(true, &allowed, &[], &[]).unwrap();

        assert_eq!(tools, vec!["memory_recall", "read_file"]);
    }

    #[test]
    fn safe_driver_ceiling_honors_memory_recall_denial() {
        let denied = vec!["memory_recall".to_string()];
        let tools = effective_ceiling_for(true, &[], &denied, &[]).unwrap();

        assert!(!tools.iter().any(|tool| tool == "memory_recall"));
        assert!(!tools.iter().any(|tool| tool == "delegate_task"));
    }

    #[test]
    fn normal_driver_ceiling_preserves_special_tool_behavior() {
        let allowed = vec!["read_file".to_string()];
        let tools = effective_ceiling_for(false, &allowed, &[], &[]).unwrap();

        for special in SPECIAL_TOOLS {
            assert!(tools.iter().any(|tool| tool == special));
        }
        assert!(tools.iter().any(|tool| tool == "read_file"));
    }

    #[test]
    fn claude_stays_starting_after_init_until_coderight_gateway_is_observed() {
        let session_started = NormalizedEventKind::SessionStarted {
            vendor_session_id: "claude-session".into(),
            init: None,
        };
        let assistant_delta = NormalizedEventKind::AssistantDelta {
            text: "working".into(),
        };
        let gateway_tool = NormalizedEventKind::ToolCallStarted {
            call_id: "tool-1".into(),
            tool_name: "mcp__coderight__read_file".into(),
            arguments: json!({}),
        };

        assert_eq!(
            driver_status_after_event(DriverKind::Claude, "starting", &session_started),
            "starting"
        );
        assert_eq!(
            driver_status_after_event(DriverKind::Claude, "starting", &assistant_delta),
            "starting"
        );
        assert_eq!(
            driver_status_after_event(DriverKind::Claude, "starting", &gateway_tool),
            "ready"
        );
    }

    fn limit(status: &str) -> NormalizedEventKind {
        NormalizedEventKind::RateLimit {
            account: None,
            status: status.into(),
            rate_limit_type: Some("five_hour".into()),
            resets_at: Some(1_790_000_000),
            utilization: Some(0.5),
            overage_status: None,
            error_code: None,
        }
    }

    #[test]
    fn driver_rate_limit_state_recovers_after_a_failed_probe() {
        use super::driver_lifecycle::{
            driver_rate_limit_state, observe_driver_rate_limit,
            record_driver_rate_limit_probe_failure,
        };
        use rightkit_agents::driver_host::RateLimitState;
        let id = "rl-test-recover";
        assert_eq!(
            driver_rate_limit_state(id, "claude"),
            RateLimitState::NoData
        );
        assert!(!observe_driver_rate_limit(
            id,
            &NormalizedEventKind::TurnCompleted
        ));
        assert_eq!(
            driver_rate_limit_state(id, "claude"),
            RateLimitState::NoData
        );

        record_driver_rate_limit_probe_failure(id, "driver exited with code Some(1)");
        assert!(matches!(
            driver_rate_limit_state(id, "claude"),
            RateLimitState::ProbeFailed {
                last_known: None,
                ..
            }
        ));

        assert!(observe_driver_rate_limit(id, &limit("allowed")));
        assert!(matches!(
            driver_rate_limit_state(id, "claude"),
            RateLimitState::Known { .. }
        ));

        // A later failure keeps the last snapshot so the UI can show it as stale ...
        record_driver_rate_limit_probe_failure(id, "protocol error");
        match driver_rate_limit_state(id, "claude") {
            RateLimitState::ProbeFailed {
                last_known: Some(last),
                ..
            } => {
                assert_eq!(last.status, "allowed")
            }
            other => panic!("expected ProbeFailed with a snapshot, got {other:?}"),
        }
        // ... and the next event recovers it.
        assert!(observe_driver_rate_limit(id, &limit("rejected")));
        match driver_rate_limit_state(id, "claude") {
            RateLimitState::Known { snapshot } => assert_eq!(snapshot.status, "rejected"),
            other => panic!("expected Known, got {other:?}"),
        }
    }

    #[test]
    fn providers_without_usage_reporting_are_unsupported_not_no_data() {
        use super::driver_lifecycle::{
            driver_rate_limit_state, record_driver_rate_limit_probe_failure,
        };
        use rightkit_agents::driver_host::RateLimitState;
        let id = "rl-test-agy";
        assert!(matches!(
            driver_rate_limit_state(id, "antigravity"),
            RateLimitState::Unsupported { .. }
        ));
        // rightkit-agents: a probe failure replaces the stored Unsupported with ProbeFailed, but the
        // status view re-marks kinds that never report usage, so it still reads Unsupported.
        record_driver_rate_limit_probe_failure(id, "boom");
        assert!(matches!(
            driver_rate_limit_state(id, "antigravity"),
            RateLimitState::Unsupported { .. }
        ));
        let json = serde_json::to_value(driver_rate_limit_state(id, "antigravity")).unwrap();
        assert_eq!(json["state"], "unsupported");
        assert_eq!(
            serde_json::to_value(driver_rate_limit_state("rl-test-codex", "codex")).unwrap()
                ["state"],
            "noData"
        );
    }

    #[test]
    fn rate_limit_telemetry_keeps_status_and_projects_account_event() {
        let who =
            rightkit_agents::driver_host::ClaudeAccountIdentity::api_key("claude", "team", "Team");
        let capped = NormalizedEventKind::RateLimit {
            account: Some(who.clone()),
            status: "rejected".into(),
            rate_limit_type: Some("five_hour".into()),
            resets_at: Some(1_790_000_000),
            utilization: Some(1.0),
            overage_status: None,
            error_code: None,
        };
        assert_eq!(
            driver_status_after_event(DriverKind::Claude, "ready", &capped),
            "ready"
        );
        assert_eq!(
            driver_status_after_event(DriverKind::Claude, "starting", &capped),
            "starting"
        );
        match super::driver_lifecycle::account_protocol_event("s", 7, "t", &capped) {
            Some(rightkit_agent_engine_protocol::ProtocolEvent::AccountRateLimited {
                account,
                account_id,
                account_kind,
                email,
                status,
                resets_at,
                ..
            }) => {
                assert_eq!(account, "Team");
                assert_eq!(account_id.as_deref(), Some("api_key/claude/team"));
                assert_eq!(account_kind.as_deref(), Some("api_key"));
                assert_eq!(email, None);
                assert_eq!(status, "rejected");
                assert_eq!(resets_at, Some(1_790_000_000));
            }
            other => panic!("unexpected {other:?}"),
        }
        let cost = NormalizedEventKind::TurnCostReported {
            account: Some(who),
            total_cost_usd: Some(0.5),
            duration_ms: None,
            num_turns: Some(1),
            model_usage: vec![],
        };
        assert!(matches!(
            super::driver_lifecycle::account_protocol_event("s", 8, "t", &cost),
            Some(rightkit_agent_engine_protocol::ProtocolEvent::AccountUsageReported { .. })
        ));
        assert!(super::driver_lifecycle::account_protocol_event(
            "s",
            9,
            "t",
            &NormalizedEventKind::TurnCompleted
        )
        .is_none());
    }

    #[cfg(unix)]
    #[test]
    fn isolated_codex_home_contains_only_managed_config_and_auth_link() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let data = root.path().join("data");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(source.join("auth.json"), "{}").unwrap();

        let home = prepare_codex_home_at(&data, &source, "binding-1").unwrap();
        assert_eq!(
            std::fs::read_link(home.join("auth.json")).unwrap(),
            source.join("auth.json")
        );
        assert_eq!(
            std::fs::read_to_string(home.join("config.toml")).unwrap(),
            "# RightKit run-scoped Codex configuration.\n"
        );
        assert_eq!(std::fs::read_dir(home).unwrap().count(), 2);
    }

    #[test]
    fn codex_home_follows_the_selected_account_and_never_keeps_a_stale_login() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let work = root.path().join("profiles/work");
        let spare = root.path().join("profiles/spare");
        for dir in [&data, &work, &spare] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(work.join("auth.json"), "{\"a\":\"work\"}").unwrap();
        std::fs::write(spare.join("auth.json"), "{\"a\":\"spare\"}").unwrap();

        let home = prepare_codex_home_at(&data, &work, "binding-1").unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).unwrap(),
            "{\"a\":\"work\"}"
        );
        // Same binding, different account: the link follows the new account.
        let home = prepare_codex_home_at(&data, &spare, "binding-1").unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).unwrap(),
            "{\"a\":\"spare\"}"
        );
        // API-key account: no auth.json at all.
        let home = prepare_codex_home_inner(&data, None, "binding-1").unwrap();
        assert!(!home.join("auth.json").exists());
        // Missing profile login fails closed.
        std::fs::remove_file(work.join("auth.json")).unwrap();
        assert!(prepare_codex_home_at(&data, &work, "binding-2").is_err());
    }

    #[test]
    fn extract_visible_codex_models_maps_documented_fields_and_drops_hidden_entries() {
        let result = json!({
            "items": [
                {
                    "id": "o3",
                    "model": "o3",
                    "displayName": "O3",
                    "description": "Frontier reasoning model",
                    "isDefault": true,
                    "hidden": false,
                    "defaultReasoningEffort": "medium",
                    "supportedReasoningEfforts": ["low", "medium", "high"],
                },
                {
                    "id": "internal-eval-only",
                    "model": "internal-eval-only",
                    "displayName": "Internal Eval",
                    "description": "Not for general use",
                    "isDefault": false,
                    "hidden": true,
                    "defaultReasoningEffort": "medium",
                    "supportedReasoningEfforts": ["medium"],
                },
                {
                    "id": "o3-mini",
                    "model": "o3-mini",
                    "displayName": "O3 Mini",
                    "description": "Faster, cheaper",
                    "isDefault": false,
                    "hidden": false,
                    "defaultReasoningEffort": "low",
                    "supportedReasoningEfforts": ["low"],
                },
            ]
        });
        let models = extract_visible_codex_models(&result);
        assert_eq!(
            models,
            vec![
                json!({
                    "id": "o3",
                    "model": "o3",
                    "displayName": "O3",
                    "description": "Frontier reasoning model",
                    "isDefault": true,
                }),
                json!({
                    "id": "o3-mini",
                    "model": "o3-mini",
                    "displayName": "O3 Mini",
                    "description": "Faster, cheaper",
                    "isDefault": false,
                }),
            ],
            "hidden models must be dropped and only the documented fields kept"
        );
    }

    #[test]
    fn extract_visible_codex_models_tolerates_a_models_wrapped_response_and_missing_ids() {
        let result = json!({
            "models": [
                {"id": "gpt-5.6-sol", "isDefault": true},
                {"model": "no-id-here", "isDefault": false},
            ]
        });
        let models = extract_visible_codex_models(&result);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0]["id"], json!("gpt-5.6-sol"));
    }

    #[tokio::test]
    async fn list_driver_models_returns_a_static_empty_catalogue_for_claude() {
        let response = list_driver_models(Path("claude".to_string()))
            .await
            .expect("claude must not require a live process query");
        let body = response.0;
        assert_eq!(body["models"], json!([]));
        assert_eq!(body["source"], json!("static"));
    }

    #[tokio::test]
    async fn list_driver_models_rejects_an_unknown_driver_kind() {
        let error = list_driver_models(Path("bogus".to_string()))
            .await
            .expect_err("an unrecognized driver kind must fail closed");
        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert_eq!(
            error.1 .0["error"],
            json!("unknown driver kind: bogus"),
            "the caller has to be told which kind was rejected"
        );
    }

    #[test]
    fn driver_models_response_schema_documents_the_source_marker_and_model_fields() {
        let schema = driver_models_response_schema();
        assert_eq!(
            schema["properties"]["source"]["enum"],
            json!(["codex-app-server", "static"])
        );
        assert_eq!(
            schema["properties"]["models"]["items"]["required"],
            json!(["id", "isDefault"])
        );
    }
}
