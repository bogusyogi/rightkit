//! Cursor Agent CLI adapter.
//!
//! PR 10 of the architecture plan. Concrete implementation of
//! [`crate::driver_host::cli_runtime::adapter::CliProviderAdapter`] for the Cursor
//! Agent CLI (`cursor-agent`). The adapter enforces the documented
//! Cursor-specific constraints from §14 of the architecture plan:
//!
//! - Browser-account authentication only. The adapter's
//!   [`authentication_status`](CliProviderAdapter::authentication_status)
//!   uses Cursor's bounded structured status command under the managed
//!   profile and never reads browser credential stores.
//! - No `--api-key` flag anywhere in argv. The
//!   [`build_launch`](CliProviderAdapter::build_launch) test asserts
//!   the flag is absent.
//! - No `CURSOR_API_KEY` / `CURSOR_API_BASE_URL` in the launched
//!   environment. Both names are members of
//!   [`crate::driver_host::cli_runtime::auth::FORBIDDEN_BYOK_ENV_VARS`] and are
//!   stripped at launch prep.
//! - ACP transport preferred (`agent acp`). Native write/Bash tools
//!   are denied structurally by never passing `--print` or any
//!   `--allow-*` flag.
//! - Live ACP capability qualification is required before launch.
//!   The qualification suite adds Cursor-specific probes for
//!   MCP attach and `session/load`.
//! - NDJSON fallback is implemented but disabled by default. The
//!   fallback only becomes launchable when the full harness canary
//!   passes; see [`crate::driver_host::cli_runtime::transports::resumable_ndjson`]
//!   for the dial-down gate.
//!
//! Cursor CLI is comparatively new; public bug reports documented
//! versions where `mcpServers` in `session/new` were accepted but
//! ignored and `session/load` was advertised but failed. The
//! qualification runner verifies these capabilities live before the
//! adapter is allowed to launch.
//!
//! [`CliAuthMode::NativeCliAccount`]: crate::driver_host::cli_runtime::ids::CliAuthMode::NativeCliAccount
//! [`CliProviderAdapter`]: crate::driver_host::cli_runtime::adapter::CliProviderAdapter

use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::LazyLock;

use crate::driver_host::cli_runtime::adapter::{
    CliAdapterError, CliAuthenticationStatus, CliDiscovery, CliLaunchRequest, CliLaunchSpec,
    CliModelCatalog, CliProviderAdapter,
};
use crate::driver_host::cli_runtime::auth;
use crate::driver_host::cli_runtime::ids::{
    CliAccountProfile, CliAuthMode, CliProviderCapabilities, CliProviderId, CliTransportKind,
    HarnessIsolationMode, HarnessRequirements, ProbePolarity, QualificationProbe, ResumeStrategy,
};
use crate::driver_host::cli_runtime::qualification::{
    now_iso as now_iso_label, DEFAULT_NEGATIVE_PROBES, DEFAULT_POSITIVE_PROBES,
};
use crate::driver_host::contracts::{NormalizedEvent, NormalizedEventKind};

pub const PROVIDER_ID: &str = "cursor";
pub const DISPLAY_NAME: &str = "Cursor";
pub const TRANSPORT: CliTransportKind = CliTransportKind::Acp;

/// Canonical Cursor binary name on `PATH`. `discover` looks for this
/// executable; the launch spec re-uses whatever path `discover`
/// resolved (so a non-PATH install location still works).
pub const CURSOR_BINARY: &str = "cursor-agent";

/// URL of Cursor's browser login. The flow completes outside CodeRight. Authentication
/// is subsequently checked only through Cursor's structured CLI status.
pub const CURSOR_LOGIN_URL: &str = "https://cursor.com/login";

/// Resume strategy Cursor uses. ACP `session/load` re-attaches by
/// vendor session id; this is the documented "chat/session ID"
/// resume the architecture plan §14.7 names.
pub const CURSOR_RESUME_STRATEGY: ResumeStrategy = ResumeStrategy::ProtocolLoadById;

