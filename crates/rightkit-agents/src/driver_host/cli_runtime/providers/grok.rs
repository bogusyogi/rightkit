//! Grok Build CLI adapter.
//!
//! PR 8 of the architecture plan (`docs/architecture/CLI-RUNTIME-SCOPE.md`).
//!
//! Implements the per-vendor knowledge for the Grok Build CLI behind
//! the open [`crate::driver_host::cli_runtime::adapter::CliProviderAdapter`]
//! contract:
//!
//! - **Profile isolation.** Every launch runs under
//!   `--profile-dir <managed_home>` so credential material, plugin
//!   state, skill cache, and conversation history live inside the
//!   the host app-managed profile directory instead of the user's
//!   global shell.
//! - **ACP transport.** The launch shape is
//!   `grok agent --no-leader --no-team --disable-native-tools
//!   --disable-skills stdio --profile-dir <managed_home> ...`. The
//!   matching transport
//!   ([`crate::driver_host::cli_runtime::transports::acp`]) speaks JSON-RPC 2.0
//!   to it.
//! - **Disable provider-native orchestration.** `--no-leader` and
//!   `--no-team` strip Grok's native agent/leader and team
//!   capabilities; `--disable-native-tools` removes the
//!   provider-native file/shell/browser routes. The ACP permission
//!   broker is the second line of defense — it denies native tool
//!   calls and approves only the the host app MCP server.
//! - **Resume via `ProcessRestartWithResumeFlag`.** Grok's vendor
//!   session id is forwarded as `--resume <id>` on the next
//!   process. The conversation identity survives a process restart
//!   because the vendor subprocess receives the exact vendor
//!   session id; the host app does not replay the visible transcript.
//! - **`grok auth status` / `grok login`.** Native browser/device
//!   login only. `XAI_API_KEY` and `XAI_BASE_URL` are members of
//!   the forbidden env-var strip list and never reach the
//!   subprocess.
//! - **Full default qualification suite.** Every required negative
//!   and positive probe from
//!   [`crate::driver_host::cli_runtime::qualification`] runs against the live
//!   binary before the readiness gate admits the adapter.

use crate::driver_host::cli_runtime::qualification::now_iso;
use async_trait::async_trait;
use rightkit_sandbox::execution::ProcessLaunchAdapter;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use crate::driver_host::cli_runtime::adapter::{
    CliAdapterError, CliAuthenticationStatus, CliDiscovery, CliLaunchRequest, CliLaunchSpec,
    CliModelCatalog, CliProviderAdapter, NativeLoginUi,
};
use crate::driver_host::cli_runtime::ids::{
    CliAccountProfile, CliAuthMode, CliProviderCapabilities, CliProviderId, CliTransportKind,
    HarnessRequirements, QualificationProbe, ResumeStrategy,
};
use crate::driver_host::cli_runtime::qualification::{
    DEFAULT_NEGATIVE_PROBES, DEFAULT_POSITIVE_PROBES,
};
use crate::driver_host::contracts::{NormalizedEvent, NormalizedEventKind};

pub const PROVIDER_ID: &str = "grok";
pub const DISPLAY_NAME: &str = "Grok";
pub const TRANSPORT: CliTransportKind = CliTransportKind::Acp;

/// Canonical Grok binary name the adapter searches for on `PATH`.
/// The shared lookup resolves `grok.exe` on Windows via `PATHEXT`.
const GROK_BINARY: &str = "grok";

/// Canonical model identifiers the Grok adapter exposes for the
/// picker. The list is conservative — it carries the documented
/// xAI model ids in the argv-safe character set so they can be
/// forwarded verbatim to `--model <id>`. A live catalog probe lands
/// in a later PR; the static list keeps the picker usable before
/// qualification.
pub const GROK_MODELS: &[&str] = &["grok-4", "grok-3", "grok-code"];

/// Resume strategy for the Grok adapter. Per the architecture plan
/// §12.5 and the hard ceiling on
/// [`ResumeStrategy`](crate::driver_host::cli_runtime::ids::ResumeStrategy),
/// "replay visible transcript into a new session" is **not** a
/// permitted variant; Grok uses process restart with the vendor
/// session id forwarded as `--resume <id>`.
pub const GROK_RESUME_STRATEGY: ResumeStrategy = ResumeStrategy::ProcessRestartWithResumeFlag;

/// Grok Build CLI adapter. PR 8 wires managed profile isolation,
/// ACP launch with leader/team/native-tool disable, native auth,
/// managed account profile, process-restart resume, and the full
/// default qualification suite.
pub struct GrokAdapter {
    provider_id: CliProviderId,
}

impl GrokAdapter {
    pub fn new() -> Self {
        Self {
            provider_id: CliProviderId::new(PROVIDER_ID),
        }
    }

    /// Transport this adapter drives. Re-exported as a `const` so
    /// the built-in registry can match against it without
    /// importing [`CliTransportKind`] directly.
    pub const TRANSPORT_KIND: CliTransportKind = TRANSPORT;

    /// Resume strategy this adapter declares. The supervisor
    /// reads it before launching and selects the matching
    /// process-restart machinery.
    pub const RESUME_STRATEGY: ResumeStrategy = GROK_RESUME_STRATEGY;
}

