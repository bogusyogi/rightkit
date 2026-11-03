//! Binding-metadata persistence for the Claude harness (PL-005).
//!
//! The daemon owns durable storage; this module owns the *stable
//! record shape* and two reference stores. The record projects the
//! recovery fields of the donor lineage — `BindingSnapshot` /
//! `DriverBindingRecoveryRow` / `DriverCursor` in
//! `cli_runtime::recovery` and `crate::driver_host::contracts::DriverBinding` —
//! onto one Claude-specific document that the harness rewrites at
//! every state change, so a daemon restart can rebuild exactly what
//! the vendor session was bound to (session ids, workspace grant,
//! process generation, delegation-policy digest, bypass violations).
//!
//! Status values mirror the flat binding state machine of
//! `cli_runtime::ids::CliBindingStatus` restricted to the states the
//! persistent root runtime actually occupies; transitions are
//! validated with [`transition_is_valid`] before any store write.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::driver_host::contracts::DriverCursor;

use super::workspace::{validate_path_component, WorkspaceTrustVerdict};

/// The engine identity stamped on every harness binding record.
pub const HARNESS_ENGINE: &str = "claude-code";

/// Flat lifecycle state of one persistent Claude root-runtime
/// binding. Session-level: a completed *turn* does not complete the
/// binding — only an explicit stop or an unrecoverable vendor exit
/// does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessBindingStatus {
    Starting,
    Ready,
    Running,
    WaitingForApproval,
    Interrupted,
    Resuming,
    Completed,
    Failed,
    RecoveryRequired,
}

/// Whether a status transition is legal. Re-persisting the same
/// status is always allowed (idempotent refresh); every real move is
/// validated so a buggy host cannot drive a binding into an
/// inconsistent lifecycle state silently.
pub fn transition_is_valid(from: HarnessBindingStatus, to: HarnessBindingStatus) -> bool {
    use HarnessBindingStatus::*;
    if from == to {
        return true;
    }
    match (from, to) {
        (Starting, Ready | Failed | RecoveryRequired | Completed) => true,
        (Ready, Running | Resuming | Failed | RecoveryRequired | Completed) => true,
        (
            Running,
            WaitingForApproval | Interrupted | Resuming | Failed | RecoveryRequired | Completed,
        ) => true,
        (
            WaitingForApproval,
            Running | Interrupted | Resuming | Failed | RecoveryRequired | Completed,
        ) => true,
        (Interrupted, Running | Resuming | Failed | RecoveryRequired | Completed) => true,
        (Resuming, Running | Failed | RecoveryRequired | Interrupted | Completed) => true,
        (Failed, RecoveryRequired | Resuming | Completed) => true,
        (RecoveryRequired, Resuming | Failed | Completed) => true,
        (Completed, Resuming) => true,
        _ => false,
    }
}

/// One persistent Claude root-runtime binding. Serialized as JSON
/// (camelCase, matching `crate::driver_host::contracts` wire conventions) by the
/// reference stores.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeHarnessBindingRecord {
    pub binding_id: String,
    /// Stamped [`HARNESS_ENGINE`]; guards a record from being loaded
    /// by a different harness seam.
    pub engine: String,
    pub host_session_id: String,
    pub coderight_agent_id: String,
    pub vendor_session_id: Option<String>,
    pub vendor_turn_id: Option<String>,
    pub model_id: Option<String>,
    pub workspace_root: PathBuf,
    pub trust: WorkspaceTrustVerdict,
    pub managed_profile_label: String,
    pub managed_home: PathBuf,
    /// Monotonic restart generation, kept in lock-step with
    /// `cli_runtime::recovery::LiveProcessGeneration`.
    pub process_generation: u64,
    /// Snapshot of the session's context epoch at last bind
    /// (`cli_runtime::ids::ContextEpoch.value`).
    pub context_epoch: u64,
    pub status: HarnessBindingStatus,
    pub protocol_version: Option<String>,
    /// Reader cursor advanced by the runtime event loop; replay
    /// suppression uses `last_event_key`, never the offsets.
    pub cursor: DriverCursor,
    /// SHA-256 of the run-scoped policy the launch was validated
    /// against (`RUN_SCOPED_POLICY`), per the executor's
    /// `policy_hash` donor shape.
    pub delegation_policy_digest: String,
    /// Count of provider-native child-spawn attempts observed in the
    /// vendor event stream (delegation-bypass audit counter).
    pub native_child_spawn_violations: u32,
    /// Replay duplicates dropped by exact-once admission.
    pub duplicate_events_dropped: u64,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
}

