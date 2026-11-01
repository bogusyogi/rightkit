//! Adapter and transport registry.
//!
//! PR 1 of the architecture plan. The registry is intentionally
//! minimal: it stores adapter and transport trait objects keyed by
//! provider/transport kind, exposes a single lookup, and refuses to
//! register an adapter whose declared requirements are
//! inconsistent. Built-in registrations live in
//! [`crate::driver_host::cli_runtime::built_in`].
//!
//! The registry is thread-safe and holds `Arc<dyn Trait>` so adapters
//! can be shared across supervisors. Mutation goes through a single
//! mutex so reads are lock-free in the common case.
//!
//! PR 3 adds the attestation-backed readiness gate
//! ([`CliAdapterRegistry::is_ready`]) and the
//! [`AttestationStore`] trait that the registry uses to read the
//! latest attestation without taking a hard dependency on
//! `rightkit-agent-engine-storage`.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::driver_host::cli_runtime::adapter::{CliProviderAdapter, CliTransport};
use crate::driver_host::cli_runtime::attestation::AttestationInputs;
use crate::driver_host::cli_runtime::ids::{CliProviderId, CliTransportKind, HarnessRequirements};
use crate::driver_host::cli_runtime::qualification::requirements_are_consistent;

/// Latest known attestation for a provider, in the shape the
/// registry needs to make a readiness decision. Mirrors the
/// `HarnessAttestation` columns the registry actually consults; the
/// full struct is intentionally not exposed here so the trait can be
/// implemented by an in-memory test double without pulling in
/// `rightkit-agent-engine-storage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestAttestation {
    pub attestation_id: String,
    /// Complete, content-addressed qualification context. Keeping this
    /// intact is required: a digest alone cannot prove that policy, profile,
    /// schema, platform, or isolation have not drifted since qualification.
    pub inputs: AttestationInputs,
    pub failed_probes: Vec<String>,
    pub verified_at_unix_secs: u64,
}

/// Storage handle for harness attestations. The registry calls this
/// once per `is_ready` check; an implementation backed by
/// `rightkit-agent-engine-storage::Storage::latest_cli_harness_attestation` is
/// the production wiring, tests use an in-memory map.
pub trait AttestationStore: Send + Sync {
    /// Return the most recent attestation for `provider_id`, or
    /// `Ok(None)` when no attestation exists yet. An empty row set
    /// is a `false` from `is_ready`.
    fn latest_attestation(
        &self,
        provider_id: &CliProviderId,
    ) -> Result<Option<LatestAttestation>, AttestationStoreError>;
}

/// Storage failure surfaced by an [`AttestationStore`]. Kept distinct
/// from the SQL-level errors in `rightkit-agent-engine-storage` so callers do not
/// have to know whether the backing store is SQLite, in-memory, or
/// something else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttestationStoreError {
    Backend(String),
}