/// Cursor adapter. Implements the open
/// [`CliProviderAdapter`](crate::driver_host::cli_runtime::adapter::CliProviderAdapter)
/// contract for the Cursor Agent CLI.
///
/// The struct holds no per-call state; the adapter is stateless
/// across launches because all per-binding state lives on the
/// `CliLaunchRequest` the executor passes in.
pub struct CursorAdapter {
    provider_id: CliProviderId,
}

impl CursorAdapter {
    pub fn new() -> Self {
        Self {
            provider_id: CliProviderId::new(PROVIDER_ID),
        }
    }

    /// Transport this adapter drives. Re-exported as a const so
    /// callers can avoid importing the enum just to assert the kind.
    pub const TRANSPORT_KIND: CliTransportKind = TRANSPORT;
}

impl Default for CursorAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Cursor-specific live-capability probe: verify the Cursor binary
/// actually attaches the the host app MCP server through ACP
/// `session/new`. Versions of Cursor that accept `mcpServers` but
/// silently drop them fail this probe and disqualify the adapter.
pub static CURSOR_MCP_ATTACH_PROBE: LazyLock<QualificationProbe> =
    LazyLock::new(|| QualificationProbe {
        id: "pos.cursor_acp_mcp_attach".into(),
        description: "Cursor ACP accepts the Host MCP server in session/new and the \
                      attached tool list is non-empty."
            .into(),
        polarity: ProbePolarity::Positive,
        mandatory: true,
    });

/// Cursor-specific live-capability probe: verify `session/load`
/// returns the same vendor session id created by `session/new`.
/// Versions that advertise `session/load` but fail to actually
/// rehydrate the session fail this probe.
pub static CURSOR_SESSION_LOAD_PROBE: LazyLock<QualificationProbe> =
    LazyLock::new(|| QualificationProbe {
        id: "pos.cursor_acp_session_load".into(),
        description: "Cursor ACP session/load returns the same vendor session id as the \
                      preceding session/new."
            .into(),
        polarity: ProbePolarity::Positive,
        mandatory: true,
    });

/// Cached Cursor qualification suite. Built once at process start
/// by concatenating the default negative and positive probe suites
/// with the two Cursor-only live-capability probes. The resulting
/// slice is leaked into `'static` so the trait method can return
/// `&'static [QualificationProbe]`.
pub fn cursor_qualification_suite() -> &'static [QualificationProbe] {
    use std::sync::OnceLock;
    static CACHE: OnceLock<&'static [QualificationProbe]> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut probes: Vec<QualificationProbe> =
            Vec::with_capacity(DEFAULT_NEGATIVE_PROBES.len() + DEFAULT_POSITIVE_PROBES.len() + 2);
        probes.extend_from_slice(&DEFAULT_NEGATIVE_PROBES);
        probes.extend_from_slice(&DEFAULT_POSITIVE_PROBES);
        probes.push((*CURSOR_MCP_ATTACH_PROBE).clone());
        probes.push((*CURSOR_SESSION_LOAD_PROBE).clone());
        Box::leak(probes.into_boxed_slice())
    })
}

