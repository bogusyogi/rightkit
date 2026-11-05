//! Codex adapter.
//!
//! PR 1 of the architecture plan declared the marker type. PR 6
//! implements [`crate::driver_host::cli_runtime::adapter::CliProviderAdapter`]
//! for `CodexAdapter`. The adapter owns provider-specific
//! knowledge for the Codex CLI:
//!
//! - argv construction, delegated to the legacy
//!   [`crate::driver_host::CodexLaunchOptions::launch_spec`] so the byte-
//!   equivalence guarantee is preserved. The argv is stored on
//!   [`crate::driver_host::cli_runtime::adapter::CliLaunchSpec::argv`] and
//!   forwarded verbatim by the matching transport
//!   ([`crate::driver_host::cli_runtime::transports::codex_app_server`]).
//! - per-event normalization for JSON-RPC notifications, delegated
//!   to the legacy [`crate::driver_host::parse_codex_jsonrpc_line`] so the
//!   event-identity guarantee is preserved. The transport returns a
//!   tagged [`crate::driver_host::CodexMessage`]; the adapter unwraps the
//!   `Notification` variant and surfaces the embedded
//!   [`crate::driver_host::NormalizedEvent`].
//!
//! Discovery and native login run through the installed vendor binary under
//! its managed profile. Model discovery is delegated to the daemon's live
//! app-server `model/list` operation, never a fabricated static catalogue.
//!
//! The split between adapter (provider knowledge) and transport
//! (protocol mechanics) is deliberate: future providers that ship a
//! different protocol can reuse the adapter's auth/model logic by
//! pairing it with their own transport.

use async_trait::async_trait;
use std::path::PathBuf;

use crate::driver_host::cli_runtime::adapter::{
    CliAdapterError, CliAuthenticationStatus, CliDiscovery, CliLaunchRequest, CliLaunchSpec,
    CliModelCatalog, CliProviderAdapter,
};
use crate::driver_host::cli_runtime::ids::{
    CliAccountProfile, CliProviderCapabilities, CliProviderId, CliTransportKind,
    HarnessRequirements, ProbePolarity, QualificationProbe,
};
use crate::driver_host::contracts::NormalizedEvent;
use crate::driver_host::CodexLaunchOptions;

pub const PROVIDER_ID: &str = "codex";
pub const DISPLAY_NAME: &str = "Codex";
pub const TRANSPORT: CliTransportKind = CliTransportKind::CodexAppServer;

/// Codex adapter. PR 6 wires argv construction and event
/// normalization behind the open adapter contract. Future PRs add
/// discovery, native login, and qualification probing.
pub struct CodexAdapter {
    provider_id: CliProviderId,
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self {
            provider_id: CliProviderId::new(PROVIDER_ID),
        }
    }

    /// Transport this adapter drives.
    pub const TRANSPORT_KIND: CliTransportKind = TRANSPORT;
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CliProviderAdapter for CodexAdapter {
    fn id(&self) -> &CliProviderId {
        &self.provider_id
    }

    fn display_name(&self) -> &'static str {
        DISPLAY_NAME
    }

    fn transport_kind(&self) -> CliTransportKind {
        TRANSPORT
    }

    async fn discover(
        &self,
        _profile: &CliAccountProfile,
    ) -> Result<CliDiscovery, CliAdapterError> {
        let executable = find_binary("codex");
        let binary_version = match executable.as_ref() {
            Some(path) => version(path).await,
            None => None,
        };
        let executable_sha256 = super::executable_sha256(executable.as_deref());
        Ok(CliDiscovery {
            installed: executable.is_some(),
            executable,
            binary_version,
            executable_sha256,
            protocol_version: Some("app-server-jsonrpc".into()),
            transport: TRANSPORT,
        })
    }

    async fn authentication_status(
        &self,
        profile: &CliAccountProfile,
    ) -> Result<CliAuthenticationStatus, CliAdapterError> {
        Ok(CliAuthenticationStatus {
            signed_in: profile.signed_in && profile.auth_mode.is_launchable(),
            auth_mode: profile.auth_mode,
            account_label: profile.account_fingerprint.clone(),
            account_fingerprint: profile.account_fingerprint.clone(),
            source_label: "managed Codex CLI profile".into(),
            observed_at: profile.last_verified_at.clone().unwrap_or_default(),
        })
    }

    async fn list_models(
        &self,
        _profile: &CliAccountProfile,
    ) -> Result<CliModelCatalog, CliAdapterError> {
        Err(CliAdapterError::new(
            self.provider_id.clone(),
            "codex_model_catalog_unavailable",
            "installed Codex model discovery must run through its app-server model/list session",
        ))
    }

    async fn build_launch(
        &self,
        request: &CliLaunchRequest,
    ) -> Result<CliLaunchSpec, CliAdapterError> {
        let legacy = translate_request_to_legacy(request);
        let spec = legacy
            .launch_spec()
            .map_err(|error| adapter_error_from_launch_error(&self.provider_id, error))?;
        Ok(CliLaunchSpec {
            provider_id: self.provider_id.clone(),
            transport: TRANSPORT,
            executable: spec.program,
            argv: spec.args,
            env_keep: Vec::new(),
            env_remove: spec.env_remove.into_iter().collect(),
            env: spec.env.into_iter().collect(),
            cwd: request.profile.managed_home.clone(),
            mcp_server: Some(request.mcp_server.clone()),
            profile_hash: crate::driver_host::cli_runtime::adapter::native_profile_hash(
                &request.profile,
            ),
            session_id: request.session_id_seed.clone(),
        })
    }

    fn capabilities(&self) -> CliProviderCapabilities {
        CliProviderCapabilities {
            supports_session_resume: true,
            supports_model_discovery: true,
            supports_native_account_login: true,
            supports_child_delegation: false,
            isolation_mode:
                crate::driver_host::cli_runtime::ids::HarnessIsolationMode::ExclusiveHostGateway,
        }
    }

    fn harness_requirements(&self) -> HarnessRequirements {
        HarnessRequirements {
            requires_managed_profile: true,
            requires_one_mcp_server: true,
            requires_scoped_lease_token: true,
            requires_driver_sandbox: true,
            forbids_native_subagent: true,
            forbids_native_shell: true,
            forbids_native_browser: true,
            forbids_native_memory_writes: true,
        }
    }

    fn qualification_suite(&self) -> &'static [QualificationProbe] {
        qualification_suite()
    }

    fn normalize_provider_event(
        &self,
        event: serde_json::Value,
    ) -> Result<Option<NormalizedEvent>, CliAdapterError> {
        // The legacy parser accepts a `&str`; serialize the JSON
        // value back to a string so we can call it without
        // depending on its private signature. The Codex parser
        // returns a tagged `CodexMessage`; the adapter unwraps
        // notifications and surfaces the embedded event, dropping
        // responses and server requests the same way the executor
        // does.
        let line = serde_json::to_string(&event).map_err(|error| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "codex_event_serialize",
                format!("failed to serialize Codex event: {error}"),
            )
        })?;
        let message = crate::driver_host::parse_codex_jsonrpc_line(&line, 0)
            .map_err(|error| adapter_error_from_parse_error(&self.provider_id, error))?;
        Ok(message.and_then(|msg| match msg {
            crate::driver_host::CodexMessage::Notification(event) => Some(*event),
            crate::driver_host::CodexMessage::Response { .. }
            | crate::driver_host::CodexMessage::ServerRequest { .. } => None,
        }))
    }
}

