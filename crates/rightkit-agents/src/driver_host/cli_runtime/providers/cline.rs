//! Cline CLI adapter.
//!
//! PR 9 of the architecture plan. Implements Cline-specific knowledge
//! behind the open [`crate::driver_host::cli_runtime::adapter::CliProviderAdapter`]
//! contract:
//!
//! - managed `--data-dir` profile directory under
//!   `~/<state-dir>/cli-profiles/cline/<label>/` so credential files,
//!   plugin state, and conversation history live inside a
//!   the host app-owned tree rather than the user's global shell;
//! - ACP launch (`cline --acp`) with `--no-teams` and explicit
//!   plugin/skill isolation (`--disable-plugins`,
//!   `--disable-skills`, `--no-marketplace`) so the only capability
//!   surface available to the model is the the host app MCP server;
//! - `cline auth status` and `cline login` for native account auth;
//! - `cline session resume <id>` for vendor session resume via
//!   [`ResumeStrategy::ProtocolLoadById`];
//! - rejects any `--key` flag or BYOK provider hint at parse time
//!   before argv construction;
//! - full default qualification suite, which includes the
//!   `pos.resume` probe that verifies protocol-load continuity.

use crate::driver_host::cli_runtime::qualification::now_iso;
use async_trait::async_trait;
use std::path::{Path, PathBuf};

use crate::driver_host::cli_runtime::adapter::{
    CliAdapterError, CliAuthenticationStatus, CliDiscovery, CliLaunchRequest, CliLaunchSpec,
    CliModelCatalog, CliProviderAdapter, NativeLoginUi,
};
#[cfg(test)]
use crate::driver_host::cli_runtime::ids::ProbePolarity;
use crate::driver_host::cli_runtime::ids::{
    CliAccountProfile, CliProviderCapabilities, CliProviderId, CliTransportKind,
    HarnessRequirements, QualificationProbe, ResumeStrategy,
};
use crate::driver_host::cli_runtime::qualification::{
    DEFAULT_NEGATIVE_PROBES, DEFAULT_POSITIVE_PROBES,
};
use crate::driver_host::contracts::{NormalizedEvent, NormalizedEventKind};

pub const PROVIDER_ID: &str = "cline";
pub const DISPLAY_NAME: &str = "Cline";
pub const TRANSPORT: CliTransportKind = CliTransportKind::Acp;

/// Cline binary name. The transport resolves this against `PATH` via
/// the shared lookup (which covers `cline.exe` through `PATHEXT`);
/// `clinex` (legacy) is tried as a fallback so the discovery pass does
/// not miss a renamed upstream install.
const CLINE_BINARY: &str = "cline";
const CLINE_LEGACY_BINARY: &str = "clinex";

/// Cline CLI adapter. PR 9 wires managed profile, ACP launch with
/// `--no-teams` + plugin/skill isolation, native auth, model list,
/// session resume, and the full default qualification suite.
pub struct ClineAdapter {
    provider_id: CliProviderId,
}

impl ClineAdapter {
    pub fn new() -> Self {
        Self {
            provider_id: CliProviderId::new(PROVIDER_ID),
        }
    }

    /// Transport this adapter drives. Re-exported as a `const` so the
    /// built-in registry can match against it without importing
    /// [`CliTransportKind`] directly.
    pub const TRANSPORT_KIND: CliTransportKind = TRANSPORT;

    /// Cline resumes a session via its protocol-level
    /// `cline session resume <id>` subcommand. The transport
    /// translates that into the matching ACP `session/load` request.
    /// "Replay visible transcript into a new session" is **not** a
    /// permitted variant for Cline, matching the hard ceiling on
    /// [`ResumeStrategy`] in `cli_runtime::ids`.
    pub const RESUME_STRATEGY: ResumeStrategy = ResumeStrategy::ProtocolLoadById;
}

