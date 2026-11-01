//! Claude Code adapter.
//!
//! PR 1 of the architecture plan declared the marker type. PR 6
//! implements [`crate::driver_host::cli_runtime::adapter::CliProviderAdapter`]
//! for `ClaudeAdapter`. The adapter owns provider-specific
//! knowledge for Claude Code:
//!
//! - argv construction, delegated to the legacy
//!   [`crate::driver_host::ClaudeLaunchOptions::launch_spec`] so the byte-
//!   equivalence guarantee is preserved. The argv is stored on
//!   [`crate::driver_host::cli_runtime::adapter::CliLaunchSpec::argv`] and
//!   forwarded verbatim by the matching transport
//!   ([`crate::driver_host::cli_runtime::transports::claude_stream_json`]).
//! - per-event normalization, delegated to the legacy
//!   [`crate::driver_host::parse_claude_stream_line`] so the event-identity
//!   guarantee is preserved.
//!
//! Discovery and native login run through the installed vendor binary under
//! its managed profile. Model discovery remains account-scoped daemon work:
//! this adapter refuses to invent model IDs when Claude offers no stable list.
//!
//! The split between adapter (provider knowledge) and transport
//! (protocol mechanics) is deliberate: future providers that ship a
//! different protocol can reuse the adapter's auth/model logic by
//! pairing it with their own transport.

use async_trait::async_trait;
use std::path::PathBuf;

use crate::driver_host::cli_runtime::adapter::{
    CliAdapterError, CliAuthenticationStatus, CliDiscovery, CliLaunchRequest, CliLaunchSpec,
    CliModelCatalog, CliProviderAdapter, CliRuntimeError, NativeLoginUi,
};
use crate::driver_host::cli_runtime::ids::{
    CliAccountProfile, CliProviderCapabilities, CliProviderId, CliTransportKind,
    HarnessRequirements, ProbePolarity, QualificationProbe,
};
use crate::driver_host::contracts::NormalizedEvent;
use crate::driver_host::ClaudeLaunchOptions;

pub const PROVIDER_ID: &str = "claude";
pub const DISPLAY_NAME: &str = "Claude Code";
pub const TRANSPORT: CliTransportKind = CliTransportKind::ClaudeStreamJson;

/// Claude Code adapter. PR 6 wires the argv construction and event
/// normalization behind the open adapter contract. Future PRs add
/// discovery, native login, and qualification probing.
pub struct ClaudeAdapter {
    provider_id: CliProviderId,
}

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self {
            provider_id: CliProviderId::new(PROVIDER_ID),
        }
    }

    /// Transport this adapter drives.
    pub const TRANSPORT_KIND: CliTransportKind = TRANSPORT;
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CliProviderAdapter for ClaudeAdapter {
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
        let executable = find_binary("claude");
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
            protocol_version: Some("stream-json".into()),
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
            source_label: "managed Claude CLI profile".into(),
            observed_at: profile.last_verified_at.clone().unwrap_or_default(),
        })
    }

    async fn begin_native_login(
        &self,
        profile: &CliAccountProfile,
        ui: &dyn NativeLoginUi,
    ) -> Result<(), CliAdapterError> {
        let executable = find_binary("claude").ok_or_else(|| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "claude_binary_not_found",
                "Claude CLI is not installed",
            )
        })?;
        ui.report_progress("Starting Claude native login").await?;
        let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
        let context = rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: profile.managed_home.to_string_lossy().into_owned(),
            sandbox_profile: Some("claude-login".into()),
            sandbox_required: true,
            network_required: true,
        };
        let spec = rightkit_sandbox::execution::CommandLaunchSpec {
            program: executable.display().to_string(),
            argv: vec!["login".into()],
            cwd: context.workspace_root.clone(),
            context,
            env: vec![
                (
                    "HOME".into(),
                    profile.managed_home.to_string_lossy().into_owned(),
                ),
                (
                    "CLAUDE_CONFIG_DIR".into(),
                    profile.managed_home.to_string_lossy().into_owned(),
                ),
            ],
        };
        let mut child = rightkit_sandbox::execution::ProcessLaunchAdapter::spawn(&adapter, spec)
            .await
            .map_err(|error| {
                CliAdapterError::new(
                    self.provider_id.clone(),
                    "claude_login_spawn",
                    format!("{error:?}"),
                )
            })?;
        let status = child.wait().await.map_err(|error| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "claude_login_wait",
                format!("{error:?}"),
            )
        })?;
        if status != 0 {
            return Err(CliAdapterError::new(
                self.provider_id.clone(),
                "claude_login_failed",
                format!("Claude login exited with status {status}"),
            ));
        }
        Ok(())
    }

    async fn list_models(
        &self,
        _profile: &CliAccountProfile,
    ) -> Result<CliModelCatalog, CliAdapterError> {
        Err(CliAdapterError::new(self.provider_id.clone(), "claude_model_catalog_unavailable", "installed Claude CLI does not expose a supported account-scoped model catalogue; choose a model only after daemon discovery"))
    }

    async fn build_launch(
        &self,
        request: &CliLaunchRequest,
    ) -> Result<CliLaunchSpec, CliAdapterError> {
        let legacy = translate_request_to_legacy(request)?;
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
            session_id: encode_session_id(request.resume, &request.session_id_seed),
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
        // depending on its private signature.
        let line = serde_json::to_string(&event).map_err(|error| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "claude_event_serialize",
                format!("failed to serialize Claude event: {error}"),
            )
        })?;
        crate::driver_host::parse_claude_stream_line(&line, 0)
            .map_err(|error| adapter_error_from_parse_error(&self.provider_id, error))
    }
}

