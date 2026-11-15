//! Multiplexes several named MCP connections and proxies their tools into
//! the host app's namespaced registry.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::Mutex;

use crate::agent_mcp::error::McpError;
use crate::session_status::{EngineConnection, EngineSource};
use crate::{parse_qualified_name, sanitize_tool_name};

fn canonical_read_tool(tool: &str) -> Option<&'static str> {
    match tool {
        "membrane_blueprint" | "blueprint_recall" | "blueprint_search" | "blueprint_expand" => {
            Some("membrane_blueprint")
        }
        "membrane_memory_read" => Some("membrane_memory_read"),
        "membrane_source_read" => Some("membrane_source_read"),
        _ => None,
    }
}
use crate::agent_mcp::server_mode::{
    McpServedInvocationAudit, McpServerModePolicy, McpServerToolSummary,
};
use crate::agent_mcp::transport::McpTransport;
use crate::agent_mcp::types::{
    McpChannelPermissions, McpResourceContent, McpResourceDef, McpServerOptions, McpToolDef,
    McpToolResult, McpTrustPolicy,
};

#[derive(Debug, Clone)]
struct RegistryEntry {
    server: String,
    original: String,
    normalized: String,
    def: McpToolDef,
}

/// What the last completed tool listing of one server returned. Kept so the connection state
/// can be read later without contacting the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListingOutcome {
    /// The listing succeeded with this many exposed tools.
    Listed { tools: usize },
    /// The listing returned an error (the error text is not kept).
    Failed,
}

/// Authenticated identity of the single canonical Membrane installation used
/// by the host app Blueprint operations. Logical server names are insufficient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalMembraneBinding {
    pub service_id: String,
    pub installation_id: String,
    pub cortex_store_id: String,
    pub release_generation: String,
    pub startup_generation: u64,
    pub runtime_origin: String,
    pub stable_install_root: Option<String>,
    pub protocol_version: u32,
    pub schema_version: u32,
}

/// Registry-owned caller identity for this session's selected repository.
#[derive(Debug, Clone, PartialEq)]
pub struct CanonicalMembraneCaller {
    pub root: String,
    pub repository_id: String,
    pub scope_id: String,
    pub scope_descriptor: Option<serde_json::Value>,
}

/// Served-invocation audit rows kept in memory per manager (newest win).
const MAX_INVOCATION_AUDIT_ROWS: usize = 1024;

/// Per-server runtime options derived from [`McpServerOptions`].
struct ServerRuntime {
    filter: Option<McpToolNameFilter>,
    startup_timeout: Option<std::time::Duration>,
    tool_timeout: Option<std::time::Duration>,
    required: bool,
    expose: crate::agent_mcp::types::McpExpose,
}

/// Holds multiple named MCP connections and proxies their tools.
pub struct McpManager {
    transports: HashMap<String, Arc<dyn McpTransport>>,
    channel_permissions: HashMap<String, McpChannelPermissions>,
    trust_policies: HashMap<String, McpTrustPolicy>,
    invocation_audit: Mutex<Vec<McpServedInvocationAudit>>,
    registry: Mutex<HashMap<String, RegistryEntry>>,
    /// Leases on shared (pooled) transports (master-change 3.4). A lease is
    /// held for the manager's lifetime so the pooled instance cannot be
    /// evicted while any session still uses it; dropping the manager drops
    /// the lease and makes the instance idle-evictable.
    pool_leases:
        HashMap<String, rightkit_agent_engine_runtime_resources::Lease<Arc<dyn McpTransport>>>,
    scope_servers: HashMap<String, HashSet<String>>,
    generation: u64,
    canonical_membrane_binding: Option<CanonicalMembraneBinding>,
    canonical_membrane_server: Option<String>,
    canonical_membrane_transport: Option<Arc<dyn McpTransport>>,
    canonical_membrane_endpoint: Option<String>,
    canonical_membrane_caller: Option<CanonicalMembraneCaller>,
    elicitation_session: Mutex<Option<String>>,
    /// Upper bound a single discovery pass waits on any one server's tool
    /// listing. Slower servers are reported as "starting" and join on a later
    /// discovery pass once their listing completes in the background.
    startup_bound: std::time::Duration,
    /// One total deadline per tool call, covering lookup, listing, the call
    /// itself and any retries underneath it.
    call_deadline: std::time::Duration,
    /// Tool listings still running in the background, keyed by server.
    inflight_listings: Mutex<HashMap<String, InflightListing>>,
    /// Per-run `--tools` / `--exclude-tools` selection over qualified
    /// `mcp__server__tool` names. `None` selects every tool.
    tool_name_filter: Option<McpToolNameFilter>,
    /// Per-server options registered from config (timeouts, tool filter, `required`).
    server_runtime: HashMap<String, ServerRuntime>,
    /// `required` servers whose startup failed, as (server, error).
    required_errors: Mutex<Vec<(String, String)>>,
    /// Last `tools_list_changed_seq` observed per server (A067).
    list_changed_seen: Mutex<HashMap<String, u64>>,
    /// Outcome of the last completed tool listing per server; read by
    /// [`McpManager::connection_states`].
    listing_outcomes: Mutex<HashMap<String, ListingOutcome>>,
}

/// Include/exclude glob selection over qualified MCP tool names.
///
/// Semantics follow pi's CLI (`--tools` / `--exclude-tools`), documented at
/// <https://github.com/badlogic/pi-mono/blob/main/packages/coding-agent/docs/cli.md>:
/// a pattern is an exact name or a glob where `*` matches any characters,
/// an empty include list selects everything, and an exclude match always wins.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpToolNameFilter {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl McpToolNameFilter {
    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }

    /// Whether `qualified_name` survives this filter.
    pub fn selects(&self, qualified_name: &str) -> bool {
        if self
            .exclude
            .iter()
            .any(|pattern| tool_glob_matches(pattern, qualified_name))
        {
            return false;
        }
        self.include.is_empty()
            || self
                .include
                .iter()
                .any(|pattern| tool_glob_matches(pattern, qualified_name))
    }
}

