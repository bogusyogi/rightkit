//! CLI account profile resolution and route/attestation matching.
//!
//! Decomposed out of `api_drivers.rs` (ra-016): this module owns profile
//! defaults, auth-mode parsing, native route resolution,
//! attestation matching, and profile persistence. It has no route handlers
//! of its own — `api_drivers.rs` re-exports everything here (`pub(crate) use
//! cli_profiles::*;`) and keeps calling these functions unqualified from its
//! HTTP handlers exactly as before the split, so the facade's public routes,
//! wire formats, and authorization boundaries are unchanged.

use axum::http::StatusCode;
use std::path::PathBuf;

use rightkit_agent_tools::coding_tools::ToolRegistry;

use crate::api::{now_iso, DaemonState};

use super::{capability_schema, DRIVER_BLOCKED_TOOLS, SPECIAL_TOOLS};

pub(crate) fn default_cli_profile(
    provider_id: rightkit_agents::driver_host::cli_runtime::CliProviderId,
) -> rightkit_agents::driver_host::cli_runtime::CliAccountProfile {
    let managed_home =
        rightkit_agents::driver_host::cli_runtime::profile::managed_home(&provider_id, "default");
    rightkit_agents::driver_host::cli_runtime::CliAccountProfile {
        id: rightkit_agents::driver_host::cli_runtime::CliAccountProfileId::new(format!(
            "{}/default",
            provider_id.as_str()
        )),
        provider_id,
        label: "default".to_string(),
        managed_home,
        auth_mode: rightkit_agents::driver_host::cli_runtime::CliAuthMode::Unknown,
        signed_in: false,
        account_fingerprint: None,
        last_verified_at: None,
    }
}

