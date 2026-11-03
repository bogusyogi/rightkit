//! Command Code CLI adapter.
//!
//! PR 11 of the architecture plan. The adapter owns
//! provider-specific knowledge for the `commandcode` CLI:
//!
//! - argv construction: the adapter builds a
//!   [`crate::driver_host::cli_runtime::adapter::CliLaunchSpec`] whose argv list
//!   includes `--managed-home`, `--mcp-config` (one the host app MCP
//!   server entry), `--hooks-config` (file / shell / read / write
//!   deny rules), `--disable-skills`, and `--disable-updates`. The
//!   matching transport
//!   ([`crate::driver_host::cli_runtime::transports::resumable_ndjson::ResumableNdjsonTransport`])
//!   delegates argv construction to the
//!   [`CommandCodeDialect`] so byte-equivalence is preserved.
//! - per-event normalization: the adapter delegates NDJSON line
//!   parsing to the [`CommandCodeDialect`] so the supervisor and
//!   the executor fold events through a single
//!   [`crate::driver_host::contracts::NormalizedEvent`] shape.
//!
//! The adapter is **default-off** until the activation acceptance
//! criteria pass. [`is_activation_passed`] returns `false`, so the
//! qualification runner reports the provider as `Unqualified` and
//! the registry refuses to launch it. The activation gate is wired
//! as a static method so callers (UI, registry, qualification
//! runner) can read it without owning an adapter instance.
//!
//! Session resume uses `ResumeStrategy::ProcessRestartWithResumeFlag`
//! per §10 of the architecture plan: each turn is a fresh process
//! invocation that re-attaches via `--resume <vendor_session_id>`.
//! The nonce survives the process restart because it is carried by
//! the the host app session-id seed, not by the vendor process state.
//!
//! Discovery, native login, and model catalog are wired by the
//! per-vendor PRs. This PR implements them so the adapter is
//! complete and self-describing.

use async_trait::async_trait;
use std::path::PathBuf;
use std::time::Duration;

use crate::driver_host::cli_runtime::adapter::{
    CliAdapterError, CliAuthenticationStatus, CliDiscovery, CliLaunchRequest, CliLaunchSpec,
    CliModelCatalog, CliProviderAdapter, CliRuntimeError, NativeLoginUi,
};
use crate::driver_host::cli_runtime::ids::{
    CliAccountProfile, CliProviderCapabilities, CliProviderId, CliTransportKind,
    HarnessIsolationMode, HarnessRequirements, QualificationProbe,
};
use crate::driver_host::cli_runtime::qualification::now_iso;
use crate::driver_host::cli_runtime::qualification::{
    DEFAULT_NEGATIVE_PROBES, DEFAULT_POSITIVE_PROBES,
};
use crate::driver_host::cli_runtime::transports::resumable_ndjson::{
    CommandCodeDialect, ResumableNdjsonDialect, ResumableNdjsonRequest,
};
use crate::driver_host::contracts::NormalizedEvent;

pub const PROVIDER_ID: &str = "commandcode";
pub const DISPLAY_NAME: &str = "Command Code";
pub const TRANSPORT: CliTransportKind = CliTransportKind::ResumableNdjson;

/// Default timeout for a probe subprocess (binary version / auth
/// status / model list). Mirrors the qualification runner's default
/// so adapter-side discovery uses the same budget.
const DISCOVERY_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Command Code adapter. PR 11 wires the full open contract:
/// discovery, auth status, native login, model list, launch spec
/// construction, capabilities, harness requirements, qualification
/// suite, and event normalization. The adapter is default-off
/// until activation acceptance passes.
pub struct CommandCodeAdapter {
    provider_id: CliProviderId,
}

impl CommandCodeAdapter {
    pub fn new() -> Self {
        Self {
            provider_id: CliProviderId::new(PROVIDER_ID),
        }
    }

    /// Transport this adapter drives.
    pub const TRANSPORT_KIND: CliTransportKind = TRANSPORT;

