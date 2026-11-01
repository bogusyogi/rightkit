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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_mcp::transport::StubTransport;

    #[test]
    fn identical_configs_share_one_instance_via_leases() {
        let pool = McpTransportPool::new(4);
        let key = pool_key(
            ServiceScope::Workspace("D:/proj".into()),
            "stdio",
            "npx server-filesystem",
            &["-y".to_string(), "server".to_string()],
            &std::collections::HashMap::from([("API_KEY".to_string(), "secret".to_string())]),
            &[],
        );
        let mut leases = Vec::new();
        for _ in 0..20 {
            leases.push(
                pool.acquire(key.clone(), || {
                    Arc::new(StubTransport::new(vec![])) as Arc<dyn McpTransport>
                })
                .expect("lease"),
            );
        }
        let snapshot = pool.snapshot();
        assert_eq!(
            snapshot.len(),
            1,
            "twenty sessions must pool to one transport"
        );
        assert_eq!(snapshot[0].leases, 20, "each session holds its own lease");
        assert_eq!(snapshot[0].key, key);
        drop(leases);
    }

    #[test]
    fn different_env_or_args_never_alias() {
        let key_a = pool_key(
            ServiceScope::Workspace("w".into()),
            "stdio",
            "cmd",
            &["--a".into()],
            &std::collections::HashMap::from([("K".to_string(), "v1".to_string())]),
            &[],
        );
        let key_b = pool_key(
            ServiceScope::Workspace("w".into()),
            "stdio",
            "cmd",
            &["--a".into()],
            &std::collections::HashMap::from([("K".to_string(), "v2".to_string())]),
            &[],
        );
        assert_ne!(key_a, key_b, "different env values must never alias");

        let key_c = pool_key(
            ServiceScope::Workspace("w".into()),
            "stdio",
            "cmd",
            &["--a".into()],
            &std::collections::HashMap::from([("K".to_string(), "v1".to_string())]),
            &[],
        );
        assert_eq!(
            key_a, key_c,
            "identical configs must produce identical keys"
        );

        // Env ORDER must not matter (canonical ordering).
        let key_d = pool_key(
            ServiceScope::Workspace("w".into()),
            "stdio",
            "cmd",
            &["--a".into()],
            &std::collections::HashMap::from([
                ("B".to_string(), "2".to_string()),
                ("A".to_string(), "1".to_string()),
            ]),
            &[],
        );
        let key_e = pool_key(
            ServiceScope::Workspace("w".into()),
            "stdio",
            "cmd",
            &["--a".into()],
            &std::collections::HashMap::from([
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "2".to_string()),
            ]),
            &[],
        );
        assert_eq!(key_d, key_e, "env insertion order must not change identity");

        // Fingerprints never leak the credential or args in plain text.
        assert!(!key_a.fingerprint.contains("secret"));
        assert!(!key_a.fingerprint.contains("--a"));
    }

    #[test]
    fn evict_idle_respects_refcounts() {
        let pool = McpTransportPool::new(4);
        let key = pool_key(
            ServiceScope::Workspace("w".into()),
            "stdio",
            "cmd",
            &[],
            &std::collections::HashMap::new(),
            &[],
        );
        let lease = pool
            .acquire(key.clone(), || {
                Arc::new(StubTransport::new(vec![])) as Arc<dyn McpTransport>
            })
            .expect("lease");

        // A zero-duration idle threshold would evict any unleased entry
        // immediately; with an active lease it must never be evicted.
        let evicted = pool.evict_idle(std::time::Duration::from_secs(0));
        assert!(
            evicted.is_empty(),
            "an entry with an active lease must never be evicted"
        );

        drop(lease);
        let evicted = pool.evict_idle(std::time::Duration::from_secs(0));
        assert_eq!(
            evicted,
            vec![key],
            "an entry becomes evictable only after every lease drops"
        );
    }

    #[test]
    fn lease_epoch_retention_across_config_reload() {
        let pool = McpTransportPool::new(4);
        let key = pool_key(
            ServiceScope::Workspace("w".into()),
            "stdio",
            "cmd",
            &[],
            &std::collections::HashMap::new(),
            &[],
        );
        let lease = pool
            .acquire(key.clone(), || {
                Arc::new(StubTransport::new(vec![])) as Arc<dyn McpTransport>
            })
            .expect("lease");
        let original_epoch = lease.generation();
        let original_ptr = Arc::as_ptr(&lease.value());

        // Simulate a config reload: the epoch must advance...
        let bumped = pool
            .bump_epoch_on_reload(&key)
            .expect("live entry retains through reload");
        assert_eq!(bumped, original_epoch + 1);
        assert_eq!(pool.current_epoch(&key), Some(bumped));

        // ...but the already-dialed connection (and the caller's existing
        // lease) must be retained, not torn down and rebuilt.
        assert_eq!(
            Arc::as_ptr(&lease.value()),
            original_ptr,
            "connection must be retained across a config reload"
        );
        assert_eq!(
            pool.snapshot()[0].leases,
            1,
            "lease count must be unaffected by a reload"
        );

        // A brand-new acquire for the same key must still share the SAME
        // retained instance, not spawn a fresh one.
        let second = pool
            .acquire(key.clone(), || {
                panic!("must reuse retained connection, not rebuild")
            })
            .expect("lease");
        assert_eq!(Arc::as_ptr(&second.value()), original_ptr);
    }

    #[test]
    fn bump_epoch_on_reload_is_noop_for_absent_key() {
        let pool = McpTransportPool::new(4);
        let key = pool_key(
            ServiceScope::Workspace("w".into()),
            "stdio",
            "cmd",
            &[],
            &std::collections::HashMap::new(),
            &[],
        );
        assert_eq!(pool.bump_epoch_on_reload(&key), None);
    }

    #[test]
    fn scopes_are_part_of_identity() {
        let daemon_wide = pool_key(
            ServiceScope::Daemon,
            "stdio",
            "cmd",
            &[],
            &std::collections::HashMap::new(),
            &[],
        );
        let workspace = pool_key(
            ServiceScope::Workspace("w".into()),
            "stdio",
            "cmd",
            &[],
            &std::collections::HashMap::new(),
            &[],
        );
        assert_ne!(daemon_wide, workspace);
    }
}
