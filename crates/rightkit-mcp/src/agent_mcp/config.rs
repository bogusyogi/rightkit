//! Load MCP server configurations and build a ready McpManager.
//!
//! Config file format is Claude desktop compatible (`.mcp.json`):
//! ```json
//! {
//!   "mcpServers": {
//!     "db":       { "type": "http",  "url": "http://127.0.0.1:9105/mcp" },
//!     "context7": { "type": "sse",   "url": "http://127.0.0.1:9100/sse" },
//!     "github":   { "type": "stdio", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"] }
//!   }
//! }
//! ```
//!
//! Merge order (later entries have higher precedence):
//! 1. `~/.config/host app/mcp.json`
//! 2. `{workspace_root}/.mcp.json`
//! 3. `{workspace_root}/<state-dir>/mcp.json`
//! 4. Path from `RIGHTKIT_MCP_CONFIG` (explicit operator override)
//!
//! All found files are merged; later files override earlier ones for the same server name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agent_mcp::manager::McpManager;
#[cfg(feature = "agent_mcp-os-keyring")]
use crate::agent_mcp::oauth::{
    load_stored_oauth_tokens, refresh_oauth_tokens, McpOAuthCredentialStore,
};
use crate::agent_mcp::transport::{
    HttpTransport, McpTransport, SseTransport, StdioTransport, StubTransport,
};
use crate::agent_mcp::trust_gate::{
    default_trust_store_path, gate_workspace_stdio_servers, load_trust_store,
};
use crate::agent_mcp::types::{McpConfigFile, McpServerConfig, StdioConfig};

/// Whether project-scoped MCP declarations (`.mcp.json`,
/// `<state-dir>/mcp.json`) may take effect.
///
/// Project config is attacker-controllable (a cloned repository), so every
/// public loader resolves this from the persisted workspace-trust store and
/// treats anything other than an explicit `trusted` record as untrusted
/// (fail closed). The explicit variants exist for callers that already hold a
/// trust decision, and for tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectTrust {
    /// Consult the persisted workspace-trust store; untrusted unless recorded.
    Resolve,
    /// The caller vouches that the workspace is trusted.
    Trusted,
    /// Never load project-scoped declarations.
    Untrusted,
}

impl ProjectTrust {
    fn permits(self, workspace_root: Option<&Path>) -> bool {
        match self {
            Self::Trusted => true,
            Self::Untrusted => false,
            Self::Resolve => workspace_root
                .is_some_and(rightkit_app_settings::agent_config::is_workspace_trusted),
        }
    }
}

/// Built-in default MCP servers, present in every session unless a user or
/// workspace config overrides the same name. context7 gives agents current,
/// version-correct library docs with no setup. Spawning still passes through
/// CR's normal MCP trust/approval gate before any tool runs.
pub fn default_mcp_servers() -> Vec<(String, McpServerConfig)> {
    // Test-wide knob: hermetic e2e runs must not spawn `npx` on every session.
    #[cfg(feature = "agent_mcp-host-env")]
    {
        if std::env::var("RIGHTKIT_DISABLE_DEFAULT_MCP").is_ok_and(|v| v == "1") {
            return Vec::new();
        }
    }
    vec![(
        "context7".to_string(),
        McpServerConfig::Stdio(StdioConfig {
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "@upstash/context7-mcp".to_string()],
            env: HashMap::new(),
        }),
    )]
}

/// Build an `McpManager` from all discoverable config files.
/// Pass `workspace_root = None` to skip workspace-level config.
///
/// Workspace-sourced stdio servers pass through the first-use trust gate
/// (audit A-6) before their transport is registered — see [`load_with_trust_gate`].
pub async fn load(workspace_root: Option<&Path>) -> McpManager {
    load_with_trust_gate(
        workspace_root,
        &default_trust_store_path(home_dir().as_deref()),
    )
    .await
}

/// Daemon-owned variant of [`load`] which routes stdio servers through host
/// process policy before they can spawn.
pub async fn load_with_process_launcher(
    workspace_root: Option<&Path>,
    launcher: Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
    launch_context: rightkit_sandbox::execution::ProcessLaunchContext,
) -> McpManager {
    load_with_trust_gate_pooled_inner(
        workspace_root,
        &default_trust_store_path(home_dir().as_deref()),
        None,
        Some((launcher, launch_context)),
    )
    .await
}