    /// Activation gate. Returns `false` until the activation
    /// acceptance criteria pass (see architecture plan, §10):
    ///
    /// 1. native Command Code subagent / delegation tool is
    ///    structurally absent or denied;
    /// 2. the host app MCP gating succeeds under
    ///    `NativeReadOnlyCompatibility`;
    /// 3. account login, structured session id, and resume +
    ///    nonce continuity have been exercised end-to-end.
    ///
    /// Until then the adapter stays experimental. The qualification
    /// runner reflects the gate by reporting every probe as failed
    /// (see [`qualification_suite`] with
    /// [`is_activation_passed`] guard).
    pub fn is_activation_passed() -> bool {
        false
    }

    /// The dialect that backs the transport's argv and parser.
    /// Exposed so tests can drive the transport directly without
    /// standing up a full supervisor.
    pub fn dialect() -> CommandCodeDialect {
        CommandCodeDialect::new()
    }
}

impl Default for CommandCodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CliProviderAdapter for CommandCodeAdapter {
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
        // Discovery is best-effort: probe the well-known binary
        // paths and capture the declared version. The daemon caches
        // the result so re-discovery is cheap.
        let probe_paths = candidate_commandcode_paths();
        let mut executable: Option<PathBuf> = None;
        let mut binary_version: Option<String> = None;
        // Candidates are probed one after another with a per-probe bound; the total is capped so
        // several slow candidates can never push discovery past the HTTP client timeout.
        let _ = tokio::time::timeout(
            crate::driver_host::binary_lookup::DISCOVERY_TOTAL_BUDGET,
            async {
                for candidate in &probe_paths {
                    if !binary_exists(candidate) {
                        continue;
                    }
                    match probe_binary_version(candidate).await {
                        Ok(version) => {
                            executable = Some(candidate.clone());
                            binary_version = Some(version.version);
                            break;
                        }
                        Err(_) => continue,
                    }
                }
            },
        )
        .await;
        let executable_sha256 = super::executable_sha256(executable.as_deref());
        Ok(CliDiscovery {
            installed: executable.is_some(),
            executable,
            binary_version,
            executable_sha256,
            protocol_version: None,
            transport: TRANSPORT,
        })
    }

    async fn authentication_status(
        &self,
        profile: &CliAccountProfile,
    ) -> Result<CliAuthenticationStatus, CliAdapterError> {
        // Probe `commandcode auth status`. The vendor prints a
        // structured payload on stdout when the user has completed
        // the browser/device flow; we surface a non-secret
        // fingerprint the registry can persist alongside the
        // attestation.
        let executable = resolve_commandcode_executable().ok_or_else(|| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "commandcode_discover_failed",
                "commandcode binary not found on PATH or in well-known locations",
            )
        })?;
        let output = match probe_auth_status(&executable, profile).await {
            Ok(output) => output,
            Err(_) => {
                return Ok(CliAuthenticationStatus {
                    signed_in: false,
                    auth_mode: crate::driver_host::cli_runtime::ids::CliAuthMode::Unknown,
                    account_label: None,
                    account_fingerprint: None,
                    source_label: "commandcode auth status".into(),
                    observed_at: now_iso(),
                });
            }
        };
        // A signed-in account surfaces `account_id` and `email` on
        // stdout; a logged-out CLI prints `signed_in: false`.
        let signed_in = output.signed_in;
        let account_label = output.account_label;
        let account_fingerprint = output.account_fingerprint;
        Ok(CliAuthenticationStatus {
            signed_in,
            auth_mode: if signed_in {
                crate::driver_host::cli_runtime::ids::CliAuthMode::NativeCliAccount
            } else {
                crate::driver_host::cli_runtime::ids::CliAuthMode::Unknown
            },
            account_label,
            account_fingerprint,
            source_label: "commandcode auth status".into(),
            observed_at: now_iso(),
        })
    }

    async fn begin_native_login(
        &self,
        profile: &CliAccountProfile,
        _ui: &dyn NativeLoginUi,
    ) -> Result<(), CliAdapterError> {
        // Begin the vendor's native login flow. The adapter spawns
        // `commandcode login` and lets the user complete the
        // browser/device flow themselves. We do NOT capture
        // credentials; the vendor writes them into the managed
        // profile directory.
        let executable = resolve_commandcode_executable().ok_or_else(|| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "commandcode_discover_failed",
                "commandcode binary not found on PATH or in well-known locations",
            )
        })?;
        run_login_command(&executable, profile)
            .await
            .map_err(|err| {
                CliAdapterError::new(
                    self.provider_id.clone(),
                    "commandcode_login_failed",
                    format!("commandcode login failed: {err}"),
                )
            })?;
        Ok(())
    }

    async fn list_models(
        &self,
        profile: &CliAccountProfile,
    ) -> Result<CliModelCatalog, CliAdapterError> {
        let executable = resolve_commandcode_executable().ok_or_else(|| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "commandcode_binary_not_found",
                "Command Code CLI is not installed",
            )
        })?;
        let models = probe_models(&executable, profile)
            .await
            .map_err(|message| {
                CliAdapterError::new(
                    self.provider_id.clone(),
                    "commandcode_model_catalog_unavailable",
                    message,
                )
            })?;
        Ok(CliModelCatalog {
            models,
            source: "commandcode models --json".into(),
            fetched_at: now_iso(),
        })
    }

    async fn build_launch(
        &self,
        request: &CliLaunchRequest,
    ) -> Result<CliLaunchSpec, CliAdapterError> {
        if request.resume && request.session_id_seed.is_empty() {
            return Err(CliAdapterError::new(
                self.provider_id.clone(),
                "commandcode_resume_missing_session",
                "resume=true requires a non-empty session_id_seed",
            ));
        }

        // The harness writes the per-binding MCP config and hooks
        // config under the managed home before launching. The
        // adapter only resolves paths; the daemon's launch layer
        // is responsible for the actual file writes.
        let managed_home = request.profile.managed_home.clone();
        let mcp_config_path = managed_home.join("mcp.json");
        let hooks_config_path = managed_home.join("hooks.json");

        // Pre-compute the dialect argv so the spec carries the
        // byte-equivalent argv the supervisor will execute. We do
        // not persist the dialect on the spec — the transport
        // carries it.
        let dialect = CommandCodeDialect::new();
        let dialect_request = ResumableNdjsonRequest {
            session_id_seed: request.session_id_seed.clone(),
            resume: request.resume,
            model_id: request.model_id.clone(),
            managed_home: managed_home.clone(),
            mcp_config_path: mcp_config_path.clone(),
            hooks_config_path: hooks_config_path.clone(),
        };
        let argv = if request.resume {
            dialect.resumed_turn_args(
                &request.session_id_seed,
                &dialect_request,
                &placeholder_launch_spec(&self.provider_id),
            )
        } else {
            dialect.first_turn_args(
                &dialect_request,
                &placeholder_launch_spec(&self.provider_id),
            )
        };
        Ok(CliLaunchSpec {
            provider_id: self.provider_id.clone(),
            transport: TRANSPORT,
            executable: request.executable.clone(),
            argv,
            env_keep: Vec::new(),
            env_remove: forbidden_env_var_names(),
            env: vec![
                (
                    "HOME".to_string(),
                    request.profile.managed_home.to_string_lossy().into_owned(),
                ),
                (
                    "COMMANDCODE_HOME".to_string(),
                    request.profile.managed_home.to_string_lossy().into_owned(),
                ),
            ],
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
            // The adapter never exposes native subagent / team
            // delegation; the harness forbids it via
            // `forbids_native_subagent`.
            supports_child_delegation: false,
            isolation_mode: HarnessIsolationMode::NativeReadOnlyCompatibility,
        }
    }

    fn harness_requirements(&self) -> HarnessRequirements {
        HarnessRequirements {
            requires_managed_profile: true,
            requires_one_mcp_server: true,
            requires_scoped_lease_token: true,
            requires_driver_sandbox: true,
            // Command Code's native subagent / delegation tool is
            // structurally absent or denied until activation
            // acceptance passes. The harness encodes the deny
            // regardless of activation status so a misconfiguration
            // cannot enable delegation by accident.
            forbids_native_subagent: true,
            forbids_native_shell: true,
            forbids_native_browser: true,
            forbids_native_memory_writes: true,
        }
    }

    fn qualification_suite(&self) -> &'static [QualificationProbe] {
        // The default positive + negative suites. The activation
        // gate is checked separately at runtime via
        // [`is_activation_passed`].
        commandcode_qualification_suite()
    }

    fn normalize_provider_event(
        &self,
        event: serde_json::Value,
    ) -> Result<Option<NormalizedEvent>, CliAdapterError> {
        // The dialect is the canonical NDJSON parser. The adapter
        // serializes the JSON value back to a line so it can call
        // the dialect's `parse_line` without depending on its
        // private signature. Source offset is 0 — the supervisor
        // owns the real offset when it reads the line off stdout.
        let line = serde_json::to_string(&event).map_err(|error| {
            CliAdapterError::new(
                self.provider_id.clone(),
                "commandcode_event_serialize",
                format!("failed to serialize Command Code event: {error}"),
            )
        })?;
        let dialect = CommandCodeDialect::new();
        dialect
            .parse_line(&line, 0)
            .map_err(|err| adapter_error_from_runtime_error(&self.provider_id, err))
    }
}