#[async_trait]
impl CliProviderAdapter for CursorAdapter {
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
        // The PR 10 discovery pass resolves the canonical Cursor
        // binary on the host. The runner treats an absent binary as
        // a soft failure (the provider surfaces as
        // `CliProviderEligibility::Unqualified` rather than as an
        // adapter error), so we never return a hard error here for
        // "not installed".
        match locate_cursor_binary().await {
            Some((executable, binary_version)) => {
                let executable_sha256 = super::executable_sha256(Some(&executable));
                Ok(CliDiscovery {
                    installed: true,
                    executable: Some(executable),
                    binary_version: Some(binary_version),
                    executable_sha256,
                    protocol_version: None,
                    transport: TRANSPORT,
                })
            }
            None => Ok(CliDiscovery {
                installed: false,
                executable: None,
                binary_version: None,
                executable_sha256: None,
                protocol_version: None,
                transport: TRANSPORT,
            }),
        }
    }

    async fn authentication_status(
        &self,
        profile: &CliAccountProfile,
    ) -> Result<CliAuthenticationStatus, CliAdapterError> {
        let executable = locate_cursor_binary()
            .await
            .map(|value| value.0)
            .ok_or_else(|| {
                CliAdapterError::new(
                    self.provider_id.clone(),
                    "cursor_binary_not_found",
                    "Cursor Agent CLI is not installed",
                )
            })?;
        let status = probe_cursor_auth_status(&executable, profile)
            .await
            .map_err(|message| {
                CliAdapterError::new(
                    self.provider_id.clone(),
                    "cursor_auth_status_unavailable",
                    message,
                )
            })?;
        Ok(CliAuthenticationStatus {
            signed_in: status.0,
            auth_mode: if status.0 {
                CliAuthMode::NativeCliAccount
            } else {
                CliAuthMode::Unknown
            },
            account_label: None,
            account_fingerprint: None,
            source_label: "cursor agent status --json".into(),
            observed_at: now_iso_label(),
        })
    }

    async fn list_models(
        &self,
        _profile: &CliAccountProfile,
    ) -> Result<CliModelCatalog, CliAdapterError> {
        Err(CliAdapterError::new(
            self.provider_id.clone(),
            "cursor_model_catalog_unavailable",
            "Cursor model list requires a qualified live agent protocol session",
        ))
    }

    async fn build_launch(
        &self,
        request: &CliLaunchRequest,
    ) -> Result<CliLaunchSpec, CliAdapterError> {
        if request.resume && request.session_id_seed.is_empty() {
            return Err(CliAdapterError::new(
                self.provider_id.clone(),
                "cursor_resume_missing_session",
                "resume=true requires a non-empty session_id_seed (Cursor chat/session id)",
            ));
        }
        reject_hostile_model_id(&self.provider_id, request.model_id.as_deref())?;

        let mut argv: Vec<String> = Vec::with_capacity(5);
        // ACP subcommand. The architecture plan §14.4 names
        // `agent acp` as the canonical Cursor ACP entry point;
        // `build_launch` only emits flags that the harness
        // controls. Flags that grant write/Bash (`--print`,
        // `--allow-write`, `--allow-shell`, `--print-mode=*`,
        // `--yolo`) are deliberately absent — the adapter denies
        // native write/Bash structurally rather than by prompt
        // instruction.
        argv.push("acp".to_string());
        if let Some(model) = &request.model_id {
            argv.push("--model".to_string());
            argv.push(model.clone());
        }
        if request.resume {
            argv.push("--resume".to_string());
            argv.push(request.session_id_seed.clone());
        }

        // The MCP server is passed to ACP through the
        // `session/new` JSON-RPC payload, not through argv. The
        // adapter attaches it to the spec so the transport forwards
        // it during session creation.
        let mcp_server = Some(request.mcp_server.clone());

        // Strip every BYOK and daemon secret. `CURSOR_API_KEY` and
        // `CURSOR_API_BASE_URL` are both members of the forbidden
        // list; an adapter that ever leaves either in the
        // environment has leaked the API-key path the architecture
        // plan forbids.
        let forbidden = auth::forbidden_env_var_set();
        let env_remove: Vec<String> = forbidden.into_iter().collect();

        Ok(CliLaunchSpec {
            provider_id: self.provider_id.clone(),
            transport: TRANSPORT,
            executable: request.executable.clone(),
            argv,
            env_keep: Vec::new(),
            env_remove,
            env: Vec::new(),
            cwd: request.profile.managed_home.clone(),
            mcp_server,
            profile_hash: profile_hash(&request.profile),
            // The open field surfaces the chat/session id verbatim
            // so the executor can hand it to the transport's
            // `session/load` on resume. A `resume:` prefix mirrors
            // the Claude encoding so downstream consumers do not
            // have to know the request shape.
            session_id: encode_cursor_session_id(request.resume, &request.session_id_seed),
        })
    }

    fn capabilities(&self) -> CliProviderCapabilities {
        CliProviderCapabilities {
            supports_session_resume: true,
            supports_model_discovery: true,
            // Browser-account login is the only path; the adapter
            // exposes `supports_native_account_login = true` so the
            // UI surfaces the Cursor login button. There is no
            // BYOK path.
            supports_native_account_login: true,
            supports_child_delegation: false,
            isolation_mode: HarnessIsolationMode::ExclusiveHostGateway,
        }
    }

    fn harness_requirements(&self) -> HarnessRequirements {
        // The Cursor adapter targets the exclusive the host app
        // gateway. Native subagent, shell, browser, and memory
        // routes are all forbidden — the architecture plan §14.6
        // explicitly names cloud handoff, background agents, native
        // subagents, provider web/browser tools, and native
        // shell/edit as denied routes.
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
        cursor_qualification_suite()
    }

    fn normalize_provider_event(
        &self,
        event: serde_json::Value,
    ) -> Result<Option<NormalizedEvent>, CliAdapterError> {
        // ACP carries events as JSON-RPC 2.0 notifications on the
        // `session/update` method. We translate the well-known
        // shapes into the open [`NormalizedEventKind`]; unknown
        // updates fail closed with a typed adapter error so the
        // executor never silently drops an ACP event.
        let object = event.as_object().ok_or_else(|| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "cursor_event_not_object",
                "ACP event root must be a JSON object",
            )
        })?;
        let method = object.get("method").and_then(serde_json::Value::as_str);
        // The Cursor adapter only sees notifications; JSON-RPC
        // responses are RPC bookkeeping and belong to the
        // transport's request waiter. Drop them with `Ok(None)` so
        // the executor does not surface them as visible events.
        if object.contains_key("id") && method.is_none() {
            return Ok(None);
        }
        let method = method.ok_or_else(|| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "cursor_event_missing_method",
                "ACP event is missing the `method` field",
            )
        })?;
        let params = object
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        translate_acp_notification(&self.provider_id, method, params)
    }
}