impl Default for ClineAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CliProviderAdapter for ClineAdapter {
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
        let executable = locate_binary().ok_or_else(|| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "cline_binary_not_found",
                "no `cline` binary on PATH; install Cline or update the PATH",
            )
        })?;
        let executable_sha256 = super::executable_sha256(Some(&executable));
        Ok(CliDiscovery {
            installed: true,
            executable: Some(executable),
            binary_version: None,
            executable_sha256,
            protocol_version: Some("acp/1".to_string()),
            transport: TRANSPORT,
        })
    }

    async fn authentication_status(
        &self,
        profile: &CliAccountProfile,
    ) -> Result<CliAuthenticationStatus, CliAdapterError> {
        let executable = locate_binary().ok_or_else(|| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "cline_binary_not_found",
                "no `cline` binary on PATH",
            )
        })?;
        // Run `cline --data-dir <managed_home> auth status` and read
        // a fingerprint from the JSON output. The wire format varies
        // across Cline releases; we surface `signed_in=false` when
        // the output indicates the user has not signed in and
        // `signed_in=true` otherwise. Any non-zero exit is a hard
        // failure that the supervisor must surface.
        let output = run_capture(
            &executable,
            &[
                "--data-dir",
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
            auth_mode: crate::driver_host::cli_runtime::ids::CliAuthMode::NativeCliAccount,
            account_label: None,
            account_fingerprint: if signed_in {
                Some(fingerprint_from_output(&output))
            } else {
                None
            },
            source_label: format!(
                "cline --data-dir {} auth status",
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
        let executable = locate_binary().ok_or_else(|| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "cline_binary_not_found",
                "no `cline` binary on PATH",
            )
        })?;
        // `cline --data-dir <managed_home> login` blocks until the
        // user completes the browser/device flow. The transport
        // surfaces the browser URL via [`NativeLoginUi`] before we
        // wait on the subprocess; the adapter itself only spawns the
        // command and reports its exit code.
        let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
        let context = rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: profile.managed_home.to_string_lossy().into_owned(),
            sandbox_profile: Some("cline-login".into()),
            sandbox_required: true,
            network_required: true,
        };
        let spec = rightkit_sandbox::execution::CommandLaunchSpec {
            program: executable.to_string_lossy().into_owned(),
            argv: vec![
                "--data-dir".into(),
                profile.managed_home.to_string_lossy().into_owned(),
                "login".into(),
            ],
            cwd: context.workspace_root.clone(),
            context,
            env: Vec::new(),
        };
        let mut child = rightkit_sandbox::execution::ProcessLaunchAdapter::spawn(&adapter, spec)
            .await
            .map_err(|error| {
                CliAdapterError::new(
                    self.provider_id.clone(),
                    "cline_login_spawn",
                    format!("failed to spawn `cline login` under policy: {error:?}"),
                )
            })?;
        let status = child.wait().await.map_err(|error| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "cline_login_wait",
                format!("login wait failed: {error:?}"),
            )
        })?;
        if status != 0 {
            return Err(CliAdapterError::new(
                self.provider_id.clone(),
                "cline_login_failed",
                format!("`cline login` exited non-zero: {status}"),
            ));
        }
        Ok(())
    }

    async fn list_models(
        &self,
        profile: &CliAccountProfile,
    ) -> Result<CliModelCatalog, CliAdapterError> {
        let executable = locate_binary().ok_or_else(|| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "cline_binary_not_found",
                "Cline CLI is not installed",
            )
        })?;
        let models = probe_models(&executable, profile)
            .await
            .map_err(|message| {
                CliAdapterError::new(
                    self.provider_id.clone(),
                    "cline_model_catalog_unavailable",
                    message,
                )
            })?;
        Ok(CliModelCatalog {
            models,
            source: "cline --data-dir models --json".into(),
            fetched_at: now_iso(),
        })
    }

    #[allow(clippy::vec_init_then_push)]
    async fn build_launch(
        &self,
        request: &CliLaunchRequest,
    ) -> Result<CliLaunchSpec, CliAdapterError> {
        // Reject every BYOK-shaped field before any argv is built.
        // A user-supplied `--key`, BYOK provider hint, or non-Cline
        // key environment variable is a launch refusal: the host app
        // runs Cline exclusively through the Cline account.
        reject_byok_fields(&self.provider_id, request)?;
        let mut argv: Vec<String> = Vec::new();
        // 1. ACP mode: Cline exposes an ACP server on stdin/stdout
        //    when launched with `--acp`. The matching transport
        //    (`transports::acp`) speaks JSON-RPC 2.0 to it.
        argv.push("--acp".into());
        // 2. Plugin/skill isolation. Cline supports a native plugin
        //    loader, a marketplace skill catalog, and a built-in
        //    skill mechanism. All three must be disabled so the only
        //    capability surface the model can reach is the the host app
        //    MCP server attached below.
        argv.push("--disable-plugins".into());
        argv.push("--disable-skills".into());
        argv.push("--no-marketplace".into());
        // 3. No Cline teams. Cline teams allow users to spawn
        //    background sub-agents that route around the harness
        //    isolation; the architecture plan (§10) names `--no-teams`
        //    as a hard Cline launch precondition.
        argv.push("--no-teams".into());
        // 4. Managed data directory. All credential files, plugin
        //    state, skill cache, and conversation history land under
        //    the the host app-managed profile directory.
        argv.push("--data-dir".into());
        argv.push(request.profile.managed_home.display().to_string());
        // 5. Vendor session id. `cline session resume <id>` is the
        //    protocol-load-by-id resume surface; a fresh session
        //    passes `--session <seed>` so Cline's own session store
        //    records it.
        if request.resume {
            if request.session_id_seed.is_empty() {
                return Err(CliAdapterError::new(
                    self.provider_id.clone(),
                    "cline_resume_missing_session",
                    "resume=true requires a non-empty session_id_seed",
                ));
            }
            argv.push("session".into());
            argv.push("resume".into());
            argv.push(request.session_id_seed.clone());
        } else if !request.session_id_seed.is_empty() {
            argv.push("--session".into());
            argv.push(request.session_id_seed.clone());
        }
        // 6. Model override (forwarded as `--model <id>`). The model
        //    id was already BYOK-rejected above.
        if let Some(model) = &request.model_id {
            argv.push("--model".into());
            argv.push(model.clone());
        }

        // 7. Strip the forbidden BYOK env vars from the inherited
        //    process environment. Cline reads several of these for
        //    BYOK routing, so the strip list is the union of the
        //    common forbidden set and the Cline-specific key vars.
        let env_remove = vec![
            "ANTHROPIC_API_KEY".into(),
            "OPENAI_API_KEY".into(),
            "OPENROUTER_API_KEY".into(),
            "MINIMAX_API_KEY".into(),
            "XAI_API_KEY".into(),
            "CURSOR_API_KEY".into(),
            "CLINE_API_KEY".into(),
            "CLINE_BYOK_KEY".into(),
            "RIGHTKIT_DAEMON_TOKEN".into(),
            "RIGHTKIT_GATEWAY_TOKEN".into(),
            "RIGHTKIT_DRIVER_INSTANCE_SECRET".into(),
        ];

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
        // Full default suite. The negative probes confirm Cline's
        // native routes are denied (including plugins/skills and
        // teams). The positive probes confirm the the host app MCP is
        // the only capability surface, including `pos.resume` for
        // `cline session resume <id>` continuity and
        // `pos.transcript_visibility` for the same-vendor-session-id
        // invariant. Built once via a `OnceLock` so the trait method
        // can return a `&'static [QualificationProbe]` without a
        // full inline copy of the default suite.
        cline_qualification_suite()
    }

    fn normalize_provider_event(
        &self,
        event: serde_json::Value,
    ) -> Result<Option<NormalizedEvent>, CliAdapterError> {
        normalize_acp_event(&self.provider_id, event)
    }
}