/// Cached Command Code qualification suite. Built once at process
/// start by concatenating the default negative and positive probe
/// suites. The resulting slice is leaked into `'static` so the
/// trait method can return `&'static [QualificationProbe]`.
/// Mirrors the pattern [`crate::driver_host::cli_runtime::providers::cursor::cursor_qualification_suite`]
/// uses so every provider reports a consistent suite shape.
pub fn commandcode_qualification_suite() -> &'static [QualificationProbe] {
    use std::sync::OnceLock;
    static CACHE: OnceLock<&'static [QualificationProbe]> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut probes: Vec<QualificationProbe> =
            Vec::with_capacity(DEFAULT_NEGATIVE_PROBES.len() + DEFAULT_POSITIVE_PROBES.len());
        probes.extend_from_slice(&DEFAULT_NEGATIVE_PROBES);
        probes.extend_from_slice(&DEFAULT_POSITIVE_PROBES);
        Box::leak(probes.into_boxed_slice())
    })
}

// ---------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------

/// Default well-known locations the adapter probes for the
/// `commandcode` binary. The first hit wins.
fn candidate_commandcode_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    // PATH hit via the shared lookup (honours Windows PATHEXT).
    paths.extend(crate::driver_host::binary_lookup::find_on_path(
        "commandcode",
    ));
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        paths.push(home.join(".local/bin/commandcode"));
        paths.push(home.join(".cargo/bin/commandcode"));
        paths.push(home.join("bin/commandcode"));
    }
    paths.push(PathBuf::from("/usr/local/bin/commandcode"));
    paths.push(PathBuf::from("/opt/homebrew/bin/commandcode"));
    paths.push(PathBuf::from("/usr/bin/commandcode"));
    paths
}

