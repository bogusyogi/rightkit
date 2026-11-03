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