fn qualification_suite() -> &'static [QualificationProbe] {
    static PROBES: std::sync::OnceLock<Vec<QualificationProbe>> = std::sync::OnceLock::new();
    PROBES.get_or_init(|| probes("claude")).as_slice()
}

fn probes(provider: &str) -> Vec<QualificationProbe> {
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
        id: format!("{provider}_{id}"),
        description: id.replace('_', " "),
        polarity,
        mandatory: true,
    })
    .collect()
}

/// Translate the open [`CliLaunchRequest`] into the legacy
/// [`ClaudeLaunchOptions`] shape the legacy argv code expects. The
/// adapter owns this translation because the legacy type is
/// provider-specific and must not leak into the open contract.
fn translate_request_to_legacy(
    request: &CliLaunchRequest,
) -> Result<ClaudeLaunchOptions, CliAdapterError> {
    if request.resume && request.session_id_seed.is_empty() {
        return Err(CliAdapterError::new(
            request.provider_id.clone(),
            "claude_resume_missing_session",
            "resume=true requires a non-empty session_id_seed",
        ));
    }
    let (session_id, resume_session_id) = if request.resume {
        (None, Some(request.session_id_seed.clone()))
    } else {
        (Some(request.session_id_seed.clone()), None)
    };
    let mcp_config = serialize_mcp_config(&request.provider_id, &request.mcp_server)?;
    // The adapter does not yet know the executable path — discovery
    // is wired by a later PR. Until then we set a sentinel that the
    // supervisor will overwrite; argv construction only needs the
    // args list which is independent of the executable path.
    Ok(ClaudeLaunchOptions {
        executable: request.executable.clone(),
        cwd: PathBuf::from("."),
        mcp_config,
        session_id,
        resume_session_id,
        model: request.model_id.clone(),
    })
}