/// Resolve a `commandcode` binary by checking the well-known
/// locations. Returns `None` when no binary is present so the
/// adapter can surface a clear "not installed" error.
fn resolve_commandcode_executable() -> Option<PathBuf> {
    candidate_commandcode_paths()
        .into_iter()
        .find(binary_exists)
}

/// Cheap existence check that does not depend on `tokio::process`.
/// We only need to know whether the file exists and is executable;
/// the runtime spawn call will surface a richer error if the path
/// is broken.
fn binary_exists(path: &PathBuf) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => !meta.is_dir(),
        Err(_) => false,
    }
}

/// Result of probing `commandcode --version`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VersionProbe {
    version: String,
}

async fn probe_binary_version(executable: &PathBuf) -> Result<VersionProbe, String> {
    let root = executable
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: root.to_string_lossy().into_owned(),
        sandbox_profile: Some("commandcode-probe".into()),
        sandbox_required: true,
        network_required: false,
    };
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program: executable.to_string_lossy().into_owned(),
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
    .await?;
    if status != 0 {
        return Err(format!("--version exited with {status}"));
    }
    let stdout = String::from_utf8_lossy(&bytes);
    Ok(VersionProbe {
        version: stdout.trim().to_string(),
    })
}

/// Result of probing `commandcode auth status`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct AuthStatusProbe {
    signed_in: bool,
    account_label: Option<String>,
    account_fingerprint: Option<String>,
}