/// Translate a single ACP `session/update` notification into a
/// [`NormalizedEvent`]. The translation is intentionally narrow —
/// only the shapes Cursor ships today are accepted; unknown update
/// kinds fail closed so a future ACP extension cannot silently
/// surface a tool the harness has not approved.
fn translate_acp_notification(
    provider_id: &CliProviderId,
    method: &str,
    params: serde_json::Value,
) -> Result<Option<NormalizedEvent>, CliAdapterError> {
    match method {
        "session/update" => {
            let update = params.get("update").cloned().unwrap_or(params);
            let update_kind = update
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    CliAdapterError::new(
                        provider_id.clone(),
                        "cursor_session_update_missing_kind",
                        "ACP session/update payload is missing the `update.kind` field",
                    )
                })?;
            let update_object = update.as_object().cloned().unwrap_or_default();
            let kind = match update_kind {
                "agentMessageChunk" => NormalizedEventKind::AssistantDelta {
                    text: text_from_chunk(&update_object),
                },
                "agentMessageComplete" => NormalizedEventKind::AssistantCompleted {
                    text: text_from_chunk(&update_object),
                },
                "toolCall" => NormalizedEventKind::ToolCallStarted {
                    call_id: string_field(&update_object, "toolCallId"),
                    tool_name: string_field(&update_object, "toolName"),
                    arguments: update_object
                        .get("args")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                },
                "toolResult" => NormalizedEventKind::ToolResult {
                    call_id: string_field(&update_object, "toolCallId"),
                    is_error: update_object
                        .get("isError")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                    content: update_object
                        .get("content")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                },
                "approvalRequest" => NormalizedEventKind::ApprovalRequested {
                    request_id: string_field(&update_object, "approvalId"),
                    action: string_field(&update_object, "action"),
                    details: update_object
                        .get("details")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null),
                },
                "sessionStarted" => NormalizedEventKind::SessionStarted {
                    vendor_session_id: string_field(&update_object, "sessionId"),
                    init: None,
                },
                "turnStarted" => NormalizedEventKind::TurnStarted {
                    vendor_turn_id: string_field(&update_object, "turnId"),
                },
                "turnCompleted" => NormalizedEventKind::TurnCompleted,
                "turnInterrupted" => NormalizedEventKind::TurnInterrupted,
                "turnFailed" => NormalizedEventKind::TurnFailed {
                    message: string_field(&update_object, "message"),
                },
                "gatewayReady" => NormalizedEventKind::GatewayReady,
                other => {
                    return Err(CliAdapterError::new(
                        provider_id.clone(),
                        "cursor_unknown_session_update",
                        format!("ACP session/update kind {other:?} is not handled"),
                    ));
                }
            };
            Ok(Some(NormalizedEvent {
                event_key: format!("cursor:0:{method}:{update_kind}"),
                driver: crate::driver_host::contracts::DriverKind::Codex,
                source_offset: 0,
                vendor_method: method.to_string(),
                kind,
            }))
        }
        "auth/required" => {
            // Cursor signals that re-authentication is needed.
            // Surface as a turn failure so the executor surfaces a
            // repair message instead of letting the binding hang.
            let message = params
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Cursor requested re-authentication");
            Ok(Some(NormalizedEvent {
                event_key: "cursor:0:auth/required".to_string(),
                driver: crate::driver_host::contracts::DriverKind::Codex,
                source_offset: 0,
                vendor_method: method.to_string(),
                kind: NormalizedEventKind::TurnFailed {
                    message: message.to_string(),
                },
            }))
        }
        other => {
            // Unknown notification methods fail closed. The Codex
            // adapter does the same thing; consistency matters
            // because the executor assumes an adapter error means
            // "stop trusting this stream" rather than "ignore the
            // unknown shape and continue".
            Err(CliAdapterError::new(
                provider_id.clone(),
                "cursor_unknown_notification",
                format!("ACP notification method {other:?} is not handled by the Cursor adapter"),
            ))
        }
    }
}