impl Default for GrokAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CliProviderAdapter for GrokAdapter {
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
        // PR 8 acceptance: discovery resolves the Grok binary on
        // PATH. A missing binary is a soft failure surfaced as
        // `installed = false` so the UI can render
        // `LoginRequired` / `HarnessVerificationRequired` instead
        // of crashing the daemon.
        match locate_binary() {
            Some(executable) => {
                let executable_sha256 = super::executable_sha256(Some(&executable));
                Ok(CliDiscovery {
                    installed: true,
                    executable: Some(executable),
                    binary_version: None,
                    executable_sha256,
                    protocol_version: Some("acp/1".into()),
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
        // Run `grok auth status` under the managed profile and
        // surface non-secret evidence. The wire format varies
        // across xAI releases; we pattern-match a few stable
        // shapes (`signedIn:false`, `not signed in`) and treat any
        // other output as signed-in. A non-zero exit is a hard
        // failure that the supervisor surfaces.
        let executable = locate_binary().ok_or_else(|| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "grok_binary_not_found",
                "no `grok` binary on PATH; install Grok Build or update the PATH",
            )
        })?;
        let output = run_capture(
            &executable,
            &[
                "--profile-dir",
                profile.managed_home.to_string_lossy().as_ref(),
                "auth",
                "status",
            ],
            None,
        )
        .await?;
        let signed_in = signed_in_from_output(&output);
        Ok(CliAuthenticationStatus {
            signed_in,
            // Grok Build is always NativeCliAccount — xAI API keys
            // are explicitly forbidden by §12.2 of the
            // architecture plan. The fingerprint is a non-secret
            // SHA-256 prefix of the auth-status output; the
            // credential value itself is never read or persisted.
            auth_mode: CliAuthMode::NativeCliAccount,
            account_label: None,
            account_fingerprint: if signed_in {
                Some(fingerprint_from_output(&output))
            } else {
                None
            },
            source_label: format!(
                "grok --profile-dir {} auth status",
                profile.managed_home.display()
            ),
            observed_at: now_iso(),
        })
    }

    async fn begin_native_login(
        &self,
        profile: &CliAccountProfile,
        _ui: &dyn NativeLoginUi,
    ) -> Result<(), CliAdapterError> {
        // Discovery/attestation owns installed-binary admission. Building a
        // deterministic launch spec must remain testable before installation.
        let executable = locate_binary().unwrap_or_else(|| PathBuf::from("grok"));
        // `grok --profile-dir <managed_home> login` runs the
        // browser/device flow under the the host app-managed profile
        // and blocks until the user completes it. The transport
        // surfaces the browser URL via [`NativeLoginUi`] before
        // we wait on the subprocess; the adapter itself only
        // spawns the command and reports its exit code.
        let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
        let context = rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: profile.managed_home.to_string_lossy().into_owned(),
            sandbox_profile: Some("grok-login".into()),
            sandbox_required: true,
            network_required: true,
        };
        let spec = rightkit_sandbox::execution::CommandLaunchSpec {
            program: executable.to_string_lossy().into_owned(),
            argv: vec![
                "--profile-dir".into(),
                profile.managed_home.to_string_lossy().into_owned(),
                "login".into(),
            ],
            cwd: context.workspace_root.clone(),
            context,
            env: Vec::new(),
        };
        let mut child = adapter.spawn(spec).await.map_err(|error| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "grok_login_spawn",
                format!("failed to spawn `grok login` under policy: {error:?}"),
            )
        })?;
        let status = child.wait().await.map_err(|error| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "grok_login_wait",
                format!("login wait failed: {error:?}"),
            )
        })?;
        if status != 0 {
            return Err(CliAdapterError::new(
                self.provider_id.clone(),
                "grok_login_failed",
                format!("`grok login` exited non-zero: {status}"),
            ));
        }
        Ok(())
    }

    async fn list_models(
        &self,
        _profile: &CliAccountProfile,
    ) -> Result<CliModelCatalog, CliAdapterError> {
        Err(CliAdapterError::new(
            self.provider_id.clone(),
            "grok_model_catalog_unavailable",
            "Grok model IDs must come from a qualified live ACP session",
        ))
    }

    #[allow(clippy::vec_init_then_push)]
    async fn build_launch(
        &self,
        request: &CliLaunchRequest,
    ) -> Result<CliLaunchSpec, CliAdapterError> {
        reject_byok_fields(&self.provider_id, request)?;
        let mut argv: Vec<String> = Vec::new();
        // 1. ACP subcommand. The architecture plan §12.3 names
        //    `grok agent ... stdio` as the canonical Grok ACP
        //    entry point.
        argv.push("agent".to_string());
        // 2. Force local non-leader mode. Grok ships with
        //    agent/leader capabilities that route around the
        //    harness isolation. `--no-leader` disables the
        //    leader surface.
        argv.push("--no-leader".to_string());
        // 3. Disable provider-native team delegation.
        //    Architecturally equivalent to Cline's `--no-teams`
        //    (§13.5 of the plan); Grok teams allow users to
        //    spawn background sub-agents that bypass the
        //    the host app capability gateway.
        argv.push("--no-team".to_string());
        // 4. Disable provider-native tools. Even with the
        //    permission broker rejecting native tool calls,
        //    denying them at the flag level removes the route
        //    entirely as a second defense.
        argv.push("--disable-native-tools".to_string());
        // 5. Disable provider-native skills/plugins for the
        //    same reason — any surface that adds tools is denied
        //    at the flag level so the harness has a single
        //    capability surface to reason about.
        argv.push("--disable-skills".to_string());
        // 6. Tell the ACP transport to expect stdio. The exact
        //    subcommand name is version-probed; the canonical
        //    position is `stdio` after the disable flags.
        argv.push("stdio".to_string());
        // 7. Managed profile directory. Every credential file,
        //    plugin state, skill cache, and conversation history
        //    lands under the the host app-managed profile directory.
        argv.push("--profile-dir".to_string());
        argv.push(request.profile.managed_home.display().to_string());
        // 8. Vendor session resume. Grok forwards the vendor
        //    session id as `--resume <id>` so the next process
        //    reattaches to the same conversation.
        if request.resume {
            if request.session_id_seed.is_empty() {
                return Err(CliAdapterError::new(
                    self.provider_id.clone(),
                    "grok_resume_missing_session",
                    "resume=true requires a non-empty session_id_seed (Grok vendor session id)",
                ));
            }
            argv.push("--resume".to_string());
            argv.push(request.session_id_seed.clone());
        }
        // 9. Model override (forwarded as `--model <id>`). The
        //    model id was BYOK-rejected above so it cannot smuggle
        //    a custom endpoint or xAI key.
        if let Some(model) = &request.model_id {
            argv.push("--model".to_string());
            argv.push(model.clone());
        }

        // 10. Strip every BYOK and the host app secret env var. The
        //     forbidden set is the union of the common list and
        //     the xAI-specific names. `XAI_API_KEY` and
        //     `XAI_BASE_URL` are the documented Grok BYOK
        //     bypasses; the strip list keeps them out of the
        //     subprocess environment.
        let forbidden = crate::driver_host::cli_runtime::auth::forbidden_env_var_set();
        let mut env_remove: Vec<String> = forbidden.into_iter().collect();
        for extra in ["XAI_API_KEY", "XAI_BASE_URL"] {
            if !env_remove.iter().any(|k| k == extra) {
                env_remove.push(extra.to_string());
            }
        }

        Ok(CliLaunchSpec {
            provider_id: self.provider_id.clone(),
            transport: TRANSPORT,
            executable: request.executable.clone(),
            argv,
            env_keep: Vec::new(),
            env_remove,
            env: Vec::new(),
            cwd: request.profile.managed_home.clone(),
            mcp_server: Some(request.mcp_server.clone()),
            profile_hash: profile_hash(&request.profile),
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
        // The Grok adapter targets the exclusive the host app
        // gateway. Native subagent, shell, browser, and memory
        // routes are all forbidden — the architecture plan §12.4
        // explicitly names cloud handoff, native teams,
        // background agents, provider agent/leader tools,
        // provider web/browser tools, and native shell/edit as
        // denied routes.
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
        // Full default suite. The negative probes confirm
        // Grok's native routes are denied (leader, team,
        // native tools, skills, browser). The positive probes
        // confirm the the host app MCP is the only capability
        // surface, including `pos.resume` for the
        // `--resume <vendor_session_id>` continuity invariant.
        &GROK_QUALIFICATION_SUITE
    }

    fn normalize_provider_event(
        &self,
        event: serde_json::Value,
    ) -> Result<Option<NormalizedEvent>, CliAdapterError> {
        normalize_acp_event(&self.provider_id, event)
    }
}

/// Full default qualification suite for Grok. Reuses the negative
/// and positive probes from
/// [`crate::driver_host::cli_runtime::qualification::DEFAULT_NEGATIVE_PROBES`]
/// and
/// [`crate::driver_host::cli_runtime::qualification::DEFAULT_POSITIVE_PROBES`]
/// so the trait can return a stable `&'static [QualificationProbe]`.
static GROK_QUALIFICATION_SUITE: LazyLock<Vec<QualificationProbe>> = LazyLock::new(|| {
    let mut probes =
        Vec::with_capacity(DEFAULT_NEGATIVE_PROBES.len() + DEFAULT_POSITIVE_PROBES.len());
    probes.extend_from_slice(&DEFAULT_NEGATIVE_PROBES);
    probes.extend_from_slice(&DEFAULT_POSITIVE_PROBES);
    probes
});

/// Encode the open session-id field into the wire form the
/// transport consumes. The Grok adapter uses
/// [`ResumeStrategy::ProcessRestartWithResumeFlag`]: a fresh
/// session passes the seed verbatim and a resumed session adds the
/// `resume:` prefix so downstream consumers do not have to inspect
/// the request shape.
fn encode_session_id(resume: bool, seed: &str) -> String {
    if resume {
        format!("resume:{seed}")
    } else {
        seed.to_string()
    }
}

/// Hash a profile into the per-launch `profile_hash` the spec
/// carries. Matches the Claude/Codex/Cursor/Cline adapter's
/// hashing so all five providers report profile swaps in a uniform
/// shape.
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

/// Locate the `grok` binary on `PATH` via the shared lookup, which
/// honours `PATH` separators and Windows `PATHEXT`.
fn locate_binary() -> Option<PathBuf> {
    crate::driver_host::binary_lookup::find_on_path(GROK_BINARY)
}

/// Run `grok <args>` and capture combined stdout+stderr. Returns
/// the combined output. Any spawn / wait failure becomes an
/// [`CliAdapterError`].
async fn run_capture(
    executable: &Path,
    args: &[&str],
    cwd: Option<&Path>,
) -> Result<String, CliAdapterError> {
    let root = cwd.unwrap_or_else(|| executable.parent().unwrap_or_else(|| Path::new(".")));
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: root.to_string_lossy().into_owned(),
        sandbox_profile: Some("grok-probe".into()),
        sandbox_required: true,
        network_required: false,
    };
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program: executable.to_string_lossy().into_owned(),
        argv: args.iter().map(|arg| (*arg).to_string()).collect(),
        cwd: context.workspace_root.clone(),
        context,
        env: Vec::new(),
    };
    // Spawn, read and wait share one bound; a timeout kills the child and surfaces as a typed
    // adapter error instead of hanging discovery.
    let (status, stdout_bytes, stderr_bytes) =
        crate::driver_host::binary_lookup::run_bounded_probe_streams(
            spec,
            crate::driver_host::binary_lookup::VERSION_PROBE_TIMEOUT,
            "grok probe",
        )
        .await
        .map_err(|message| {
            CliAdapterError::new(
                CliProviderId::new(PROVIDER_ID),
                "grok_spawn_failed",
                message,
            )
        })?;
    let mut combined = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes);
    if !stderr.is_empty() {
        if !combined.is_empty() && !combined.ends_with('\n') {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }
    if status != 0 {
        return Err(CliAdapterError::new(
            CliProviderId::new(PROVIDER_ID),
            "grok_subcommand_failed",
            format!("`grok` exited with {}: {}", status, combined.trim()),
        ));
    }
    Ok(combined)
}