/// Full default qualification suite for Cline. Concatenates the
/// default negative and positive probes from
/// [`crate::driver_host::cli_runtime::qualification::DEFAULT_NEGATIVE_PROBES`] and
/// [`crate::driver_host::cli_runtime::qualification::DEFAULT_POSITIVE_PROBES`].
/// Returned by [`CliProviderAdapter::qualification_suite`] so the
/// runner can verify the live binary before launch.
///
/// Returning a `&'static [QualificationProbe]` requires a single
/// compile-time-allocated slice; the helper
/// [`cline_qualification_suite`] builds the slice lazily via a
/// `OnceLock` and leaks the resulting boxed slice into `'static`.
/// The set of probes is fixed at compile time; the leak is one-shot
/// at first call and never freed because the registry holds the
/// pointer for the daemon's lifetime.
static CLINE_QUALIFICATION_SUITE: std::sync::OnceLock<Vec<QualificationProbe>> =
    std::sync::OnceLock::new();

/// Return the Cline qualification suite. The result is cached after
/// the first call so repeated invocations do not re-allocate.
fn cline_qualification_suite() -> &'static [QualificationProbe] {
    CLINE_QUALIFICATION_SUITE
        .get_or_init(|| {
            let mut probes: Vec<QualificationProbe> =
                Vec::with_capacity(DEFAULT_NEGATIVE_PROBES.len() + DEFAULT_POSITIVE_PROBES.len());
            probes.extend_from_slice(&DEFAULT_NEGATIVE_PROBES);
            probes.extend_from_slice(&DEFAULT_POSITIVE_PROBES);
            probes
        })
        .as_slice()
}

/// Inline copy of every default probe for the assertion-only
/// `qualification_suite_includes_*` test. The live suite is the
/// [`cline_qualification_suite`] helper above; this constant exists
/// so the test can fail loudly if any default probe is dropped
/// from the import list.
#[cfg(test)]
const CLINE_QUALIFICATION_PROBE_IDS: &[&str] = &[
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
];

/// Encode the open session-id field into the wire form the launch
/// argv consumes. The open field is a single string; Cline's flag
/// set distinguishes fresh-session from resume by which subcommand is
/// used. We surface that choice on the open field with a `resume:`
/// prefix so the wire form is unambiguous to downstream consumers.
fn encode_session_id(resume: bool, seed: &str) -> String {
    if resume {
        format!("resume:{seed}")
    } else {
        seed.to_string()
    }
}

/// Hash a profile into the per-launch `profile_hash` the spec
/// carries. Matches the Claude/Codex adapter's hashing so all three
/// providers report profile swaps in a uniform shape.
async fn probe_models(
    executable: &std::path::Path,
    profile: &CliAccountProfile,
) -> Result<Vec<String>, String> {
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: profile.managed_home.to_string_lossy().into_owned(),
        sandbox_profile: Some("cline-models".into()),
        sandbox_required: true,
        network_required: false,
    };
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program: executable.to_string_lossy().into_owned(),
        argv: vec![
            "--data-dir".into(),
            profile.managed_home.to_string_lossy().into_owned(),
            "models".into(),
            "--json".into(),
        ],
        cwd: context.workspace_root.clone(),
        context,
        env: Vec::new(),
    };
    let (status, bytes) = crate::driver_host::binary_lookup::run_bounded_probe(
        spec,
        crate::driver_host::binary_lookup::VERSION_PROBE_TIMEOUT,
        "Cline model probe",
    )
    .await?;
    if status != 0 {
        return Err(format!("Cline models exited with {status}"));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Cline models output was not JSON: {error}"))?;
    let values = value
        .get("models")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Cline models output omitted models array".to_string())?;
    let models = values
        .iter()
        .filter_map(|item| {
            item.as_str()
                .or_else(|| item.get("id").and_then(serde_json::Value::as_str))
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Err("Cline models output contained no model ids".into());
    }
    Ok(models)
}

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

/// Locate the `cline` binary on `PATH` via the shared lookup, falling
/// back to the legacy `clinex` name. Returns `None` if neither resolves.
fn locate_binary() -> Option<PathBuf> {
    crate::driver_host::binary_lookup::find_on_path(CLINE_BINARY)
        .or_else(|| crate::driver_host::binary_lookup::find_on_path(CLINE_LEGACY_BINARY))
}

/// Run `cline <args>` and capture combined stdout+stderr. Returns the
/// combined output. Any spawn / wait failure becomes an
/// [`CliAdapterError`].
async fn run_capture(
    executable: &Path,
    args: &[&str],
    cwd: Option<&Path>,
) -> Result<String, CliAdapterError> {
    let root = cwd.unwrap_or_else(|| executable.parent().unwrap_or_else(|| Path::new(".")));
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: root.to_string_lossy().into_owned(),
        sandbox_profile: Some("cline-probe".into()),
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
            "cline probe",
        )
        .await
        .map_err(|message| {
            CliAdapterError::new(
                CliProviderId::new(PROVIDER_ID),
                "cline_spawn_failed",
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
            "cline_subcommand_failed",
            format!("`cline` exited with {}: {}", status, combined.trim()),
        ));
    }
    Ok(combined)
}