/// Match `value` against `pattern`, where `*` matches any run of characters
/// (including none) and every other character matches itself.
pub fn tool_glob_matches(pattern: &str, value: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let value: Vec<char> = value.chars().collect();
    let mut previous = vec![false; value.len() + 1];
    previous[0] = true;
    for token in pattern {
        let mut current = vec![false; value.len() + 1];
        if token == '*' {
            current[0] = previous[0];
            for index in 1..=value.len() {
                current[index] = previous[index] || current[index - 1];
            }
        } else {
            for index in 1..=value.len() {
                current[index] = previous[index - 1] && token == value[index - 1];
            }
        }
        previous = current;
    }
    previous[value.len()]
}

type InflightListing = tokio::task::JoinHandle<Result<Vec<McpToolDef>, McpError>>;

impl Default for McpManager {
    fn default() -> Self {
        Self::new()
    }
}

impl McpManager {
    pub fn set_elicitation_handler(
        &self,
        session_id: String,
        handler: crate::agent_mcp::transport::ElicitationHandler,
    ) {
        *self
            .elicitation_session
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(session_id.clone());
        for (server_name, transport) in &self.transports {
            transport.set_elicitation_handler(
                session_id.clone(),
                server_name.clone(),
                handler.clone(),
            );
        }
    }
    pub fn new() -> Self {
        Self {
            transports: HashMap::new(),
            channel_permissions: HashMap::new(),
            trust_policies: HashMap::new(),
            invocation_audit: Mutex::new(Vec::new()),
            registry: Mutex::new(HashMap::new()),
            pool_leases: HashMap::new(),
            scope_servers: HashMap::new(),
            generation: 0,
            canonical_membrane_binding: None,
            canonical_membrane_server: None,
            canonical_membrane_transport: None,
            canonical_membrane_endpoint: None,
            canonical_membrane_caller: None,
            elicitation_session: Mutex::new(None),
            startup_bound: crate::agent_mcp::limits::MCP_STARTUP_BOUND,
            call_deadline: crate::agent_mcp::limits::MCP_CALL_DEADLINE,
            inflight_listings: Mutex::new(HashMap::new()),
            tool_name_filter: None,
            server_runtime: HashMap::new(),
            required_errors: Mutex::new(Vec::new()),
            list_changed_seen: Mutex::new(HashMap::new()),
            listing_outcomes: Mutex::new(HashMap::new()),
        }
    }

    /// Restrict the tools this manager exposes and invokes. An empty filter
    /// clears the restriction.
    pub fn set_tool_name_filter(&mut self, filter: McpToolNameFilter) {
        self.tool_name_filter = (!filter.is_empty()).then_some(filter);
    }

    /// Whether the per-run tool selection permits this qualified tool name.
    pub fn is_tool_name_selected(&self, qualified_name: &str) -> bool {
        self.tool_name_filter
            .as_ref()
            .is_none_or(|filter| filter.selects(qualified_name))
    }

    /// Override the single total per-call deadline (default 120s).
    pub fn set_call_deadline(&mut self, deadline: std::time::Duration) {
        self.call_deadline = deadline;
    }

    /// Override the per-server discovery bound (default 3s).
    pub fn set_startup_bound(&mut self, bound: std::time::Duration) {
        self.startup_bound = bound;
    }

    /// Discovery bound for one server: its `startup_timeout_sec`, else the manager default.
    fn startup_bound_for(&self, server: &str) -> std::time::Duration {
        self.server_runtime
            .get(server)
            .and_then(|runtime| runtime.startup_timeout)
            .unwrap_or(self.startup_bound)
    }

    /// Bound for a server the caller named on purpose (a call, or a listing of that one server):
    /// its own `startup_timeout_sec`, else the longer explicit bound, so a cold `npx` start does
    /// not cost the model a "still starting" turn. Session-start discovery keeps the short bound.
    fn explicit_startup_bound_for(&self, server: &str) -> std::time::Duration {
        self.server_runtime
            .get(server)
            .and_then(|runtime| runtime.startup_timeout)
            .unwrap_or_else(|| {
                self.startup_bound
                    .max(crate::agent_mcp::limits::MCP_EXPLICIT_STARTUP_BOUND)
            })
    }

    /// Whether the last listing showed this tool with `readOnlyHint: true`. Reads the routing
    /// cache only (no I/O), so a tool not listed yet is not known to be read-only; the engine
    /// uses it to run such calls side by side.
    pub fn is_listed_read_only(&self, qualified_name: &str) -> bool {
        if !self.is_tool_name_selected(qualified_name) {
            return false;
        }
        self.registry
            .lock()
            .ok()
            .and_then(|registry| {
                registry
                    .get(qualified_name)
                    .map(|entry| entry.def.is_read_only_hint())
            })
            .unwrap_or(false)
    }

    /// Wait for the named server to be ready (up to its explicit bound) before a listing that
    /// asked for it. Errors are left for the listing itself to report.
    pub async fn wait_for_server(&self, server_fragment: &str) {
        for (orig, transport) in &self.transports {
            let sanitized = sanitize_tool_name(orig, "");
            let fragment = sanitized
                .strip_prefix("mcp__")
                .and_then(|rest| rest.strip_suffix("__"))
                .unwrap_or(orig);
            if fragment.eq_ignore_ascii_case(server_fragment)
                || orig.eq_ignore_ascii_case(server_fragment)
            {
                let _ = self
                    .list_server_bounded_for(orig, transport, self.explicit_startup_bound_for(orig))
                    .await;
            }
        }
    }

    /// Total call deadline for a qualified tool name: the larger of the owning
    /// server's `tool_timeout_sec` and the manager default, so a short
    /// per-server timeout (applied to the transport request) never shrinks the
    /// whole-invoke bound below the manager default.
    fn call_deadline_for(&self, qualified_name: &str) -> std::time::Duration {
        let Some((server_part, _tool_part)) = parse_qualified_name(qualified_name) else {
            return self.call_deadline;
        };
        let prefix = format!("mcp__{server_part}__");
        let mut matched = self
            .transports
            .keys()
            .filter(|orig| sanitize_tool_name(orig.as_str(), "") == prefix);
        match (matched.next(), matched.next()) {
            (Some(server), None) => self
                .server_runtime
                .get(server)
                .and_then(|runtime| runtime.tool_timeout)
                .map_or(self.call_deadline, |tool_timeout| {
                    tool_timeout.max(self.call_deadline)
                }),
            _ => self.call_deadline,
        }
    }