/// [`load`] with canonical transport pooling (master-change 3.4): pass a
/// daemon-lifetime [`crate::agent_mcp::McpTransportPool`] so N sessions loading the same
/// server share one transport via one lease each.
pub async fn load_with_pool(
    workspace_root: Option<&Path>,
    pool: &crate::agent_mcp::pool::McpTransportPool,
) -> McpManager {
    load_with_trust_gate_pooled(
        workspace_root,
        &default_trust_store_path(home_dir().as_deref()),
        Some(pool),
    )
    .await
}

/// Daemon-owned variant of [`load_with_pool`] which routes every stdio server
/// through host process policy before it can spawn.
pub async fn load_with_pool_and_process_launcher(
    workspace_root: Option<&Path>,
    pool: &crate::agent_mcp::pool::McpTransportPool,
    launcher: Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
    launch_context: rightkit_sandbox::execution::ProcessLaunchContext,
) -> McpManager {
    load_with_trust_gate_pooled_inner(
        workspace_root,
        &default_trust_store_path(home_dir().as_deref()),
        Some(pool),
        Some((launcher, launch_context)),
    )
    .await
}

/// Session-scoped explicit config file; it is appended at highest precedence
/// without mutating process environment for other concurrent sessions.
pub async fn load_with_config_path_pool_and_process_launcher(
    workspace_root: Option<&Path>,
    config_path: &Path,
    pool: &crate::agent_mcp::pool::McpTransportPool,
    launcher: Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
    launch_context: rightkit_sandbox::execution::ProcessLaunchContext,
) -> McpManager {
    load_with_trust_gate_pooled_impl(
        workspace_root,
        &default_trust_store_path(home_dir().as_deref()),
        Some(pool),
        Some((launcher, launch_context)),
        Some(config_path),
        ProjectTrust::Resolve,
    )
    .await
}

/// Daemon session loader with the caller's own workspace-trust decision.
///
/// A live session has already resolved (and bound) its workspace trust, so it
/// passes that decision here instead of re-reading the persisted store.
/// `ProjectTrust::Untrusted` (or `Resolve` without a persisted `trusted`
/// record) loads user-level and built-in servers only.
pub async fn load_for_session(
    workspace_root: Option<&Path>,
    explicit_config_path: Option<&Path>,
    pool: &crate::agent_mcp::pool::McpTransportPool,
    launcher: Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
    launch_context: rightkit_sandbox::execution::ProcessLaunchContext,
    project_trust: ProjectTrust,
) -> McpManager {
    load_with_trust_gate_pooled_impl(
        workspace_root,
        &default_trust_store_path(home_dir().as_deref()),
        Some(pool),
        Some((launcher, launch_context)),
        explicit_config_path,
        project_trust,
    )
    .await
}

/// Same as [`load`], but with an explicit trust-store path (used by tests and
/// by callers that need a non-default location).
pub async fn load_with_trust_gate(
    workspace_root: Option<&Path>,
    trust_store_path: &Path,
) -> McpManager {
    load_with_trust_gate_pooled_inner(workspace_root, trust_store_path, None, None).await
}

async fn load_with_trust_gate_pooled_inner(
    workspace_root: Option<&Path>,
    trust_store_path: &Path,
    pool: Option<&crate::agent_mcp::pool::McpTransportPool>,
    process_launcher: Option<(
        Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
        rightkit_sandbox::execution::ProcessLaunchContext,
    )>,
) -> McpManager {
    load_with_trust_gate_pooled_impl(
        workspace_root,
        trust_store_path,
        pool,
        process_launcher,
        None,
        ProjectTrust::Resolve,
    )
    .await
}

/// [`load_with_trust_gate`] with optional canonical transport pooling.
pub async fn load_with_trust_gate_pooled(
    workspace_root: Option<&Path>,
    trust_store_path: &Path,
    pool: Option<&crate::agent_mcp::pool::McpTransportPool>,
) -> McpManager {
    load_with_trust_gate_pooled_impl(
        workspace_root,
        trust_store_path,
        pool,
        None,
        None,
        ProjectTrust::Resolve,
    )
    .await
}