pub(crate) fn cli_auth_mode(value: &str) -> rightkit_agents::driver_host::cli_runtime::CliAuthMode {
    match value {
        "native_cli_account" => {
            rightkit_agents::driver_host::cli_runtime::CliAuthMode::NativeCliAccount
        }
        "byok_or_external_api" => {
            rightkit_agents::driver_host::cli_runtime::CliAuthMode::ByokOrExternalApi
        }
        _ => rightkit_agents::driver_host::cli_runtime::CliAuthMode::Unknown,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedCliLaunchRoute {
    pub launch_profile: rightkit_agents::driver_host::cli_runtime::CliAccountProfile,
    pub native_profile_hash: String,
    pub effective_profile_hash: String,
    pub model_id: Option<String>,
}
#[derive(Debug)]
pub(crate) enum CliLaunchRouteError {
    InvalidRouteMode,
    NativeLoginRequired,
    ModelUnavailable,
    AttestationMissing,
    AttestationDrift,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CliRouteMode {
    Native,
}

pub(crate) fn parse_cli_route_mode(value: &str) -> Result<CliRouteMode, CliLaunchRouteError> {
    match value {
        "" | "native" => Ok(CliRouteMode::Native),
        _ => Err(CliLaunchRouteError::InvalidRouteMode),
    }
}

pub(crate) fn missing_default_profile_is_allowed(
    mode: CliRouteMode,
    provider_id: &rightkit_agents::driver_host::cli_runtime::CliProviderId,
    profile_id: &str,
) -> bool {
    mode == CliRouteMode::Native && provider_id.as_str() == "codex" && profile_id == "codex/default"
}

pub(crate) fn missing_default_profile_is_allowed_current(
    provider_id: &rightkit_agents::driver_host::cli_runtime::CliProviderId,
    profile_id: &str,
) -> Result<bool, CliLaunchRouteError> {
    let mode = parse_cli_route_mode(&std::env::var("RIGHTKIT_CLI_ROUTE_MODE").unwrap_or_default())?;
    Ok(missing_default_profile_is_allowed(
        mode,
        provider_id,
        profile_id,
    ))
}

pub(crate) fn binding_route_tuple_matches(
    binding: &rightkit_agent_engine_storage::DriverBindingRow,
    resolved: &ResolvedCliLaunchRoute,
) -> bool {
    binding.profile_hash.as_deref() == Some(resolved.effective_profile_hash.as_str())
        && binding.model_id == resolved.model_id
}
impl CliLaunchRouteError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidRouteMode => "invalid_route_mode",
            Self::NativeLoginRequired => "native_login_required",
            Self::ModelUnavailable => "model_unavailable",
            Self::AttestationMissing => "harness_verification_required",
            Self::AttestationDrift => "route_attestation_drift",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedCliAttestation {
    pub row: rightkit_agent_engine_storage::CliHarnessAttestationRow,
    pub inputs: rightkit_agents::driver_host::cli_runtime::AttestationInputs,
}

pub(crate) async fn matching_route_attestation(
    state: &DaemonState,
    provider: &rightkit_agents::driver_host::cli_runtime::CliProviderId,
    discovery: &rightkit_agents::driver_host::cli_runtime::CliDiscovery,
    adapter: &dyn rightkit_agents::driver_host::cli_runtime::CliProviderAdapter,
    resolved: &ResolvedCliLaunchRoute,
) -> Result<ResolvedCliAttestation, CliLaunchRouteError> {
    let storage = state
        .storage
        .as_ref()
        .ok_or(CliLaunchRouteError::AttestationMissing)?;
    let rows = storage
        .lock()
        .await
        .list_cli_harness_attestations(provider.as_str())
        .map_err(|_| CliLaunchRouteError::AttestationMissing)?;
    let row = rows
        .into_iter()
        .find(|row| {
            row.result == "passed"
                && row.profile_hash == resolved.effective_profile_hash
                && row.executable_sha256 == discovery.executable_sha256.clone().unwrap_or_default()
        })
        .ok_or(CliLaunchRouteError::AttestationMissing)?;
    let mut inputs = current_launch_attestation_inputs(provider, &row, discovery, adapter)
        .map_err(|_| CliLaunchRouteError::AttestationDrift)?;
    inputs.profile_hash = resolved.effective_profile_hash.clone();
    if row.profile_hash != inputs.profile_hash || !state.cli_registry.is_ready_for_inputs(&inputs) {
        return Err(CliLaunchRouteError::AttestationDrift);
    }
    Ok(ResolvedCliAttestation { row, inputs })
}
impl std::fmt::Display for CliLaunchRouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

pub(crate) async fn resolve_cli_launch_route(
    _state: &DaemonState,
    _provider_id: &rightkit_agents::driver_host::cli_runtime::CliProviderId,
    profile: rightkit_agents::driver_host::cli_runtime::CliAccountProfile,
    requested_model_id: Option<&str>,
) -> Result<ResolvedCliLaunchRoute, CliLaunchRouteError> {
    parse_cli_route_mode(&std::env::var("RIGHTKIT_CLI_ROUTE_MODE").unwrap_or_default())?;
    let native_hash = rightkit_agents::driver_host::cli_runtime::native_profile_hash(&profile);
    if !profile.signed_in
        || profile.auth_mode
            != rightkit_agents::driver_host::cli_runtime::CliAuthMode::NativeCliAccount
    {
        return Err(CliLaunchRouteError::NativeLoginRequired);
    }
    Ok(ResolvedCliLaunchRoute {
        launch_profile: profile,
        native_profile_hash: native_hash.clone(),
        effective_profile_hash: native_hash,
        model_id: requested_model_id.map(str::to_string),
    })
}

pub(crate) fn current_launch_attestation_inputs(
    provider_id: &rightkit_agents::driver_host::cli_runtime::CliProviderId,
    row: &rightkit_agent_engine_storage::CliHarnessAttestationRow,
    discovery: &rightkit_agents::driver_host::cli_runtime::CliDiscovery,
    adapter: &dyn rightkit_agents::driver_host::cli_runtime::CliProviderAdapter,
) -> Result<rightkit_agents::driver_host::cli_runtime::AttestationInputs, StatusCode> {
    Ok(
        rightkit_agents::driver_host::cli_runtime::AttestationInputs {
            provider_id: provider_id.clone(),
            executable_sha256: discovery
                .executable_sha256
                .clone()
                .ok_or(StatusCode::CONFLICT)?,
            binary_version: discovery
                .binary_version
                .clone()
                .ok_or(StatusCode::CONFLICT)?,
            protocol_version: discovery
                .protocol_version
                .clone()
                .unwrap_or_else(|| adapter.transport_kind().label().to_string()),
            profile_hash: row.profile_hash.clone(),
            policy_hash: rightkit_fs::digest::sha256_bytes(AsRef::<[u8]>::as_ref(
                &rightkit_agents::driver_host::RUN_SCOPED_POLICY.as_bytes(),
            )),
            tool_schema_hash: gateway_tool_schema_hash()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            operating_system: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            isolation_mode: adapter.capabilities().isolation_mode,
        },
    )
}

pub(crate) fn gateway_tool_schema_hash() -> Result<String, serde_json::Error> {
    let registry = ToolRegistry::build_default();
    let mut names = registry
        .all()
        .iter()
        .map(|tool| tool.name().to_string())
        .chain(SPECIAL_TOOLS.iter().map(|name| (*name).to_string()))
        .filter(|name| !DRIVER_BLOCKED_TOOLS.contains(&name.as_str()))
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    let schemas = names
        .iter()
        .filter_map(|name| capability_schema(&registry, name))
        .collect::<Vec<_>>();
    serde_json::to_vec(&schemas)
        .map(|bytes| rightkit_fs::digest::sha256_bytes(AsRef::<[u8]>::as_ref(&bytes)))
}

pub(crate) async fn persisted_cli_profile(
    state: &DaemonState,
    provider_id: rightkit_agents::driver_host::cli_runtime::CliProviderId,
) -> Result<rightkit_agents::driver_host::cli_runtime::CliAccountProfile, StatusCode> {
    let fallback = default_cli_profile(provider_id.clone());
    let Some(storage) = state.storage.as_ref() else {
        return Ok(fallback);
    };
    let profile_id = fallback.id.as_str().to_string();
    let row = storage
        .lock()
        .await
        .get_cli_account_profile(&profile_id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(row.map_or(fallback, |row| {
        rightkit_agents::driver_host::cli_runtime::CliAccountProfile {
            id: rightkit_agents::driver_host::cli_runtime::CliAccountProfileId::new(row.id),
            provider_id,
            label: row.label,
            managed_home: PathBuf::from(row.managed_home),
            auth_mode: cli_auth_mode(&row.auth_mode),
            signed_in: row.signed_in,
            account_fingerprint: row.account_fingerprint,
            last_verified_at: row.last_verified_at,
        }
    }))
}

pub(crate) async fn launchable_cli_profile(
    state: &DaemonState,
    provider_id: rightkit_agents::driver_host::cli_runtime::CliProviderId,
) -> Result<rightkit_agents::driver_host::cli_runtime::CliAccountProfile, StatusCode> {
    let profile = persisted_cli_profile(state, provider_id.clone()).await?;
    if !profile.signed_in
        && !missing_default_profile_is_allowed_current(&provider_id, profile.id.as_str())
            .map_err(|_| StatusCode::CONFLICT)?
    {
        return Err(StatusCode::CONFLICT);
    }
    Ok(profile)
}

pub(crate) async fn persist_cli_profile(
    state: &DaemonState,
    profile: &rightkit_agents::driver_host::cli_runtime::CliAccountProfile,
) -> Result<(), StatusCode> {
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let now = now_iso();
    // #341: when the vendor CLI exposes an identity, the fingerprint is provider + email + org
    // UUID read from the profile's own metadata, not a hash of the profile label.
    let account_fingerprint =
        rightkit_agents::driver_host::cli_runtime::account_identity::fingerprint_for_home(
            &profile.provider_id,
            &profile.managed_home,
        )
        .or_else(|| profile.account_fingerprint.clone());
    storage
        .lock()
        .await
        .upsert_cli_account_profile(&rightkit_agent_engine_storage::CliAccountProfileRow {
            id: profile.id.as_str().to_string(),
            provider_id: profile.provider_id.as_str().to_string(),
            label: profile.label.clone(),
            managed_home: profile.managed_home.to_string_lossy().into_owned(),
            auth_mode: profile.auth_mode.label().to_string(),
            signed_in: profile.signed_in,
            account_fingerprint,
            last_verified_at: profile.last_verified_at.clone(),
            created_at: now.clone(),
            updated_at: now,
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