/// Decide whether the `cline auth status` output indicates the user
/// has signed in. The wire format varies across Cline releases, so we
/// pattern-match a few stable shapes: `signedIn:false`, the literal
/// phrase `not signed in`, and an exit-non-zero status (which the
/// caller has already converted to an error). Anything else — empty
/// output, an opaque JSON `account` field, an HTTP-style 200 — counts
/// as signed in.
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
/// output. Falls back to a deterministic placeholder when the output
/// has no usable identifier; the supervisor uses the fingerprint to
/// detect account switches.
fn fingerprint_from_output(output: &str) -> String {
    let mut h = rightkit_fs::digest::Sha256Hasher::new();
    h.update(output.as_bytes());
    let digest = h.finalize_bytes();
    rightkit_crypto::hex_encode(&digest)
        .chars()
        .take(16)
        .collect()
}

/// Cheap UTC ISO timestamp suitable for `observed_at` /
/// `fetched_at`. Matches the `YYYY-MM-DDTHH:MM:SSZ` shape the daemon
/// uses elsewhere so SQL string comparisons remain correct.

/// Reject every BYOK-shaped field on the launch request. A
/// `--key` flag, a non-Cline provider hint, or any other BYOK-shaped
/// input is a launch refusal: Cline launches exclusively through the
/// Cline account.
fn reject_byok_fields(
    provider_id: &CliProviderId,
    request: &CliLaunchRequest,
) -> Result<(), CliAdapterError> {
    if let Some(model) = &request.model_id {
        reject_byok_string(provider_id, "model_id", model)?;
    }
    if request.session_id_seed.contains("--key") {
        return Err(CliAdapterError::new(
            provider_id.clone(),
            "cline_byok_session",
            "session_id_seed contains a `--key` substring; BYOK routes are forbidden",
        ));
    }
    Ok(())
}

/// Single-string BYOK rejector. Looks for the canonical
/// `--key`, `--api-key`, and BYOK provider hints. Lower-cases the
/// field before matching so casing tricks do not bypass the check.
fn reject_byok_string(
    provider_id: &CliProviderId,
    field: &str,
    value: &str,
) -> Result<(), CliAdapterError> {
    let lower = value.to_lowercase();
    for forbidden in [
        "--key",
        "--api-key",
        "byok",
        "openrouter",
        "minimax",
        "tinyfish",
        "sk-",
    ] {
        if lower.contains(forbidden) {
            return Err(CliAdapterError::new(
                provider_id.clone(),
                "cline_byok_rejected",
                format!("{field} contains a forbidden BYOK token ({forbidden}); Cline launches exclusively through the Cline account"),
            ));
        }
    }
    Ok(())
}

/// Translate one ACP inbound JSON-RPC 2.0 notification into a
/// [`NormalizedEvent`]. Cline's `session/update` notifications
/// carry an opaque `update` payload whose `type` field picks one of
/// the shapes below. Unknown shapes and unknown notification methods
/// fail closed so a future ACP extension cannot silently surface a
/// tool the harness has not approved.
fn normalize_acp_event(
    provider_id: &CliProviderId,
    event: serde_json::Value,
) -> Result<Option<NormalizedEvent>, CliAdapterError> {
    let object = event.as_object().ok_or_else(|| {
        CliAdapterError::new(
            provider_id.clone(),
            "cline_event_not_object",
            "ACP event root must be a JSON object",
        )
    })?;
    let method = object.get("method").and_then(serde_json::Value::as_str);
    // JSON-RPC responses are RPC bookkeeping, not visible events.
    // Drop them with `Ok(None)` so the executor does not surface
    // them as part of the transcript.
    if object.contains_key("id") && method.is_none() {
        return Ok(None);
    }
    let method = method.ok_or_else(|| {
        CliAdapterError::new(
            provider_id.clone(),
            "cline_event_missing_method",
            "ACP event is missing the `method` field",
        )
    })?;
    let params = object
        .get("params")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    match method {
        "session/update" => translate_session_update(provider_id, params),
        // Unknown notification methods fail closed. The transport's
        // request waiter already handles responses (we drop them
        // above); anything else reaching this point is an unexpected
        // extension we have not approved.
        other => Err(CliAdapterError::new(
            provider_id.clone(),
            "cline_unknown_notification",
            format!("ACP notification method {other:?} is not handled by the Cline adapter"),
        )),
    }
}