    /// The tools of every server configured with `expose = "direct"`, ready to go into the
    /// model's tool list. Waits for each such server up to the discovery bound; a server that is
    /// not ready by then adds its tools on a later call. Empty when no server is direct.
    pub async fn direct_tools(&self) -> Vec<McpToolDef> {
        let direct: Vec<String> = self
            .transports
            .keys()
            .filter(|server| {
                self.server_runtime.get(*server).is_some_and(|runtime| {
                    runtime.expose == crate::agent_mcp::types::McpExpose::Direct
                })
            })
            .cloned()
            .collect();
        if direct.is_empty() {
            return Vec::new();
        }
        for server in &direct {
            self.wait_for_server(server).await;
        }
        let prefixes: Vec<String> = direct
            .iter()
            .map(|server| sanitize_tool_name(server, ""))
            .collect();
        let mut tools: Vec<McpToolDef> = self
            .discover_all()
            .await
            .into_iter()
            .filter(|tool| prefixes.iter().any(|prefix| tool.name.starts_with(prefix)))
            .collect();
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        tools
    }

    /// Servers whose tool listing has not completed yet ("still starting").
    /// Their tools are omitted from discovery until they become ready.
    pub fn starting_servers(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .inflight_listings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, handle)| !handle.is_finished())
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        names
    }

    fn record_listing(&self, server: &str, outcome: ListingOutcome) {
        self.listing_outcomes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(server.to_string(), outcome);
    }

    fn forget_listing(&self, server: &str) {
        self.listing_outcomes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(server);
    }

    /// The connection state of every registered server, read from what the manager already
    /// holds. It never starts a server, lists tools, calls a tool, or reads a URL, header or
    /// token, and it carries no error text.
    ///
    /// A server whose listing is still running is `Starting`; one whose last completed listing
    /// returned tools is `Connected { tools }` (zero tools stays `Connected { tools: 0 }`); one
    /// whose last listing errored is `Failed`; one that has not been listed yet is `Starting`
    /// (it connects lazily on first use). The manager has no per-server disabled switch and
    /// its error type carries no sign-in kind, so it never reports `Disabled` or `NeedsAuth`.
    /// Servers are sorted by name.
    pub fn connection_states(&self) -> Vec<EngineSource> {
        let starting = self.starting_servers();
        let outcomes = self
            .listing_outcomes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let mut sources: Vec<EngineSource> = self
            .transports
            .keys()
            .map(|name| {
                let connection = if starting.contains(name) {
                    EngineConnection::Starting
                } else {
                    match outcomes.get(name) {
                        Some(ListingOutcome::Listed { tools }) => {
                            EngineConnection::Connected { tools: *tools }
                        }
                        Some(ListingOutcome::Failed) => EngineConnection::Failed,
                        None => EngineConnection::Starting,
                    }
                };
                EngineSource {
                    name: name.clone(),
                    connection,
                }
            })
            .collect();
        sources.sort_by(|a, b| a.name.cmp(&b.name));
        sources
    }

    /// Register a transport under a logical server name.
    pub fn register(&mut self, server_name: &str, transport: Arc<dyn McpTransport>) {
        self.forget_listing(server_name);
        for servers in self.scope_servers.values_mut() {
            servers.remove(server_name);
        }
        self.transports.insert(server_name.to_string(), transport);
        // Invalidate registry so next discover rebuilds (handles hot-reload / reconnect).
        if let Ok(mut reg) = self.registry.lock() {
            reg.clear();
        }
        self.pool_leases.remove(server_name);
        self.generation = self.generation.saturating_add(1);
    }

    /// Register a server under a disposable extension scope.
    pub fn register_scoped(
        &mut self,
        scope: &str,
        server_name: &str,
        transport: Arc<dyn McpTransport>,
    ) {
        self.register(server_name, transport);
        self.scope_servers
            .entry(scope.to_string())
            .or_default()
            .insert(server_name.to_string());
    }

    /// Remove all MCP registrations owned by `scope`.
    pub fn unload_scope(&mut self, scope: &str) -> usize {
        let servers = self.scope_servers.remove(scope).unwrap_or_default();
        let removed = servers.len();
        for server in servers {
            self.transports.remove(&server);
            self.forget_listing(&server);
            self.channel_permissions.remove(&server);
            self.trust_policies.remove(&server);
            self.pool_leases.remove(&server);
        }
        if removed > 0 {
            self.generation = self.generation.saturating_add(1);
            if let Ok(mut registry) = self.registry.lock() {
                // Discovery is the canonical projection; invalidate it atomically
                // so no unloaded server remains routable from a stale cache.
                registry.clear();
            }
        }
        removed
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Bind the already authenticated canonical Membrane installation. This
    /// is startup-only state; Blueprint callers must fail closed when absent.
    pub fn bind_canonical_membrane(&mut self, binding: CanonicalMembraneBinding) {
        self.canonical_membrane_binding = Some(binding);
    }

    /// Bind identity to the exact transport instance selected during startup.
    /// A logical server name alone never establishes canonical authority.
    pub fn bind_canonical_membrane_server(
        &mut self,
        server_name: &str,
        binding: CanonicalMembraneBinding,
        expected_endpoint: &str,
    ) -> bool {
        let Some(transport) = self.transports.get(server_name).cloned() else {
            return false;
        };
        if transport.endpoint_identity().as_deref() != Some(expected_endpoint) {
            return false;
        }
        self.canonical_membrane_server = Some(server_name.to_string());
        self.canonical_membrane_transport = Some(transport);
        self.canonical_membrane_endpoint = Some(expected_endpoint.to_string());
        self.canonical_membrane_binding = Some(binding);
        true
    }

    pub fn canonical_membrane_binding(&self) -> Option<&CanonicalMembraneBinding> {
        self.canonical_membrane_binding.as_ref()
    }

    /// Bind an existing Membrane registry identity to this session manager.
    /// Identity is accepted only after the installed transport was attested.
    pub fn bind_canonical_membrane_caller(&mut self, caller: CanonicalMembraneCaller) -> bool {
        if self.canonical_membrane_binding.is_none()
            || self.canonical_membrane_server.is_none()
            || caller.root.trim().is_empty()
            || caller.repository_id.trim().is_empty()
            || caller.scope_id.trim().is_empty()
        {
            return false;
        }
        self.canonical_membrane_caller = Some(caller);
        true
    }

    pub fn canonical_membrane_caller(&self) -> Option<&CanonicalMembraneCaller> {
        self.canonical_membrane_caller.as_ref()
    }

    pub fn has_canonical_membrane_read_bridge(&self, tool: &str) -> bool {
        canonical_read_tool(tool).is_some()
            && self.canonical_membrane_binding.is_some()
            && self.canonical_membrane_caller.is_some()
            && self
                .canonical_membrane_server
                .as_deref()
                .is_some_and(|server| {
                    self.is_canonical_membrane_tool(&sanitize_tool_name(server, "bridge"))
                })
    }

    /// Whether this is one of the canonical public Pull/Push entries.
    pub fn is_canonical_membrane_public_tool(&self, qualified_name: &str) -> bool {
        matches!(
            parse_qualified_name(qualified_name),
            Some((server, tool)) if matches!(tool.as_str(), "pull" | "push")
                && self.is_canonical_membrane_tool(qualified_name)
                && sanitize_tool_name(
                    self.canonical_membrane_server.as_deref().unwrap_or_default(), ""
                ) == format!("mcp__{server}__")
        )
    }

    /// Name guard for the reserved `membrane` Pull/Push aliases, including
    /// unbound or replaced transports that must fail closed at the caller.
    pub fn is_membrane_public_tool_name(&self, qualified_name: &str) -> bool {
        matches!(parse_qualified_name(qualified_name), Some((server, tool))
            if matches!(tool.as_str(), "pull" | "push") && server == "membrane")
    }

    /// Endpoint identity of a registered transport, for canonical
    /// attestation of a server the operator already configured.
    pub fn transport_endpoint_identity(&self, server_name: &str) -> Option<String> {
        self.transports.get(server_name)?.endpoint_identity()
    }

    pub fn is_canonical_membrane_tool(&self, qualified_name: &str) -> bool {
        let Some((server, _)) = parse_qualified_name(qualified_name) else {
            return false;
        };
        let Some(expected_server) = self.canonical_membrane_server.as_deref() else {
            return false;
        };
        if sanitize_tool_name(expected_server, "") != format!("mcp__{server}__") {
            return false;
        }
        let Some(expected_transport) = self.canonical_membrane_transport.as_ref() else {
            return false;
        };
        let Some(current_transport) = self.transports.get(expected_server) else {
            return false;
        };
        self.canonical_membrane_endpoint
            .as_deref()
            .is_some_and(|endpoint| {
                Arc::ptr_eq(current_transport, expected_transport)
                    && current_transport.endpoint_identity().as_deref() == Some(endpoint)
            })
    }

    /// Invoke one of the existing, deliberately unadvertised Membrane read
    /// operations through the exact installed transport attested at startup.
    /// This does not expose a general unadvertised-tool bypass.
    pub async fn invoke_canonical_membrane_read_with_cancel(
        &self,
        tool: &str,
        args: serde_json::Value,
        session_id: Option<&str>,
        channel: &str,
        cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Result<McpToolResult, McpError> {
        let Some(canonical_tool) = canonical_read_tool(tool) else {
            return Err(McpError::ToolNotFound(tool.to_string()));
        };
        let allowed_operation = match canonical_tool {
            "membrane_blueprint" => matches!(
                args.get("operation").and_then(serde_json::Value::as_str),
                Some("recall" | "search" | "expand")
            ),
            "membrane_memory_read" | "membrane_source_read" => args.get("operation").is_none(),
            _ => false,
        };
        if !allowed_operation {
            return Err(McpError::ToolNotFound(tool.to_string()));
        }

        let Some(binding) = self.canonical_membrane_binding.as_ref() else {
            return Err(McpError::NotConnected);
        };
        let Some(server) = self.canonical_membrane_server.as_deref() else {
            return Err(McpError::NotConnected);
        };
        let Some(expected_transport) = self.canonical_membrane_transport.as_ref() else {
            return Err(McpError::NotConnected);
        };
        let Some(expected_endpoint) = self.canonical_membrane_endpoint.as_deref() else {
            return Err(McpError::NotConnected);
        };
        let Some(caller) = self.canonical_membrane_caller.as_ref() else {
            return Err(McpError::NotConnected);
        };
        if args.get("repository").and_then(serde_json::Value::as_str)
            != Some(caller.repository_id.as_str())
        {
            return Err(McpError::ToolNotFound(tool.to_string()));
        }
        let caller_args = args.get("caller").and_then(serde_json::Value::as_object);
        let Some(caller_args) = caller_args else {
            return Err(McpError::ToolNotFound(tool.to_string()));
        };
        if caller_args.get("root").and_then(serde_json::Value::as_str) != Some(caller.root.as_str())
            || caller_args
                .get("repositoryId")
                .and_then(serde_json::Value::as_str)
                != Some(caller.repository_id.as_str())
            || caller_args
                .get("scopeId")
                .and_then(serde_json::Value::as_str)
                != Some(caller.scope_id.as_str())
            || caller_args.get("scopeDescriptor") != caller.scope_descriptor.as_ref()
        {
            return Err(McpError::ToolNotFound(tool.to_string()));
        }
        let Some(transport) = self.transports.get(server) else {
            return Err(McpError::NotConnected);
        };
        if !Arc::ptr_eq(transport, expected_transport)
            || transport.endpoint_identity().as_deref() != Some(expected_endpoint)
            || binding.runtime_origin.trim().is_empty()
        {
            return Err(McpError::NotConnected);
        }
        if self
            .transports
            .keys()
            .filter(|name| sanitize_tool_name(name, "") == sanitize_tool_name(server, ""))
            .count()
            != 1
        {
            return Err(McpError::ToolNotFound(tool.to_string()));
        }
        if !self.is_channel_allowed(server, channel) {
            return Err(McpError::ToolNotFound(tool.to_string()));
        }

        // Hidden native reads have the same read-only policy shape as the
        // canonical operations they represent. Evaluate both existing
        // channel mutability and trust-tier gates before the direct dispatch.
        let policy_tool = McpToolDef {
            name: canonical_tool.to_string(),
            description: "Canonical Membrane read operation".to_string(),
            annotations: crate::agent_mcp::types::McpToolAnnotations {
                read_only_hint: Some(true),
                ..Default::default()
            },
            input_schema: serde_json::json!({"type":"object"}),
        };
        if !self.is_tool_exposed(server, &policy_tool) {
            return Err(McpError::ToolNotFound(tool.to_string()));
        }
        if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
            return Err(McpError::Cancelled);
        }
        let result = transport
            .call_tool_with_cancel(canonical_tool, args, session_id, cancel_rx)
            .await;
        if result.is_ok() {
            self.record_served_invocation(&sanitize_tool_name(server, canonical_tool), session_id);
        }
        result
    }

    /// Register a POOLED transport plus the lease that keeps it alive
    /// (master-change 3.4). Same as [`register`](Self::register) for tool
    /// discovery/calls; the difference is ownership: the shared instance
    /// outlives this manager only as long as some session still leases it.
    pub fn register_pooled(
        &mut self,
        server_name: &str,
        transport: Arc<dyn McpTransport>,
        lease: rightkit_agent_engine_runtime_resources::Lease<Arc<dyn McpTransport>>,
    ) {
        self.forget_listing(server_name);
        self.transports.insert(server_name.to_string(), transport);
        if let Ok(mut reg) = self.registry.lock() {
            reg.clear();
        }
        self.pool_leases.insert(server_name.to_string(), lease);
        self.generation = self.generation.saturating_add(1);
    }

    /// Register runtime channel permissions for a logical server name.
    pub fn register_channel_permissions(
        &mut self,
        server_name: &str,
        permissions: McpChannelPermissions,
    ) {
        self.channel_permissions
            .insert(server_name.to_string(), permissions);
    }

    /// Register the per-server options (timeouts, tool filter, `required`) for a
    /// logical server name.
    pub fn register_server_options(&mut self, server_name: &str, options: McpServerOptions) {
        self.server_runtime.insert(
            server_name.to_string(),
            ServerRuntime {
                filter: options.tool_filter(),
                startup_timeout: options.startup_timeout(),
                tool_timeout: options.tool_timeout(),
                required: options.required,
                expose: options.expose,
            },
        );
    }

    /// `required` servers that failed to start, as (server, error). Empty when
    /// every required server listed its tools. The caller decides whether to abort.
    pub fn required_server_errors(&self) -> Vec<(String, String)> {
        self.required_errors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Record that `server` failed to start. Only a server registered as
    /// `required` is recorded; the line is printed once per distinct error.
    pub fn note_required_failure(&self, server: &str, error: &str) {
        if !self
            .server_runtime
            .get(server)
            .is_some_and(|runtime| runtime.required)
        {
            return;
        }
        let mut errors = self
            .required_errors
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if errors
            .iter()
            .any(|(name, last)| name == server && last == error)
        {
            return;
        }
        eprintln!("[mcp] required server '{server}' failed to start: {error}");
        errors.retain(|(name, _)| name != server);
        errors.push((server.to_string(), error.to_string()));
    }

    fn clear_required_failure(&self, server: &str) {
        self.required_errors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(name, _)| name != server);
    }

    /// Register the configured trust policy for a logical server name.
    pub fn register_trust_policy(&mut self, server_name: &str, policy: McpTrustPolicy) {
        self.trust_policies.insert(server_name.to_string(), policy);
    }

    /// Return the configured trust policy for a server.
    pub fn trust_policy(&self, server_name: &str) -> Option<&McpTrustPolicy> {
        self.trust_policies.get(server_name)
    }

    /// Return the configured runtime channel permissions for a server.
    pub fn channel_permissions(&self, server_name: &str) -> Option<&McpChannelPermissions> {
        self.channel_permissions.get(server_name)
    }

    /// Whether a server may be used by the active UI/remote channel.
    ///
    /// Unscoped servers and empty allow-lists remain allowed for backward compatibility with
    /// existing `.mcp.json` files. Operators can opt into narrower policy by setting one or more
    /// explicit allowed channel ids.
    pub fn is_channel_allowed(&self, server_name: &str, channel: &str) -> bool {
        let Some(permissions) = self.channel_permissions(server_name) else {
            return true;
        };
        if permissions.allowed_channels.is_empty() {
            return true;
        }
        permissions
            .allowed_channels
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(channel))
    }

    /// Whether a server allows tools that are not explicitly read-only.
    ///
    /// Servers without a channel-permissions row keep legacy behavior and allow all tool calls.
    pub fn is_mutating_allowed(&self, server_name: &str) -> bool {
        self.channel_permissions(server_name)
            .map(|permissions| permissions.allow_mutating)
            .unwrap_or(true)
    }

    /// Whether a discovered tool can be invoked under the server's mutating-call policy.
    pub fn is_tool_allowed_by_mutating_policy(&self, server_name: &str, tool: &McpToolDef) -> bool {
        self.is_mutating_allowed(server_name) || tool.is_read_only_hint()
    }

    /// Whether a discovered tool may be exposed under the configured trust tier.
    pub fn is_tool_allowed_by_trust_policy(&self, server_name: &str, tool: &McpToolDef) -> bool {
        let Some(policy) = self.trust_policy(server_name) else {
            return true;
        };
        // The presets written by `mcp add`/`mcp install`/desktop are not the tier names the
        // policy parser knows; map the reviewed ones to Trusted. Read-only and unreviewed
        // remote presets, and anything unknown, stay Untrusted (read-only tools only).
        let tier = match policy.preset.trim().to_ascii_lowercase().as_str() {
            "local_process" | "local_dev" | "remote_full" => "trusted".to_string(),
            _ => policy.preset.clone(),
        };
        let server_policy = McpServerModePolicy::new(&tier, self.is_mutating_allowed(server_name));
        let summary = McpServerToolSummary::new(&tool.name, tool.is_read_only_hint());
        server_policy.exposes_tool(&summary)
    }

    fn is_tool_exposed(&self, server_name: &str, tool: &McpToolDef) -> bool {
        // Per-server `enabled_tools` / `disabled_tools` over the raw tool name.
        self.server_runtime
            .get(server_name)
            .and_then(|runtime| runtime.filter.as_ref())
            .is_none_or(|filter| filter.selects(&tool.name))
            && self.is_tool_allowed_by_mutating_policy(server_name, tool)
            && self.is_tool_allowed_by_trust_policy(server_name, tool)
    }

    /// Verify whether a qualified tool can be invoked under the server's mutating-call policy.
    pub async fn is_qualified_tool_allowed_by_mutating_policy(
        &self,
        qualified_name: &str,
    ) -> Result<bool, McpError> {
        let (server_part, _tool_part) = parse_qualified_name(qualified_name)
            .ok_or_else(|| McpError::ToolNotFound(qualified_name.to_string()))?;
        let mut matched_servers: Vec<String> = Vec::new();
        for orig in self.transports.keys() {
            if sanitize_tool_name(orig, "") == format!("mcp__{server_part}__") {
                matched_servers.push(orig.clone());
            }
        }
        if matched_servers.is_empty() {
            return Err(McpError::ToolNotFound(qualified_name.to_string()));
        }
        if matched_servers.len() > 1 {
            return Err(McpError::ToolNotFound(qualified_name.to_string()));
        }
        let server_original = matched_servers.into_iter().next().unwrap();
        if self.is_mutating_allowed(&server_original) {
            return Ok(true);
        }
        let transport = self
            .transports
            .get(&server_original)
            .ok_or_else(|| McpError::ToolNotFound(server_original.clone()))?;
        let tools = self
            .list_server_bounded_for(
                &server_original,
                transport,
                self.explicit_startup_bound_for(&server_original),
            )
            .await?;
        let mut found: Option<McpToolDef> = None;
        let mut collision = false;
        for tool in &tools {
            if sanitize_tool_name(&server_original, &tool.name) == qualified_name {
                if found.is_some() {
                    collision = true;
                    break;
                }
                found = Some(tool.clone());
            }
        }
        if collision {
            return Err(McpError::ToolNotFound(qualified_name.to_string()));
        }
        let tool = found.ok_or_else(|| McpError::ToolNotFound(qualified_name.to_string()))?;
        Ok(self.is_tool_exposed(&server_original, &tool))
    }

    /// List ONE server's tools, connecting it on first use (lazy connect).
    ///
    /// Bounded by `startup_bound` exactly like discovery: a server that is not
    /// ready within the bound keeps connecting in the background (its handle is
    /// parked in `inflight_listings` and joined by the next call) and the caller
    /// gets a retryable "still starting" error instead of blocking the turn.
    async fn list_server_bounded(
        &self,
        server: &str,
        transport: &Arc<dyn McpTransport>,
    ) -> Result<Vec<McpToolDef>, McpError> {
        self.list_server_bounded_for(server, transport, self.startup_bound_for(server))
            .await
    }

    async fn list_server_bounded_for(
        &self,
        server: &str,
        transport: &Arc<dyn McpTransport>,
        bound: std::time::Duration,
    ) -> Result<Vec<McpToolDef>, McpError> {
        let existing = self
            .inflight_listings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(server);
        let mut handle = existing.unwrap_or_else(|| {
            let transport = Arc::clone(transport);
            tokio::spawn(async move { transport.list_tools().await })
        });
        match tokio::time::timeout(bound, &mut handle).await {
            Ok(joined) => {
                let listing = joined.unwrap_or_else(|e| Err(McpError::Serde(e.to_string())));
                match &listing {
                    Ok(tools) => {
                        self.record_listing(
                            server,
                            ListingOutcome::Listed {
                                tools: tools
                                    .iter()
                                    .filter(|tool| self.is_tool_exposed(server, tool))
                                    .count(),
                            },
                        );
                        self.clear_required_failure(server);
                    }
                    Err(e) => {
                        self.record_listing(server, ListingOutcome::Failed);
                        self.note_required_failure(server, &e.to_string());
                    }
                }
                listing
            }
            Err(_) => {
                self.inflight_listings
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(server.to_string(), handle);
                let message = format!(
                    "MCP server '{server}' is still starting (not ready within {} ms); retry shortly",
                    bound.as_millis()
                );
                self.note_required_failure(server, &message);
                Err(McpError::Transport(message))
            }
        }
    }

    /// A server's tool catalog changed (`notifications/tools/list_changed`):
    /// drop its in-flight listing and the routing registry so the next
    /// discovery or call lists live.
    pub fn notify_tools_list_changed(&self, server: &str) {
        if let Some(handle) = self
            .inflight_listings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(server)
        {
            handle.abort();
        }
        if let Ok(mut reg) = self.registry.lock() {
            reg.clear();
        }
    }

    /// Apply a transport-observed `list_changed` since the last check.
    fn sync_list_changed(&self, server: &str, transport: &Arc<dyn McpTransport>) {
        let seq = transport.tools_list_changed_seq();
        let changed = {
            let mut seen = self
                .list_changed_seen
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let last = seen.insert(server.to_string(), seq).unwrap_or(0);
            last != seq
        };
        if changed {
            self.notify_tools_list_changed(server);
        }
    }

    async fn collect_normalized_map(&self) -> HashMap<String, Vec<RegistryEntry>> {
        let mut by_normalized: HashMap<String, Vec<RegistryEntry>> = HashMap::new();
        // List every server concurrently, each bounded by `startup_bound`. A
        // server that is not ready keeps listing in the background and joins
        // a later pass; it never blocks the caller beyond the bound.
        let listings = futures::future::join_all(self.transports.iter().map(
            |(server, transport)| async move {
                self.sync_list_changed(server, transport);
                let existing = self
                    .inflight_listings
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(server);
                let mut handle = existing.unwrap_or_else(|| {
                    let transport = Arc::clone(transport);
                    tokio::spawn(async move { transport.list_tools().await })
                });
                match tokio::time::timeout(self.startup_bound_for(server), &mut handle).await {
                    Ok(joined) => (
                        server,
                        Some(joined.unwrap_or_else(|e| Err(McpError::Serde(e.to_string())))),
                    ),
                    Err(_) => {
                        self.inflight_listings
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(server.clone(), handle);
                        (server, None)
                    }
                }
            },
        ))
        .await;
        for (server, listing) in listings {
            let tools = match listing {
                Some(Ok(t)) => {
                    self.record_listing(
                        server,
                        ListingOutcome::Listed {
                            tools: t
                                .iter()
                                .filter(|tool| self.is_tool_exposed(server, tool))
                                .count(),
                        },
                    );
                    self.clear_required_failure(server);
                    t
                }
                Some(Err(e)) => {
                    self.record_listing(server, ListingOutcome::Failed);
                    eprintln!("[mcp] list_tools failed for server '{server}': {e}");
                    self.note_required_failure(server, &e.to_string());
                    continue;
                }
                None => {
                    eprintln!("[mcp] server '{server}' still starting; its tools join when ready");
                    self.note_required_failure(
                        server,
                        &format!(
                            "not ready within {} ms",
                            self.startup_bound_for(server).as_millis()
                        ),
                    );
                    continue;
                }
            };
            for tool in tools {
                if !self.is_tool_exposed(server, &tool) {
                    continue;
                }
                let normalized = sanitize_tool_name(server, &tool.name);
                // Reject empty normalized fragments (e.g., tool name sanitizes to empty).
                if parse_qualified_name(&normalized).is_none() {
                    eprintln!(
                        "[mcp] rejecting tool with empty normalized name: server='{}' original='{}' -> '{}'",
                        server, tool.name, normalized
                    );
                    continue;
                }
                let entry = RegistryEntry {
                    server: server.clone(),
                    original: tool.name.clone(),
                    normalized: normalized.clone(),
                    def: tool,
                };
                by_normalized.entry(normalized).or_default().push(entry);
            }
        }
        by_normalized
    }

    /// List every tool across all registered servers, with each tool's `name`
    /// rewritten to its qualified `mcp__server__tool` form.
    ///
    /// Builds a `(server, original, normalized)` registry once per call,
    /// rejects collisions atomically (no colliding tool is registered), and
    /// caches the resulting non-colliding registry for `invoke` routing.
    pub async fn discover_all(&self) -> Vec<McpToolDef> {
        let mut tools = self.discover_all_unfiltered().await;
        tools.retain(|tool| self.is_tool_name_selected(&tool.name));
        tools
    }

    async fn discover_all_unfiltered(&self) -> Vec<McpToolDef> {
        match self.discover_all_checked().await {
            Ok(tools) => tools,
            Err(e) => {
                eprintln!("[mcp] discover_all collision: {e}");
                // On collision, return only non-colliding tools (atomic reject keeps colliding out).
                // Re-collect filtered list without error.
                let by_normalized = self.collect_normalized_map().await;
                let mut filtered = Vec::new();
                let mut new_registry = HashMap::new();
                for (_normalized, entries) in by_normalized {
                    if entries.len() != 1 {
                        continue;
                    }
                    let entry = entries.into_iter().next().unwrap();
                    let mut def = entry.def.clone();
                    def.name = entry.normalized.clone();
                    filtered.push(def.clone());
                    new_registry.insert(entry.normalized.clone(), entry);
                }
                if let Ok(mut reg) = self.registry.lock() {
                    *reg = new_registry;
                }
                filtered
            }
        }
    }

    /// Checked variant that returns `Err(Collision)` when any normalized name
    /// collides across `(server, original)` pairs. On success, the registry is
    /// atomically populated with only non-colliding entries. On error, the
    /// registry is still populated with non-colliding entries but the error
    /// is returned so callers can surface colliding servers.
    pub async fn discover_all_checked(&self) -> Result<Vec<McpToolDef>, McpError> {
        let by_normalized = self.collect_normalized_map().await;
        let mut collisions: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();
        let mut new_registry: HashMap<String, RegistryEntry> = HashMap::new();
        let mut result: Vec<McpToolDef> = Vec::new();

        // Sort keys for deterministic error reporting.
        let mut keys: Vec<String> = by_normalized.keys().cloned().collect();
        keys.sort();
        for normalized in keys {
            let entries = &by_normalized[&normalized];
            if entries.len() > 1 {
                let mut servers: Vec<String> = entries.iter().map(|e| e.server.clone()).collect();
                let mut originals: Vec<String> =
                    entries.iter().map(|e| e.original.clone()).collect();
                servers.sort();
                originals.sort();
                collisions.push((normalized.clone(), servers, originals));
            } else {
                let entry = &entries[0];
                let mut def = entry.def.clone();
                def.name = entry.normalized.clone();
                result.push(def);
                new_registry.insert(entry.normalized.clone(), entry.clone());
            }
        }

        // Atomically swap registry: non-colliding entries only.
        {
            if let Ok(mut reg) = self.registry.lock() {
                *reg = new_registry;
            }
        }

        if let Some((normalized, servers, originals)) = collisions.into_iter().next() {
            return Err(McpError::Collision {
                normalized,
                servers,
                originals,
            });
        }

        // Sort result for deterministic test order.
        result.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(result)
    }

    /// Invoke a tool by its qualified name, routing to the owning server.
    /// `session_id` is forwarded to transports that support per-session state
    /// (streamable HTTP with `--stateful`).
    pub async fn invoke(
        &self,
        qualified_name: &str,
        args: serde_json::Value,
        session_id: Option<&str>,
    ) -> Result<McpToolResult, McpError> {
        self.invoke_with_cancel(qualified_name, args, session_id, None)
            .await
    }

    /// Invoke a tool with cancellation retained through transport dispatch.
    pub async fn invoke_with_cancel(
        &self,
        qualified_name: &str,
        args: serde_json::Value,
        session_id: Option<&str>,
        cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Result<McpToolResult, McpError> {
        if !self.is_tool_name_selected(qualified_name) {
            return Err(McpError::ToolFiltered(qualified_name.to_string()));
        }
        // One deadline bounds the whole call (listing, collision scan, the
        // call and its retries); dropping the future aborts every stage.
        let call_deadline = self.call_deadline_for(qualified_name);
        match tokio::time::timeout(
            call_deadline,
            self.invoke_unbounded(qualified_name, args, session_id, cancel_rx),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(McpError::Timeout {
                tool: qualified_name.to_string(),
                limit_ms: call_deadline.as_millis() as u64,
            }),
        }
    }

    async fn invoke_unbounded(
        &self,
        qualified_name: &str,
        args: serde_json::Value,
        session_id: Option<&str>,
        cancel_rx: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Result<McpToolResult, McpError> {
        let (server_part, _tool_part) = parse_qualified_name(qualified_name)
            .ok_or_else(|| McpError::ToolNotFound(qualified_name.to_string()))?;

        // Resolve sanitized server fragment to original server name(s).
        let mut matched_servers: Vec<String> = Vec::new();
        for orig in self.transports.keys() {
            if sanitize_tool_name(orig, "") == format!("mcp__{server_part}__") {
                matched_servers.push(orig.clone());
            }
        }
        if matched_servers.is_empty() {
            return Err(McpError::ToolNotFound(qualified_name.to_string()));
        }
        if matched_servers.len() > 1 {
            // Server-name collision after sanitization -> atomic reject.
            return Err(McpError::ToolNotFound(qualified_name.to_string()));
        }
        let server_original = matched_servers.into_iter().next().unwrap();
        let transport = self
            .transports
            .get(&server_original)
            .ok_or_else(|| McpError::ToolNotFound(server_original.clone()))?;

        self.sync_list_changed(&server_original, transport);
        // List tools from the owning server; propagate transport errors (e.g., spawn failure)
        // so callers see the underlying failure, not a generic ToolNotFound.
        // Lazy connect: only the owning server is contacted, bounded by the
        // startup bound; unrelated servers stay unconnected.
        let tools = self
            .list_server_bounded_for(
                &server_original,
                transport,
                self.explicit_startup_bound_for(&server_original),
            )
            .await?;

        // Find the unique tool whose sanitized qualified name matches.
        let mut found: Option<McpToolDef> = None;
        let mut collision = false;
        for tool in &tools {
            if !self.is_tool_exposed(&server_original, tool) {
                continue;
            }
            if sanitize_tool_name(&server_original, &tool.name) == qualified_name {
                if found.is_some() {
                    collision = true;
                    break;
                }
                found = Some(tool.clone());
            }
        }
        if collision {
            return Err(McpError::ToolNotFound(qualified_name.to_string()));
        }
        let tool_def = found.ok_or_else(|| McpError::ToolNotFound(qualified_name.to_string()))?;

        // Cross-server collision check. A different server can only produce this
        // qualified name when its own sanitized prefix is a prefix of it (for
        // example server "a" with tool "b__c" versus server "a__b" with tool
        // "c"), so only those candidates are consulted; every other server
        // stays unconnected (lazy connect).
        let candidates: Vec<(&String, &Arc<dyn McpTransport>)> = self
            .transports
            .iter()
            .filter(|(orig, _)| {
                **orig != server_original
                    && qualified_name.starts_with(&sanitize_tool_name(orig, ""))
            })
            .collect();
        for (orig, candidate) in candidates {
            // A candidate that cannot list contributes no tools, matching
            // discovery's skip-on-error behaviour.
            let Ok(other_tools) = self.list_server_bounded(orig, candidate).await else {
                continue;
            };
            if other_tools.iter().any(|tool| {
                self.is_tool_exposed(orig, tool)
                    && sanitize_tool_name(orig, &tool.name) == qualified_name
            }) {
                return Err(McpError::ToolNotFound(qualified_name.to_string()));
            }
        }

        let result = transport
            .call_tool_with_cancel(&tool_def.name, args, session_id, cancel_rx)
            .await;
        if result.is_ok() {
            self.record_served_invocation(qualified_name, session_id);
        }
        result
    }

    fn record_served_invocation(&self, qualified_name: &str, session_id: Option<&str>) {
        let trust_tier = parse_qualified_name(qualified_name)
            .and_then(|(server, _)| {
                self.trust_policy(&server)
                    .map(|policy| policy.preset.clone())
            })
            .unwrap_or_else(|| "unconfigured".to_string());
        let audit = McpServedInvocationAudit {
            tool: qualified_name.to_string(),
            session: session_id.unwrap_or("mcp").to_string(),
            at: unix_timestamp_seconds().to_string(),
            outcome: "served".to_string(),
            trust_tier,
            allow_mutate: parse_qualified_name(qualified_name)
                .map(|(server, _)| self.is_mutating_allowed(&server))
                .unwrap_or(true),
        };
        if let Ok(mut rows) = self.invocation_audit.lock() {
            // Most recent rows only, so a long session cannot grow this without limit
            // (audit 2026-11-05 L6).
            if rows.len() >= MAX_INVOCATION_AUDIT_ROWS {
                let excess = rows.len() + 1 - MAX_INVOCATION_AUDIT_ROWS;
                rows.drain(..excess);
            }
            rows.push(audit);
        }
    }

    /// Snapshot of served MCP invocation audit rows.
    pub fn invocation_audit_rows(&self) -> Vec<McpServedInvocationAudit> {
        self.invocation_audit
            .lock()
            .map(|rows| rows.clone())
            .unwrap_or_default()
    }

    /// List all resources from the named server.
    pub async fn list_resources(&self, server: &str) -> Result<Vec<McpResourceDef>, McpError> {
        let transport = self
            .transports
            .get(server)
            .ok_or_else(|| McpError::ToolNotFound(server.to_string()))?;
        transport.list_resources().await
    }

    /// Read a resource by URI from the named server.
    pub async fn read_resource(
        &self,
        server: &str,
        uri: &str,
    ) -> Result<McpResourceContent, McpError> {
        let transport = self
            .transports
            .get(server)
            .ok_or_else(|| McpError::ToolNotFound(server.to_string()))?;
        transport.read_resource(uri).await
    }

    /// The prompt templates the named server offers.
    pub async fn list_prompts(
        &self,
        server: &str,
    ) -> Result<Vec<crate::agent_mcp::types::McpPromptDef>, McpError> {
        let transport = self
            .transports
            .get(server)
            .ok_or_else(|| McpError::ToolNotFound(server.to_string()))?;
        transport.list_prompts().await
    }

    /// Render one prompt of the named server to text.
    pub async fn get_prompt(
        &self,
        server: &str,
        name: &str,
        args: serde_json::Value,
    ) -> Result<String, McpError> {
        let transport = self
            .transports
            .get(server)
            .ok_or_else(|| McpError::ToolNotFound(server.to_string()))?;
        transport.get_prompt(name, args).await
    }

    /// Names of all registered servers.
    pub fn server_names(&self) -> Vec<String> {
        self.transports.keys().cloned().collect()
    }

    /// Whether no servers are registered.
    pub fn is_empty(&self) -> bool {
        self.transports.is_empty()
    }
}

impl Drop for McpManager {
    fn drop(&mut self) {
        let session_id = self
            .elicitation_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(session_id) = session_id {
            for transport in self.transports.values() {
                transport.clear_elicitation_handler(&session_id);
            }
        }
    }
}

fn unix_timestamp_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
