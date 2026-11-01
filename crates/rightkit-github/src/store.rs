use super::{GitHubAppError, GitHubDenial, InstallationBinding};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const STORE_SCHEMA_VERSION: u32 = 1;
const MAX_DELIVERY_ID_CHARS: usize = 256;

/// Errors returned by a durable backend. `replace_bytes` must provide an
/// atomic replace boundary; state memory is updated only after it succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    Unavailable(String),
}

/// Persistence seam for daemon storage, SQLite, or a platform atomic-file
/// implementation. Bytes are opaque to backend implementations.
pub trait GitHubAppStateBackend {
    fn load_bytes(&self) -> Result<Option<Vec<u8>>, BackendError>;

    fn replace_bytes(&self, bytes: &[u8]) -> Result<(), BackendError>;
}

/// Typed storage outcomes. Corrupt state never degrades into an empty store;
/// unavailable writes never update in-memory authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHubStoreError {
    LoadUnavailable(String),
    LoadCorrupt(String),
    WriteUnavailable(String),
    InvalidState(String),
    StaleGeneration { installation_id: u64, current: u64 },
    NotFound { installation_id: u64 },
    Denied(GitHubDenial),
}

impl std::fmt::Display for GitHubStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LoadUnavailable(reason) => write!(formatter, "GitHub App state unavailable: {reason}"),
            Self::LoadCorrupt(reason) => write!(formatter, "GitHub App state corrupt: {reason}"),
            Self::WriteUnavailable(reason) => write!(formatter, "GitHub App state write unavailable: {reason}"),
            Self::InvalidState(reason) => write!(formatter, "invalid GitHub App state: {reason}"),
            Self::StaleGeneration { installation_id, current } => write!(
                formatter,
                "stale GitHub App binding generation for installation {installation_id}; current={current}"
            ),
            Self::NotFound { installation_id } => {
                write!(formatter, "GitHub App installation {installation_id} not found")
            }
            Self::Denied(denial) => write!(formatter, "GitHub App state denied: {denial:?}"),
        }
    }
}

impl std::error::Error for GitHubStoreError {}

impl From<BackendError> for GitHubStoreError {
    fn from(error: BackendError) -> Self {
        match error {
            BackendError::Unavailable(reason) => Self::WriteUnavailable(reason),
        }
    }
}