fn text_from_chunk(object: &serde_json::Map<String, serde_json::Value>) -> String {
    object
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn string_field(object: &serde_json::Map<String, serde_json::Value>, field: &str) -> String {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Hash a profile into the per-launch `profile_hash` the spec
/// carries. Matches the shape used by the Claude and Codex
/// adapters so the supervisor detects a profile swap uniformly
/// across providers.
fn profile_hash(profile: &CliAccountProfile) -> String {
    let mut h = rightkit_fs::digest::Sha256Hasher::new();
    h.update(profile.id.as_str().as_bytes());
    h.update(b"\0");
    if let Some(fp) = &profile.account_fingerprint {
        h.update(fp.as_bytes());
    }
    let digest = h.finalize_bytes();
    rightkit_crypto::hex_encode(&digest)
        .chars()
        .take(16)
        .collect()
}

/// Encode the open session-id field into the wire form the
/// transport consumes. A `resume:` prefix distinguishes a
/// load-by-id request from a fresh session; the seed itself is the
/// chat/session id (Cursor's ACP and NDJSON both use the same id
/// space, so we do not split the encoding by transport).
fn encode_cursor_session_id(resume: bool, seed: &str) -> String {
    if resume {
        format!("resume:{seed}")
    } else {
        seed.to_string()
    }
}

/// Reject model ids that contain characters the Cursor argv parser
/// would interpret as shell metacharacters. The check is deliberately
/// conservative: the harness refuses to forward anything outside
/// `[A-Za-z0-9._:/+-]`.
fn reject_hostile_model_id(
    provider_id: &CliProviderId,
    model_id: Option<&str>,
) -> Result<(), CliAdapterError> {
    if let Some(id) = model_id {
        if !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '+' | '-'))
        {
            return Err(CliAdapterError::new(
                provider_id.clone(),
                "cursor_launch_hostile_model",
                format!("model id {id:?} contains characters the Cursor argv parser rejects"),
            ));
        }
    }
    Ok(())
}