/// Translate the inner `update` payload of a `session/update`
/// notification into a [`NormalizedEvent`]. Mirrors the cursor
/// adapter's narrow whitelist: every accepted `update.type`
/// corresponds to a [`NormalizedEventKind`] the executor knows how
/// to render; unknown kinds fail closed.
fn translate_session_update(
    provider_id: &CliProviderId,
    params: serde_json::Value,
) -> Result<Option<NormalizedEvent>, CliAdapterError> {
    let outer_session_id = params
        .get("sessionId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let update = params.get("update").cloned().unwrap_or(params);
    let update_kind = update
        .get("type")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            CliAdapterError::new(
                provider_id.clone(),
                "cline_session_update_missing_type",
                "ACP session/update payload is missing the `update.type` field",
            )
        })?;
    let update_object = update.as_object().cloned().unwrap_or_default();
    let kind = match update_kind {
        "agentMessageChunk" | "text_chunk" => NormalizedEventKind::AssistantDelta {
            text: string_field(&update_object, "text"),
        },
        "agentMessage" | "agentMessageComplete" | "text_done" => {
            NormalizedEventKind::AssistantCompleted {
                text: string_field(&update_object, "text"),
            }
        }
        "toolCall" | "tool_call" => NormalizedEventKind::ToolCallStarted {
            call_id: string_field(&update_object, "callId")
                .if_empty_use(string_field(&update_object, "id")),
            tool_name: string_field(&update_object, "toolName")
                .if_empty_use(string_field(&update_object, "name")),
            arguments: update_object
                .get("args")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        },
        "toolResult" | "tool_result" => NormalizedEventKind::ToolResult {
            call_id: string_field(&update_object, "callId")
                .if_empty_use(string_field(&update_object, "id")),
            is_error: update_object
                .get("isError")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
            content: update_object
                .get("content")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        },
        "approvalRequest" | "approval_request" | "requestApproval" => {
            NormalizedEventKind::ApprovalRequested {
                request_id: string_field(&update_object, "requestId")
                    .if_empty_use(string_field(&update_object, "approvalId")),
                action: string_field(&update_object, "action"),
                details: update_object
                    .get("details")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null),
            }
        }
        "turnStarted" => NormalizedEventKind::TurnStarted {
            vendor_turn_id: string_field(&update_object, "turnId"),
        },
        "turnCompleted" | "turn_completed" => NormalizedEventKind::TurnCompleted,
        "turnInterrupted" | "turn_interrupted" => NormalizedEventKind::TurnInterrupted,
        "turnFailed" | "turn_failed" => NormalizedEventKind::TurnFailed {
            message: string_field(&update_object, "message"),
        },
        "sessionReady" | "session_ready" | "sessionStarted" => {
            NormalizedEventKind::SessionStarted {
                vendor_session_id: string_field(&update_object, "sessionId")
                    .if_empty_use(outer_session_id),
                init: None,
            }
        }
        "mcpReady" | "mcp_ready" | "gatewayReady" => NormalizedEventKind::GatewayReady,
        other => {
            return Err(CliAdapterError::new(
                provider_id.clone(),
                "cline_unknown_session_update",
                format!("ACP session/update kind {other:?} is not handled by the Cline adapter"),
            ));
        }
    };
    Ok(Some(NormalizedEvent {
        event_key: format!("cline:0:session/update:{update_kind}"),
        // DriverKind only ships Codex and Claude today. Cline runs on
        // ACP and shares the cursor adapter's choice; the open
        // contract keys replay suppression on this enum so reusing
        // Codex keeps the dedupe path consistent.
        driver: crate::driver_host::contracts::DriverKind::Codex,
        source_offset: 0,
        vendor_method: "session/update".to_string(),
        kind,
    }))
}