async fn probe_auth_status(
    executable: &PathBuf,
    profile: &CliAccountProfile,
) -> Result<AuthStatusProbe, String> {
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: profile.managed_home.to_string_lossy().into_owned(),
        sandbox_profile: Some("commandcode-auth".into()),
        sandbox_required: true,
        network_required: false,
    };
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program: executable.display().to_string(),
        argv: vec!["auth".into(), "status".into(), "--json".into()],
        cwd: context.workspace_root.clone(),
        context,
        env: vec![
            (
                "HOME".into(),
                profile.managed_home.to_string_lossy().into_owned(),
            ),
            (
                "COMMANDCODE_HOME".into(),
                profile.managed_home.to_string_lossy().into_owned(),
            ),
        ],
    };
    // Spawn, read and wait share one bound; a timeout kills the child (previously only the stdout
    // read was bounded and the child was left running).
    let (status, output) = crate::driver_host::binary_lookup::run_bounded_probe(
        spec,
        DISCOVERY_PROBE_TIMEOUT,
        "auth status probe",
    )
    .await?;
    if status != 0 {
        return Err(format!("auth status exited with {status}"));
    }
    let stdout = String::from_utf8_lossy(&output);
    parse_auth_status_payload(&stdout)
}

/// Parse the JSON payload `commandcode auth status --json` prints
/// to stdout. The vendor's schema is `{ "signed_in": bool, ... }`;
/// unknown shapes are surfaced as `signed_in: false` so the
/// registry does not assume `NativeCliAccount`.
fn parse_auth_status_payload(payload: &str) -> Result<AuthStatusProbe, String> {
    let value: serde_json::Value = serde_json::from_str(payload)
        .map_err(|err| format!("auth status payload not JSON: {err}"))?;
    let signed_in = value
        .get("signed_in")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let account_label = value
        .get("account")
        .and_then(|v| v.get("email"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let account_fingerprint = value
        .get("account")
        .and_then(|v| v.get("id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    Ok(AuthStatusProbe {
        signed_in,
        account_label,
        account_fingerprint,
    })
}

async fn run_login_command(
    executable: &PathBuf,
    profile: &CliAccountProfile,
) -> Result<(), String> {
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: profile.managed_home.to_string_lossy().into_owned(),
        sandbox_profile: Some("commandcode-login".into()),
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
                "COMMANDCODE_HOME".into(),
                profile.managed_home.to_string_lossy().into_owned(),
            ),
        ],
    };
    let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
    let mut child = rightkit_sandbox::execution::ProcessLaunchAdapter::spawn(&adapter, spec)
        .await
        .map_err(|e| format!("spawn error: {e:?}"))?;
    let mut stdin = child
        .take_stdin()
        .ok_or_else(|| "login stdin unavailable".to_string())?;
    let mut stdout = child
        .take_stdout()
        .ok_or_else(|| "login stdout unavailable".to_string())?;
    let mut stderr = child
        .take_stderr()
        .ok_or_else(|| "login stderr unavailable".to_string())?;
    let pump = async {
        let mut terminal_in = tokio::io::stdin();
        let mut terminal_out = tokio::io::stdout();
        let mut terminal_err = tokio::io::stderr();
        tokio::join!(
            async {
                let _ = tokio::io::copy(&mut terminal_in, &mut stdin).await;
            },
            async {
                let _ = tokio::io::copy(&mut stdout, &mut terminal_out).await;
            },
            async {
                let _ = tokio::io::copy(&mut stderr, &mut terminal_err).await;
            },
        );
    };
    tokio::pin!(pump);
    let status = match tokio::time::timeout(Duration::from_secs(120), async {
        tokio::select! {
            result = child.wait() => result,
            _ = &mut pump => child.wait().await,
        }
    })
    .await
    {
        Ok(result) => result.map_err(|e| format!("login wait failed: {e:?}"))?,
        Err(_) => {
            let _ = child.cancel().await;
            return Err("login timed out after 120s".to_string());
        }
    };
    if status != 0 {
        return Err(format!("login exited with status {status}"));
    }
    Ok(())
}