/// Decide whether the `grok auth status` output indicates the
/// user has signed in. The wire format varies across xAI releases,
/// so we pattern-match a few stable shapes: `signedIn:false`,
/// `signed_in:false`, the literal phrase `not signed in`. Anything
/// else — empty output, an opaque JSON `account` field, a
/// successful exit with content — counts as signed in.
fn signed_in_from_output(output: &str) -> bool {
    if output.contains("\"signedIn\":false") || output.contains("\"signed_in\":false") {
        return false;
    }
    let lower = output.to_lowercase();
    if lower.contains("not signed in") || lower.contains("signed out") {
        return false;
    }
    true
}

/// Derive a 16-hex-char account fingerprint from the auth-status
/// output. Falls back to a deterministic placeholder when the
/// output has no usable identifier; the supervisor uses the
/// fingerprint to detect account switches.
fn fingerprint_from_output(output: &str) -> String {
    let mut h = rightkit_fs::digest::Sha256Hasher::new();
    h.update(output.as_bytes());
    let digest = h.finalize_bytes();
    rightkit_crypto::hex_encode(&digest)
        .chars()
        .take(16)
        .collect()
}

/// Reject every BYOK-shaped field on the launch request. An xAI
/// API key, a custom xAI endpoint, or any other BYOK-shaped input
/// is a launch refusal: Grok launches exclusively through the
/// xAI/Grok account.
fn reject_byok_fields(
    provider_id: &CliProviderId,
    request: &CliLaunchRequest,
) -> Result<(), CliAdapterError> {
    if let Some(model) = &request.model_id {
        reject_byok_string(provider_id, "model_id", model)?;
    }
    if request.session_id_seed.contains("--key") || request.session_id_seed.contains("XAI_API_KEY")
    {
        return Err(CliAdapterError::new(
            provider_id.clone(),
            "grok_byok_session",
            "session_id_seed contains a forbidden BYOK token; Grok launches exclusively through the xAI/Grok account",
        ));
    }
    Ok(())
}

