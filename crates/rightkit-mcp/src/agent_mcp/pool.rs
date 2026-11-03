//! Canonical MCP transport pooling (master-change row 3.4).
//!
//! Twenty sessions that configure the same stdio server must share ONE
//! spawned process via twenty leases, not twenty processes. Pooling is keyed
//! on the transport's full behavior identity —
//! `{transport kind, endpoint, args hash, env auth fingerprint, config hash}`
//! — and scoped Workspace by default (daemon-wide scope only when a config
//! explicitly declares itself shareable).
//!
//! The pool is the 3.1 `SharedServiceRegistry` mechanism applied to
//! `Arc<dyn McpTransport>`: same key ⇒ same instance, refcounted leases,
//! idle eviction after the last lease drops. Fingerprints hash credentials
//! (never embed them in plain text) and include every behavior-affecting
//! config input so two servers that merely share a name never alias.

use crate::agent_mcp::transport::McpTransport;
use rightkit_agent_engine_runtime_resources::{
    Lease, ResourceSnapshot, ServiceKey, ServiceScope, SharedServiceRegistry,
};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Default ceiling on simultaneously pooled MCP processes per daemon.
pub const DEFAULT_MAX_POOLED_INSTANCES: usize = 16;

type Registry = SharedServiceRegistry<Arc<dyn McpTransport>>;

/// Daemon-lifetime pool of shared MCP transports.
pub struct McpTransportPool {
    registry: Registry,
}

impl Default for McpTransportPool {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_POOLED_INSTANCES)
    }
}

impl McpTransportPool {
    pub fn new(max_instances: usize) -> Self {
        Self {
            registry: Registry::new(max_instances),
        }
    }

    /// Acquire (or create) the shared transport for a canonical key. Callers
    /// that get the same key receive leases on the SAME instance; the
    /// instance becomes evictable only after every lease drops.
    pub fn acquire(
        &self,
        key: ServiceKey,
        create: impl FnOnce() -> Arc<dyn McpTransport>,
    ) -> Result<Lease<Arc<dyn McpTransport>>, ResourceSnapshot> {
        self.registry.acquire(key, create)
    }

    /// Live pool inventory (diagnostics: leases, generations, idle time).
    pub fn snapshot(&self) -> Vec<ResourceSnapshot> {
        self.registry.snapshot()
    }

    /// Current lease `generation` for `key` in the lower registry, or `None` if
    /// the entry has been evicted (or never created). The shared single-flight
    /// pool uses this to detect that its cached transport has drifted from the
    /// leasing layer and must be rebuilt (FM-100).
    pub fn current_epoch(&self, key: &ServiceKey) -> Option<u64> {
        self.registry.current_epoch(key)
    }

    /// Evict idle zero-lease entries that have been unused for `minimum_idle`.
    pub fn evict_idle(&self, minimum_idle: std::time::Duration) -> Vec<ServiceKey> {
        self.registry.evict_idle(minimum_idle)
    }

    /// San-pattern lease-epoch retention (`internal/mcp/registry.go:36,128`):
    /// on a config reload, bump `key`'s epoch so callers that revalidate
    /// against `current_epoch` (e.g. the shared single-flight pool in
    /// `shared_pool.rs`) observe the reload, WITHOUT discarding the already
    /// established connection. The lease's underlying transport and lease
    /// count are left untouched — only the generation counter advances.
    /// Returns `None` if `key` has no live entry (nothing to retain).
    pub fn bump_epoch_on_reload(&self, key: &ServiceKey) -> Option<u64> {
        self.registry.bump_epoch(key)
    }
}

/// Short non-reversible digest for fingerprint components. Credentials and
/// argument bodies are hashed, never embedded in plain text.
fn digest(parts: &[&str]) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update(&[0]);
    }
    let bytes = hasher.finalize_bytes();
    bytes[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Canonical pool key for an MCP server config under a given scope.
///
/// - `scope` defaults to Workspace; daemon-wide pooling only when a caller
///   has explicitly declared the server safe to share across workspaces.
/// - The fingerprint covers transport kind, endpoint (or command), the full
///   argument vector, the env var set with hashed values, and any extra
///   behavior-affecting config fields supplied by the caller.
pub fn pool_key(
    scope: ServiceScope,
    transport_kind: &str,
    endpoint: &str,
    args: &[String],
    env: &std::collections::HashMap<String, String>,
    config_extras: &[(&str, &str)],
) -> ServiceKey {
    // BTreeMap: canonical order so identical env sets always hash equally.
    let env_ordered: BTreeMap<&String, &String> = env.iter().collect();
    let env_names: Vec<&str> = env_ordered.keys().map(|key| key.as_str()).collect();
    let env_values: Vec<&str> = env_ordered.values().map(|value| value.as_str()).collect();
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let extras: Vec<&str> = config_extras
        .iter()
        .flat_map(|(name, value)| [*name, *value])
        .collect();

    let fingerprint = format!(
        "v1|kind={}|endpoint={}|args={}|env_names={}|env_auth={}|cfg={}",
        digest(&[transport_kind]),
        digest(&[endpoint]),
        digest(&arg_refs),
        digest(&env_names),
        digest(&env_values),
        digest(&extras),
    );
    ServiceKey::new("mcp", scope, fingerprint)
}