/// [`load_with_trust_gate`] with an explicit project-trust decision instead of
/// resolving it from the persisted workspace-trust store.
pub async fn load_with_trust_gate_and_project_trust(
    workspace_root: Option<&Path>,
    trust_store_path: &Path,
    project_trust: ProjectTrust,
) -> McpManager {
    load_with_trust_gate_pooled_impl(
        workspace_root,
        trust_store_path,
        None,
        None,
        None,
        project_trust,
    )
    .await
}

async fn load_with_trust_gate_pooled_impl(
    workspace_root: Option<&Path>,
    trust_store_path: &Path,
    pool: Option<&crate::agent_mcp::pool::McpTransportPool>,
    process_launcher: Option<(
        Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
        rightkit_sandbox::execution::ProcessLaunchContext,
    )>,
    explicit_config_path: Option<&Path>,
    project_trust: ProjectTrust,
) -> McpManager {
    // Project-scoped declarations load only for a trusted workspace.
    let project_config_allowed = project_trust.permits(workspace_root);
    // Seed built-in defaults first so user/workspace config overrides by name.
    // Defaults are exempt from the workspace trust gate — the host app ships them.
    let mut merged = McpConfigFile::default();
    for (name, cfg) in default_mcp_servers() {
        merged.mcp_servers.insert(name, cfg);
    }

    let workspace_config_paths = workspace_root
        .map(|root| {
            [root.join(".mcp.json"), root.join(".rightkit/mcp.json")]
                .into_iter()
                .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_default();
    let mut workspace_stdio_servers: HashMap<String, StdioConfig> = HashMap::new();

    let mut paths = config_paths(workspace_root);
    if let Some(path) = explicit_config_path {
        paths.push(path.to_path_buf());
    }
    for path in paths {
        let is_workspace_file = workspace_config_paths.contains(&path);
        if let Ok(text) = tokio::fs::read_to_string(&path).await {
            match serde_json::from_str::<McpConfigFile>(&text) {
                Ok(file) if is_workspace_file && !project_config_allowed => {
                    // Fail closed: an untrusted workspace contributes nothing
                    // (servers, channel permissions, trust presets, OAuth) and
                    // cannot shadow a user-level server of the same name.
                    for name in file.mcp_servers.keys() {
                        eprintln!(
                            "[mcp] workspace server '{name}' needs trust approval before it can run — project config in {} is ignored until the workspace is trusted (run `rightkit-agent trust`); not spawned",
                            path.display()
                        );
                    }
                }
                Ok(file) => {
                    for (name, cfg) in file.mcp_servers {
                        // Track workspace-sourced stdio servers by name BEFORE
                        // merging, so a later (env override / user config) file
                        // overriding the same name still gates the workspace's
                        // own declaration correctly — the gate gets the config
                        // that was actually LOADED from the workspace file.
                        if is_workspace_file {
                            if let McpServerConfig::Stdio(stdio) = &cfg {
                                workspace_stdio_servers.insert(name.clone(), stdio.clone());
                            }
                        }
                        merged.mcp_servers.insert(name, cfg);
                    }
                    for (name, permissions) in file.mcp_channel_permissions {
                        merged.mcp_channel_permissions.insert(name, permissions);
                    }
                    for (name, policy) in file.mcp_trust {
                        merged.mcp_trust.insert(name, policy);
                    }
                    for (name, oauth) in file.mcp_oauth {
                        merged.mcp_oauth.insert(name, oauth);
                    }
                }
                Err(e) => {
                    eprintln!("[mcp] failed to parse {}: {e}", path.display());
                }
            }
        }
    }

    // `${VAR}` placeholders expand from the process environment only in
    // desktop, daemon and test builds (`host-env`); the mobile runtime expands
    // against an empty environment.
    #[cfg(feature = "agent_mcp-host-env")]
    let env_vars = std::env::vars().collect::<HashMap<_, _>>();
    #[cfg(not(feature = "agent_mcp-host-env"))]
    let env_vars = HashMap::<String, String>::new();
    expand_config_env(&mut merged, &env_vars);
    refresh_and_attach_oauth_tokens(&mut merged).await;

    if workspace_stdio_servers.is_empty() {
        return build_manager_pooled(merged, pool, workspace_root, process_launcher.as_ref()).await;
    }

    let workspace_key = workspace_root
        .map(|root| root.to_string_lossy().to_string())
        .unwrap_or_default();
    let trust_store = load_trust_store(trust_store_path);
    let (approved_names, pending) =
        gate_workspace_stdio_servers(&trust_store, &workspace_key, &workspace_stdio_servers);

    for approval in &pending {
        // Daemon-headless mode (no UI attached) never blocks boot on this —
        // the server simply stays unspawned and a diagnostic line is logged
        // once. A UI surface can later read these via a dedicated route; the
        // pending set itself is intentionally NOT persisted here (only
        // approve/deny decisions are), so it is recomputed fresh every load
        // and never silently disappears.
        eprintln!(
            "[mcp] workspace server '{}' ({} {}) needs trust approval before it can run — not spawned",
            approval.server,
            approval.command,
            approval.args.join(" ")
        );
    }

    let approved: std::collections::HashSet<&str> =
        approved_names.iter().map(String::as_str).collect();
    // Pull every gated (pending or denied) workspace stdio server OUT of the
    // set `build_manager` turns into real transports — a `StubTransport`
    // (zero tools, never spawns a process) is registered for it instead, so
    // the server name still resolves without ever running the
    // workspace-declared command until it is explicitly approved.
    let mut gated_names: Vec<String> = Vec::new();
    for name in workspace_stdio_servers.keys() {
        if !approved.contains(name.as_str()) {
            merged.mcp_servers.remove(name);
            gated_names.push(name.clone());
        }
    }

    let mut manager =
        build_manager_pooled(merged, pool, workspace_root, process_launcher.as_ref()).await;
    for name in gated_names {
        let stub: Arc<dyn McpTransport> = Arc::new(StubTransport::new(Vec::new()));
        manager.register(&name, stub);
    }
    manager
}

/// Expand `${ENV_VAR}` placeholders in stdio MCP environment values.
///
/// Missing variables are left as placeholders so a misconfigured secret remains
/// visible to diagnostics instead of silently becoming an empty string.
pub fn expand_config_env(config: &mut McpConfigFile, env_vars: &HashMap<String, String>) {
    for server in config.mcp_servers.values_mut() {
        expand_server_env(server, env_vars);
    }
}

pub fn attach_stored_oauth_tokens(
    config: &mut McpConfigFile,
    tokens: &HashMap<String, crate::agent_mcp::oauth::McpOAuthTokens>,
) {
    let mut attach = Vec::new();
    for (name, server) in config.mcp_servers.iter() {
        let Some(token) = tokens.get(name) else {
            continue;
        };
        if !token.token_type.eq_ignore_ascii_case("bearer") {
            continue;
        }
        match server {
            McpServerConfig::Http(_) | McpServerConfig::Sse(_) => {
                attach.push((name.clone(), token.clone()))
            }
            McpServerConfig::Stdio(_) => {}
        }
    }
    config.mcp_oauth_tokens.extend(attach);
}

#[cfg(not(feature = "agent_mcp-os-keyring"))]
/// Stored OAuth tokens live in the OS keychain, which this host does not
/// have; there is nothing to refresh or attach.
async fn refresh_and_attach_oauth_tokens(_config: &mut McpConfigFile) {}

#[cfg(feature = "agent_mcp-os-keyring")]
async fn refresh_and_attach_oauth_tokens(config: &mut McpConfigFile) {
    let server_names = config.mcp_servers.keys().cloned().collect::<Vec<_>>();
    let mut tokens = load_stored_oauth_tokens(server_names);
    let now = unix_timestamp_seconds();

    for name in config.mcp_servers.keys() {
        let Some(existing) = tokens.get(name).cloned() else {
            continue;
        };
        if !existing.should_refresh(now) {
            continue;
        }
        let oauth = config.mcp_oauth.get(name);
        let Some(oauth) = oauth else {
            continue;
        };
        match refresh_oauth_tokens(oauth, &existing).await {
            Ok(refreshed) => {
                if let Err(e) = McpOAuthCredentialStore::set(name, &refreshed) {
                    eprintln!("[mcp] failed to persist refreshed OAuth token for '{name}': {e}");
                }
                tokens.insert(name.clone(), refreshed);
            }
            Err(e) => eprintln!("[mcp] OAuth refresh failed for '{name}': {e}"),
        }
    }

    attach_stored_oauth_tokens(config, &tokens);
}

/// Expand `${ENV_VAR}` placeholders in one MCP server config.
pub fn expand_server_env(config: &mut McpServerConfig, env_vars: &HashMap<String, String>) {
    let McpServerConfig::Stdio(stdio) = config else {
        return;
    };

    for value in stdio.env.values_mut() {
        *value = expand_env_placeholders(value, env_vars);
    }
}

fn expand_env_placeholders(value: &str, env_vars: &HashMap<String, String>) -> String {
    let mut expanded = String::with_capacity(value.len());
    let mut rest = value;

    while let Some(start) = rest.find("${") {
        expanded.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find('}') else {
            expanded.push_str(&rest[start..]);
            return expanded;
        };

        let name = &after_start[..end];
        let placeholder = &rest[start..start + 3 + name.len()];
        // A repository-supplied MCP config must never read licence or device material through
        // `${VAR}`, whatever map the caller built from the process environment: such a name is
        // left as the literal placeholder, exactly like an unset one.
        if is_env_placeholder_name(name)
            && !rightkit_app_settings::agent_config::licence_env::is_licence_env_name(name)
        {
            match env_vars.get(name) {
                Some(replacement) => expanded.push_str(replacement),
                None => expanded.push_str(placeholder),
            }
        } else {
            expanded.push_str(placeholder);
        }
        rest = &after_start[end + 1..];
    }

    expanded.push_str(rest);
    expanded
}

fn is_env_placeholder_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Ordered MCP declaration sources. The context inspector uses this same list
/// so discovery and runtime loading cannot silently disagree.
pub fn config_paths(workspace_root: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Lowest precedence: user-level defaults.
    if let Some(home) = home_dir() {
        paths.push(home.join(".config").join("rightkit-agent").join("mcp.json"));
    }

    // Workspace declarations override user defaults. the host app-local config
    // is deliberately later than the compatibility `.mcp.json`.
    if let Some(root) = workspace_root {
        paths.push(root.join(".mcp.json"));
        paths.push(root.join(".rightkit").join("mcp.json"));
    }

    // Highest precedence: the explicit operator-selected file.
    #[cfg(feature = "agent_mcp-host-env")]
    {
        if let Ok(p) = std::env::var("RIGHTKIT_MCP_CONFIG") {
            paths.push(PathBuf::from(p));
        }
    }

    paths
}

fn home_dir() -> Option<PathBuf> {
    rightkit_app_settings::agent_config::os_home_dir()
}

/// Construct the concrete transport for one server config (FM-100 helper).
/// Extracted from [`build_manager_pooled`] so the single-flight pool can build
/// the transport inside its `get_or_init` closure without restructuring the
/// surrounding manager construction.
#[allow(dead_code)]
fn build_transport(
    name: &str,
    cfg: McpServerConfig,
    config: &McpConfigFile,
) -> Arc<dyn McpTransport> {
    build_transport_with_launcher(name, cfg, config, None)
}

fn build_transport_with_launcher(
    name: &str,
    cfg: McpServerConfig,
    config: &McpConfigFile,
    process_launcher: Option<&(
        Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
        rightkit_sandbox::execution::ProcessLaunchContext,
    )>,
) -> Arc<dyn McpTransport> {
    match cfg {
        McpServerConfig::Http(c) => Arc::new(HttpTransport::with_access_token(
            c.url.clone(),
            config
                .mcp_oauth_tokens
                .get(name)
                .map(|tokens| tokens.access_token.clone()),
        )),
        McpServerConfig::Sse(c) => Arc::new(SseTransport::with_access_token_and_origin_policy(
            c.url.clone(),
            config
                .mcp_oauth_tokens
                .get(name)
                .map(|tokens| tokens.access_token.clone()),
            c.allow_cross_origin_endpoint,
        )),
        McpServerConfig::Stdio(c) => {
            let transport = StdioTransport::new(c.command.clone(), c.args.clone(), c.env.clone());
            match process_launcher {
                Some((launcher, context)) => Arc::new(
                    transport
                        .with_process_launch_adapter(launcher.clone())
                        .with_launch_context(context.clone()),
                ),
                None => Arc::new(transport),
            }
        }
    }
}

async fn build_manager_pooled(
    config: McpConfigFile,
    pool: Option<&crate::agent_mcp::pool::McpTransportPool>,
    workspace_root: Option<&Path>,
    process_launcher: Option<&(
        Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
        rightkit_sandbox::execution::ProcessLaunchContext,
    )>,
) -> McpManager {
    let mut manager = McpManager::new();
    // Env is expanded before fingerprinting so `$VAR`-style configs from the
    // same resolved environment pool together, and different resolved
    // environments never alias.
    let scope = workspace_root.map(|root| {
        rightkit_agent_engine_runtime_resources::ServiceScope::Workspace(
            root.to_string_lossy().to_string(),
        )
    });

    for (name, cfg) in &config.mcp_servers {
        // Pooling identity is derived BEFORE the config is consumed by the
        // transport constructor.
        let (kind, endpoint, args, env) = match cfg {
            McpServerConfig::Http(c) => (
                "http",
                c.url.clone(),
                Vec::new(),
                std::collections::HashMap::new(),
            ),
            McpServerConfig::Sse(c) => (
                "sse",
                c.url.clone(),
                Vec::new(),
                std::collections::HashMap::new(),
            ),
            McpServerConfig::Stdio(c) => {
                ("stdio", c.command.clone(), c.args.clone(), c.env.clone())
            }
        };
        // Single-flight build of the transport for this server (FM-100):
        // concurrent loads for the same key share one in-flight construction via
        // the absorbed `SharedMcpPool`, then the resulting transport is handed to
        // the host app's existing leasing layer (`McpTransportPool`).
        let transport: Arc<dyn McpTransport> = match (pool, scope.as_ref()) {
            (Some(pool), Some(scope)) => {
                let key = crate::agent_mcp::pool::pool_key(
                    scope.clone(),
                    kind,
                    &endpoint,
                    &args,
                    &env,
                    &[],
                );
                let sf_key = format!("{key:?}");
                let shared_pool = crate::agent_mcp::shared_pool::get_or_init_shared_pool().await;
                let built = shared_pool
                    .get_or_init_pooled(sf_key.clone(), key.clone(), Some(pool), || {
                        let cfg = cfg.clone();
                        let name = name.to_string();
                        let config = config.clone();
                        async move {
                            Ok::<Arc<dyn McpTransport>, String>(build_transport_with_launcher(
                                &name,
                                cfg,
                                &config,
                                process_launcher,
                            ))
                        }
                    })
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            server = %name,
                            error = %e,
                            "MCP single-flight build failed; building directly"
                        );
                        build_transport_with_launcher(name, cfg.clone(), &config, process_launcher)
                    });
                match pool.acquire(key, || built.clone()) {
                    Ok(lease) => {
                        // Stamp the shared entry's lower-lease generation so the
                        // single-flight pool can later detect eviction/drift.
                        shared_pool.record_epoch(&sf_key, lease.generation()).await;
                        let shared: Arc<dyn McpTransport> = (*lease.value()).clone();
                        manager.register_pooled(name, shared, lease);
                        // Pooled transport registered; permissions/policy below.
                        if let Some(permissions) = config.mcp_channel_permissions.get(name) {
                            manager.register_channel_permissions(name, permissions.clone());
                        }
                        if let Some(policy) = config.mcp_trust.get(name) {
                            manager.register_trust_policy(name, policy.clone());
                        }
                        continue;
                    }
                    Err(snapshot) => {
                        tracing::warn!(
                            server = %name,
                            leases = snapshot.leases,
                            "MCP pool ceiling reached; falling back to an unshared transport"
                        );
                        built
                    }
                }
            }
            _ => build_transport_with_launcher(name, cfg.clone(), &config, process_launcher),
        };
        manager.register(name, transport);
        if let Some(permissions) = config.mcp_channel_permissions.get(name) {
            manager.register_channel_permissions(name, permissions.clone());
        }
        if let Some(policy) = config.mcp_trust.get(name) {
            manager.register_trust_policy(name, policy.clone());
        }
    }

    manager
}

fn unix_timestamp_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