impl std::fmt::Display for AttestationStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AttestationStoreError::Backend(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for AttestationStoreError {}

/// The maximum age of an attestation before it must be re-run. The
/// plan (§7.4) says "older than 24 hours" disqualifies; we use a
/// `pub const` rather than a hard-coded literal so PR 13 (recovery)
/// can override it per adapter if needed.
pub const ATTESTATION_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// In-memory registry. The interface is deliberately small so future
/// PRs can add discovery, listing, and warm-up without disturbing
/// existing callers.
#[derive(Default)]
pub struct CliAdapterRegistry {
    inner: Arc<RwLock<RegistryState>>,
    /// Backing store for the most-recent attestation per provider.
    /// May be `None` for tests that do not exercise the readiness
    /// gate; when unset, [`CliAdapterRegistry::is_ready`] returns
    /// `false` because no attestation exists.
    attestation_store: Arc<RwLock<Option<Arc<dyn AttestationStore>>>>,
}

#[derive(Default)]
struct RegistryState {
    adapters: HashMap<CliProviderId, Arc<dyn CliProviderAdapter>>,
    transports: HashMap<CliTransportKind, Arc<dyn CliTransport>>,
    requirements: HashMap<CliProviderId, HarnessRequirements>,
}

impl CliAdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install an [`AttestationStore`] used by
    /// [`CliAdapterRegistry::is_ready`]. Replaces any prior store.
    /// Pass `None` to disable the gate (the daemon should only do
    /// this during recovery).
    pub fn set_attestation_store(&self, store: Option<Arc<dyn AttestationStore>>) {
        if let Ok(mut slot) = self.attestation_store.write() {
            *slot = store;
        }
    }

    /// Register a provider adapter. Returns `Err` if the adapter's
    /// requirements are inconsistent (e.g. forbids native subagents
    /// but allows native shell). Existing registrations for the same
    /// provider are replaced.
    pub fn register_adapter(
        &self,
        adapter: Arc<dyn CliProviderAdapter>,
    ) -> Result<(), RegistryError> {
        let id = adapter.id().clone();
        let requirements = adapter.harness_requirements();
        requirements_are_consistent(&requirements)
            .map_err(RegistryError::InconsistentRequirements)?;

        let mut state = self
            .inner
            .write()
            .map_err(|_| RegistryError::LockPoisoned)?;
        state.adapters.insert(id.clone(), adapter);
        state.requirements.insert(id, requirements);
        Ok(())
    }

    /// Register a transport implementation. Existing registrations
    /// for the same transport kind are replaced.
    pub fn register_transport(
        &self,
        transport: Arc<dyn CliTransport>,
    ) -> Result<(), RegistryError> {
        let kind = transport.kind();
        let mut state = self
            .inner
            .write()
            .map_err(|_| RegistryError::LockPoisoned)?;
        state.transports.insert(kind, transport);
        Ok(())
    }

    /// Look up a provider adapter by id.
    pub fn adapter(&self, id: &CliProviderId) -> Option<Arc<dyn CliProviderAdapter>> {
        let state = self.inner.read().ok()?;
        state.adapters.get(id).cloned()
    }

    /// Look up a transport implementation by kind.
    pub fn transport(&self, kind: CliTransportKind) -> Option<Arc<dyn CliTransport>> {
        let state = self.inner.read().ok()?;
        state.transports.get(&kind).cloned()
    }

    /// List registered provider ids, sorted. Used by the UI to
    /// enumerate available providers.
    pub fn provider_ids(&self) -> Vec<CliProviderId> {
        let state = match self.inner.read() {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let mut ids: Vec<CliProviderId> = state.adapters.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Read the requirements declared by a registered adapter.
    pub fn requirements(&self, id: &CliProviderId) -> Option<HarnessRequirements> {
        let state = self.inner.read().ok()?;
        state.requirements.get(id).cloned()
    }

    /// Readiness gate (PR 3). Returns `true` only when every one of
    /// these is satisfied for the given provider:
    ///
    /// 1. an adapter is registered;
    /// 2. an attestation store is installed and returned a row;
    /// 3. the attestation address matches all persisted content-addressed
    ///    inputs, then its `executable_sha256` matches
    ///    `current_executable_sha256` (the digest the registry just
    ///    computed against the on-disk binary);
    /// 4. no mandatory probe failed (the row's `failed_probes` is
    ///    empty);
    /// 5. the attestation is no older than [`ATTESTATION_MAX_AGE`].
    ///
    /// Any failure makes the provider non-launchable. There is no
    /// warning-then-launch path: the architecture plan (§7.4) calls
    /// this a tripwire, not a hint.
    pub fn is_ready(&self, provider_id: &CliProviderId, current_executable_sha256: &str) -> bool {
        let Some(latest) = self.latest_attestation(provider_id) else {
            return false;
        };
        let mut current = latest.inputs.clone();
        current.executable_sha256 = current_executable_sha256.to_string();
        self.is_ready_for_inputs(&current)
    }

    /// Full launch-time readiness check. Callers that have resolved current
    /// profile, policy, schema, transport, host, & isolation context must use
    /// this method so any drift fails closed before process launch.
    pub fn is_ready_for_inputs(&self, current: &AttestationInputs) -> bool {
        let provider_id = &current.provider_id;
        let Some(latest) = self.latest_attestation(provider_id) else {
            return false;
        };
        latest.inputs == *current
            && latest.attestation_id == latest.inputs.address()
            && latest.failed_probes.is_empty()
            && !attestation_is_stale(latest.verified_at_unix_secs)
    }

    fn latest_attestation(&self, provider_id: &CliProviderId) -> Option<LatestAttestation> {
        self.adapter(provider_id)?;
        let store = match self.attestation_store.read() {
            Ok(slot) => slot.clone(),
            Err(_) => return None,
        };
        let store = store?;
        let latest = match store.latest_attestation(provider_id) {
            Ok(Some(row)) => row,
            Ok(None) => return None,
            Err(_) => return None,
        };
        Some(latest)
    }
}

/// True when the attestation is older than [`ATTESTATION_MAX_AGE`].
/// `verified_at` is stored as a Unix-second count on the
/// [`LatestAttestation`] view; the production attestation row keeps
/// the human-readable ISO string for the UI, and the daemon converts
/// to Unix-seconds at read time so the gate does not have to parse
/// strings.
fn attestation_is_stale(verified_at_unix_secs: u64) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now.saturating_sub(verified_at_unix_secs) > ATTESTATION_MAX_AGE.as_secs()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    InconsistentRequirements(String),
    LockPoisoned,
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::InconsistentRequirements(msg) => {
                write!(f, "inconsistent adapter requirements: {msg}")
            }
            RegistryError::LockPoisoned => f.write_str("registry lock poisoned"),
        }
    }
}