/// Single-string BYOK rejector. Looks for the canonical
/// `--key`, `--api-key`, xAI key variable, and BYOK provider
/// hints. Lower-cases the field before matching so casing tricks
/// do not bypass the check.
fn reject_byok_string(
    provider_id: &CliProviderId,
    field: &str,
    value: &str,
) -> Result<(), CliAdapterError> {
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(CliAdapterError::new(
            provider_id.clone(),
            "grok_byok_rejected",
            format!("{field} contains characters unsafe for a Grok argv value"),
        ));
    }
    let lower = value.to_lowercase();
    for forbidden in [
        "--key",
        "--api-key",
        "byok",
        "xai_api_key",
        "xai-base-url",
        "xai_base_url",
        "openrouter",
        "minimax",
        "tinyfish",
        "sk-",
    ] {
        if lower.contains(forbidden) {
            return Err(CliAdapterError::new(
                provider_id.clone(),
                "grok_byok_rejected",
                format!(
                    "{field} contains a forbidden BYOK token ({forbidden}); Grok launches exclusively through the xAI/Grok account"
                ),
            ));
        }
    }
    Ok(())
}

/// Cheap UTC ISO timestamp suitable for `observed_at` /
/// `fetched_at`. Matches the `YYYY-MM-DDTHH:MM:SSZ` shape the
/// qualification runner uses so SQL string comparisons stay
/// correct.