async fn probe_models(
    executable: &PathBuf,
    profile: &CliAccountProfile,
) -> Result<Vec<String>, String> {
    let context = rightkit_sandbox::execution::ProcessLaunchContext {
        workspace_root: profile.managed_home.to_string_lossy().into_owned(),
        sandbox_profile: Some("commandcode-models".into()),
        sandbox_required: true,
        network_required: false,
    };
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program: executable.to_string_lossy().into_owned(),
        argv: vec!["models".into(), "--json".into()],
        cwd: context.workspace_root.clone(),
        context,
        env: vec![
            (
                "HOME".into(),
                profile.managed_home.to_string_lossy().into_owned(),
            ),
            (
                "COMMANDCODE_HOME".into(),
                profile.managed_home.to_string_lossy().into_owned(),
            ),
        ],
    };
    let adapter = rightkit_agent_tools::coding_tools::shell::NativeCommandSandboxAdapter;
    let mut child = rightkit_sandbox::execution::ProcessLaunchAdapter::spawn(&adapter, spec)
        .await
        .map_err(|e| format!("sandboxed model probe failed: {e:?}"))?;
    let mut bytes = Vec::new();
    if let Some(mut stdout) = child.take_stdout() {
        tokio::io::AsyncReadExt::read_to_end(&mut stdout, &mut bytes)
            .await
            .map_err(|e| e.to_string())?;
    }
    let status = child
        .wait()
        .await
        .map_err(|e| format!("model probe wait failed: {e:?}"))?;
    if status != 0 {
        return Err(format!("models exited with {status}"));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("models output was not JSON: {error}"))?;
    let values = value
        .get("models")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "models output omitted models array".to_string())?;
    let models = values
        .iter()
        .filter_map(|item| {
            item.as_str()
                .or_else(|| item.get("id").and_then(serde_json::Value::as_str))
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Err("models output contained no model ids".into());
    }
    Ok(models)
}

/// Translate the open session-id field into the wire form the
/// transport carries on [`crate::driver_host::cli_runtime::adapter::CliLaunchSpec::session_id`].
/// Resume takes a `resume:` prefix so the wire form is unambiguous.
fn encode_session_id(resume: bool, seed: &str) -> String {
    if resume {
        format!("resume:{seed}")
    } else {
        seed.to_string()
    }
}

/// Build a placeholder launch spec the dialect can read while it
/// builds argv. The dialect only reads `executable`; the rest of
/// the spec is filled in by [`build_launch`].
fn placeholder_launch_spec(provider_id: &CliProviderId) -> CliLaunchSpec {
    CliLaunchSpec {
        provider_id: provider_id.clone(),
        transport: TRANSPORT,
        executable: PathBuf::from("/usr/local/bin/commandcode"),
        argv: Vec::new(),
        env_keep: Vec::new(),
        env_remove: Vec::new(),
        env: Vec::new(),
        cwd: PathBuf::from("."),
        mcp_server: None,
        profile_hash: String::new(),
        session_id: String::new(),
    }
}

/// Hash a profile into the per-launch `profile_hash` the spec
/// carries. Matches the Claude/Codex adapter's hashing so all
/// providers report profile swaps in a uniform shape.
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

/// The list of env-var names that must be stripped before
/// `commandcode` launches. Mirrors the union used by the Claude /
/// Codex adapters so all providers carry the same BYOK + daemon
/// token strip list.
fn forbidden_env_var_names() -> Vec<String> {
    crate::driver_host::cli_runtime::auth::FORBIDDEN_BYOK_ENV_VARS
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

fn adapter_error_from_runtime_error(
    provider_id: &CliProviderId,
    error: CliRuntimeError,
) -> CliAdapterError {
    CliAdapterError::new(
        provider_id.clone(),
        "commandcode_event_parse",
        format!("{}: {}", error.code, error.message),
    )
}
