use rightkit_sandbox::execution::ProcessLaunchAdapter;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{PluginError, PluginIsolationMode, PluginSandboxPolicy};

/// Stable owner identity for dynamic registrations contributed by one plugin.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PluginRegistrationScope(String);

impl PluginRegistrationScope {
    pub fn new(plugin_id: impl Into<String>) -> Self {
        Self(format!("plugin:{}", plugin_id.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct PluginInvocation {
    pub kind: String,
    pub name: String,
    pub args: Value,
    pub session_id: String,
    pub workspace_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInvocationResult {
    pub content: String,
    pub is_error: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InvocationPayload<'a> {
    kind: &'a str,
    name: &'a str,
    args: &'a Value,
    context: InvocationContext<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InvocationContext<'a> {
    session_id: &'a str,
    workspace_root: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct JsonPluginOutput {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    is_error: Option<bool>,
    #[serde(default)]
    success: Option<bool>,
}

/// Adapter-owned entry point for daemon hosts. Hosts that can supply native
/// process policy should use this seam; legacy wrapper remains available for
/// compatibility with embedded callers.
pub async fn run_plugin_entry_with_policy_and_adapter(
    entry: impl AsRef<Path>,
    plugin_root: impl AsRef<Path>,
    invocation: PluginInvocation,
    policy: PluginSandboxPolicy,
    adapter: Arc<dyn ProcessLaunchAdapter>,
) -> Result<PluginInvocationResult, PluginError> {
    let plugin_root = plugin_root.as_ref();
    let effective_policy = effective_policy(policy);
    if effective_policy.isolation == PluginIsolationMode::MetadataOnly {
        return Err(PluginError::Io(
            "plugin sandbox isolation is metadataOnly; local subprocess execution is disabled"
                .into(),
        ));
    }
    let entry_path = resolve_entry(entry.as_ref(), plugin_root, &effective_policy)?;
    let (program, args) = command_for_entry(&entry_path);
    let payload = InvocationPayload {
        kind: &invocation.kind,
        name: &invocation.name,
        args: &invocation.args,
        context: InvocationContext {
            session_id: &invocation.session_id,
            workspace_root: invocation.workspace_root.display().to_string(),
        },
    };
    let stdin = serde_json::to_vec(&payload).map_err(|e| PluginError::Parse(e.to_string()))?;
    let spec = rightkit_sandbox::execution::CommandLaunchSpec {
        program,
        argv: args,
        cwd: plugin_root.display().to_string(),
        context: rightkit_sandbox::execution::ProcessLaunchContext {
            workspace_root: plugin_root.display().to_string(),
            sandbox_profile: Some(rightkit_sandbox::execution::PLUGIN_SANDBOX_PROFILE.into()),
            sandbox_required: true,
            // Network stays available only when the policy does not strip it.
            network_required: !effective_policy.strip_network_env,
        },
        // The complete environment the plugin sees. The launcher starts a `plugin` profile
        // child from an empty environment (contract R6), so nothing else reaches it.
        env: plugin_environment(plugin_root, &invocation, &effective_policy),
    };
    let mut child = adapter
        .spawn(spec)
        .await
        .map_err(|e| PluginError::Io(format!("failed to start plugin: {e}")))?;
    let mut sink = child
        .take_stdin()
        .ok_or_else(|| PluginError::Io("plugin stdin unavailable".into()))?;
    if let Err(error) = tokio::io::AsyncWriteExt::write_all(&mut sink, &stdin).await {
        let _ = child.cancel().await;
        let _ = child.wait().await;
        return Err(PluginError::Io(format!(
            "failed to write plugin stdin: {error}"
        )));
    }
    // Plugin protocols consume one JSON document through stdin, then wait for
    // EOF before producing output. Close our pipe before waiting on process.
    drop(sink);
    let mut stdout = match child.take_stdout() {
        Some(stdout) => stdout,
        None => {
            let _ = child.cancel().await;
            let _ = child.wait().await;
            return Err(PluginError::Io("plugin stdout unavailable".into()));
        }
    };
    let mut stderr = match child.take_stderr() {
        Some(stderr) => stderr,
        None => {
            let _ = child.cancel().await;
            let _ = child.wait().await;
            return Err(PluginError::Io("plugin stderr unavailable".into()));
        }
    };
    let stdout_task = tokio::spawn(async move {
        let mut data = Vec::new();
        let _ = tokio::io::AsyncReadExt::read_to_end(&mut stdout, &mut data).await;
        data
    });
    let stderr_task = tokio::spawn(async move {
        let mut data = Vec::new();
        let _ = tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut data).await;
        data
    });
    let timeout_ms = effective_policy.timeout_ms.clamp(1_000, 120_000);
    let status = match tokio::time::timeout(
        std::time::Duration::from_millis(timeout_ms),
        child.wait(),
    )
    .await
    {
        Ok(result) => {
            result.map_err(|e| PluginError::Io(format!("failed to wait for plugin: {e}")))?
        }
        Err(_) => {
            let _ = child.cancel().await;
            let _ = child.wait().await;
            return Err(PluginError::Io(format!(
                "plugin timed out after {timeout_ms}ms"
            )));
        }
    };
    let stdout = String::from_utf8_lossy(&stdout_task.await.unwrap_or_default())
        .trim()
        .to_string();
    let stderr = String::from_utf8_lossy(&stderr_task.await.unwrap_or_default())
        .trim()
        .to_string();
    if status != 0 {
        return Ok(PluginInvocationResult {
            content: if stderr.is_empty() {
                format!("plugin exited with status {status}")
            } else {
                format!("plugin exited with status {status}: {stderr}")
            },
            is_error: true,
        });
    }
    Ok(parse_plugin_output(&stdout))
}

fn effective_policy(mut policy: PluginSandboxPolicy) -> PluginSandboxPolicy {
    match policy.isolation {
        PluginIsolationMode::TrustedSubprocess => {}
        PluginIsolationMode::RestrictedSubprocess => {
            policy.allow_absolute_entry = false;
            policy.inherit_env = false;
            policy.strip_network_env = true;
            policy.timeout_ms = policy.timeout_ms.min(15_000);
        }
        PluginIsolationMode::MetadataOnly => {}
    }
    policy
}

fn resolve_entry(
    entry: &Path,
    plugin_root: &Path,
    policy: &PluginSandboxPolicy,
) -> Result<PathBuf, PluginError> {
    if entry.is_absolute() {
        if policy.allow_absolute_entry {
            Ok(entry.to_path_buf())
        } else {
            Err(PluginError::Io(
                "plugin entry must be relative to the plugin root".into(),
            ))
        }
    } else {
        let root = plugin_root
            .canonicalize()
            .map_err(|e| PluginError::Io(format!("failed to resolve plugin root: {e}")))?;
        let candidate = plugin_root.join(entry);
        let resolved = candidate
            .canonicalize()
            .map_err(|e| PluginError::Io(format!("failed to resolve plugin entry: {e}")))?;
        if resolved.starts_with(&root) {
            Ok(resolved)
        } else {
            Err(PluginError::Io(
                "plugin entry resolved outside the plugin root".into(),
            ))
        }
    }
}

fn command_for_entry(entry: &Path) -> (String, Vec<String>) {
    let entry_s = entry.display().to_string();
    match entry.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "py" => {
            let python = default_python();
            #[cfg(windows)]
            if python == "py" {
                return (python, vec!["-3.11".into(), entry_s]);
            }
            (python, vec![entry_s])
        }
        "js" | "mjs" | "cjs" => ("node".into(), vec![entry_s]),
        _ => (entry_s, Vec::new()),
    }
}

/// Resolve the Python interpreter to run a plugin. `$PYTHON` wins; otherwise prefer
/// `python3` (macOS/Linux ship no bare `python`) and fall back to `python`. Resolving to
/// an absolute path keeps it working even when the sandbox strips PATH from the child.
fn default_python() -> String {
    if let Ok(explicit) = std::env::var("PYTHON") {
        if !explicit.is_empty() {
            return explicit;
        }
    }
    #[cfg(windows)]
    {
        "py".into()
    }
    #[cfg(not(windows))]
    {
        for candidate in ["python3", "python"] {
            if let Some(resolved) = resolve_on_path(candidate) {
                return resolved;
            }
        }
        "python3".into()
    }
}

#[cfg(not(windows))]
fn resolve_on_path(name: &str) -> Option<String> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .map(|candidate| candidate.display().to_string())
}

/// The complete environment a plugin process starts with (contract R6). `inheritEnv=false`
/// gives it only the minimal platform variables plus `allowedEnv`; `deniedEnv` is removed in
/// every case; `stripNetworkEnv` removes the proxy, certificate and cloud-credential variables.
fn plugin_environment(
    plugin_root: &Path,
    invocation: &PluginInvocation,
    policy: &PluginSandboxPolicy,
) -> Vec<(String, String)> {
    let mut env: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    if policy.inherit_env {
        for (key, value) in std::env::vars_os() {
            if let (Some(key), Some(value)) = (key.to_str(), value.to_str()) {
                env.insert(key.to_string(), value.to_string());
            }
        }
    } else {
        for key in minimal_env_keys() {
            if let Ok(value) = std::env::var(key) {
                env.insert((*key).to_string(), value);
            }
        }
        for key in &policy.allowed_env {
            if let Ok(value) = std::env::var(key) {
                env.insert(key.clone(), value);
            }
        }
    }
    for key in &policy.denied_env {
        env.remove(key);
    }
    if policy.strip_network_env {
        for key in network_env_keys() {
            env.remove(&key);
        }
        env.insert("RIGHTKIT_PLUGIN_NETWORK".into(), "disabled".into());
    }
    env.insert(
        "RIGHTKIT_PLUGIN_ROOT".into(),
        plugin_root.display().to_string(),
    );
    env.insert(
        "RIGHTKIT_WORKSPACE_ROOT".into(),
        invocation.workspace_root.display().to_string(),
    );
    env.insert(
        "RIGHTKIT_PLUGIN_ENV_INHERITED".into(),
        if policy.inherit_env { "true" } else { "false" }.into(),
    );
    env.into_iter().collect()
}

fn minimal_env_keys() -> &'static [&'static str] {
    if cfg!(windows) {
        &[
            "PATH",
            "Path",
            "PATHEXT",
            "SYSTEMROOT",
            "SystemRoot",
            "WINDIR",
            "TEMP",
            "TMP",
            "USERPROFILE",
            "LOCALAPPDATA",
            "APPDATA",
            "PYTHON",
            "NODE_OPTIONS",
        ]
    } else {
        &[
            "PATH",
            "HOME",
            "TMPDIR",
            "TEMP",
            "PYTHON",
            "NODE_OPTIONS",
            "LANG",
            "LC_ALL",
        ]
    }
}

fn network_env_keys() -> Vec<String> {
    let base = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "no_proxy",
        "REQUESTS_CA_BUNDLE",
        "SSL_CERT_FILE",
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "GITHUB_TOKEN",
        "GH_TOKEN",
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENROUTER_API_KEY",
        "GOOGLE_API_KEY",
    ];
    let mut keys = HashSet::new();
    for key in base {
        keys.insert(key.to_string());
        keys.insert(key.to_ascii_lowercase());
    }
    keys.into_iter().collect()
}

fn parse_plugin_output(stdout: &str) -> PluginInvocationResult {
    if stdout.is_empty() {
        return PluginInvocationResult {
            content: String::new(),
            is_error: false,
        };
    }

    match serde_json::from_str::<JsonPluginOutput>(stdout) {
        Ok(parsed) => {
            let is_error = parsed
                .is_error
                .unwrap_or_else(|| parsed.success.map(|s| !s).unwrap_or(false));
            let content = parsed
                .content
                .or(parsed.output)
                .or(parsed.error)
                .unwrap_or_default();
            PluginInvocationResult { content, is_error }
        }
        Err(_) => PluginInvocationResult {
            content: stdout.to_string(),
            is_error: false,
        },
    }
}

// ── Per-session activation ─────────────────────────────────────────────────

use std::collections::HashMap;
use std::sync::Mutex;

use crate::manifest::PluginManifest;

/// Per-session plugin activation overrides.
///
/// Global enable/disable is stored in `PluginManifest.enabled` on disk.
/// Per-session overrides let a session enable a globally-disabled plugin or
/// disable a globally-enabled plugin without touching the persisted manifest.
/// This is what FM-156 "per-session activation" requires: a session can have
/// its own plugin set while the global install remains unchanged.
#[derive(Debug, Default, Clone)]
pub struct SessionPluginActivations {
    /// session_id -> set of plugin ids explicitly enabled for that session
    enabled_overrides: HashMap<String, HashSet<String>>,
    /// session_id -> set of plugin ids explicitly disabled for that session
    disabled_overrides: HashMap<String, HashSet<String>>,
}

impl SessionPluginActivations {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enable a plugin for a specific session. Overrides global disabled state.
    pub fn enable_for_session(&mut self, plugin_id: &str, session_id: &str) {
        self.disabled_overrides
            .entry(session_id.to_string())
            .or_default()
            .remove(plugin_id);
        self.enabled_overrides
            .entry(session_id.to_string())
            .or_default()
            .insert(plugin_id.to_string());
    }