impl std::error::Error for RegistryError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::driver_host::cli_runtime::ids::{
        CliProviderCapabilities, HarnessIsolationMode, HarnessRequirements,
    };

    /// A no-op adapter for unit tests.
    struct TestAdapter {
        id: CliProviderId,
        transport: CliTransportKind,
        requirements: HarnessRequirements,
    }

    #[async_trait::async_trait]
    impl CliProviderAdapter for TestAdapter {
        fn id(&self) -> &CliProviderId {
            &self.id
        }
        fn display_name(&self) -> &'static str {
            "Test"
        }
        fn transport_kind(&self) -> CliTransportKind {
            self.transport
        }
        async fn discover(
            &self,
            _profile: &crate::driver_host::cli_runtime::ids::CliAccountProfile,
        ) -> Result<
            crate::driver_host::cli_runtime::adapter::CliDiscovery,
            crate::driver_host::cli_runtime::adapter::CliAdapterError,
        > {
            unimplemented!()
        }
        async fn authentication_status(
            &self,
            _profile: &crate::driver_host::cli_runtime::ids::CliAccountProfile,
        ) -> Result<
            crate::driver_host::cli_runtime::adapter::CliAuthenticationStatus,
            crate::driver_host::cli_runtime::adapter::CliAdapterError,
        > {
            unimplemented!()
        }
        async fn list_models(
            &self,
            _profile: &crate::driver_host::cli_runtime::ids::CliAccountProfile,
        ) -> Result<
            crate::driver_host::cli_runtime::adapter::CliModelCatalog,
            crate::driver_host::cli_runtime::adapter::CliAdapterError,
        > {
            unimplemented!()
        }
        async fn build_launch(
            &self,
            _request: &crate::driver_host::cli_runtime::adapter::CliLaunchRequest,
        ) -> Result<
            crate::driver_host::cli_runtime::adapter::CliLaunchSpec,
            crate::driver_host::cli_runtime::adapter::CliAdapterError,
        > {
            unimplemented!()
        }
        fn capabilities(&self) -> CliProviderCapabilities {
            CliProviderCapabilities {
                isolation_mode: HarnessIsolationMode::ExclusiveHostGateway,
                ..Default::default()
            }
        }
        fn harness_requirements(&self) -> HarnessRequirements {
            self.requirements.clone()
        }
        fn qualification_suite(
            &self,
        ) -> &'static [crate::driver_host::cli_runtime::ids::QualificationProbe] {
            &[]
        }
        fn normalize_provider_event(
            &self,
            _event: serde_json::Value,
        ) -> Result<
            Option<crate::driver_host::contracts::NormalizedEvent>,
            crate::driver_host::cli_runtime::adapter::CliAdapterError,
        > {
            Ok(None)
        }
    }

    #[test]
    fn register_and_lookup_roundtrip() {
        let registry = CliAdapterRegistry::new();
        let id = CliProviderId::new("test");
        let adapter: Arc<dyn CliProviderAdapter> = Arc::new(TestAdapter {
            id: id.clone(),
            transport: CliTransportKind::Acp,
            requirements: HarnessRequirements {
                requires_managed_profile: true,
                requires_one_mcp_server: true,
                requires_scoped_lease_token: true,
                requires_driver_sandbox: true,
                forbids_native_subagent: true,
                forbids_native_shell: true,
                forbids_native_browser: true,
                forbids_native_memory_writes: true,
            },
        });
        registry.register_adapter(adapter).unwrap();
        assert!(registry.adapter(&id).is_some());
        assert!(registry.provider_ids().iter().any(|p| p == &id));
    }

    fn make_test_adapter(id: &str) -> Arc<dyn CliProviderAdapter> {
        Arc::new(TestAdapter {
            id: CliProviderId::new(id),
            transport: CliTransportKind::Acp,
            requirements: HarnessRequirements {
                requires_managed_profile: true,
                requires_one_mcp_server: true,
                requires_scoped_lease_token: true,
                requires_driver_sandbox: true,
                forbids_native_subagent: true,
                forbids_native_shell: true,
                forbids_native_browser: true,
                forbids_native_memory_writes: true,
            },
        })
    }

    struct InMemoryAttestationStore {
        rows: std::sync::Mutex<HashMap<String, LatestAttestation>>,
    }

    impl InMemoryAttestationStore {
        fn new() -> Self {
            Self {
                rows: std::sync::Mutex::new(HashMap::new()),
            }
        }
        fn put(&self, provider: &str, row: LatestAttestation) {
            self.rows.lock().unwrap().insert(provider.to_string(), row);
        }
    }

    impl AttestationStore for InMemoryAttestationStore {
        fn latest_attestation(
            &self,
            provider_id: &CliProviderId,
        ) -> Result<Option<LatestAttestation>, AttestationStoreError> {
            let rows = self.rows.lock().unwrap();
            Ok(rows.get(provider_id.as_str()).cloned())
        }
    }

    fn fresh_attestation(provider: &str, executable_sha256: &str) -> LatestAttestation {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let inputs = AttestationInputs {
            provider_id: CliProviderId::new(provider),
            executable_sha256: executable_sha256.to_string(),
            binary_version: "1.0.0".into(),
            protocol_version: "acp/1".into(),
            profile_hash: "profile".into(),
            policy_hash: "policy".into(),
            tool_schema_hash: "schema".into(),
            operating_system: "macos".into(),
            architecture: "arm64".into(),
            isolation_mode: HarnessIsolationMode::ExclusiveHostGateway,
        };
        LatestAttestation {
            attestation_id: inputs.address(),
            inputs,
            failed_probes: Vec::new(),
            verified_at_unix_secs: now,
        }
    }

    fn stale_attestation(provider: &str, executable_sha256: &str) -> LatestAttestation {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // 25 hours ago — older than the 24h ceiling.
        let old = now.saturating_sub(25 * 60 * 60);
        let mut row = fresh_attestation(provider, executable_sha256);
        row.verified_at_unix_secs = old;
        row
    }

    #[test]
    fn is_ready_false_when_no_attestation_store_installed() {
        let registry = CliAdapterRegistry::new();
        registry
            .register_adapter(make_test_adapter("claude"))
            .unwrap();
        assert!(!registry.is_ready(&CliProviderId::new("claude"), "digest"));
    }

    #[test]
    fn is_ready_false_when_no_attestation_exists() {
        let registry = CliAdapterRegistry::new();
        registry
            .register_adapter(make_test_adapter("claude"))
            .unwrap();
        let store = Arc::new(InMemoryAttestationStore::new());
        registry.set_attestation_store(Some(store));
        assert!(!registry.is_ready(&CliProviderId::new("claude"), "digest"));
    }

    #[test]
    fn is_ready_true_when_attestation_matches_and_recent() {
        let registry = CliAdapterRegistry::new();
        registry
            .register_adapter(make_test_adapter("claude"))
            .unwrap();
        let store = Arc::new(InMemoryAttestationStore::new());
        store.put("claude", fresh_attestation("claude", "digest-v1"));
        registry.set_attestation_store(Some(store));
        assert!(registry.is_ready(&CliProviderId::new("claude"), "digest-v1"));
    }

    #[test]
    fn changing_binary_invalidates_readiness() {
        let registry = CliAdapterRegistry::new();
        registry
            .register_adapter(make_test_adapter("claude"))
            .unwrap();
        let store = Arc::new(InMemoryAttestationStore::new());
        // Attestation captured for v1; the on-disk binary is now v2.
        store.put("claude", fresh_attestation("claude", "digest-v1"));
        registry.set_attestation_store(Some(store));
        assert!(
            !registry.is_ready(&CliProviderId::new("claude"), "digest-v2"),
            "binary change must invalidate readiness"
        );
        // Reverting to the old digest re-enables readiness, proving the
        // gate is keyed on the digest and not on some cached decision.
        assert!(registry.is_ready(&CliProviderId::new("claude"), "digest-v1"));
    }

    #[test]
    fn changing_any_content_addressed_input_invalidates_full_readiness() {
        let registry = CliAdapterRegistry::new();
        registry
            .register_adapter(make_test_adapter("claude"))
            .unwrap();
        let store = Arc::new(InMemoryAttestationStore::new());
        let row = fresh_attestation("claude", "digest");
        let mut current = row.inputs.clone();
        store.put("claude", row);
        registry.set_attestation_store(Some(store));
        assert!(registry.is_ready_for_inputs(&current));
        current.policy_hash = "changed-policy".into();
        assert!(!registry.is_ready_for_inputs(&current));
        current.policy_hash = "policy".into();
        current.isolation_mode = HarnessIsolationMode::NativeReadOnlyCompatibility;
        assert!(!registry.is_ready_for_inputs(&current));
    }

    #[test]
    fn malformed_content_address_is_never_ready() {
        let registry = CliAdapterRegistry::new();
        registry
            .register_adapter(make_test_adapter("claude"))
            .unwrap();
        let store = Arc::new(InMemoryAttestationStore::new());
        let mut row = fresh_attestation("claude", "digest");
        row.attestation_id = "forged".into();
        store.put("claude", row);
        registry.set_attestation_store(Some(store));
        assert!(!registry.is_ready(&CliProviderId::new("claude"), "digest"));
    }

    #[test]
    fn failed_mandatory_probe_disqualifies_provider() {
        let registry = CliAdapterRegistry::new();
        registry
            .register_adapter(make_test_adapter("claude"))
            .unwrap();
        let store = Arc::new(InMemoryAttestationStore::new());
        let mut row = fresh_attestation("claude", "digest");
        row.failed_probes = vec!["neg.direct_native_file_write".to_string()];
        store.put("claude", row);
        registry.set_attestation_store(Some(store));
        assert!(!registry.is_ready(&CliProviderId::new("claude"), "digest"));
    }

    #[test]
    fn stale_attestation_older_than_24h_disqualifies_provider() {
        let registry = CliAdapterRegistry::new();
        registry
            .register_adapter(make_test_adapter("claude"))
            .unwrap();
        let store = Arc::new(InMemoryAttestationStore::new());
        store.put("claude", stale_attestation("claude", "digest"));
        registry.set_attestation_store(Some(store));
        assert!(!registry.is_ready(&CliProviderId::new("claude"), "digest"));
    }

    #[test]
    fn unknown_provider_is_not_ready() {
        let registry = CliAdapterRegistry::new();
        let store = Arc::new(InMemoryAttestationStore::new());
        store.put("claude", fresh_attestation("claude", "digest"));
        registry.set_attestation_store(Some(store));
        assert!(!registry.is_ready(&CliProviderId::new("ghost"), "digest"));
    }

    #[test]
    fn attestation_store_error_is_a_failure_not_a_panic() {
        struct ExplodingStore;
        impl AttestationStore for ExplodingStore {
            fn latest_attestation(
                &self,
                _id: &CliProviderId,
            ) -> Result<Option<LatestAttestation>, AttestationStoreError> {
                Err(AttestationStoreError::Backend("boom".into()))
            }
        }
        let registry = CliAdapterRegistry::new();
        registry
            .register_adapter(make_test_adapter("claude"))
            .unwrap();
        registry.set_attestation_store(Some(Arc::new(ExplodingStore)));
        assert!(!registry.is_ready(&CliProviderId::new("claude"), "digest"));
    }
}