/// Locate the canonical Cursor binary on the host. Returns the
/// resolved path and `--version` output (best-effort). The check is
/// deliberately conservative: the harness treats "not found" as a
/// soft failure surfaced through `CliDiscovery.installed = false`
/// rather than as an error.
async fn locate_cursor_binary() -> Option<(PathBuf, String)> {
    let path = crate::driver_host::binary_lookup::find_on_path(CURSOR_BINARY)?;
    let version = read_cursor_version(&path).await.unwrap_or_default();
    Some((path, version))
}

async fn read_cursor_version(path: &std::path::Path) -> std::io::Result<String> {
    let root = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: root.to_string_lossy().into_owned(),
        sandbox_profile: Some("cursor-probe".into()),
        sandbox_required: true,
        network_required: false,
    };
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program: path.to_string_lossy().into_owned(),
        argv: vec!["--version".into()],
        cwd: context.workspace_root.clone(),
        context,
        env: Vec::new(),
    };
    let (status, bytes) = crate::driver_host::binary_lookup::run_bounded_probe(
        spec,
        crate::driver_host::binary_lookup::VERSION_PROBE_TIMEOUT,
        "--version",
    )
    .await
    .map_err(std::io::Error::other)?;
    if status != 0 {
        return Err(std::io::Error::other(format!(
            "--version exited with {}",
            status
        )));
    }
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