fn find_binary(name: &str) -> Option<PathBuf> {
    // PATH split and PATHEXT probing are shared with the other vendor CLIs.
    crate::driver_host::binary_lookup::find_on_path(name)
        // Governed launch requires an absolute program path; a relative PATH
        // entry otherwise yields an unspawnable relative executable.
        .map(|path| {
            let canon = path.canonicalize().unwrap_or(path);
            let s = canon.to_string_lossy();
            if let Some(stripped) = s.strip_prefix(r"\\?\") {
                if !stripped.starts_with(r"UNC\") {
                    PathBuf::from(stripped)
                } else {
                    canon
                }
            } else {
                canon
            }
        })
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

/// Serialize the open [`crate::driver_host::cli_runtime::adapter::CliMcpServerSpec`]
/// back into the JSON string the legacy Claude argv expects. The
/// legacy argv accepts exactly one MCP server (the `--mcp-config`
/// flag references a single JSON document with a single named
/// server); the open contract carries the same single-server
/// invariant.
fn serialize_mcp_config(
    provider_id: &CliProviderId,
    spec: &crate::driver_host::cli_runtime::adapter::CliMcpServerSpec,
) -> Result<String, CliAdapterError> {
    let mut entry = serde_json::Map::new();
    entry.insert(
        "command".into(),
        serde_json::Value::String(spec.command.display().to_string()),
    );
    entry.insert(
        "args".into(),
        serde_json::Value::Array(
            spec.args
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    if !spec.env.is_empty() {
        let mut env_obj = serde_json::Map::new();
        for (k, v) in &spec.env {
            env_obj.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        entry.insert("env".into(), serde_json::Value::Object(env_obj));
    }
    let mut servers = serde_json::Map::new();
    servers.insert(spec.name.clone(), serde_json::Value::Object(entry));
    let mut outer = serde_json::Map::new();
    outer.insert("mcpServers".into(), serde_json::Value::Object(servers));
    serde_json::to_string(&outer).map_err(|error| {
        CliAdapterError::new(
            provider_id.clone(),
            "claude_mcp_config_serialize",
            format!("failed to serialize MCP config: {error}"),
        )
    })
}

/// Encode the open session-id field into the wire form the legacy
/// argv consumes. The open field is a single string; Claude's
/// flag set distinguishes fresh-session from resume by which flag
/// is used. We surface that choice on the open field with a
/// `resume:` prefix so the wire form is unambiguous.
fn encode_session_id(resume: bool, seed: &str) -> String {
    if resume {
        format!("resume:{seed}")
    } else {
        seed.to_string()
    }
}

/// Hash a profile into the per-launch `profile_hash` the spec
/// carries. The hash is opaque; PR 6 uses a deterministic
/// SHA-256-of-the-id prefix so the supervisor can detect a
/// profile swap without exposing the underlying identifier.
fn adapter_error_from_launch_error(
    provider_id: &CliProviderId,
    error: crate::driver_host::DriverHostError,
) -> CliAdapterError {
    match error {
        crate::driver_host::DriverHostError::InvalidMessage { message, .. } => {
            CliAdapterError::new(provider_id.clone(), "claude_launch_invalid", message)
        }
        crate::driver_host::DriverHostError::UnknownRequiredEvent { method, .. } => {
            CliAdapterError::new(provider_id.clone(), "claude_launch_unknown_event", method)
        }
    }
}

fn adapter_error_from_parse_error(
    provider_id: &CliProviderId,
    error: crate::driver_host::DriverHostError,
) -> CliAdapterError {
    match error {
        crate::driver_host::DriverHostError::InvalidMessage { message, .. } => {
            CliAdapterError::new(provider_id.clone(), "claude_parse_invalid", message)
        }
        crate::driver_host::DriverHostError::UnknownRequiredEvent { method, .. } => {
            CliAdapterError::new(provider_id.clone(), "claude_parse_unknown_event", method)
        }
    }
}

// Re-export the runtime-error helper so downstream callers that
// reach for the adapter from a non-async context don't pull in
// the executor crate just to translate errors.
#[allow(dead_code)]
fn runtime_error_from_launch_error(error: crate::driver_host::DriverHostError) -> CliRuntimeError {
    match error {
        crate::driver_host::DriverHostError::InvalidMessage { message, .. } => {
            CliRuntimeError::new("claude_launch_invalid", message)
        }
        crate::driver_host::DriverHostError::UnknownRequiredEvent { method, .. } => {
            CliRuntimeError::new("claude_launch_unknown_event", method)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::cli_runtime::adapter::CliMcpServerSpec;
    use crate::driver_host::cli_runtime::ids::CliAccountProfileId;
    use crate::driver_host::contracts::NormalizedEventKind;
    use crate::driver_host::RUN_SCOPED_POLICY;
    use std::path::PathBuf;

    fn sample_profile() -> CliAccountProfile {
        CliAccountProfile {
            id: CliAccountProfileId::new("claude/default"),
            provider_id: CliProviderId::new(PROVIDER_ID),
            label: "default".into(),
            managed_home: PathBuf::from("/tmp/rightkit-agent/cli-profiles/claude/default"),
            auth_mode: crate::driver_host::cli_runtime::ids::CliAuthMode::NativeCliAccount,
            signed_in: true,
            account_fingerprint: Some("fp-1".into()),
            last_verified_at: None,
        }
    }

    fn sample_mcp() -> CliMcpServerSpec {
        CliMcpServerSpec {
            name: "rightkit-agent".into(),
            command: PathBuf::from("/usr/local/bin/rightkit-agent-mcp"),
            args: Vec::new(),
            env: Vec::new(),
        }
    }

    /// serde_json `preserve_order` is feature-unified across the workspace, so
    /// JSON object key order in argv is not stable; compare JSON args as values.
    fn normalize_argv_json(argv: &[&str]) -> Vec<serde_json::Value> {
        argv.iter()
            .map(|arg| match serde_json::from_str::<serde_json::Value>(arg) {
                Ok(value @ serde_json::Value::Object(_)) => value,
                _ => serde_json::Value::String((*arg).to_string()),
            })
            .collect()
    }

    fn sample_request(model: Option<&str>, resume: bool) -> CliLaunchRequest {
        CliLaunchRequest {
            provider_id: CliProviderId::new(PROVIDER_ID),
            executable: PathBuf::from("/usr/local/bin/claude"),
            profile: sample_profile(),
            model_id: model.map(str::to_string),
            session_id_seed: if resume {
                "existing-session".into()
            } else {
                "550e8400-e29b-41d4-a716-446655440000".into()
            },
            mcp_server: sample_mcp(),
            policy: RUN_SCOPED_POLICY.into(),
            resume,
        }
    }

    /// The adapter must produce argv byte-equivalent to the frozen
    /// snapshot. This is the headline PR 6 acceptance criterion:
    /// byte-equivalent Claude launch evidence.
    #[tokio::test]
    async fn build_launch_argv_matches_frozen_snapshot() {
        let adapter = ClaudeAdapter::new();
        let request = sample_request(None, false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        let argv_refs: Vec<&str> = spec.argv.iter().map(String::as_str).collect();
        assert_eq!(
            normalize_argv_json(&argv_refs),
            normalize_argv_json(crate::driver_host::cli_runtime::transports::argv_snapshot::CLAUDE_BASELINE_ARGV),
            "Claude adapter argv must match the frozen snapshot byte-for-byte (JSON arguments compared as parsed values)"
        );
        // The adapter must surface the canonical session id on the
        // open field for downstream consumers.
        assert_eq!(spec.session_id, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(spec.transport, CliTransportKind::ClaudeStreamJson);
    }

    /// The adapter must encode resume in the open session_id field
    /// (with a `resume:` prefix) and emit the resume-shape argv.
    #[tokio::test]
    async fn build_launch_emits_resume_argv_when_requested() {
        let adapter = ClaudeAdapter::new();
        let request = sample_request(None, true);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        let argv_refs: Vec<&str> = spec.argv.iter().map(String::as_str).collect();
        assert_eq!(
            normalize_argv_json(&argv_refs),
            normalize_argv_json(crate::driver_host::cli_runtime::transports::argv_snapshot::CLAUDE_RESUME_ARGV),
            "resume argv must match the frozen snapshot byte-for-byte (JSON arguments compared as parsed values)"
        );
        assert_eq!(spec.session_id, "resume:existing-session");
    }

    /// Resume with an empty session id seed is a hard error: the
    /// legacy argv has no way to express a resume without a
    /// target session id.
    #[tokio::test]
    async fn build_launch_rejects_resume_without_session_seed() {
        let adapter = ClaudeAdapter::new();
        let mut request = sample_request(None, true);
        request.session_id_seed = String::new();
        let error = adapter.build_launch(&request).await.unwrap_err();
        assert_eq!(error.code, "claude_resume_missing_session");
    }

    /// A model override must be appended as `--model <id>`. The
    /// adapter never re-validates the model id; the legacy argv
    /// code rejects hostile ids before they reach the process.
    #[tokio::test]
    async fn build_launch_appends_model_flag_when_set() {
        let adapter = ClaudeAdapter::new();
        let request = sample_request(Some("sonnet"), false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        assert!(
            spec.argv
                .windows(2)
                .any(|pair| pair == ["--model", "sonnet"]),
            "model flag must be appended when request carries a model_id"
        );
    }

    /// Hostile model ids are rejected by the legacy argv code
    /// before the spec is returned. The adapter must surface the
    /// rejection as a typed `CliAdapterError`.
    #[tokio::test]
    async fn build_launch_rejects_hostile_model_id() {
        let adapter = ClaudeAdapter::new();
        let request = sample_request(Some("sonnet; rm -rf /"), false);
        let error = adapter.build_launch(&request).await.unwrap_err();
        assert_eq!(error.code, "claude_launch_invalid");
    }

    /// The spec must strip the forbidden BYOK env vars. The legacy
    /// code populates this set; the adapter forwards it.
    #[tokio::test]
    async fn build_launch_forwards_forbidden_env_var_strip_list() {
        let adapter = ClaudeAdapter::new();
        let request = sample_request(None, false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        for name in [
            "OPENROUTER_API_KEY",
            "MINIMAX_API_KEY",
            "RIGHTKIT_DAEMON_TOKEN",
            "RIGHTKIT_GATEWAY_TOKEN",
        ] {
            assert!(
                spec.env_remove.contains(&name.to_string()),
                "adapter must strip {name}"
            );
        }
    }

    /// The adapter must round-trip a stream-event JSON payload
    /// into the same `NormalizedEvent` kind the legacy parser
    /// produced. PR 6 acceptance gate #2: existing event fixtures
    /// parse into the same `NormalizedEvent`.
    #[test]
    fn normalize_provider_event_matches_legacy_for_text_delta() {
        let adapter = ClaudeAdapter::new();
        let event: serde_json::Value = serde_json::from_str(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hello"}}}"#,
        )
        .unwrap();
        let normalized = adapter
            .normalize_provider_event(event)
            .unwrap()
            .expect("event must normalize to Some");
        assert_eq!(
            normalized.kind,
            NormalizedEventKind::AssistantDelta {
                text: "hello".into()
            }
        );
    }

    /// The adapter drops presentation heartbeats the same way the
    /// legacy parser does.
    #[test]
    fn normalize_provider_event_returns_none_for_hook_started() {
        let adapter = ClaudeAdapter::new();
        let event: serde_json::Value = serde_json::from_str(
            r#"{"type":"system","subtype":"hook_started","session_id":"session-1"}"#,
        )
        .unwrap();
        let normalized = adapter.normalize_provider_event(event).unwrap();
        assert!(normalized.is_none());
    }

    /// Capabilities must declare `ExclusiveCodeRightGateway` as the
    /// isolation target. Compatibility mode is not a Claude target.
    #[test]
    fn capabilities_target_exclusive_host_gateway() {
        let adapter = ClaudeAdapter::new();
        let caps = adapter.capabilities();
        assert_eq!(
            caps.isolation_mode,
            crate::driver_host::cli_runtime::ids::HarnessIsolationMode::ExclusiveHostGateway
        );
    }

    /// Harness requirements must reflect every Claude-specific
    /// restriction. Drift here is a security-relevant diff.
    #[test]
    fn harness_requirements_match_claude_policy() {
        let adapter = ClaudeAdapter::new();
        let reqs = adapter.harness_requirements();
        assert!(reqs.requires_managed_profile);
        assert!(reqs.requires_one_mcp_server);
        assert!(reqs.requires_scoped_lease_token);
        assert!(reqs.requires_driver_sandbox);
        assert!(reqs.forbids_native_subagent);
        assert!(reqs.forbids_native_shell);
        assert!(reqs.forbids_native_browser);
        assert!(reqs.forbids_native_memory_writes);
    }

    /// Identity round-trip: id() and display_name() must match the
    /// canonical strings.
    #[test]
    fn identity_matches_canonical_strings() {
        let adapter = ClaudeAdapter::new();
        assert_eq!(adapter.id().as_str(), PROVIDER_ID);
        assert_eq!(adapter.display_name(), DISPLAY_NAME);
        assert_eq!(adapter.transport_kind(), TRANSPORT);
    }
}