impl ClaudeHarnessBindingRecord {
    /// Construct the initial record for a freshly opened binding
    /// (status `Starting`). The runtime applies validated transitions
    /// afterwards.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        binding_id: impl Into<String>,
        host_session_id: impl Into<String>,
        coderight_agent_id: impl Into<String>,
        trust: WorkspaceTrustVerdict,
        managed_profile_label: impl Into<String>,
        managed_home: PathBuf,
        delegation_policy_digest: impl Into<String>,
        now_unix_ms: u64,
    ) -> Self {
        Self {
            binding_id: binding_id.into(),
            engine: HARNESS_ENGINE.to_string(),
            host_session_id: host_session_id.into(),
            coderight_agent_id: coderight_agent_id.into(),
            vendor_session_id: None,
            vendor_turn_id: None,
            model_id: None,
            workspace_root: PathBuf::new(),
            trust,
            managed_profile_label: managed_profile_label.into(),
            managed_home,
            process_generation: 0,
            context_epoch: 0,
            status: HarnessBindingStatus::Starting,
            protocol_version: Some("stream-json".to_string()),
            cursor: DriverCursor::default(),
            delegation_policy_digest: delegation_policy_digest.into(),
            native_child_spawn_violations: 0,
            duplicate_events_dropped: 0,
            created_at_unix_ms: now_unix_ms,
            updated_at_unix_ms: now_unix_ms,
        }
    }
}

/// Store errors. Every variant names the repair action.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BindingStoreError {
    #[error("binding id is not a safe store key: {0}")]
    InvalidBindingId(String),
    #[error("binding record serialization failed: {0}")]
    Serialization(String),
    #[error("binding i/o failed: {0}")]
    Io(String),
    #[error("binding not found: {0}")]
    UnknownBinding(String),
}

/// Durable binding-metadata store. The daemon implements this against
/// its own storage layer; two reference implementations are provided
/// ([`InMemoryBindingMetadataStore`] for tests/ephemeral runs and
/// [`JsonFileBindingMetadataStore`] for a durable on-disk layout).
pub trait BindingMetadataStore: Send + Sync {
    fn save(&self, record: &ClaudeHarnessBindingRecord) -> Result<(), BindingStoreError>;
    fn load(
        &self,
        binding_id: &str,
    ) -> Result<Option<ClaudeHarnessBindingRecord>, BindingStoreError>;
    fn list(&self) -> Result<Vec<ClaudeHarnessBindingRecord>, BindingStoreError>;
    /// Delete is idempotent: deleting an unknown binding succeeds.
    fn delete(&self, binding_id: &str) -> Result<(), BindingStoreError>;
}

/// Ephemeral in-memory store (tests, dry runs).
#[derive(Debug, Default)]
pub struct InMemoryBindingMetadataStore {
    records: Mutex<BTreeMap<String, ClaudeHarnessBindingRecord>>,
}

impl InMemoryBindingMetadataStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl BindingMetadataStore for InMemoryBindingMetadataStore {
    fn save(&self, record: &ClaudeHarnessBindingRecord) -> Result<(), BindingStoreError> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| BindingStoreError::Io("in-memory store mutex poisoned".to_string()))?;
        records.insert(record.binding_id.clone(), record.clone());
        Ok(())
    }

    fn load(
        &self,
        binding_id: &str,
    ) -> Result<Option<ClaudeHarnessBindingRecord>, BindingStoreError> {
        let records = self
            .records
            .lock()
            .map_err(|_| BindingStoreError::Io("in-memory store mutex poisoned".to_string()))?;
        Ok(records.get(binding_id).cloned())
    }

    fn list(&self) -> Result<Vec<ClaudeHarnessBindingRecord>, BindingStoreError> {
        let records = self
            .records
            .lock()
            .map_err(|_| BindingStoreError::Io("in-memory store mutex poisoned".to_string()))?;
        Ok(records.values().cloned().collect())
    }

    fn delete(&self, binding_id: &str) -> Result<(), BindingStoreError> {
        let mut records = self
            .records
            .lock()
            .map_err(|_| BindingStoreError::Io("in-memory store mutex poisoned".to_string()))?;
        records.remove(binding_id);
        Ok(())
    }
}

/// Durable JSON-file store: one atomic document per binding under
/// `root/<binding_id>.json`. Writes go through `rightkit_fs::atomic_write_with`
/// so a crash never leaves a torn record (donor durability shape:
/// journal `.pending` + atomic publish, `CG-C01`/codex
/// `rollout_migration.rs` convention, applied at record scope).
#[derive(Debug)]
pub struct JsonFileBindingMetadataStore {
    root: PathBuf,
    write_lock: Mutex<()>,
}

impl JsonFileBindingMetadataStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            write_lock: Mutex::new(()),
        }
    }

    fn path_for(&self, binding_id: &str) -> Result<PathBuf, BindingStoreError> {
        validate_path_component(binding_id)
            .map_err(|error| BindingStoreError::InvalidBindingId(error.to_string()))?;
        Ok(self.root.join(format!("{binding_id}.json")))
    }
}

impl BindingMetadataStore for JsonFileBindingMetadataStore {
    fn save(&self, record: &ClaudeHarnessBindingRecord) -> Result<(), BindingStoreError> {
        let path = self.path_for(&record.binding_id)?;
        let body = serde_json::to_vec_pretty(record)
            .map_err(|error| BindingStoreError::Serialization(error.to_string()))?;
        let _guard = self
            .write_lock
            .lock()
            .map_err(|_| BindingStoreError::Io("json store mutex poisoned".to_string()))?;
        rightkit_fs::atomic_write_with(
            &path,
            rightkit_fs::AtomicOptions {
                create_parents: true,
                ..rightkit_fs::AtomicOptions::default()
            },
            |file| std::io::Write::write_all(file, &body),
        )
        .map_err(|error| BindingStoreError::Io(error.to_string()))?;
        Ok(())
    }

    fn load(
        &self,
        binding_id: &str,
    ) -> Result<Option<ClaudeHarnessBindingRecord>, BindingStoreError> {
        let path = self.path_for(binding_id)?;
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(BindingStoreError::Io(error.to_string())),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| BindingStoreError::Serialization(error.to_string()))
    }

    fn list(&self) -> Result<Vec<ClaudeHarnessBindingRecord>, BindingStoreError> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(BindingStoreError::Io(error.to_string())),
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| BindingStoreError::Io(error.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let bytes =
                std::fs::read(&path).map_err(|error| BindingStoreError::Io(error.to_string()))?;
            let record: ClaudeHarnessBindingRecord = serde_json::from_slice(&bytes)
                .map_err(|error| BindingStoreError::Serialization(error.to_string()))?;
            records.push(record);
        }
        records.sort_by(|a, b| a.binding_id.cmp(&b.binding_id));
        Ok(records)
    }

    fn delete(&self, binding_id: &str) -> Result<(), BindingStoreError> {
        let path = self.path_for(binding_id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(BindingStoreError::Io(error.to_string())),
        }
    }
}