async fn probe_cursor_auth_status(
    executable: &std::path::Path,
    profile: &CliAccountProfile,
) -> Result<(bool,), String> {
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: profile.managed_home.to_string_lossy().into_owned(),
        sandbox_profile: Some("cursor-auth".into()),
        sandbox_required: true,
        network_required: false,
    };
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program: executable.display().to_string(),
        argv: vec!["agent".into(), "status".into(), "--json".into()],
        cwd: context.workspace_root.clone(),
        context,
        env: vec![(
            "HOME".into(),
            profile.managed_home.to_string_lossy().into_owned(),
        )],
    };
    // Spawn, read and wait share one bound; a timeout kills the child and maps to the same
    // "unavailable" error shape as any other probe failure.
    let (status, output) = crate::driver_host::binary_lookup::run_bounded_probe(
        spec,
        crate::driver_host::binary_lookup::VERSION_PROBE_TIMEOUT,
        "Cursor status probe",
    )
    .await?;
    if status != 0 {
        return Err(format!("Cursor status exited with {status}"));
    }
    let value: serde_json::Value = serde_json::from_slice(&output)
        .map_err(|error| format!("Cursor status was not JSON: {error}"))?;
    Ok((value
        .get("signedIn")
        .or_else(|| value.get("authenticated"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false),))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::cli_runtime::adapter::CliMcpServerSpec;
    use crate::driver_host::cli_runtime::ids::CliAccountProfileId;
    use std::path::PathBuf;

    fn sample_profile() -> CliAccountProfile {
        CliAccountProfile {
            id: CliAccountProfileId::new("cursor/default"),
            provider_id: CliProviderId::new(PROVIDER_ID),
            label: "default".into(),
            managed_home: PathBuf::from("/tmp/rightkit-agent/cli-profiles/cursor/default"),
            auth_mode: CliAuthMode::NativeCliAccount,
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

    fn sample_request(model: Option<&str>, resume: bool) -> CliLaunchRequest {
        CliLaunchRequest {
            provider_id: CliProviderId::new(PROVIDER_ID),
            executable: PathBuf::from(format!("/usr/local/bin/{CURSOR_BINARY}")),
            profile: sample_profile(),
            model_id: model.map(str::to_string),
            session_id_seed: if resume {
                "existing-cursor-session".into()
            } else {
                "550e8400-e29b-41d4-a716-446655440000".into()
            },
            mcp_server: sample_mcp(),
            policy: String::new(),
            resume,
        }
    }

    /// Identity round-trip: id() and display_name() must match the
    /// canonical strings; transport_kind() must be ACP.
    #[test]
    fn identity_matches_canonical_strings() {
        let adapter = CursorAdapter::new();
        assert_eq!(adapter.id().as_str(), PROVIDER_ID);
        assert_eq!(adapter.display_name(), DISPLAY_NAME);
        assert_eq!(adapter.transport_kind(), TRANSPORT);
    }

    /// PR 10 acceptance #1: build_launch must include the
    /// `apiKeySource == login` assertion by **never** emitting
    /// `--api-key` anywhere in argv. The Cursor adapter has no
    /// API-key path at all.
    #[tokio::test]
    async fn build_launch_argv_never_contains_api_key_flag() {
        let adapter = CursorAdapter::new();
        let request = sample_request(None, false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        for token in &spec.argv {
            assert_ne!(
                token, "--api-key",
                "Cursor adapter must never emit --api-key (apiKeySource == login only)"
            );
            assert!(
                !token.starts_with("--api-key="),
                "Cursor adapter must never emit --api-key=* (apiKeySource == login only)"
            );
        }
    }

    /// PR 10 acceptance #2: build_launch must strip every Cursor
    /// API-key / endpoint variable so they cannot leak into the
    /// subprocess environment.
    #[tokio::test]
    async fn build_launch_env_remove_includes_cursor_api_key_vars() {
        let adapter = CursorAdapter::new();
        let request = sample_request(None, false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        for forbidden in ["CURSOR_API_KEY", "CURSOR_API_BASE_URL"] {
            assert!(
                spec.env_remove.contains(&forbidden.to_string()),
                "adapter must strip {forbidden}"
            );
        }
        // The spec must never carry CURSOR_API_KEY in the explicit
        // env map either; the strip list is the single source of
        // truth.
        for (k, _) in &spec.env {
            assert_ne!(k, "CURSOR_API_KEY", "env must not contain CURSOR_API_KEY");
            assert_ne!(
                k, "CURSOR_API_BASE_URL",
                "env must not contain CURSOR_API_BASE_URL"
            );
        }
    }

    /// PR 10 acceptance #3: native write/Bash must be denied
    /// structurally. The argv shape the adapter emits must not
    /// carry `--print` (Cursor print mode has all-tool access), nor
    /// any `--allow-*` flag that would re-grant a denied route.
    #[tokio::test]
    async fn build_launch_denies_native_write_and_bash() {
        let adapter = CursorAdapter::new();
        let request = sample_request(None, false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        for token in &spec.argv {
            assert_ne!(
                token, "--print",
                "Cursor adapter must not enter print mode (all-tool access)"
            );
            assert_ne!(
                token, "--yolo",
                "Cursor adapter must not pass --yolo (grants native write/Bash)"
            );
            assert!(
                !token.starts_with("--allow-write"),
                "Cursor adapter must not grant write"
            );
            assert!(
                !token.starts_with("--allow-shell"),
                "Cursor adapter must not grant shell"
            );
            assert!(
                !token.starts_with("--allow-bash"),
                "Cursor adapter must not grant bash"
            );
        }
        // argv[0] must be the ACP subcommand — the only safe entry point.
        assert_eq!(spec.argv.first().map(String::as_str), Some("acp"));
    }

    /// PR 10 acceptance #4: resume must retain the chat/session
    /// id. argv must carry the original session_id_seed as the
    /// value of --resume, and the open session_id field must carry
    /// the same id with a `resume:` prefix.
    #[tokio::test]
    async fn build_launch_resume_retains_chat_session_id() {
        let adapter = CursorAdapter::new();
        let request = sample_request(None, true);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        assert!(spec
            .argv
            .windows(2)
            .any(|pair| pair == ["--resume", "existing-cursor-session"]));
        assert_eq!(spec.session_id, "resume:existing-cursor-session");
    }

    /// Resume without a session id is a hard error.
    #[tokio::test]
    async fn build_launch_rejects_resume_without_session_seed() {
        let adapter = CursorAdapter::new();
        let mut request = sample_request(None, true);
        request.session_id_seed = String::new();
        let error = adapter.build_launch(&request).await.unwrap_err();
        assert_eq!(error.code, "cursor_resume_missing_session");
    }

    /// Hostile model ids are rejected with a typed adapter error.
    #[tokio::test]
    async fn build_launch_rejects_hostile_model_id() {
        let adapter = CursorAdapter::new();
        let request = sample_request(Some("cursor; rm -rf /"), false);
        let error = adapter.build_launch(&request).await.unwrap_err();
        assert_eq!(error.code, "cursor_launch_hostile_model");
    }

    /// Harness requirements reflect every Cursor-specific
    /// restriction from §14.6.
    #[test]
    fn harness_requirements_match_cursor_policy() {
        let adapter = CursorAdapter::new();
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

    /// Capabilities target the exclusive gateway.
    #[test]
    fn capabilities_target_exclusive_host_gateway() {
        let adapter = CursorAdapter::new();
        let caps = adapter.capabilities();
        assert_eq!(
            caps.isolation_mode,
            HarnessIsolationMode::ExclusiveHostGateway
        );
        assert!(caps.supports_native_account_login);
    }

    /// Qualification suite includes the two Cursor-only live-
    /// capability probes plus the default probes.
    #[test]
    fn qualification_suite_includes_cursor_live_capability_probes() {
        let suite = CursorAdapter::new().qualification_suite();
        let ids: std::collections::BTreeSet<&str> = suite.iter().map(|p| p.id.as_str()).collect();
        for required in [
            "pos.cursor_acp_mcp_attach",
            "pos.cursor_acp_session_load",
            "pos.rightkit_mcp_discovery",
            "pos.resume",
            "neg.direct_native_file_write",
            "neg.direct_native_shell",
        ] {
            assert!(
                ids.contains(required),
                "qualification suite must include {required}"
            );
        }
    }

    /// `agentMessageChunk` notifications surface as AssistantDelta.
    #[test]
    fn normalize_provider_event_translates_agent_message_chunk() {
        let adapter = CursorAdapter::new();
        let event: serde_json::Value = serde_json::from_str(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-1","update":{"kind":"agentMessageChunk","text":"hello"}}}"#,
        ).unwrap();
        let normalized = adapter
            .normalize_provider_event(event)
            .unwrap()
            .expect("must normalize");
        assert_eq!(
            normalized.kind,
            NormalizedEventKind::AssistantDelta {
                text: "hello".into()
            }
        );
        assert_eq!(normalized.vendor_method, "session/update");
    }

    /// JSON-RPC response frames drop to `None` — they are RPC
    /// bookkeeping, not the event stream.
    #[test]
    fn normalize_provider_event_drops_response_messages() {
        let adapter = CursorAdapter::new();
        let event: serde_json::Value =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#).unwrap();
        let normalized = adapter.normalize_provider_event(event).unwrap();
        assert!(normalized.is_none());
    }

    /// Unknown `session/update` kinds fail closed.
    #[test]
    fn normalize_provider_event_fails_closed_on_unknown_update() {
        let adapter = CursorAdapter::new();
        let event: serde_json::Value = serde_json::from_str(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"kind":"futureUnknownKind"}}}"#,
        ).unwrap();
        let error = adapter.normalize_provider_event(event).unwrap_err();
        assert_eq!(error.code, "cursor_unknown_session_update");
    }

    /// `auth/required` notifications surface as a turn failure so
    /// the executor renders a repair message instead of letting the
    /// binding hang.
    #[test]
    fn normalize_provider_event_surfaces_auth_required_as_failure() {
        let adapter = CursorAdapter::new();
        let event: serde_json::Value = serde_json::from_str(
            r#"{"jsonrpc":"2.0","method":"auth/required","params":{"message":"session expired"}}"#,
        )
        .unwrap();
        let normalized = adapter
            .normalize_provider_event(event)
            .unwrap()
            .expect("must normalize");
        assert_eq!(
            normalized.kind,
            NormalizedEventKind::TurnFailed {
                message: "session expired".into()
            }
        );
    }

    /// The Cursor resume strategy is `ProtocolLoadById` per §14.7.
    /// Drift here breaks the "same chat ID resumes" guarantee.
    #[test]
    fn resume_strategy_is_protocol_load_by_id() {
        assert_eq!(CURSOR_RESUME_STRATEGY, ResumeStrategy::ProtocolLoadById);
    }
}