impl From<GitHubAppError> for GitHubStoreError {
    fn from(error: GitHubAppError) -> Self {
        match error {
            GitHubAppError::Denied(denial) => Self::Denied(denial),
            GitHubAppError::InvalidBinding(reason)
            | GitHubAppError::InvalidToken(reason)
            | GitHubAppError::Adapter(reason) => Self::InvalidState(reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ReplayRecord {
    app_id: u64,
    installation_id: u64,
    delivery_id: String,
    received_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PersistedState {
    schema_version: u32,
    bindings: BTreeMap<u64, InstallationBinding>,
    replay: Vec<ReplayRecord>,
}

impl PersistedState {
    fn empty() -> Self {
        Self {
            schema_version: STORE_SCHEMA_VERSION,
            bindings: BTreeMap::new(),
            replay: Vec::new(),
        }
    }

    fn validate(&self) -> Result<(), GitHubStoreError> {
        if self.schema_version != STORE_SCHEMA_VERSION {
            return Err(GitHubStoreError::LoadCorrupt(format!(
                "unsupported schema version {}",
                self.schema_version
            )));
        }
        for (installation_id, binding) in &self.bindings {
            if *installation_id == 0
                || binding.installation_id != *installation_id
                || binding.app_id == 0
            {
                return Err(GitHubStoreError::LoadCorrupt(
                    "binding key does not match non-zero installation identity".to_string(),
                ));
            }
        }
        for record in &self.replay {
            if record.app_id == 0
                || record.installation_id == 0
                || !valid_delivery_id(&record.delivery_id)
            {
                return Err(GitHubStoreError::LoadCorrupt(
                    "replay key contains invalid identity".to_string(),
                ));
            }
        }
        Ok(())
    }
}

/// Durable binding/replay state machine. Mutations clone current state,
/// validate and atomically persist clone, then publish clone in memory.
pub struct GitHubAppStore<B> {
    backend: B,
    state: Mutex<PersistedState>,
    replay_capacity: usize,
    replay_ttl_seconds: u64,
}

impl<B: GitHubAppStateBackend> GitHubAppStore<B> {
    pub fn open(
        backend: B,
        replay_capacity: usize,
        replay_ttl_seconds: u64,
    ) -> Result<Self, GitHubStoreError> {
        if replay_capacity == 0 || replay_ttl_seconds == 0 {
            return Err(GitHubStoreError::InvalidState(
                "replay capacity and TTL must be non-zero".to_string(),
            ));
        }
        let state = match backend.load_bytes() {
            Ok(None) => PersistedState::empty(),
            Ok(Some(bytes)) => {
                serde_json::from_slice::<PersistedState>(&bytes).map_err(|error| {
                    GitHubStoreError::LoadCorrupt(format!("JSON decode failed: {error}"))
                })?
            }
            Err(BackendError::Unavailable(reason)) => {
                return Err(GitHubStoreError::LoadUnavailable(reason));
            }
        };
        state.validate()?;
        if state.replay.len() > replay_capacity {
            return Err(GitHubStoreError::LoadCorrupt(
                "replay state exceeds configured capacity".to_string(),
            ));
        }
        Ok(Self {
            backend,
            state: Mutex::new(state),
            replay_capacity,
            replay_ttl_seconds,
        })
    }

    pub fn binding(
        &self,
        app_id: u64,
        installation_id: u64,
    ) -> Result<InstallationBinding, GitHubStoreError> {
        let state = self.lock_state()?;
        let binding = state
            .bindings
            .get(&installation_id)
            .ok_or(GitHubStoreError::NotFound { installation_id })?;
        if binding.app_id != app_id {
            return Err(GitHubStoreError::Denied(GitHubDenial::InstallationMismatch));
        }
        Ok(binding.clone())
    }

    /// Replace binding only with strictly newer generation. Existing app ID
    /// cannot be silently swapped under same installation identity.
    pub fn replace_binding(
        &self,
        binding: InstallationBinding,
    ) -> Result<InstallationBinding, GitHubStoreError> {
        let mut state = self.lock_state()?;
        let installation_id = binding.installation_id;
        if let Some(current) = state.bindings.get(&installation_id) {
            if current.app_id != binding.app_id {
                return Err(GitHubStoreError::Denied(GitHubDenial::InstallationMismatch));
            }
            if binding.generation <= current.generation {
                return Err(GitHubStoreError::StaleGeneration {
                    installation_id,
                    current: current.generation,
                });
            }
        }
        let mut next = state.clone();
        next.bindings.insert(installation_id, binding.clone());
        self.commit(&mut state, next)?;
        Ok(binding)
    }

    /// Revoke by disabling binding and advancing generation, preserving
    /// durable identity while invalidating future authority checks.
    pub fn revoke_binding(
        &self,
        app_id: u64,
        installation_id: u64,
        expected_generation: Option<u64>,
    ) -> Result<InstallationBinding, GitHubStoreError> {
        let mut state = self.lock_state()?;
        let current = state
            .bindings
            .get(&installation_id)
            .ok_or(GitHubStoreError::NotFound { installation_id })?;
        if current.app_id != app_id {
            return Err(GitHubStoreError::Denied(GitHubDenial::InstallationMismatch));
        }
        if expected_generation.is_some_and(|generation| generation != current.generation) {
            return Err(GitHubStoreError::StaleGeneration {
                installation_id,
                current: current.generation,
            });
        }
        let mut revoked = current.clone();
        revoked.enabled = false;
        revoked.generation = revoked.generation.checked_add(1).ok_or_else(|| {
            GitHubStoreError::InvalidState("binding generation overflow".to_string())
        })?;
        let mut next = state.clone();
        next.bindings.insert(installation_id, revoked.clone());
        self.commit(&mut state, next)?;
        Ok(revoked)
    }

    /// Reserve delivery identity before dispatch. Expired entries are pruned;
    /// live entries are never evicted to bypass replay protection.
    pub fn reserve_delivery(
        &self,
        app_id: u64,
        installation_id: u64,
        delivery_id: impl Into<String>,
        now_unix: u64,
    ) -> Result<(), GitHubStoreError> {
        if app_id == 0 || installation_id == 0 {
            return Err(GitHubStoreError::InvalidState(
                "replay identity must be non-zero".to_string(),
            ));
        }
        let delivery_id = delivery_id.into();
        if !valid_delivery_id(&delivery_id) {
            return Err(GitHubStoreError::Denied(if delivery_id.is_empty() {
                GitHubDenial::MissingDeliveryId
            } else {
                GitHubDenial::InvalidDeliveryId
            }));
        }
        let mut state = self.lock_state()?;
        let mut next = state.clone();
        let ttl_seconds = self.replay_ttl_seconds;
        next.replay
            .retain(|record| now_unix.saturating_sub(record.received_at) <= ttl_seconds);
        let key = ReplayRecord {
            app_id,
            installation_id,
            delivery_id: delivery_id.clone(),
            received_at: now_unix,
        };
        if next.replay.iter().any(|record| {
            record.app_id == key.app_id
                && record.installation_id == key.installation_id
                && record.delivery_id == key.delivery_id
        }) {
            return Err(GitHubStoreError::Denied(GitHubDenial::Replay {
                delivery_id,
            }));
        }
        if next.replay.len() >= self.replay_capacity {
            return Err(GitHubStoreError::Denied(GitHubDenial::ReplayWindowFull));
        }
        next.replay.push(key);
        self.commit(&mut state, next)
    }

    pub fn serialized_state(&self) -> Result<Vec<u8>, GitHubStoreError> {
        let state = self.lock_state()?;
        serde_json::to_vec(&*state)
            .map_err(|error| GitHubStoreError::InvalidState(format!("JSON encode failed: {error}")))
    }

    fn lock_state(&self) -> Result<std::sync::MutexGuard<'_, PersistedState>, GitHubStoreError> {
        self.state
            .lock()
            .map_err(|_| GitHubStoreError::WriteUnavailable("state mutex poisoned".to_string()))
    }

    fn commit(
        &self,
        state: &mut std::sync::MutexGuard<'_, PersistedState>,
        next: PersistedState,
    ) -> Result<(), GitHubStoreError> {
        next.validate()?;
        let bytes = serde_json::to_vec(&next).map_err(|error| {
            GitHubStoreError::InvalidState(format!("JSON encode failed: {error}"))
        })?;
        self.backend
            .replace_bytes(&bytes)
            .map_err(|error| match error {
                BackendError::Unavailable(reason) => GitHubStoreError::WriteUnavailable(reason),
            })?;
        **state = next;
        Ok(())
    }
}

fn valid_delivery_id(delivery_id: &str) -> bool {
    !delivery_id.is_empty()
        && delivery_id.chars().count() <= MAX_DELIVERY_ID_CHARS
        && delivery_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

/// Test/backend seam. Production should supply SQLite or platform atomic-file
/// implementation rather than relying on this process-local backend.
#[derive(Clone, Default)]
pub struct MemoryStateBackend {
    bytes: Arc<Mutex<Option<Vec<u8>>>>,
    fail_load: Arc<Mutex<bool>>,
    fail_write: Arc<Mutex<bool>>,
}

impl MemoryStateBackend {
    pub fn with_bytes(bytes: Vec<u8>) -> Self {
        Self {
            bytes: Arc::new(Mutex::new(Some(bytes))),
            ..Self::default()
        }
    }

    pub fn set_fail_load(&self, fail: bool) {
        *self.fail_load.lock().unwrap() = fail;
    }

    pub fn set_fail_write(&self, fail: bool) {
        *self.fail_write.lock().unwrap() = fail;
    }

    pub fn bytes(&self) -> Option<Vec<u8>> {
        self.bytes.lock().unwrap().clone()
    }
}

impl GitHubAppStateBackend for MemoryStateBackend {
    fn load_bytes(&self) -> Result<Option<Vec<u8>>, BackendError> {
        if *self.fail_load.lock().unwrap() {
            return Err(BackendError::Unavailable("test load failure".to_string()));
        }
        Ok(self.bytes.lock().unwrap().clone())
    }

    fn replace_bytes(&self, bytes: &[u8]) -> Result<(), BackendError> {
        if *self.fail_write.lock().unwrap() {
            return Err(BackendError::Unavailable("test write failure".to_string()));
        }
        *self.bytes.lock().unwrap() = Some(bytes.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccessLevel, GitHubPermission, RepositoryScope};
    use std::collections::{BTreeMap, BTreeSet};

    fn binding(generation: u64) -> InstallationBinding {
        InstallationBinding::new(
            7,
            9,
            Some(11),
            "acme",
            RepositoryScope::Explicit(BTreeSet::from([42])),
            BTreeMap::from([(GitHubPermission::Issues, AccessLevel::Write)]),
            generation,
        )
        .unwrap()
    }

    #[test]
    fn replace_and_revoke_are_generation_checked_and_persistent() {
        let backend = MemoryStateBackend::default();
        let store = GitHubAppStore::open(backend.clone(), 8, 300).unwrap();
        store.replace_binding(binding(1)).unwrap();
        assert!(matches!(
            store.replace_binding(binding(1)),
            Err(GitHubStoreError::StaleGeneration { .. })
        ));
        let revoked = store.revoke_binding(7, 9, Some(1)).unwrap();
        assert!(!revoked.enabled);
        assert_eq!(revoked.generation, 2);
        assert!(!backend.bytes().unwrap().is_empty());
        let reopened = GitHubAppStore::open(backend, 8, 300).unwrap();
        assert!(!reopened.binding(7, 9).unwrap().enabled);
        assert_eq!(reopened.binding(7, 9).unwrap().generation, 2);
    }

    #[test]
    fn write_failure_rolls_back_in_memory_state() {
        let backend = MemoryStateBackend::default();
        let store = GitHubAppStore::open(backend.clone(), 8, 300).unwrap();
        store.replace_binding(binding(1)).unwrap();
        backend.set_fail_write(true);
        assert!(matches!(
            store.replace_binding(binding(2)),
            Err(GitHubStoreError::WriteUnavailable(_))
        ));
        assert_eq!(store.binding(7, 9).unwrap().generation, 1);
    }

    #[test]
    fn corrupt_and_unavailable_loads_are_typed() {
        let corrupt =
            GitHubAppStore::open(MemoryStateBackend::with_bytes(b"not-json".to_vec()), 8, 300);
        assert!(matches!(corrupt, Err(GitHubStoreError::LoadCorrupt(_))));
        let backend = MemoryStateBackend::default();
        backend.set_fail_load(true);
        assert!(matches!(
            GitHubAppStore::open(backend, 8, 300),
            Err(GitHubStoreError::LoadUnavailable(_))
        ));
    }

    #[test]
    fn replay_retention_is_bounded_without_live_eviction() {
        let backend = MemoryStateBackend::default();
        let store = GitHubAppStore::open(backend.clone(), 1, 300).unwrap();
        store.reserve_delivery(7, 9, "one", 100).unwrap();
        assert!(matches!(
            store.reserve_delivery(7, 9, "one", 101),
            Err(GitHubStoreError::Denied(GitHubDenial::Replay { .. }))
        ));
        assert!(matches!(
            store.reserve_delivery(7, 9, "two", 101),
            Err(GitHubStoreError::Denied(GitHubDenial::ReplayWindowFull))
        ));
        store.reserve_delivery(7, 9, "two", 401).unwrap();
        let reopened = GitHubAppStore::open(backend, 1, 300).unwrap();
        assert!(matches!(
            reopened.reserve_delivery(7, 9, "two", 402),
            Err(GitHubStoreError::Denied(GitHubDenial::Replay { .. }))
        ));
    }

    #[test]
    fn serialized_state_contains_no_credential_fields_or_values() {
        let backend = MemoryStateBackend::default();
        let store = GitHubAppStore::open(backend, 8, 300).unwrap();
        store.replace_binding(binding(1)).unwrap();
        let serialized = String::from_utf8(store.serialized_state().unwrap()).unwrap();
        assert!(!serialized.contains("token"));
        assert!(!serialized.contains("private_key"));
        assert!(serialized.contains("installation_id"));
    }
}
