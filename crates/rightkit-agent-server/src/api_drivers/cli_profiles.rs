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

use rightkit_agent_coding_tools::ToolRegistry;

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
    pub effective_profile_hash: String,
    pub model_id: Option<String>,
}
#[derive(Debug)]
pub(crate) enum CliLaunchRouteError {
    InvalidRouteMode,
    NativeLoginRequired,
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
    // No vendor program may start without a stored profile. The one exception is the Codex
    // native profile, and only for a host that offers the Codex built-in provider.
    mode == CliRouteMode::Native
        && provider_id.as_str() == "codex"
        && profile_id == "codex/default"
        && rightkit_agents::driver_host::host_options::built_in_provider_offered("codex")
}

pub(crate) fn missing_default_profile_is_allowed_current(
    provider_id: &rightkit_agents::driver_host::cli_runtime::CliProviderId,
    profile_id: &str,
) -> Result<bool, CliLaunchRouteError> {
    let mode = parse_cli_route_mode(
        &std::env::var(rightkit_agent_config::env_name("CLI_ROUTE_MODE")).unwrap_or_default(),
    )?;
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
    parse_cli_route_mode(
        &std::env::var(rightkit_agent_config::env_name("CLI_ROUTE_MODE")).unwrap_or_default(),
    )?;
    let native_hash = rightkit_agents::driver_host::cli_runtime::native_profile_hash(&profile);
    if !profile.signed_in
        || profile.auth_mode
            != rightkit_agents::driver_host::cli_runtime::CliAuthMode::NativeCliAccount
    {
        return Err(CliLaunchRouteError::NativeLoginRequired);
    }
    Ok(ResolvedCliLaunchRoute {
        launch_profile: profile,
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
            policy_hash: rightkit_fs::digest::sha256_bytes(
                rightkit_agents::driver_host::run_scoped_policy().as_bytes(),
            ),
            tool_schema_hash: gateway_tool_schema_hash()
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            operating_system: std::env::consts::OS.to_string(),
            architecture: std::env::consts::ARCH.to_string(),
            isolation_mode: adapter.capabilities().isolation_mode,
        },
    )
}

pub(crate) fn gateway_tool_schema_hash() -> Result<String, serde_json::Error> {
    let mut registry = ToolRegistry::build_default();
    crate::host_options::apply_tool_registry_controls(&mut registry);
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
    let is_claude = claims_claude_account(&provider_id);
    let mut profile = row.map_or(fallback, |row| {
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
    });
    if is_claude {
        apply_claude_account(&mut profile, None);
    }
    Ok(profile)
}

/// Whether the profile of `provider_id` takes the person's own Claude Code folder: the Claude
/// provider, for a host that links Claude profiles
/// (`rightkit_agents::driver_host::host_options::set_linked_profiles`).
fn claims_claude_account(
    provider_id: &rightkit_agents::driver_host::cli_runtime::CliProviderId,
) -> bool {
    provider_id.as_str() == "claude"
        && rightkit_agents::driver_host::host_options::linked_profiles()
}

/// Point the Claude profile at the person's own Claude Code folder.
///
/// A host that links Claude profiles has no Claude sign-in of its own: the person signs in with Claude
/// Code itself, in a folder of their own, and adds that folder in Settings. This sets the
/// profile's home to the folder the launch will run on and marks it signed in when that folder
/// holds a sign-in (a Claude plan, or a set-up for Bedrock or Vertex).
///
/// Which folder: the account picked for the chat (`session` is the chat id and its workspace),
/// else the project's default account. A picked account that cannot be used leaves the profile
/// not signed in, so the launch is refused rather than run on another account. With nothing
/// picked the launch runs on Claude Code's own default login (`~/.claude`) when one exists.
/// Without a chat (the harness check, the provider list) the default login is used, else the
/// first added account that is signed in.
///
/// The profile id and the stored fingerprint are left alone: the harness check is about the
/// program, the policy and the tool channel, which are the same for every account.
pub(crate) fn apply_claude_account(
    profile: &mut rightkit_agents::driver_host::cli_runtime::CliAccountProfile,
    session: Option<(&str, &str)>,
) {
    use rightkit_agents::driver_host::cli_runtime::account_identity as accounts;
    let holds_sign_in = |folder: &std::path::Path| {
        folder.is_dir()
            && (accounts::read_linked_claude_identity(folder).is_some()
                || accounts::claude_platform_of(folder).is_some())
    };
    let default_login = || {
        accounts::user_home()
            .map(|home| home.join(".claude"))
            .filter(|folder| holds_sign_in(folder))
    };
    let picked = session.map(|(session_id, workspace)| {
        crate::main_runtime::resolve_claude_account(session_id, workspace)
    });
    let folder = match picked {
        Some(Ok(Some(resolved))) => match resolved.launch {
            rightkit_agents::driver_host::ClaudeAccountLaunch::Subscription { managed_home } => {
                Some(managed_home).filter(|folder| holds_sign_in(folder))
            }
            // An API-key account runs on the host's own engine, not through Claude Code.
            rightkit_agents::driver_host::ClaudeAccountLaunch::ApiKey { .. } => None,
        },
        Some(Err(_)) => None,
        Some(Ok(None)) => default_login(),
        None => default_login().or_else(|| {
            accounts::list_subscription_accounts()
                .into_iter()
                .find(|account| account.provider == "claude" && account.signed_in)
                .map(|account| account.managed_home)
        }),
    };
    match folder {
        Some(folder) => {
            profile.managed_home = folder;
            profile.signed_in = true;
            profile.auth_mode =
                rightkit_agents::driver_host::cli_runtime::CliAuthMode::NativeCliAccount;
        }
        None => profile.signed_in = false,
    }
}

/// The Claude profile for one chat: `persisted_cli_profile` with the chat's own account applied.
pub(crate) async fn cli_profile_for_session(
    state: &DaemonState,
    provider_id: rightkit_agents::driver_host::cli_runtime::CliProviderId,
    session_id: &str,
    workspace: &str,
) -> Result<rightkit_agents::driver_host::cli_runtime::CliAccountProfile, StatusCode> {
    let is_claude = claims_claude_account(&provider_id);
    let mut profile = persisted_cli_profile(state, provider_id).await?;
    if is_claude {
        apply_claude_account(&mut profile, Some((session_id, workspace)));
    }
    Ok(profile)
}

/// `cli_profile_for_session`, refused with 409 when the chat's account holds no sign-in.
pub(crate) async fn launchable_cli_profile_for_session(
    state: &DaemonState,
    provider_id: rightkit_agents::driver_host::cli_runtime::CliProviderId,
    session_id: &str,
    workspace: &str,
) -> Result<rightkit_agents::driver_host::cli_runtime::CliAccountProfile, StatusCode> {
    let profile = cli_profile_for_session(state, provider_id, session_id, workspace).await?;
    if !profile.signed_in {
        return Err(StatusCode::CONFLICT);
    }
    Ok(profile)
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