fn qualification_suite() -> &'static [QualificationProbe] {
    static PROBES: std::sync::OnceLock<Vec<QualificationProbe>> = std::sync::OnceLock::new();
    PROBES
        .get_or_init(|| {
            [
                ("mcp_discovery", ProbePolarity::Positive),
                ("governed_write", ProbePolarity::Positive),
                ("continuity_resume", ProbePolarity::Positive),
                ("native_file_write_denied", ProbePolarity::Negative),
                ("native_shell_denied", ProbePolarity::Negative),
                ("native_delegation_denied", ProbePolarity::Negative),
            ]
            .into_iter()
            .map(|(id, polarity)| QualificationProbe {
                id: format!("codex_{id}"),
                description: id.replace('_', " "),
                polarity,
                mandatory: true,
            })
            .collect()
        })
        .as_slice()
}

/// Translate the open [`CliLaunchRequest`] into the legacy
/// [`CodexLaunchOptions`] shape the legacy argv code expects. The
/// adapter owns this translation because the legacy type is
/// provider-specific and must not leak into the open contract.
fn translate_request_to_legacy(request: &CliLaunchRequest) -> CodexLaunchOptions {
    CodexLaunchOptions {
        executable: request.executable.clone(),
        cwd: PathBuf::from("."),
        strict_config: true,
        model: request.model_id.clone(),
    }
}

fn find_binary(name: &str) -> Option<PathBuf> {
    // PATH split and PATHEXT probing are shared with the other vendor CLIs.
    crate::driver_host::binary_lookup::find_on_path(name)
}

async fn version(path: &std::path::Path) -> Option<String> {
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: std::env::current_dir().ok()?.display().to_string(),
        sandbox_profile: Some("provider-discovery".into()),
        sandbox_required: true,
        network_required: false,
    };
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program: path.display().to_string(),
        argv: vec!["--version".into()],
        cwd: context.workspace_root.clone(),
        context,
        env: Vec::new(),
    };
    let (status, output) = crate::driver_host::binary_lookup::run_bounded_probe(
        spec,
        crate::driver_host::binary_lookup::VERSION_PROBE_TIMEOUT,
        "--version",
    )
    .await
    .ok()?;
    if status != 0 {
        return None;
    }
    let value = String::from_utf8_lossy(&output).trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Hash a profile into the per-launch `profile_hash` the spec
/// carries. Matches the Claude adapter's hashing so both providers
/// report profile swaps in a uniform shape.
fn adapter_error_from_launch_error(
    provider_id: &CliProviderId,
    error: crate::driver_host::DriverHostError,
) -> CliAdapterError {
    match error {
        crate::driver_host::DriverHostError::InvalidMessage { message, .. } => {
            CliAdapterError::new(provider_id.clone(), "codex_launch_invalid", message)
        }
        crate::driver_host::DriverHostError::UnknownRequiredEvent { method, .. } => {
            CliAdapterError::new(provider_id.clone(), "codex_launch_unknown_event", method)
        }
    }
}

fn adapter_error_from_parse_error(
    provider_id: &CliProviderId,
    error: crate::driver_host::DriverHostError,
) -> CliAdapterError {
    match error {
        crate::driver_host::DriverHostError::InvalidMessage { message, .. } => {
            CliAdapterError::new(provider_id.clone(), "codex_parse_invalid", message)
        }
        crate::driver_host::DriverHostError::UnknownRequiredEvent { method, .. } => {
            CliAdapterError::new(provider_id.clone(), "codex_parse_unknown_event", method)
        }
    }
}
