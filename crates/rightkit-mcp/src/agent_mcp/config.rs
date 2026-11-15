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
//! 1. `~/.config/<config-dir-segment>/mcp.json` (see [`crate::agent_mcp::set_config_dir_segment`])
//! 2. `{workspace_root}/.mcp.json`
//! 3. `{workspace_root}/<state-dir>/mcp.json`
//! 4. Path from `<PREFIX>_MCP_CONFIG` (explicit operator override)
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
use crate::agent_mcp::types::{
    HttpConfig, McpConfigFile, McpServerConfig, McpServerOptions, StdioConfig,
};

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
            Self::Resolve => {
                workspace_root.is_some_and(rightkit_agent_config::is_workspace_trusted)
            }
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
        if std::env::var(rightkit_agent_config::env_name("DISABLE_DEFAULT_MCP"))
            .is_ok_and(|v| v == "1")
        {
            return Vec::new();
        }
    }
    vec![(
        "context7".to_string(),
        McpServerConfig::Stdio(StdioConfig {
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "@upstash/context7-mcp".to_string()],
            env: HashMap::new(),
            options: McpServerOptions::default(),
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
    // There is no built-in server: nothing starts a package until the user installs one
    // (`mcp install <name>`, an explicit action that records a pinned entry).
    let mut merged = McpConfigFile::default();

    let workspace_config_paths = workspace_root
        .map(|root| {
            [
                root.join(".mcp.json"),
                root.join(rightkit_agent_config::state_dir_name())
                    .join("mcp.json"),
            ]
            .into_iter()
            .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_default();
    let mut workspace_stdio_servers: HashMap<String, StdioConfig> = HashMap::new();
    // Names whose merged entry came from a repository file (`.mcp.json`,
    // `<state-dir>/mcp.json`); the credential-name block applies only to those.
    let mut workspace_sourced_servers: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    let mut paths = config_paths(workspace_root);
    if let Some(path) = explicit_config_path {
        paths.push(path.to_path_buf());
    }
    for path in paths {
        let is_workspace_file = workspace_config_paths.contains(&path);
        if let Ok(text) = tokio::fs::read_to_string(&path).await {
            match McpConfigFile::from_json_lenient(&text) {
                Ok((file, _)) if is_workspace_file && !project_config_allowed => {
                    // Fail closed: an untrusted workspace contributes nothing
                    // (servers, channel permissions, trust presets, OAuth) and
                    // cannot shadow a user-level server of the same name.
                    for name in file.mcp_servers.keys() {
                        tracing::info!(event = "mcp.server.load", server = %name, outcome = "refused_untrusted_workspace");
                        eprintln!(
                            "[mcp] workspace server '{name}' needs trust approval before it can run — project config in {} is ignored until the workspace is trusted (run `{} trust`); not spawned",
                            path.display(),
                            crate::agent_mcp::client_name()
                        );
                    }
                }
                Ok((file, skipped)) => {
                    for message in &skipped {
                        eprintln!("[mcp] {}: {message}", path.display());
                    }
                    for (name, cfg) in file.mcp_servers {
                        // Track workspace-sourced stdio servers by name BEFORE
                        // merging, so the gate checks the config that was actually
                        // LOADED from the workspace file. A later non-workspace file
                        // (explicit --mcp-config / env override) replaces the entry,
                        // so the workspace declaration is no longer gated or stubbed.
                        if is_workspace_file {
                            if let McpServerConfig::Stdio(stdio) = &cfg {
                                workspace_stdio_servers.insert(name.clone(), stdio.clone());
                            }
                            workspace_sourced_servers.insert(name.clone());
                        } else {
                            // A later user-scope file replaced the entry: the operator wrote it.
                            workspace_sourced_servers.remove(&name);
                            workspace_stdio_servers.remove(&name);
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
    for (name, server) in merged.mcp_servers.iter_mut() {
        let scope = if workspace_sourced_servers.contains(name) {
            PlaceholderScope::Repository
        } else {
            PlaceholderScope::User
        };
        expand_server_env_in_scope(server, &env_vars, scope);
    }
    refresh_and_attach_oauth_tokens(&mut merged).await;

    // Resolve each HTTP server's bearer now that stored OAuth credentials are attached.
    let mut http_bearer = HashMap::new();
    for (name, server) in merged.mcp_servers.iter() {
        let McpServerConfig::Http(http) = server else {
            continue;
        };
        let scope = if workspace_sourced_servers.contains(name) {
            PlaceholderScope::Repository
        } else {
            PlaceholderScope::User
        };
        let oauth = merged
            .mcp_oauth_tokens
            .get(name)
            .map(|stored| stored.access_token.as_str());
        http_bearer.insert(
            name.clone(),
            resolve_http_bearer_with_command(name, http, oauth, &env_vars, scope).await,
        );
    }
    merged.http_bearer = http_bearer;

    // Contract R4 / D16: a stdio server whose script no longer exists is not spawned; its name
    // still resolves to a zero-tool stub so the settings list can show why.
    let mut unreachable_names: Vec<String> = Vec::new();
    // `required` servers that will run as zero-tool stubs, as (server, reason).
    let mut required_failures: Vec<(String, String, McpServerOptions)> = Vec::new();
    for (name, cfg) in merged.mcp_servers.iter() {
        if let McpServerConfig::Stdio(stdio) = cfg {
            if let Some(reason) = crate::agent_mcp::stale::stdio_unreachable_reason(stdio) {
                tracing::info!(event = "mcp.server.load", server = %name, outcome = "unreachable");
                eprintln!("[mcp] server '{name}' {reason}; not spawned");
                if stdio.options.required {
                    required_failures.push((
                        name.clone(),
                        reason.to_string(),
                        stdio.options.clone(),
                    ));
                }
                unreachable_names.push(name.clone());
            }
        }
    }
    for name in &unreachable_names {
        merged.mcp_servers.remove(name);
    }

    if workspace_stdio_servers.is_empty() {
        let mut manager =
            build_manager_pooled(merged, pool, workspace_root, process_launcher.as_ref()).await;
        register_unreachable_stubs(&mut manager, &unreachable_names);
        record_required_stub_failures(&mut manager, &required_failures);
        return manager;
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
        tracing::info!(event = "mcp.trust", server = %approval.server, outcome = "approval_pending");
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
            if let Some(stdio) = workspace_stdio_servers
                .get(name)
                .filter(|stdio| stdio.options.required)
            {
                required_failures.push((
                    name.clone(),
                    "workspace server needs trust approval before it can run".to_string(),
                    stdio.options.clone(),
                ));
            }
            gated_names.push(name.clone());
        }
    }

    let mut manager =
        build_manager_pooled(merged, pool, workspace_root, process_launcher.as_ref()).await;
    for name in gated_names {
        let stub: Arc<dyn McpTransport> = Arc::new(StubTransport::new(Vec::new()));
        manager.register(&name, stub);
    }
    register_unreachable_stubs(&mut manager, &unreachable_names);
    record_required_stub_failures(&mut manager, &required_failures);
    manager
}

/// A required server replaced by a zero-tool stub still counts as failed to start.
fn record_required_stub_failures(
    manager: &mut McpManager,
    failures: &[(String, String, McpServerOptions)],
) {
    for (name, message, options) in failures {
        manager.register_server_options(name, options.clone());
        manager.note_required_failure(name, message);
    }
}

fn register_unreachable_stubs(manager: &mut McpManager, names: &[String]) {
    for name in names {
        let stub: Arc<dyn McpTransport> = Arc::new(StubTransport::new(Vec::new()));
        manager.register(name, stub);
    }
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
        // Contract R2: the token goes only to the origin it was issued for, so a config
        // that reuses the server's name with another URL gets no token.
        let url = match server {
            McpServerConfig::Http(http) => http.url.as_str(),
            McpServerConfig::Sse(sse) => sse.url.as_str(),
            McpServerConfig::Stdio(_) => continue,
        };
        if token.issued_for_server(url) {
            tracing::info!(event = "mcp.oauth.token", server = %name, outcome = "attached");
            attach.push((name.clone(), token.clone()));
        } else {
            tracing::info!(event = "mcp.oauth.token", server = %name, outcome = "refused_origin_mismatch");
            eprintln!(
                "[mcp] stored OAuth token for '{name}' was issued for another origin; not attached"
            );
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

    // Refresh every due token at once, each bounded: several slow token endpoints must not
    // add up and hold session creation (a token that misses the bound keeps its old value).
    let mut due = Vec::new();
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
        if !existing.may_refresh_with(&oauth.resource, &oauth.token_url) {
            eprintln!(
                "[mcp] OAuth refresh for '{name}' refused: the configured endpoints differ from the ones that issued the token"
            );
            continue;
        }
        due.push((name.clone(), oauth.clone(), existing));
    }
    let outcomes =
        futures::future::join_all(due.iter().map(|(name, oauth, existing)| async move {
            let result = tokio::time::timeout(
                crate::agent_mcp::limits::MCP_OAUTH_REFRESH_BOUND,
                refresh_oauth_tokens(oauth, existing),
            )
            .await;
            (name, result)
        }))
        .await;
    for (name, result) in outcomes {
        match result {
            Ok(Ok(refreshed)) => {
                tracing::info!(event = "mcp.oauth.refresh", server = %name, outcome = "ok");
                if let Err(e) = McpOAuthCredentialStore::set(name, &refreshed) {
                    eprintln!("[mcp] failed to persist refreshed OAuth token for '{name}': {e}");
                }
                tokens.insert(name.clone(), refreshed);
            }
            Ok(Err(e)) => {
                tracing::info!(event = "mcp.oauth.refresh", server = %name, outcome = "failed");
                eprintln!("[mcp] OAuth refresh failed for '{name}': {e}")
            }
            Err(_) => {
                tracing::info!(event = "mcp.oauth.refresh", server = %name, outcome = "timed_out");
                eprintln!("[mcp] OAuth refresh for '{name}' did not finish in time; keeping the old token");
            }
        }
    }

    attach_stored_oauth_tokens(config, &tokens);
}

/// Who wrote an MCP server entry, which decides which `${VAR}` names it may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceholderScope {
    /// A file the repository supplies (`.mcp.json`, `<state-dir>/mcp.json`), or any caller that
    /// cannot tell: the strict rule.
    Repository,
    /// The operator's own file (`~/.config/<config-dir-segment>/mcp.json`, `<PREFIX>_MCP_CONFIG`).
    User,
}

/// Expand `${ENV_VAR}` placeholders in one MCP server config under the strict
/// [`PlaceholderScope::Repository`] rule. A caller that knows the entry is the operator's own
/// uses [`expand_server_env_in_scope`] with [`PlaceholderScope::User`].
pub fn expand_server_env(config: &mut McpServerConfig, env_vars: &HashMap<String, String>) {
    expand_server_env_in_scope(config, env_vars, PlaceholderScope::Repository);
}

/// Expand `${ENV_VAR}` placeholders in one MCP server config written in `scope`.
pub fn expand_server_env_in_scope(
    config: &mut McpServerConfig,
    env_vars: &HashMap<String, String>,
    scope: PlaceholderScope,
) {
    // `command`, `args` and `url` expand too, so `"--token", "${TOKEN}"` and a templated URL reach the
    // server resolved; the same scope rule applies as for `env`.
    match config {
        McpServerConfig::Stdio(stdio) => {
            // The command too, so `"command": "${HOME}/bin/server"` resolves; the stale-script
            // check and the launcher both see the expanded text.
            stdio.command = expand_env_placeholders_in_scope(&stdio.command, env_vars, scope);
            for value in stdio.env.values_mut() {
                *value = expand_env_placeholders_in_scope(value, env_vars, scope);
            }
            for arg in stdio.args.iter_mut() {
                *arg = expand_env_placeholders_in_scope(arg, env_vars, scope);
            }
        }
        McpServerConfig::Http(http) => {
            http.url = expand_env_placeholders_in_scope(&http.url, env_vars, scope);
        }
        McpServerConfig::Sse(sse) => {
            sse.url = expand_env_placeholders_in_scope(&sse.url, env_vars, scope);
        }
    }
}

/// Whether a `${name}` placeholder written in `scope` may expand.
///
/// Rule (audit 2026-11-05 M3, narrowed in round 2 finding 2):
/// - Licence and device material and the daemon's own secrets (the daemon token, the
///   gateway and driver secrets) never expand, in any scope: no MCP server needs them, and R46
///   says no child receives them.
/// - A repository-supplied entry cannot name any other credential the launcher scrubs (vendor
///   API keys, `CLAUDE_CODE_OAUTH_TOKEN`, `GITHUB_TOKEN`, route keys): a cloned repository must
///   not rename a scrubbed secret (`"T": "${CLAUDE_CODE_OAUTH_TOKEN}"`) past the name-based scrub.
/// - The operator's own config may name those credentials. Writing `"${GITHUB_TOKEN}"` in their
///   own file is the operator handing that secret to a server they chose; blocking it only broke
///   the standard GitHub/OpenAI MCP entries with a bogus literal token and no stated cause. The
///   scope is decided by where the merged entry was loaded from, so a repository file cannot
///   claim user scope.
///   (Allow-listing by the server's declared env was rejected: the declared env is the very map
///   a repository file writes, so it would let a repository allow-list itself.)
fn placeholder_may_expand(name: &str, scope: PlaceholderScope) -> bool {
    if !is_env_placeholder_name(name) || crate::agent_mcp::host::is_never_expanded_env_name(name) {
        return false;
    }
    match scope {
        PlaceholderScope::User => true,
        PlaceholderScope::Repository => !crate::agent_mcp::host::is_credential_env_name(name),
    }
}

fn expand_env_placeholders_in_scope(
    value: &str,
    env_vars: &HashMap<String, String>,
    scope: PlaceholderScope,
) -> String {
    let mut expanded = String::with_capacity(value.len());
    let mut rest = value;

    while let Some(start) = rest.find("${") {
        expanded.push_str(&rest[..start]);
        let after_start = &rest[start + 2..];
        let Some(end) = after_start.find('}') else {
            expanded.push_str(&rest[start..]);
            return expanded;
        };

        let full_name = &after_start[..end];
        let placeholder = &rest[start..start + 3 + full_name.len()];
        // `${VAR:-default}`: the default applies when VAR is unset or empty.
        let (name, default) = match full_name.split_once(":-") {
            Some((name, default)) => (name, Some(default)),
            None => (full_name, None),
        };
        // A name the scope may not read stays the literal placeholder, exactly like an unset
        // one, and is logged (by name only) so the cause is visible.
        if placeholder_may_expand(name, scope) {
            match (env_vars.get(name), default) {
                (Some(replacement), Some(_)) if replacement.is_empty() => {
                    expanded.push_str(default.unwrap_or_default())
                }
                (Some(replacement), _) => expanded.push_str(replacement),
                (None, Some(default)) => expanded.push_str(default),
                (None, None) => expanded.push_str(placeholder),
            }
        } else {
            if is_env_placeholder_name(name) {
                tracing::warn!(
                    event = "mcp.config.placeholder",
                    variable = %name,
                    ?scope,
                    outcome = "blocked_credential_name"
                );
            }
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

/// Names only, never values: the text lands in logs and in the server's failure line.
fn read_credential_env(
    server: &str,
    kind: &str,
    var: &str,
    env: &HashMap<String, String>,
    scope: PlaceholderScope,
) -> Result<String, String> {
    if !is_env_placeholder_name(var) {
        // Not echoed: a secret pasted into a name field must not reach a log.
        return Err(format!(
            "MCP server '{server}': {kind} does not name a valid environment variable"
        ));
    }
    if !placeholder_may_expand(var, scope) {
        return Err(format!(
            "MCP server '{server}': {var} is a credential a repository MCP config may not read"
        ));
    }
    let found = env.get(var);
    // Windows environment names are case-insensitive; the map is built verbatim.
    #[cfg(windows)]
    let found = found.or_else(|| {
        env.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(var))
            .map(|(_, value)| value)
    });
    match found.map(|value| value.trim()) {
        Some(value) if !value.is_empty() => Ok(value.to_string()),
        _ => Err(format!("MCP server '{server}': {kind} {var} is not set")),
    }
}

/// Expand `${VAR}` placeholders in an `Authorization` header value, failing (by variable name
/// only) on a placeholder that is blocked, unset or malformed.
fn expand_authorization_value(
    server: &str,
    value: &str,
    env: &HashMap<String, String>,
    scope: PlaceholderScope,
) -> Result<String, String> {
    let unresolved = || {
        format!(
            "MCP server '{server}': Authorization header has an unresolved ${{VAR}} placeholder"
        )
    };
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(unresolved());
        };
        let name = &after[..end];
        if is_env_placeholder_name(name) {
            if !placeholder_may_expand(name, scope) {
                return Err(format!(
                    "MCP server '{server}': {name} is a credential a repository MCP config may not read"
                ));
            }
            if !env.contains_key(name) {
                return Err(format!(
                    "MCP server '{server}': Authorization header variable {name} is not set"
                ));
            }
        }
        rest = &after[end + 1..];
    }
    let expanded = expand_env_placeholders_in_scope(value, env, scope);
    if expanded.contains("${") {
        return Err(unresolved());
    }
    Ok(expanded)
}

fn bearer_from_authorization(server: &str, value: &str) -> Result<String, String> {
    let value = value.trim();
    let (scheme, rest) = value.split_once(char::is_whitespace).unwrap_or((value, ""));
    let rest = rest.trim();
    if scheme.eq_ignore_ascii_case("bearer") && !rest.is_empty() {
        Ok(rest.to_string())
    } else {
        Err(format!(
            "MCP server '{server}': Authorization must be 'Bearer <token>'"
        ))
    }
}

/// Resolve the bearer credential an HTTP server sends.
///
/// Precedence: `bearer_token_env_var`; an `Authorization` header from `http_headers`
/// (placeholders expanded) or `env_http_headers`; then the stored OAuth credential `oauth`.
/// Any other header is an error: this build's transport cannot send it, and dropping it
/// silently would send requests the operator did not configure. Errors name variables, never
/// values. `scope` decides which credential names a `${VAR}`/env reference may read.
pub fn resolve_http_bearer(
    server: &str,
    cfg: &HttpConfig,
    oauth: Option<&str>,
    env: &HashMap<String, String>,
    scope: PlaceholderScope,
) -> Result<Option<String>, String> {
    let mut unsupported: Vec<&String> = cfg
        .http_headers
        .keys()
        .chain(cfg.env_http_headers.keys())
        .filter(|name| !name.eq_ignore_ascii_case("authorization"))
        .collect();
    unsupported.sort();
    if let Some(header) = unsupported.first() {
        return Err(format!(
            "MCP server '{server}': header '{header}' is not supported: this build's MCP HTTP transport (rightkit-mcp 0.7.2) sends no custom headers; only Authorization: Bearer is supported"
        ));
    }
    if let Some(var) = cfg.bearer_token_env_var.as_deref() {
        return read_credential_env(server, "bearer_token_env_var", var, env, scope).map(Some);
    }
    if let Some((_, value)) = cfg
        .http_headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
    {
        let expanded = expand_authorization_value(server, value, env, scope)?;
        return bearer_from_authorization(server, &expanded).map(Some);
    }
    if let Some((_, var)) = cfg
        .env_http_headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
    {
        let value = read_credential_env(server, "env_http_headers", var, env, scope)?;
        return bearer_from_authorization(server, &value).map(Some);
    }
    Ok(oauth.map(str::to_string))
}

/// Longest a `bearer_token_command` may run.
#[cfg(feature = "agent_mcp-host-env")]
const BEARER_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Most output a `bearer_token_command` may print.
#[cfg(feature = "agent_mcp-host-env")]
const BEARER_COMMAND_MAX_OUTPUT: usize = 16 * 1024;

/// [`resolve_http_bearer`], plus the `bearer_token_command` of a user-scope server: when the
/// server names one and no `bearer_token_env_var` outranks it, the command's first output line
/// is the bearer (any other configuration error, such as an unsupported header, still wins).
/// Errors name the server and the failure, never the command's output.
pub async fn resolve_http_bearer_with_command(
    server: &str,
    cfg: &HttpConfig,
    oauth: Option<&str>,
    env: &HashMap<String, String>,
    scope: PlaceholderScope,
) -> Result<Option<String>, String> {
    let base = resolve_http_bearer(server, cfg, oauth, env, scope);
    match (&cfg.bearer_token_command, &cfg.bearer_token_env_var, &base) {
        (Some(command), None, Ok(_)) => run_bearer_token_command(server, &command.argv(), scope)
            .await
            .map(Some),
        _ => base,
    }
}

#[cfg(feature = "agent_mcp-host-env")]
async fn run_bearer_token_command(
    server: &str,
    argv: &[String],
    scope: PlaceholderScope,
) -> Result<String, String> {
    if scope == PlaceholderScope::Repository {
        return Err(format!(
            "MCP server '{server}': a repository MCP config may not run bearer_token_command; declare the server in your user config"
        ));
    }
    let Some((program, args)) = argv.split_first() else {
        return Err(format!(
            "MCP server '{server}': bearer_token_command names no program"
        ));
    };
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = match tokio::time::timeout(BEARER_COMMAND_TIMEOUT, command.output()).await {
        Err(_) => {
            return Err(format!(
                "MCP server '{server}': bearer_token_command did not finish within {} seconds",
                BEARER_COMMAND_TIMEOUT.as_secs()
            ))
        }
        Ok(Err(error)) => {
            return Err(format!(
                "MCP server '{server}': bearer_token_command could not start ({})",
                error.kind()
            ))
        }
        Ok(Ok(output)) => output,
    };
    if !output.status.success() {
        return Err(format!(
            "MCP server '{server}': bearer_token_command failed ({})",
            output.status.code().map_or_else(
                || "killed by a signal".to_string(),
                |code| format!("exit status {code}")
            )
        ));
    }
    if output.stdout.len() > BEARER_COMMAND_MAX_OUTPUT {
        return Err(format!(
            "MCP server '{server}': bearer_token_command printed more than {BEARER_COMMAND_MAX_OUTPUT} bytes"
        ));
    }
    let printed = String::from_utf8_lossy(&output.stdout);
    let token = printed.lines().next().unwrap_or("").trim();
    if token.is_empty() {
        return Err(format!(
            "MCP server '{server}': bearer_token_command printed no token"
        ));
    }
    Ok(token.to_string())
}

/// The command runs only in the host build; the portable graph never spawns one.
#[cfg(not(feature = "agent_mcp-host-env"))]
async fn run_bearer_token_command(
    server: &str,
    _argv: &[String],
    _scope: PlaceholderScope,
) -> Result<String, String> {
    Err(format!(
        "MCP server '{server}': bearer_token_command needs the desktop host build"
    ))
}

/// Ordered MCP declaration sources. The context inspector uses this same list
/// so discovery and runtime loading cannot silently disagree.
pub fn config_paths(workspace_root: Option<&Path>) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    // Lowest precedence: user-level defaults.
    if let Some(home) = home_dir() {
        paths.push(
            home.join(".config")
                .join(crate::agent_mcp::trust_gate::config_dir_segment())
                .join("mcp.json"),
        );
    }

    // Workspace declarations override user defaults. The host's own config
    // is deliberately later than the compatibility `.mcp.json`.
    if let Some(root) = workspace_root {
        paths.push(root.join(".mcp.json"));
        paths.push(
            root.join(rightkit_agent_config::state_dir_name())
                .join("mcp.json"),
        );
    }

    // Highest precedence: the explicit operator-selected file.
    #[cfg(feature = "agent_mcp-host-env")]
    {
        if let Ok(p) = std::env::var(rightkit_agent_config::env_name("MCP_CONFIG")) {
            paths.push(PathBuf::from(p));
        }
    }

    paths
}

fn home_dir() -> Option<PathBuf> {
    rightkit_agent_config::os_home_dir()
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
        McpServerConfig::Http(c) => {
            // A resolved bearer (env var / header / OAuth) wins; a resolution error
            // yields a transport that fails with that message instead of connecting
            // unauthenticated.
            let transport = match config.http_bearer.get(name) {
                Some(Err(message)) => {
                    HttpTransport::with_configuration_error(c.url.clone(), message.clone())
                }
                Some(Ok(bearer)) => HttpTransport::with_access_token(c.url.clone(), bearer.clone()),
                None => HttpTransport::with_access_token(
                    c.url.clone(),
                    config
                        .mcp_oauth_tokens
                        .get(name)
                        .map(|stored| stored.access_token.clone()),
                ),
            };
            Arc::new(transport.with_timeouts(c.options.startup_timeout(), c.options.tool_timeout()))
        }
        McpServerConfig::Sse(c) => Arc::new(
            SseTransport::with_access_token_and_origin_policy(
                c.url.clone(),
                config
                    .mcp_oauth_tokens
                    .get(name)
                    .map(|tokens| tokens.access_token.clone()),
                c.allow_cross_origin_endpoint,
            )
            .with_timeouts(c.options.startup_timeout(), c.options.tool_timeout()),
        ),
        McpServerConfig::Stdio(c) => {
            let transport = StdioTransport::new(c.command.clone(), c.args.clone(), c.env.clone())
                .with_timeouts(c.options.startup_timeout(), c.options.tool_timeout());
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

/// The launch settings that decide how a pooled stdio server's process is started and confined
/// (audit 2026-11-05 M1). HTTP and SSE servers start no process, so they carry none.
fn launch_pool_extras(
    kind: &str,
    process_launcher: Option<&(
        Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>,
        rightkit_sandbox::execution::ProcessLaunchContext,
    )>,
) -> Vec<(&'static str, String)> {
    if kind != "stdio" {
        return Vec::new();
    }
    match process_launcher {
        None => vec![("launcher", "direct".to_string())],
        Some((_, context)) => vec![
            ("launcher", "managed".to_string()),
            ("launch_root", context.workspace_root.clone()),
            // The profile only changes the launch when confinement is enforced; keying on it
            // otherwise would split unconfined sessions over a label alone.
            (
                "sandbox_profile",
                if context.sandbox_required {
                    format!("{:?}", context.sandbox_profile)
                } else {
                    String::new()
                },
            ),
            ("sandbox_required", context.sandbox_required.to_string()),
            ("network_required", context.network_required.to_string()),
        ],
    }
}

/// Settings that change what a pooled transport does without changing its endpoint: the resolved
/// HTTP credential (hashed by `pool_key`, never kept in the clear) and the per-server timeouts.
fn option_pool_extras(
    cfg: &McpServerConfig,
    config: &McpConfigFile,
    name: &str,
) -> Vec<(&'static str, String)> {
    let mut extras = Vec::new();
    if matches!(cfg, McpServerConfig::Http(_)) {
        let bearer = match config.http_bearer.get(name) {
            Some(Ok(Some(value))) => value.clone(),
            Some(Ok(None)) => String::new(),
            Some(Err(message)) => format!("error:{message}"),
            None => config
                .mcp_oauth_tokens
                .get(name)
                .map(|stored| stored.access_token.clone())
                .unwrap_or_default(),
        };
        extras.push(("bearer", bearer));
    }
    let options = cfg.options();
    if let Some(startup) = options.startup_timeout() {
        extras.push(("startup_timeout_ms", startup.as_millis().to_string()));
    }
    if let Some(request) = options.tool_timeout() {
        extras.push(("tool_timeout_ms", request.as_millis().to_string()));
    }
    extras
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
        manager.register_server_options(name, cfg.options().clone());
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
                // M1: launch and sandbox settings are part of the identity, so a safe-mode
                // session never leases a process another session spawned unconfined.
                let mut launch = launch_pool_extras(kind, process_launcher);
                launch.extend(option_pool_extras(cfg, &config, name));
                let launch_refs: Vec<(&str, &str)> = launch
                    .iter()
                    .map(|(name, value)| (*name, value.as_str()))
                    .collect();
                let key = crate::agent_mcp::pool::pool_key(
                    scope.clone(),
                    kind,
                    &endpoint,
                    &args,
                    &env,
                    &launch_refs,
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