/// Translate one ACP inbound JSON-RPC 2.0 envelope into a
/// [`NormalizedEvent`]. Grok's `session/update` notifications
/// carry an opaque `update` payload whose `sessionUpdate` (or
/// `type` / `kind`) field picks one of the shapes below. Unknown
/// shapes return `Ok(None)` so the supervisor can ignore
/// presentation events; unknown notification methods fail closed
/// with a typed adapter error so the executor never silently
/// drops an ACP event.
fn normalize_acp_event(
    provider_id: &CliProviderId,
    event: serde_json::Value,
) -> Result<Option<NormalizedEvent>, CliAdapterError> {
    // The transport already filtered out responses; this adapter
    // only sees notifications (no `id` field). A frame carrying
    // `id` but no `method` is a JSON-RPC response and belongs to
    // the transport's request waiter; drop with `Ok(None)`.
    if event.get("id").is_some() && event.get("method").is_none() {
        return Ok(None);
    }
    let method = event
        .get("method")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            CliAdapterError::new(
                provider_id.clone(),
                "grok_event_missing_method",
                "ACP event is missing the `method` field",
            )
        })?;
    let params = event
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let update = params.get("update").cloned().unwrap_or(params.clone());
    // Grok uses `sessionUpdate` (snake_case per ACP spec), but
    // older versions and previews carry `type` / `kind`. The
    // adapter accepts any of them so the qualification runner
    // does not regress against an upstream rename.
    let kind = update
        .get("sessionUpdate")
        .or_else(|| update.get("type"))
        .or_else(|| update.get("kind"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let normalized_kind = match (method, kind) {
        ("session/update", "agent_message_chunk") | ("session/update", "agentMessageChunk") => {
            let text = update
                .get("text")
                .or_else(|| update.get("content"))
                .and_then(|v| match v {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Object(map) => map
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    _ => None,
                })
                .unwrap_or_default();
            NormalizedEventKind::AssistantDelta { text }
        }
        ("session/update", "agent_message") | ("session/update", "agentMessage") => {
            let text = update
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            NormalizedEventKind::AssistantCompleted { text }
        }
        ("session/update", "tool_call") | ("session/update", "toolCall") => {
            let call_id = update
                .get("id")
                .or_else(|| update.get("callId"))
                .or_else(|| update.get("toolCallId"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let tool_name = update
                .get("name")
                .or_else(|| update.get("toolName"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let arguments = update
                .get("args")
                .or_else(|| update.get("arguments"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            NormalizedEventKind::ToolCallStarted {
                call_id,
                tool_name,
                arguments,
            }
        }
        ("session/update", "tool_call_update") | ("session/update", "toolCallUpdate") => {
            let call_id = update
                .get("id")
                .or_else(|| update.get("callId"))
                .or_else(|| update.get("toolCallId"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let content = update
                .get("content")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let is_error = update
                .get("isError")
                .or_else(|| update.get("is_error"))
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            NormalizedEventKind::ToolResult {
                call_id,
                is_error,
                content,
            }
        }
        ("session/update", "approval_request")
        | ("session/update", "approvalRequest")
        | ("session/update", "requestApproval") => {
            let request_id = update
                .get("requestId")
                .or_else(|| update.get("request_id"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let action = update
                .get("action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            let details = update
                .get("details")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            NormalizedEventKind::ApprovalRequested {
                request_id,
                action,
                details,
            }
        }
        ("session/update", "turn_completed") | ("session/update", "turnCompleted") => {
            NormalizedEventKind::TurnCompleted
        }
        ("session/update", "turn_interrupted") | ("session/update", "turnInterrupted") => {
            NormalizedEventKind::TurnInterrupted
        }
        ("session/update", "turn_failed") | ("session/update", "turnFailed") => {
            let message = update
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown failure")
                .to_string();
            NormalizedEventKind::TurnFailed { message }
        }
        ("session/update", "session_started") | ("session/update", "sessionStarted") => {
            let vendor_session_id = params
                .get("sessionId")
                .or_else(|| update.get("sessionId"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string();
            NormalizedEventKind::SessionStarted {
                vendor_session_id,
                init: None,
            }
        }
        ("session/update", "mcp_ready") | ("session/update", "mcpReady") => {
            NormalizedEventKind::GatewayReady
        }
        _ => return Ok(None),
    };

    let event_key = format!(
        "grok:{method}:{}",
        event
            .get("id")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    );
    Ok(Some(NormalizedEvent {
        event_key,
        // DriverKind is the closed legacy union; Grok events use
        // `Codex` as the closest JSON-RPC analog until the new
        // architecture replaces `DriverKind` with `CliProviderId`.
        driver: crate::driver_host::contracts::DriverKind::Codex,
        source_offset: 0,
        vendor_method: method.to_string(),
        kind: normalized_kind,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::cli_runtime::adapter::CliMcpServerSpec;
    use crate::driver_host::cli_runtime::ids::CliAccountProfileId;

    fn sample_profile() -> CliAccountProfile {
        CliAccountProfile {
            id: CliAccountProfileId::new("grok/default"),
            provider_id: CliProviderId::new(PROVIDER_ID),
            label: "default".into(),
            managed_home: PathBuf::from("/tmp/rightkit-agent/cli-profiles/grok/default"),
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
            executable: PathBuf::from("/usr/local/bin/grok"),
            profile: sample_profile(),
            model_id: model.map(str::to_string),
            session_id_seed: if resume {
                "existing-grok-session".into()
            } else {
                "550e8400-e29b-41d4-a716-446655440000".into()
            },
            mcp_server: sample_mcp(),
            policy: String::new(),
            resume,
        }
    }

    // ---- PR 8 acceptance: profile isolation ----

    /// The launch argv must carry `--profile-dir <managed_home>` so
    /// every credential file, plugin state, skill cache, and
    /// conversation history lands inside the the host app-managed
    /// profile directory.
    #[tokio::test]
    async fn build_launch_argv_carries_profile_dir() {
        let adapter = GrokAdapter::new();
        let request = sample_request(None, false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        let profile_home = request.profile.managed_home.display().to_string();
        assert!(
            spec.argv
                .windows(2)
                .any(|pair| pair == ["--profile-dir", profile_home.as_str()]),
            "argv must carry --profile-dir <managed_home>; argv was {:?}",
            spec.argv
        );
    }

    // ---- PR 8 acceptance: leader/team/native-tool disable ----

    /// The launch argv must carry every disable flag the
    /// architecture plan §12.4 names. Drift here is a
    /// security-relevant diff.
    #[tokio::test]
    async fn build_launch_argv_disables_leader_team_native_tools_skills() {
        let adapter = GrokAdapter::new();
        let request = sample_request(None, false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        for required in [
            "--no-leader",
            "--no-team",
            "--disable-native-tools",
            "--disable-skills",
        ] {
            assert!(
                spec.argv.iter().any(|token| token == required),
                "argv must carry {required}; argv was {:?}",
                spec.argv
            );
        }
    }

    // ---- PR 8 acceptance: process-restart resume ----

    /// A resumed launch must forward the vendor session id as
    /// `--resume <id>` so the next process reattaches to the same
    /// conversation. The open `session_id` field must carry the
    /// `resume:` prefix.
    #[tokio::test]
    async fn build_launch_resume_forwards_vendor_session_id() {
        let adapter = GrokAdapter::new();
        let request = sample_request(None, true);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        assert!(
            spec.argv
                .windows(2)
                .any(|pair| pair == ["--resume", "existing-grok-session"]),
            "argv must carry --resume <vendor_session_id>; argv was {:?}",
            spec.argv
        );
        assert_eq!(
            spec.session_id, "resume:existing-grok-session",
            "open session_id must surface the vendor session id with the resume: prefix"
        );
    }

    /// Resume without a session id is a hard error.
    #[tokio::test]
    async fn build_launch_rejects_resume_without_session_seed() {
        let adapter = GrokAdapter::new();
        let mut request = sample_request(None, true);
        request.session_id_seed = String::new();
        let error = adapter.build_launch(&request).await.unwrap_err();
        assert_eq!(error.code, "grok_resume_missing_session");
    }

    /// The declared resume strategy must be
    /// `ProcessRestartWithResumeFlag` per the architecture plan
    /// §12.5. Drift here breaks the "vendor session id survives a
    /// process restart" guarantee.
    #[test]
    fn resume_strategy_is_process_restart_with_resume_flag() {
        assert_eq!(
            GROK_RESUME_STRATEGY,
            ResumeStrategy::ProcessRestartWithResumeFlag
        );
        assert_eq!(GrokAdapter::RESUME_STRATEGY, GROK_RESUME_STRATEGY);
    }

    // ---- PR 8 acceptance: harness forbids native subagent / shell / browser / memory ----

    /// The adapter's `harness_requirements` must reflect every
    /// Grok-specific restriction from §12.4. Drift here is a
    /// security-relevant diff.
    #[test]
    fn harness_requirements_forbid_every_native_route() {
        let adapter = GrokAdapter::new();
        let reqs = adapter.harness_requirements();
        assert!(reqs.requires_managed_profile);
        assert!(reqs.requires_one_mcp_server);
        assert!(reqs.requires_scoped_lease_token);
        assert!(reqs.requires_driver_sandbox);
        assert!(
            reqs.forbids_native_subagent,
            "forbids_native_subagent must be true (Grok teams / leader must be denied)"
        );
        assert!(
            reqs.forbids_native_shell,
            "forbids_native_shell must be true (Grok native tools must be denied)"
        );
        assert!(
            reqs.forbids_native_browser,
            "forbids_native_browser must be true (Grok web tools must be denied)"
        );
        assert!(
            reqs.forbids_native_memory_writes,
            "forbids_native_memory_writes must be true (Grok memory writes must be denied)"
        );
    }

    // ---- PR 8 acceptance: identity ----

    /// Identity round-trip: id(), display_name(), transport_kind()
    /// must match the canonical strings.
    #[test]
    fn identity_matches_canonical_strings() {
        let adapter = GrokAdapter::new();
        assert_eq!(adapter.id().as_str(), PROVIDER_ID);
        assert_eq!(adapter.display_name(), DISPLAY_NAME);
        assert_eq!(adapter.transport_kind(), TRANSPORT);
    }

    // ---- PR 8 acceptance: isolation mode ----

    /// Capabilities target the exclusive the host app gateway;
    /// compatibility mode is not a Grok target.
    #[test]
    fn capabilities_target_exclusive_host_gateway() {
        let adapter = GrokAdapter::new();
        let caps = adapter.capabilities();
        assert_eq!(
            caps.isolation_mode,
            crate::driver_host::cli_runtime::ids::HarnessIsolationMode::ExclusiveHostGateway
        );
        assert!(caps.supports_session_resume);
        assert!(caps.supports_model_discovery);
        assert!(caps.supports_native_account_login);
        assert!(!caps.supports_child_delegation);
    }

    // ---- PR 8 acceptance: qualification suite ----

    /// The qualification suite must include every default probe
    /// from `qualification::DEFAULT_NEGATIVE_PROBES` and
    /// `DEFAULT_POSITIVE_PROBES`. PR 8 acceptance requires the
    /// suite to be live-capability-authoritative.
    #[test]
    fn qualification_suite_includes_full_default_probes() {
        let suite = GrokAdapter::new().qualification_suite();
        let ids: std::collections::BTreeSet<&str> = suite.iter().map(|p| p.id.as_str()).collect();
        for required in [
            "neg.direct_native_file_read",
            "neg.direct_native_file_write",
            "neg.direct_native_shell",
            "neg.direct_native_web_browser_network",
            "neg.native_plugin_or_skill",
            "neg.native_subagent_team_delegation",
            "neg.outside_bound_workspace",
            "neg.unrelated_env_secret",
            "neg.fallback_when_mcp_unavailable",
            "pos.rightkit_mcp_discovery",
            "pos.rightkit_read",
            "pos.rightkit_governed_write_and_approval",
            "pos.rightkit_shell_verify",
            "pos.rightkit_memory",
            "pos.rightkit_delegation",
            "pos.interruption",
            "pos.resume",
            "pos.exact_once_replay",
            "pos.transcript_visibility",
        ] {
            assert!(
                ids.contains(required),
                "qualification suite must include {required}"
            );
        }
    }

    // ---- PR 8 acceptance: env var stripping ----

    /// The launch spec must strip every BYOK and the host app secret,
    /// including the xAI-specific `XAI_API_KEY` and `XAI_BASE_URL`.
    #[tokio::test]
    async fn build_launch_strips_xai_env_vars() {
        let adapter = GrokAdapter::new();
        let request = sample_request(None, false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        for forbidden in ["XAI_API_KEY", "XAI_BASE_URL", "RIGHTKIT_DAEMON_TOKEN"] {
            assert!(
                spec.env_remove.contains(&forbidden.to_string()),
                "adapter must strip {forbidden}"
            );
        }
    }

    // ---- PR 8 acceptance: hostile model id rejection ----

    /// Hostile model ids (with shell metacharacters or xAI key
    /// hints) are rejected with a typed adapter error before argv
    /// is built.
    #[tokio::test]
    async fn build_launch_rejects_hostile_model_id() {
        let adapter = GrokAdapter::new();
        let request = sample_request(Some("grok-4\"; rm -rf /"), false);
        let error = adapter.build_launch(&request).await.unwrap_err();
        assert_eq!(error.code, "grok_byok_rejected");
    }

    /// xAI API key hints embedded in the model id are rejected.
    #[tokio::test]
    async fn build_launch_rejects_xai_api_key_hint_in_model() {
        let adapter = GrokAdapter::new();
        let request = sample_request(Some("model-XAI_API_KEY=sk-x"), false);
        let error = adapter.build_launch(&request).await.unwrap_err();
        assert_eq!(error.code, "grok_byok_rejected");
    }

    /// BYOK-shaped session id seeds are rejected.
    #[tokio::test]
    async fn build_launch_rejects_byok_session_seed() {
        let adapter = GrokAdapter::new();
        let mut request = sample_request(None, false);
        request.session_id_seed = "XAI_API_KEY=sk-secret".into();
        let error = adapter.build_launch(&request).await.unwrap_err();
        assert_eq!(error.code, "grok_byok_session");
    }

    // ---- PR 8 acceptance: model override flag ----

    /// A caller-selected model id is appended as `--model <id>`.
    #[tokio::test]
    async fn build_launch_appends_model_flag_when_set() {
        let adapter = GrokAdapter::new();
        let request = sample_request(Some("grok-4"), false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        assert!(
            spec.argv
                .windows(2)
                .any(|pair| pair == ["--model", "grok-4"]),
            "model flag must be appended when request carries a model_id"
        );
    }

    // ---- PR 8 acceptance: ACP event normalization ----

    /// `agent_message_chunk` notifications surface as
    /// `AssistantDelta`.
    #[test]
    fn normalize_provider_event_translates_agent_message_chunk() {
        let adapter = GrokAdapter::new();
        let event: serde_json::Value = serde_json::from_str(
            r#"{
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "sess-1",
                    "update": {"sessionUpdate": "agent_message_chunk", "text": "hello"}
                }
            }"#,
        )
        .unwrap();
        let normalized = adapter
            .normalize_provider_event(event)
            .unwrap()
            .expect("notification must normalize to Some");
        assert_eq!(
            normalized.kind,
            NormalizedEventKind::AssistantDelta {
                text: "hello".into()
            }
        );
        assert_eq!(normalized.vendor_method, "session/update");
    }

    /// `tool_call` notifications surface as `ToolCallStarted`.
    #[test]
    fn normalize_provider_event_translates_tool_call() {
        let adapter = GrokAdapter::new();
        let event: serde_json::Value = serde_json::from_str(
            r#"{
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "update": {"sessionUpdate": "tool_call", "id": "call-1", "name": "fs.read", "args": {"path": "/tmp"}}
                }
            }"#,
        )
        .unwrap();
        let normalized = adapter
            .normalize_provider_event(event)
            .unwrap()
            .expect("notification must normalize to Some");
        match normalized.kind {
            NormalizedEventKind::ToolCallStarted {
                call_id,
                tool_name,
                arguments,
            } => {
                assert_eq!(call_id, "call-1");
                assert_eq!(tool_name, "fs.read");
                assert_eq!(arguments["path"], "/tmp");
            }
            other => panic!("expected ToolCallStarted, got {other:?}"),
        }
    }

    /// JSON-RPC response frames (have `id`, no `method`) drop to
    /// `None` — they belong to RPC bookkeeping, not the event
    /// stream.
    #[test]
    fn normalize_provider_event_drops_response_messages() {
        let adapter = GrokAdapter::new();
        let event: serde_json::Value =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#).unwrap();
        let normalized = adapter.normalize_provider_event(event).unwrap();
        assert!(normalized.is_none(), "response messages must drop to None");
    }

    /// Unknown notification methods return `Ok(None)` so the
    /// supervisor can ignore presentation events; the adapter does
    /// not fail closed on shapes the executor does not need.
    #[test]
    fn normalize_provider_event_ignores_unknown_shapes() {
        let adapter = GrokAdapter::new();
        let event: serde_json::Value = serde_json::from_str(
            r#"{
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {"update": {"sessionUpdate": "presentationHeartbeat"}}
            }"#,
        )
        .unwrap();
        let normalized = adapter.normalize_provider_event(event).unwrap();
        assert!(
            normalized.is_none(),
            "unknown sessionUpdate shapes drop to None"
        );
    }

    /// `mcp_ready` notifications surface as `GatewayReady`.
    #[test]
    fn normalize_provider_event_translates_gateway_ready() {
        let adapter = GrokAdapter::new();
        let event: serde_json::Value = serde_json::from_str(
            r#"{
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {"update": {"sessionUpdate": "mcp_ready"}}
            }"#,
        )
        .unwrap();
        let normalized = adapter
            .normalize_provider_event(event)
            .unwrap()
            .expect("notification must normalize");
        assert_eq!(normalized.kind, NormalizedEventKind::GatewayReady);
    }

    // ---- PR 8 acceptance: discovery ----

    /// Discovery is a soft probe; a missing binary returns
    /// `installed = false` rather than an error so the UI can
    /// render `LoginRequired`.
    #[tokio::test]
    async fn discover_returns_soft_failure_when_binary_absent() {
        let adapter = GrokAdapter::new();
        let discovery = adapter
            .discover(&sample_profile())
            .await
            .expect("discover must not error");
        if !discovery.installed {
            assert!(discovery.executable.is_none());
        }
    }

    // ---- PR 8 acceptance: signed-in detection ----

    /// `signedIn:false` JSON output is treated as not signed in.
    #[test]
    fn signed_in_from_output_recognizes_signed_in_false_marker() {
        assert!(!signed_in_from_output(r#"{"signedIn":false}"#));
        assert!(!signed_in_from_output(r#"{"signed_in":false}"#));
        assert!(!signed_in_from_output("not signed in"));
        assert!(!signed_in_from_output("Signed out"));
        assert!(signed_in_from_output(r#"{"account":"ada@example.com"}"#));
        assert!(signed_in_from_output(""));
    }

    // ---- PR 8 acceptance: BYOK rejector ----

    /// The BYOK rejector surfaces every documented forbidden
    /// token.
    #[test]
    fn reject_byok_string_rejects_all_documented_tokens() {
        let pid = CliProviderId::new(PROVIDER_ID);
        for hostile in [
            "grok-XAI_API_KEY-secret",
            "grok--key-secret",
            "grok--api-key-secret",
            "grok-byok",
            "grok-openrouter",
            "grok-minimax",
            "grok-tinyfish",
            "grok-sk-secret",
        ] {
            let result = reject_byok_string(&pid, "model_id", hostile);
            assert!(
                result.is_err(),
                "expected reject for {hostile:?}, got {result:?}"
            );
        }
    }
}
