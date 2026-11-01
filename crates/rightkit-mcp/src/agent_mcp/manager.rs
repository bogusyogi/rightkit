//! Multiplexes several named MCP connections and proxies their tools into
//! the host app's namespaced registry.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::Mutex;

use crate::agent_mcp::error::McpError;
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
    McpChannelPermissions, McpResourceContent, McpResourceDef, McpToolDef, McpToolResult,
    McpTrustPolicy,
};

#[derive(Debug, Clone)]
struct RegistryEntry {
    server: String,
    original: String,
    normalized: String,
    def: McpToolDef,
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
    /// Last `tools_list_changed_seq` observed per server (A067).
    list_changed_seen: Mutex<HashMap<String, u64>>,
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
            list_changed_seen: Mutex::new(HashMap::new()),
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
            .map_or(true, |filter| filter.selects(qualified_name))
    }

    /// Override the single total per-call deadline (default 120s).
    pub fn set_call_deadline(&mut self, deadline: std::time::Duration) {
        self.call_deadline = deadline;
    }

    /// Override the per-server discovery bound (default 3s).
    pub fn set_startup_bound(&mut self, bound: std::time::Duration) {
        self.startup_bound = bound;
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

    /// Register a transport under a logical server name.
    pub fn register(&mut self, server_name: &str, transport: Arc<dyn McpTransport>) {
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
        matches!(canonical_read_tool(tool), Some(_))
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
        let server_policy =
            McpServerModePolicy::new(&policy.preset, self.is_mutating_allowed(server_name));
        let summary = McpServerToolSummary::new(&tool.name, tool.is_read_only_hint());
        server_policy.exposes_tool(&summary)
    }

    fn is_tool_exposed(&self, server_name: &str, tool: &McpToolDef) -> bool {
        self.is_tool_allowed_by_mutating_policy(server_name, tool)
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
            .list_server_bounded(&server_original, transport)
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
        let existing = self
            .inflight_listings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(server);
        let mut handle = existing.unwrap_or_else(|| {
            let transport = Arc::clone(transport);
            tokio::spawn(async move { transport.list_tools().await })
        });
        match tokio::time::timeout(self.startup_bound, &mut handle).await {
            Ok(joined) => joined.unwrap_or_else(|e| Err(McpError::Serde(e.to_string()))),
            Err(_) => {
                self.inflight_listings
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(server.to_string(), handle);
                Err(McpError::Transport(format!(
                    "MCP server '{server}' is still starting (not ready within {} ms); retry shortly",
                    self.startup_bound.as_millis()
                )))
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
                match tokio::time::timeout(self.startup_bound, &mut handle).await {
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
                Some(Ok(t)) => t,
                Some(Err(e)) => {
                    eprintln!("[mcp] list_tools failed for server '{server}': {e}");
                    continue;
                }
                None => {
                    eprintln!("[mcp] server '{server}' still starting; its tools join when ready");
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
        match tokio::time::timeout(
            self.call_deadline,
            self.invoke_unbounded(qualified_name, args, session_id, cancel_rx),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(McpError::Timeout {
                tool: qualified_name.to_string(),
                limit_ms: self.call_deadline.as_millis() as u64,
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
            .list_server_bounded(&server_original, transport)
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

#[cfg(test)]
mod startup_bound_tests {
    use super::*;
    use crate::agent_mcp::types::McpToolResult;

    struct NeverAnswers;
    #[async_trait::async_trait]
    impl McpTransport for NeverAnswers {
        async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError> {
            std::future::pending().await
        }
        async fn call_tool(
            &self,
            name: &str,
            _args: serde_json::Value,
            _session: Option<&str>,
        ) -> Result<McpToolResult, McpError> {
            Err(McpError::ToolNotFound(name.to_string()))
        }
        async fn list_resources(&self) -> Result<Vec<McpResourceDef>, McpError> {
            Ok(vec![])
        }
        async fn read_resource(&self, uri: &str) -> Result<McpResourceContent, McpError> {
            Err(McpError::ResourceNotFound(uri.to_string()))
        }
    }

    struct Ready;
    #[async_trait::async_trait]
    impl McpTransport for Ready {
        async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError> {
            Ok(vec![McpToolDef {
                name: "ping".into(),
                description: "d".into(),
                input_schema: serde_json::json!({"type":"object"}),
                annotations: Default::default(),
            }])
        }
        async fn call_tool(
            &self,
            name: &str,
            _args: serde_json::Value,
            _session: Option<&str>,
        ) -> Result<McpToolResult, McpError> {
            Err(McpError::ToolNotFound(name.to_string()))
        }
        async fn list_resources(&self) -> Result<Vec<McpResourceDef>, McpError> {
            Ok(vec![])
        }
        async fn read_resource(&self, uri: &str) -> Result<McpResourceContent, McpError> {
            Err(McpError::ResourceNotFound(uri.to_string()))
        }
    }

    #[tokio::test]
    async fn hung_server_does_not_delay_discovery_beyond_bound() {
        let mut mgr = McpManager::new();
        mgr.set_startup_bound(std::time::Duration::from_millis(300));
        mgr.register("hung", Arc::new(NeverAnswers));
        mgr.register("ready", Arc::new(Ready));

        let started = std::time::Instant::now();
        let tools = mgr.discover_all().await;
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "discovery blocked {elapsed:?} on a server that never answers"
        );
        assert_eq!(tools.len(), 1, "ready server's tools are still listed");
        assert!(tools[0].name.contains("ping"));
        assert_eq!(mgr.starting_servers(), vec!["hung".to_string()]);
    }

    /// Answers `initialize`/listing only after `delay`, like a slow `npx` start.
    struct AnswersLate {
        delay: std::time::Duration,
    }
    #[async_trait::async_trait]
    impl McpTransport for AnswersLate {
        async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError> {
            tokio::time::sleep(self.delay).await;
            Ready.list_tools().await
        }
        async fn call_tool(
            &self,
            name: &str,
            _args: serde_json::Value,
            _session: Option<&str>,
        ) -> Result<McpToolResult, McpError> {
            Err(McpError::ToolNotFound(name.to_string()))
        }
        async fn list_resources(&self) -> Result<Vec<McpResourceDef>, McpError> {
            Ok(vec![])
        }
        async fn read_resource(&self, uri: &str) -> Result<McpResourceContent, McpError> {
            Err(McpError::ResourceNotFound(uri.to_string()))
        }
    }

    #[tokio::test]
    async fn late_server_tools_join_a_later_discovery_after_the_first_proceeds() {
        let mut mgr = McpManager::new();
        mgr.set_startup_bound(std::time::Duration::from_millis(100));
        mgr.register(
            "late",
            Arc::new(AnswersLate {
                delay: std::time::Duration::from_millis(600),
            }),
        );

        // First turn: proceeds within the bound with no tools from the slow server.
        let started = std::time::Instant::now();
        let first = mgr.discover_all().await;
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "first discovery blocked {:?}",
            started.elapsed()
        );
        assert!(first.is_empty(), "slow server must not be listed yet");
        assert_eq!(mgr.starting_servers(), vec!["late".to_string()]);

        // Once the server answers, a later discovery joins its parked listing.
        tokio::time::sleep(std::time::Duration::from_millis(700)).await;
        let later = mgr.discover_all().await;
        assert_eq!(later.len(), 1, "late server's tools must appear: {later:?}");
        assert!(later[0].name.contains("ping"));
        assert!(mgr.starting_servers().is_empty());
    }

    struct SlowCall;
    #[async_trait::async_trait]
    impl McpTransport for SlowCall {
        async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError> {
            Ready.list_tools().await
        }
        async fn call_tool(
            &self,
            _name: &str,
            _args: serde_json::Value,
            _session: Option<&str>,
        ) -> Result<McpToolResult, McpError> {
            std::future::pending().await
        }
        async fn list_resources(&self) -> Result<Vec<McpResourceDef>, McpError> {
            Ok(vec![])
        }
        async fn read_resource(&self, uri: &str) -> Result<McpResourceContent, McpError> {
            Err(McpError::ResourceNotFound(uri.to_string()))
        }
    }

    #[tokio::test]
    async fn hung_call_returns_typed_timeout_at_single_deadline() {
        let mut mgr = McpManager::new();
        mgr.set_call_deadline(std::time::Duration::from_millis(200));
        mgr.register("slow", Arc::new(SlowCall));
        let started = std::time::Instant::now();
        let err = mgr
            .invoke("mcp__slow__ping", serde_json::json!({}), None)
            .await
            .unwrap_err();
        assert!(
            matches!(err, McpError::Timeout { limit_ms: 200, .. }),
            "{err:?}"
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[tokio::test]
    async fn hung_listing_during_invoke_shares_the_same_deadline() {
        let mut mgr = McpManager::new();
        mgr.set_call_deadline(std::time::Duration::from_millis(200));
        mgr.register("hung", Arc::new(NeverAnswers));
        let started = std::time::Instant::now();
        let err = mgr
            .invoke("mcp__hung__ping", serde_json::json!({}), None)
            .await
            .unwrap_err();
        assert!(matches!(err, McpError::Timeout { .. }), "{err:?}");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tool_name_filter_tests {
    use super::*;

    fn filter(include: &[&str], exclude: &[&str]) -> McpToolNameFilter {
        McpToolNameFilter {
            include: include.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn glob_star_matches_any_run_and_nothing_else() {
        assert!(tool_glob_matches("mcp__radius__*", "mcp__radius__search"));
        assert!(tool_glob_matches("mcp__*", "mcp__a__b"));
        assert!(tool_glob_matches("*__ping", "mcp__slow__ping"));
        assert!(tool_glob_matches("mcp__a__b", "mcp__a__b"));
        assert!(!tool_glob_matches("mcp__radius__*", "mcp__other__search"));
        assert!(!tool_glob_matches("mcp__a__b", "mcp__a__bc"));
        // `?` is a literal under pi semantics, not a wildcard.
        assert!(!tool_glob_matches("mcp__a__?", "mcp__a__b"));
    }

    #[test]
    fn empty_filter_selects_everything() {
        assert!(filter(&[], &[]).selects("mcp__a__b"));
    }

    #[test]
    fn include_restricts_and_exclude_wins() {
        let f = filter(&["mcp__radius__*"], &["mcp__radius__danger"]);
        assert!(f.selects("mcp__radius__search"));
        assert!(!f.selects("mcp__radius__danger"));
        assert!(!f.selects("mcp__other__search"));
    }

    #[test]
    fn manager_applies_filter_to_invoke() {
        let mut manager = McpManager::new();
        assert!(manager.is_tool_name_selected("mcp__a__b"));
        manager.set_tool_name_filter(filter(&[], &["mcp__a__*"]));
        assert!(!manager.is_tool_name_selected("mcp__a__b"));
        assert!(manager.is_tool_name_selected("mcp__c__d"));
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(manager.invoke("mcp__a__b", serde_json::json!({}), None));
        assert!(
            matches!(result, Err(McpError::ToolFiltered(_))),
            "{result:?}"
        );
        manager.set_tool_name_filter(McpToolNameFilter::default());
        assert!(manager.is_tool_name_selected("mcp__a__b"));
    }
}

#[cfg(test)]
mod lazy_connect_tests {
    use super::*;
    use crate::agent_mcp::types::McpToolResult;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counts how many times the manager contacts (lists/calls) the server.
    struct Counting {
        tool: &'static str,
        contacts: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl McpTransport for Counting {
        async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError> {
            self.contacts.fetch_add(1, Ordering::SeqCst);
            Ok(vec![McpToolDef {
                name: self.tool.into(),
                description: "d".into(),
                input_schema: serde_json::json!({"type":"object"}),
                annotations: Default::default(),
            }])
        }
        async fn call_tool(
            &self,
            _name: &str,
            _args: serde_json::Value,
            _session: Option<&str>,
        ) -> Result<McpToolResult, McpError> {
            self.contacts.fetch_add(1, Ordering::SeqCst);
            Ok(McpToolResult {
                content: String::new(),
                is_error: false,
            })
        }
        async fn list_resources(&self) -> Result<Vec<McpResourceDef>, McpError> {
            Ok(vec![])
        }
        async fn read_resource(&self, uri: &str) -> Result<McpResourceContent, McpError> {
            Err(McpError::ResourceNotFound(uri.to_string()))
        }
    }

    struct NeverReady;

    #[async_trait::async_trait]
    impl McpTransport for NeverReady {
        async fn list_tools(&self) -> Result<Vec<McpToolDef>, McpError> {
            std::future::pending().await
        }
        async fn call_tool(
            &self,
            name: &str,
            _args: serde_json::Value,
            _session: Option<&str>,
        ) -> Result<McpToolResult, McpError> {
            Err(McpError::ToolNotFound(name.to_string()))
        }
        async fn list_resources(&self) -> Result<Vec<McpResourceDef>, McpError> {
            Ok(vec![])
        }
        async fn read_resource(&self, uri: &str) -> Result<McpResourceContent, McpError> {
            Err(McpError::ResourceNotFound(uri.to_string()))
        }
    }

    #[tokio::test]
    async fn registering_servers_connects_nothing() {
        let a = Arc::new(AtomicUsize::new(0));
        let mut mgr = McpManager::new();
        mgr.register(
            "alpha",
            Arc::new(Counting {
                tool: "ping",
                contacts: a.clone(),
            }),
        );
        assert_eq!(
            a.load(Ordering::SeqCst),
            0,
            "session start must not connect"
        );
    }

    #[tokio::test]
    async fn invoking_one_server_never_connects_the_others() {
        let a = Arc::new(AtomicUsize::new(0));
        let b = Arc::new(AtomicUsize::new(0));
        let mut mgr = McpManager::new();
        mgr.register(
            "alpha",
            Arc::new(Counting {
                tool: "ping",
                contacts: a.clone(),
            }),
        );
        mgr.register(
            "beta",
            Arc::new(Counting {
                tool: "pong",
                contacts: b.clone(),
            }),
        );

        let result = mgr
            .invoke("mcp__alpha__ping", serde_json::json!({}), None)
            .await;

        assert!(result.is_ok(), "{result:?}");
        assert!(
            a.load(Ordering::SeqCst) > 0,
            "owning server connects on first use"
        );
        assert_eq!(
            b.load(Ordering::SeqCst),
            0,
            "an unrelated server must stay unconnected"
        );
    }

    #[tokio::test]
    async fn a_hung_server_fails_its_own_call_within_the_startup_bound() {
        let mut mgr = McpManager::new();
        mgr.set_startup_bound(std::time::Duration::from_millis(200));
        mgr.register("slow", Arc::new(NeverReady));

        let started = std::time::Instant::now();
        let result = mgr
            .invoke("mcp__slow__anything", serde_json::json!({}), None)
            .await;

        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "connect must honour the startup bound, took {:?}",
            started.elapsed()
        );
        match result {
            Err(McpError::Transport(message)) => {
                assert!(message.contains("still starting"), "{message}")
            }
            other => panic!("expected a retryable still-starting error, got {other:?}"),
        }
        assert_eq!(mgr.starting_servers(), vec!["slow".to_string()]);
    }
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod catalog_tests;