/// Helper: read a string field from an ACP update object.
fn string_field(update_object: &serde_json::Map<String, serde_json::Value>, key: &str) -> String {
    update_object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Tiny extension trait that lets us fall through to a secondary
/// field name when the first is empty. Used by the tool-call and
/// approval-request branches to accept both ACP v1's snake_case and
/// Cline's own camelCase names without duplicating the call site.
trait StringFieldExt {
    fn if_empty_use(self, fallback: String) -> String;
}

impl StringFieldExt for String {
    fn if_empty_use(self, fallback: String) -> String {
        if self.is_empty() {
            fallback
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::cli_runtime::adapter::CliMcpServerSpec;
    use crate::driver_host::cli_runtime::ids::{
        CliAccountProfileId, CliAuthMode, HarnessIsolationMode,
    };

    fn sample_profile() -> CliAccountProfile {
        CliAccountProfile {
            id: CliAccountProfileId::new("cline/default"),
            provider_id: CliProviderId::new(PROVIDER_ID),
            label: "default".into(),
            managed_home: PathBuf::from("/tmp/rightkit-agent/cli-profiles/cline/default"),
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
            executable: PathBuf::from("/usr/local/bin/cline"),
            profile: sample_profile(),
            model_id: model.map(str::to_string),
            session_id_seed: if resume {
                "existing-session-uuid".into()
            } else {
                "550e8400-e29b-41d4-a716-446655440000".into()
            },
            mcp_server: sample_mcp(),
            policy: String::new(),
            resume,
        }
    }

    /// Identity round-trip: id(), display_name(), transport_kind()
    /// all return the canonical Cline strings.
    #[test]
    fn identity_matches_canonical_strings() {
        let adapter = ClineAdapter::new();
        assert_eq!(adapter.id().as_str(), PROVIDER_ID);
        assert_eq!(adapter.display_name(), DISPLAY_NAME);
        assert_eq!(adapter.transport_kind(), TRANSPORT);
    }

    /// Resume strategy must be ProtocolLoadById: "replay visible
    /// transcript into a new session" is forbidden for Cline, and
    /// ProcessRestartWithResumeFlag is the only disposable variant.
    #[test]
    fn resume_strategy_is_protocol_load_by_id() {
        assert_eq!(
            ClineAdapter::RESUME_STRATEGY,
            ResumeStrategy::ProtocolLoadById
        );
    }

    /// Harness requirements: every native route is forbidden, every
    /// harness precondition is on. Drift here is a security-relevant
    /// diff.
    #[test]
    fn harness_requirements_forbid_every_native_route() {
        let adapter = ClineAdapter::new();
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

    /// Capabilities: ExclusiveCodeRightGateway is the only isolation
    /// target for Cline. Compatibility mode is a transition state for
    /// Command Code, not Cline.
    #[test]
    fn capabilities_target_exclusive_host_gateway() {
        let adapter = ClineAdapter::new();
        let caps = adapter.capabilities();
        assert_eq!(
            caps.isolation_mode,
            HarnessIsolationMode::ExclusiveHostGateway
        );
        assert!(caps.supports_session_resume);
        assert!(caps.supports_native_account_login);
        assert!(!caps.supports_child_delegation);
    }

    /// `build_launch` must always include `--no-teams`. This is the
    /// headline Cline safety invariant: a missing `--no-teams` flag
    /// would let the model spawn background sub-agents that bypass
    /// the harness isolation.
    #[tokio::test]
    async fn build_launch_includes_no_teams_flag() {
        let adapter = ClineAdapter::new();
        let request = sample_request(None, false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        assert!(
            spec.argv.contains(&"--no-teams".to_string()),
            "argv must contain --no-teams; got {:?}",
            spec.argv
        );
    }

    /// `build_launch` must include the full plugin/skill isolation
    /// surface: `--disable-plugins`, `--disable-skills`, and
    /// `--no-marketplace`. Cline supports native plugins and
    /// marketplace skills; without these flags the model can reach
    /// capability surfaces outside the the host app MCP.
    #[tokio::test]
    async fn build_launch_denies_all_native_tools() {
        let adapter = ClineAdapter::new();
        let request = sample_request(None, false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        for required in [
            "--disable-plugins",
            "--disable-skills",
            "--no-marketplace",
            "--no-teams",
        ] {
            assert!(
                spec.argv.contains(&required.to_string()),
                "argv must contain {required} to deny the native route; got {:?}",
                spec.argv
            );
        }
    }

    /// `build_launch` must include `--acp` so Cline exposes its
    /// ACP server rather than starting an interactive TUI.
    #[tokio::test]
    async fn build_launch_starts_in_acp_mode() {
        let adapter = ClineAdapter::new();
        let request = sample_request(None, false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        assert!(
            spec.argv.contains(&"--acp".to_string()),
            "argv must include --acp; got {:?}",
            spec.argv
        );
    }

    /// `build_launch` must include `--data-dir <managed_home>` so the
    /// vendor CLI writes credentials and conversation history inside
    /// the the host app-owned profile directory.
    #[tokio::test]
    async fn build_launch_sets_managed_data_dir() {
        let adapter = ClineAdapter::new();
        let request = sample_request(None, false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        assert!(
            spec.argv.contains(&"--data-dir".to_string()),
            "argv must include --data-dir; got {:?}",
            spec.argv
        );
        let managed = request.profile.managed_home.display().to_string();
        assert!(
            spec.argv.contains(&managed),
            "argv must include the managed home {managed}; got {:?}",
            spec.argv
        );
    }

    /// Resume=true appends `session resume <seed>` after `--acp` so
    /// Cline re-enters the protocol-load-by-id path.
    #[tokio::test]
    async fn build_launch_resume_emits_session_resume_subcommand() {
        let adapter = ClineAdapter::new();
        let request = sample_request(None, true);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        assert!(
            spec.argv.windows(2).any(|w| w == ["session", "resume"]),
            "resume argv must include `session resume`; got {:?}",
            spec.argv
        );
        assert!(
            spec.argv.contains(&"existing-session-uuid".to_string()),
            "resume argv must include the session id; got {:?}",
            spec.argv
        );
        assert_eq!(spec.session_id, "resume:existing-session-uuid");
    }

    /// Resume=true without a session id is a hard error: Cline has
    /// no way to express a protocol-load-by-id resume without a
    /// target id.
    #[tokio::test]
    async fn build_launch_rejects_resume_without_session_seed() {
        let adapter = ClineAdapter::new();
        let mut request = sample_request(None, true);
        request.session_id_seed = String::new();
        let error = adapter.build_launch(&request).await.unwrap_err();
        assert_eq!(error.code, "cline_resume_missing_session");
    }

    /// A model override is forwarded as `--model <id>`.
    #[tokio::test]
    async fn build_launch_appends_model_flag_when_set() {
        let adapter = ClineAdapter::new();
        let request = sample_request(Some("claude-opus-4-1"), false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        assert!(
            spec.argv
                .windows(2)
                .any(|w| w == ["--model", "claude-opus-4-1"]),
            "model flag must be appended when request carries a model_id; got {:?}",
            spec.argv
        );
    }

    /// BYOK `--key` flags in `model_id` are rejected at parse time.
    /// Cline launches exclusively through the Cline account; any
    /// attempt to inject a `--key` flag or BYOK provider hint is a
    /// launch refusal.
    #[tokio::test]
    async fn build_launch_rejects_byok_key_flag_in_model_id() {
        let adapter = ClineAdapter::new();
        let request = sample_request(Some("--key=sk-attacker"), false);
        let error = adapter.build_launch(&request).await.unwrap_err();
        assert_eq!(error.code, "cline_byok_rejected");
    }

    /// BYOK `sk-` prefixes (the OpenAI/Anthropic key marker) are
    /// rejected at parse time.
    #[tokio::test]
    async fn build_launch_rejects_sk_key_prefix_in_model_id() {
        let adapter = ClineAdapter::new();
        let request = sample_request(Some("sk-anything"), false);
        let error = adapter.build_launch(&request).await.unwrap_err();
        assert_eq!(error.code, "cline_byok_rejected");
    }

    /// BYOK provider hints in the session id seed are rejected.
    #[tokio::test]
    async fn build_launch_rejects_byok_session_id_seed() {
        let adapter = ClineAdapter::new();
        let mut request = sample_request(None, false);
        request.session_id_seed = "--key=sk-via-session".into();
        let error = adapter.build_launch(&request).await.unwrap_err();
        assert_eq!(error.code, "cline_byok_session");
    }

    /// The launch spec must strip every forbidden BYOK env var, plus
    /// the Cline-specific `CLINE_API_KEY` / `CLINE_BYOK_KEY` markers.
    #[tokio::test]
    async fn build_launch_strips_forbidden_byok_env_vars() {
        let adapter = ClineAdapter::new();
        let request = sample_request(None, false);
        let spec = adapter.build_launch(&request).await.expect("build_launch");
        for name in [
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "OPENROUTER_API_KEY",
            "CLINE_API_KEY",
            "RIGHTKIT_DAEMON_TOKEN",
            "RIGHTKIT_GATEWAY_TOKEN",
        ] {
            assert!(
                spec.env_remove.contains(&name.to_string()),
                "env_remove must include {name}; got {:?}",
                spec.env_remove
            );
        }
    }

    /// The session id on a fresh launch is the bare seed; on resume
    /// it carries a `resume:` prefix so downstream consumers see
    /// which path the wire form took.
    #[test]
    fn encode_session_id_round_trips() {
        assert_eq!(encode_session_id(false, "abc"), "abc");
        assert_eq!(encode_session_id(true, "abc"), "resume:abc");
    }

    /// `profile_hash` is deterministic for a profile+fingerprint pair
    /// and changes when either changes — the supervisor uses it to
    /// detect profile swaps.
    #[test]
    fn profile_hash_is_deterministic_and_bounded() {
        let profile = sample_profile();
        let a = profile_hash(&profile);
        let b = profile_hash(&profile);
        assert_eq!(a, b);
        assert_eq!(a.len(), 16, "profile_hash is a 16-hex-char prefix");

        let mut other = profile.clone();
        other.account_fingerprint = Some("fp-2".into());
        assert_ne!(a, profile_hash(&other));
    }

    /// The qualification suite covers both negative and positive
    /// probes, including the `pos.resume` probe that the plan
    /// identifies as the continuity gate. Drift between the live
    /// default suite and the inlined probe-id list is a
    /// security-relevant diff — this test catches it.
    #[test]
    fn qualification_suite_includes_every_default_probe() {
        let adapter = ClineAdapter::new();
        let suite = adapter.qualification_suite();
        let ids: std::collections::BTreeSet<&str> = suite.iter().map(|p| p.id.as_str()).collect();
        for required in CLINE_QUALIFICATION_PROBE_IDS {
            assert!(
                ids.contains(required),
                "qualification suite must include {required}; got {:?}",
                ids
            );
        }
        // `pos.resume` is the headline continuity invariant; we
        // assert it explicitly so the test output names the probe.
        assert!(
            ids.contains("pos.resume"),
            "qualification suite must include the pos.resume continuity probe"
        );
    }

    /// Negative probes are mandatory: a missing mandatory probe is
    /// a security-relevant diff. The plan (§10) names every native
    /// route as mandatory for Cline.
    #[test]
    fn qualification_suite_negative_probes_are_mandatory() {
        let adapter = ClineAdapter::new();
        let suite = adapter.qualification_suite();
        for probe in suite {
            if matches!(probe.polarity, ProbePolarity::Negative) {
                assert!(
                    probe.mandatory,
                    "negative probe {} must be mandatory",
                    probe.id
                );
            }
        }
    }

    /// `normalize_provider_event` translates a `session/update`
    /// `agentMessageChunk` notification into an `AssistantDelta`.
    /// This is the headline continuity invariant: the wire shape
    /// survives the adapter boundary.
    #[test]
    fn normalize_provider_event_translates_agent_message_chunk() {
        let adapter = ClineAdapter::new();
        let event: serde_json::Value = serde_json::from_str(
            r#"{"method":"session/update","params":{"sessionId":"sess-1","update":{"type":"agentMessageChunk","text":"hello"}}}"#,
        )
        .unwrap();
        let normalized = adapter
            .normalize_provider_event(event)
            .unwrap()
            .expect("agentMessageChunk must normalize to Some");
        assert_eq!(
            normalized.kind,
            NormalizedEventKind::AssistantDelta {
                text: "hello".into()
            }
        );
    }

    /// `toolCall` notifications surface as `ToolCallStarted`.
    #[test]
    fn normalize_provider_event_translates_tool_call() {
        let adapter = ClineAdapter::new();
        let event: serde_json::Value = serde_json::from_str(
            r#"{"method":"session/update","params":{"sessionId":"sess-1","update":{"type":"toolCall","id":"call-1","name":"read","args":{"path":"a.txt"}}}}"#,
        )
        .unwrap();
        let normalized = adapter
            .normalize_provider_event(event)
            .unwrap()
            .expect("toolCall must normalize to Some");
        match normalized.kind {
            NormalizedEventKind::ToolCallStarted {
                ref call_id,
                ref tool_name,
                ..
            } => {
                assert_eq!(call_id, "call-1");
                assert_eq!(tool_name, "read");
            }
            other => panic!("expected ToolCallStarted, got {other:?}"),
        }
    }

    /// `sessionReady` notifications surface as `SessionStarted` and
    /// carry the vendor session id back to the supervisor.
    #[test]
    fn normalize_provider_event_translates_session_ready() {
        let adapter = ClineAdapter::new();
        let event: serde_json::Value = serde_json::from_str(
            r#"{"method":"session/update","params":{"sessionId":"vendor-sess-42","update":{"type":"sessionReady"}}}"#,
        )
        .unwrap();
        let normalized = adapter
            .normalize_provider_event(event)
            .unwrap()
            .expect("sessionReady must normalize to Some");
        assert_eq!(
            normalized.kind,
            NormalizedEventKind::SessionStarted {
                vendor_session_id: "vendor-sess-42".into(),
                init: None,
            }
        );
    }

    /// Unknown `session/update` shapes fail closed with a typed
    /// adapter error so a future ACP extension cannot silently
    /// surface a tool the harness has not approved.
    #[test]
    fn normalize_provider_event_fails_closed_on_unknown_session_update_kind() {
        let adapter = ClineAdapter::new();
        let event: serde_json::Value = serde_json::from_str(
            r#"{"method":"session/update","params":{"update":{"type":"futureThing"}}}"#,
        )
        .unwrap();
        let error = adapter
            .normalize_provider_event(event)
            .expect_err("unknown update kind must fail closed");
        assert_eq!(error.code, "cline_unknown_session_update");
    }

    /// Unknown notification methods fail closed with a typed adapter
    /// error; the executor treats an adapter error as
    /// "stop trusting this stream".
    #[test]
    fn normalize_provider_event_fails_closed_on_unknown_notification_method() {
        let adapter = ClineAdapter::new();
        let event: serde_json::Value =
            serde_json::from_str(r#"{"method":"session/neverHeardOfThis","params":{}}"#).unwrap();
        let error = adapter
            .normalize_provider_event(event)
            .expect_err("unknown notification method must fail closed");
        assert_eq!(error.code, "cline_unknown_notification");
    }

    /// JSON-RPC responses (carry `id`, no `method`) are RPC
    /// bookkeeping, not visible events, and must drop to `None`.
    #[test]
    fn normalize_provider_event_drops_jsonrpc_responses() {
        let adapter = ClineAdapter::new();
        let event: serde_json::Value =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#).unwrap();
        let normalized = adapter.normalize_provider_event(event).unwrap();
        assert!(
            normalized.is_none(),
            "JSON-RPC responses must drop to None so the executor does not surface them"
        );
    }

    /// `signed_in_from_output` recognises the documented Cline shapes.
    #[test]
    fn signed_in_recognition_matches_documented_shapes() {
        assert!(!signed_in_from_output("{\"signedIn\":false}"));
        assert!(!signed_in_from_output("not signed in"));
        assert!(signed_in_from_output("{\"account\":\"alice@example.com\"}"));
        assert!(signed_in_from_output(""));
    }

    /// The harness requirements are internally consistent: the
    /// registry refuses to register an adapter whose declared
    /// requirements are inconsistent. The Cline requirements pass.
    #[test]
    fn harness_requirements_pass_registry_consistency_check() {
        let adapter = ClineAdapter::new();
        let reqs = adapter.harness_requirements();
        crate::driver_host::cli_runtime::qualification::requirements_are_consistent(&reqs)
            .expect("Cline requirements must satisfy the registry consistency check");
    }

    /// Provider identifiers retain their declared literal so built-in
    /// registration and adapter lookup agree.
    #[test]
    fn provider_id_matches_declared_literal() {
        let id = CliProviderId::new(PROVIDER_ID);
        assert_eq!(id.as_str(), "cline");
    }
}