    /// Disable a plugin for a specific session. Overrides global enabled state.
    pub fn disable_for_session(&mut self, plugin_id: &str, session_id: &str) {
        self.enabled_overrides
            .entry(session_id.to_string())
            .or_default()
            .remove(plugin_id);
        self.disabled_overrides
            .entry(session_id.to_string())
            .or_default()
            .insert(plugin_id.to_string());
    }

    /// Clear all per-session overrides for a session (e.g. on session delete).
    pub fn clear_session(&mut self, session_id: &str) {
        self.enabled_overrides.remove(session_id);
        self.disabled_overrides.remove(session_id);
    }

    /// Resolve whether a plugin is active for a given session.
    ///
    /// Precedence: per-session disabled > per-session enabled > global `enabled`.
    pub fn is_active(&self, plugin_id: &str, session_id: &str, global_enabled: bool) -> bool {
        if let Some(disabled) = self.disabled_overrides.get(session_id) {
            if disabled.contains(plugin_id) {
                return false;
            }
        }
        if let Some(enabled) = self.enabled_overrides.get(session_id) {
            if enabled.contains(plugin_id) {
                return true;
            }
        }
        global_enabled
    }

    /// Resolve active manifests for a session from a slice of manifests.
    pub fn active_for_session<'a>(
        &self,
        manifests: &'a [PluginManifest],
        session_id: &str,
    ) -> Vec<&'a PluginManifest> {
        manifests
            .iter()
            .filter(|m| self.is_active(&m.id, session_id, m.enabled))
            .collect()
    }

    /// List plugin ids that are enabled for a session (including global enabled).
    pub fn enabled_ids_for_session(
        &self,
        manifests: &[PluginManifest],
        session_id: &str,
    ) -> Vec<String> {
        self.active_for_session(manifests, session_id)
            .into_iter()
            .map(|m| m.id.clone())
            .collect()
    }

    /// Snapshot of overrides for a session (for diagnostics / API).
    pub fn overrides_for_session(&self, session_id: &str) -> (Vec<String>, Vec<String>) {
        let enabled = self
            .enabled_overrides
            .get(session_id)
            .map(|s| {
                let mut v: Vec<String> = s.iter().cloned().collect();
                v.sort();
                v
            })
            .unwrap_or_default();
        let disabled = self
            .disabled_overrides
            .get(session_id)
            .map(|s| {
                let mut v: Vec<String> = s.iter().cloned().collect();
                v.sort();
                v
            })
            .unwrap_or_default();
        (enabled, disabled)
    }

    pub fn has_overrides_for(&self, session_id: &str) -> bool {
        self.enabled_overrides
            .get(session_id)
            .map(|s| !s.is_empty())
            .unwrap_or(false)
            || self
                .disabled_overrides
                .get(session_id)
                .map(|s| !s.is_empty())
                .unwrap_or(false)
    }

    pub fn enabled_overrides_keys(&self) -> Vec<String> {
        self.enabled_overrides.keys().cloned().collect()
    }

    pub fn disabled_overrides_keys(&self) -> Vec<String> {
        self.disabled_overrides.keys().cloned().collect()
    }
}

/// Thread-safe shared per-session activation store.
pub type SharedSessionActivations = Arc<Mutex<SessionPluginActivations>>;

pub fn new_shared_activations() -> SharedSessionActivations {
    Arc::new(Mutex::new(SessionPluginActivations::new()))
}

/// Check whether a plugin invocation should proceed for the given session.
///
/// Returns `Ok(())` if the plugin is active for the session, otherwise
/// returns a `PluginError::NotFound` describing that the plugin is disabled
/// for that session. This is used by the daemon before dispatching a tool
/// call to a plugin subprocess.
pub fn check_plugin_active_for_session(
    manifest: &PluginManifest,
    session_id: &str,
    activations: &SessionPluginActivations,
) -> Result<(), crate::PluginError> {
    if activations.is_active(&manifest.id, session_id, manifest.enabled) {
        Ok(())
    } else {
        Err(crate::PluginError::NotFound(format!(
            "plugin '{}' is disabled for session '{}'",
            manifest.id, session_id
        )))
    }
}
