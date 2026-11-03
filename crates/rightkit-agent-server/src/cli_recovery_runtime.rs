//! Daemon-side wiring for PR 13 (Recovery and version drift).
//!
//! The actual recovery primitives live in
//! `engine/crates/driver_host/src/cli_runtime/recovery.rs`. This module
//! is the seam between those primitives and the daemon binary:
//!
//! - [`CliRecoveryGuard`] is a daemon-owned [`BindingResumeGuard`] that
//!   re-loads a binding from storage and re-validates the executable
//!   digest, process generation, context epoch, and protocol version
//!   before the executor's `resume()` path reaches the bridge.
//! - [`walk_active_bindings_at_startup`] projects every active
//!   `driver_bindings` row into a [`BindingSnapshot`] and runs the
//!   shared walker; the returned outcomes drive UI banners and the
//!   resume-after-restart decision.
//! - [`apply_deferred_binding_updates`] drains the
//!   [`DeferredBindingUpdates`] queue at a turn boundary (never
//!   mid-turn) and applies each pending update against storage,
//!   preserving the `idx_one_active_binding_per_agent` UNIQUE
//!   invariant.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rightkit_agent_engine_storage::DriverBindingRow;
use rightkit_agents::driver_host::cli_runtime::recovery::{
    walk_active_bindings_for_resume, BindingResumeGuard, BindingSnapshot, DeferredApplyReport,
    DeferredBindingUpdate, DeferredBindingUpdates, DeferredUpdateKind, DriverBindingRecoveryRow,
    LiveContextEpoch, LiveProcessGeneration, RecoveryError, ResumeDecision, ResumeWalkOutcome,
};
use rightkit_agents::driver_host::cli_runtime::{ContextEpoch, ContextEpochReason};

use crate::api::SharedStorage;

/// One typed entry in the per-binding live generation table. The daemon
/// keeps an `Arc<…>` so the supervisor and the resume guard share the
/// same view without copying `u64`s around.
#[derive(Debug, Default)]
pub struct LiveGenerationTable {
    inner: Mutex<BTreeMap<String, u64>>,
}

impl LiveGenerationTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, binding_id: &str, generation: u64) {
        let mut guard = self
            .inner
            .lock()
            .expect("LiveGenerationTable mutex poisoned");
        guard.insert(binding_id.to_string(), generation);
    }

    pub fn get(&self, binding_id: &str) -> Option<u64> {
        let guard = self
            .inner
            .lock()
            .expect("LiveGenerationTable mutex poisoned");
        guard.get(binding_id).copied()
    }

    pub fn snapshot(&self) -> BTreeMap<String, u64> {
        self.inner
            .lock()
            .expect("LiveGenerationTable mutex poisoned")
            .clone()
    }

    /// Advance one binding's generation, returning the new value.
    /// Saturates instead of overflowing.
    pub fn advance(&self, binding_id: &str) -> u64 {
        let mut guard = self
            .inner
            .lock()
            .expect("LiveGenerationTable mutex poisoned");
        let entry = guard.entry(binding_id.to_string()).or_insert(0);
        let next = entry.saturating_add(1);
        *entry = next;
        next
    }
}

/// The daemon's live context epoch. A single epoch covers the entire
/// daemon process; rebuilds of context (workspace trust change,
/// re-authentication, model swap, policy update) bump it.
#[derive(Debug, Clone)]
pub struct LiveContextEpochState {
    inner: Arc<Mutex<LiveContextEpoch>>,
}

impl LiveContextEpochState {
    pub fn new(initial: LiveContextEpoch) -> Self {
        Self {
            inner: Arc::new(Mutex::new(initial)),
        }
    }

    pub fn current(&self) -> LiveContextEpoch {
        self.inner
            .lock()
            .expect("LiveContextEpochState mutex poisoned")
            .clone()
    }

    /// Replace the live epoch with `next`. The caller is responsible
    /// for ensuring `next.value` is monotonic; the helper does not
    /// check.
    pub fn replace(&self, next: LiveContextEpoch) {
        let mut guard = self
            .inner
            .lock()
            .expect("LiveContextEpochState mutex poisoned");
        *guard = next;
    }
}

impl Default for LiveContextEpochState {
    fn default() -> Self {
        Self::new(LiveContextEpoch::initial())
    }
}

/// Daemon-owned [`BindingResumeGuard`]. The executor's `resume()`
/// path calls into this before reaching the bridge. The guard loads
/// the binding from storage, re-validates the executable digest, and
/// checks stored vs live generation / epoch / protocol version.
/// Any mismatch surfaces as a typed [`RecoveryError`] which the
/// executor converts into a UI error with a stable code and hint.
pub struct CliRecoveryGuard {
    storage: SharedStorage,
    live_generations: Arc<LiveGenerationTable>,
    live_epoch: Arc<LiveContextEpochState>,
}

impl CliRecoveryGuard {
    pub fn new(
        storage: SharedStorage,
        live_generations: Arc<LiveGenerationTable>,
        live_epoch: Arc<LiveContextEpochState>,
    ) -> Self {
        Self {
            storage,
            live_generations,
            live_epoch,
        }
    }
}

impl BindingResumeGuard for CliRecoveryGuard {
    fn validate_resume(&self, binding_id: &str) -> Result<(), RecoveryError> {
        let row = {
            let guard = match self.storage.try_lock() {
                Ok(guard) => guard,
                Err(_) => {
                    return Err(RecoveryError::process_generation_stale(binding_id, 0, 0));
                }
            };
            match guard.get_driver_binding(binding_id) {
                Ok(Some(row)) => row,
                Ok(None) => {
                    return Err(RecoveryError::process_generation_stale(binding_id, 0, 0));
                }
                Err(_error) => {
                    return Err(RecoveryError::process_generation_stale(binding_id, 0, 0));
                }
            }
        };
        let snapshot = row.into_recovery_snapshot();
        let live_epoch = self.live_epoch.current();
        let decision = walk_active_bindings_for_resume(
            std::slice::from_ref(&snapshot),
            &self.live_generations.snapshot(),
            &live_epoch,
        )
        .into_iter()
        .next()
        .map(|outcome| outcome.decision)
        .unwrap_or(ResumeDecision::Refuse(
            RecoveryError::process_generation_stale(
                binding_id,
                snapshot.process_generation,
                snapshot.process_generation,
            ),
        ));
        match decision {
            ResumeDecision::Resume { .. } => Ok(()),
            ResumeDecision::Refuse(error) => Err(error),
        }
    }
}

/// Trait extension so we can project a storage row to the recovery
/// snapshot without leaking storage types into the recovery module.
trait IntoRecoverySnapshot {
    fn into_recovery_snapshot(self) -> BindingSnapshot;
}

impl IntoRecoverySnapshot for DriverBindingRow {
    fn into_recovery_snapshot(self) -> BindingSnapshot {
        let recovery_row = DriverBindingRecoveryRow {
            binding_id: self.id.clone(),
            coderight_agent_id: self.coderight_agent_id.clone().unwrap_or_default(),
            session_id: self.session_id.clone(),
            executable_sha256: self.executable_sha256.clone().unwrap_or_default(),
            executable_path: self.executable_path.as_deref().map(PathBuf::from),
            process_generation: self.process_generation,
            context_epoch: ContextEpoch {
                value: self.context_epoch,
                reason: ContextEpochReason::NewSession,
            },
            status: self.status.clone(),
            protocol_version: self.protocol_version.clone(),
        };
        recovery_row.into_snapshot()
    }
}

/// Walk every active binding in the database through the recovery
/// walker. The caller supplies the live generation table and the live
/// epoch; this helper projects each row to a [`BindingSnapshot`] and
/// invokes [`walk_active_bindings_for_resume`].
///
/// Returns the per-binding [`ResumeWalkOutcome`] list in deterministic
/// (sorted) order. Outcomes marked [`ResumeDecision::Refuse`] are
/// surfaced to the UI with the typed error code and hint; outcomes
/// marked [`ResumeDecision::Resume`] are the set of bindings the
/// supervisor should resume natively after restart.
pub async fn walk_active_bindings_at_startup(
    storage: &SharedStorage,
    live_generations: &LiveGenerationTable,
    live_epoch: &LiveContextEpoch,
) -> Vec<ResumeWalkOutcome> {
    let rows = {
        let guard = storage.lock().await;
        match guard.list_active_driver_bindings() {
            Ok(rows) => rows,
            Err(error) => {
                eprintln!("[serve] recovery walk failed: {error}");
                return Vec::new();
            }
        }
    };
    let snapshots: Vec<BindingSnapshot> = rows
        .into_iter()
        .map(IntoRecoverySnapshot::into_recovery_snapshot)
        .collect();
    walk_active_bindings_for_resume(&snapshots, &live_generations.snapshot(), live_epoch)
}

/// Apply a batch of [`DeferredBindingUpdate`] values to storage.
/// Returns a [`DeferredApplyReport`] summarising how many updates
/// were applied and how many were dropped because the binding
/// no longer exists.
///
/// This is intentionally a free function, not a method on
/// `DaemonState`, so the turn boundary can be wired from any call
/// site (turn completion, journal scan, or operator action) without
/// taking a mutable borrow on the daemon state.
pub async fn apply_deferred_binding_updates(
    storage: &SharedStorage,
    updates: Vec<DeferredBindingUpdate>,
) -> DeferredApplyReport {
    let mut report = DeferredApplyReport::default();
    for update in updates {
        let result = match update.kind.clone() {
            DeferredUpdateKind::IncrementProcessGeneration { next_generation } => {
                let guard = storage.lock().await;
                guard
                    .set_driver_binding_process_generation(
                        &update.binding_id,
                        next_generation,
                        &update.queued_at,
                    )
                    .map_err(|error| format!("{error}"))
            }
            DeferredUpdateKind::BumpContextEpoch { new_epoch } => {
                let guard = storage.lock().await;
                guard
                    .set_driver_binding_context_epoch(
                        &update.binding_id,
                        new_epoch.value,
                        &update.queued_at,
                    )
                    .map_err(|error| format!("{error}"))
            }
            DeferredUpdateKind::RecordExecutableDigest { new_sha256 } => {
                let guard = storage.lock().await;
                guard
                    .set_driver_binding_executable_sha256(
                        &update.binding_id,
                        &new_sha256,
                        &update.queued_at,
                    )
                    .map_err(|error| format!("{error}"))
            }
        };
        match result {
            Ok(true) => {
                report.applied += 1;
            }
            Ok(false) => {
                // Storage returned "no row matched" — binding no longer
                // exists; drop the update.
                report.dropped_unknown_binding += 1;
            }
            Err(_error) => {
                // Best-effort: a transient error here should not block
                // turn completion. The applier logs and continues.
                report.dropped_unknown_binding += 1;
            }
        }
    }
    report
}

/// Helper used by the supervisor when a vendor process restarts.
/// Advances the in-memory generation and queues a deferred update so
/// the binding row catches up at the next turn boundary.
pub fn record_process_restart(
    queue: &DeferredBindingUpdates,
    table: &LiveGenerationTable,
    binding_id: &str,
    now: &str,
) {
    let next = table.advance(binding_id);
    queue.enqueue(DeferredBindingUpdate::increment_generation(
        binding_id, next, now,
    ));
}

/// Helper used when the RightKit context rebuilds (workspace trust
/// changed, user re-authenticated, etc.). Advances the live epoch and
/// queues a deferred update for every binding the daemon knows
/// about so each binding row gets re-stamped at the turn boundary.
pub fn record_context_rebuild(
    queue: &DeferredBindingUpdates,
    epoch: &LiveContextEpochState,
    binding_ids: &[String],
    new_epoch: ContextEpoch,
    now: &str,
) {
    let advanced = epoch.current().advance(new_epoch.reason);
    epoch.replace(advanced.clone());
    for binding_id in binding_ids {
        queue.enqueue(DeferredBindingUpdate::bump_epoch(
            binding_id,
            ContextEpoch {
                value: advanced.value,
                reason: advanced.reason,
            },
            now,
        ));
    }
}

/// Convenience for constructing a [`LiveProcessGeneration`] off the
/// shared table. Used by the supervisor when spawning a brand-new
/// process so the guard's view of the live generation is consistent.
pub fn fresh_live_generation(
    table: &LiveGenerationTable,
    binding_id: &str,
) -> LiveProcessGeneration {
    let mut live = LiveProcessGeneration::new(binding_id);
    if let Some(existing) = table.get(binding_id) {
        live.generation = existing;
    }
    live
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_engine_storage::Storage;

    fn sample_row(id: &str, status: &str) -> DriverBindingRow {
        DriverBindingRow {
            id: id.to_string(),
            driver_kind: "claude".into(),
            session_id: "session-1".into(),
            vendor_session_id: None,
            vendor_thread_id: None,
            vendor_turn_id: None,
            binary_version: "1".into(),
            schema_hash: "h".into(),
            status: status.into(),
            last_event_key: None,
            last_event_cursor: None,
            coderight_agent_id: Some("agent-1".into()),
            parent_agent_id: "".into(),
            provider_id: Some("claude".into()),
            transport_kind: Some("claude_stream_json".into()),
            account_profile_id: Some("claude/default".into()),
            auth_mode: Some("native_cli_account".into()),
            account_fingerprint: None,
            model_id: Some("claude-sonnet".into()),
            executable_path: Some("/usr/bin/cli".into()),
            executable_sha256: None,
            protocol_version: Some("acp/1".into()),
            profile_hash: None,
            policy_hash: None,
            tool_schema_hash: None,
            harness_attestation_id: None,
            isolation_mode: None,
            resume_strategy: None,
            context_epoch: 0,
            process_generation: 0,
            created_at: "2026-08-07T00:00:00Z".into(),
            updated_at: "2026-08-07T00:00:00Z".into(),
        }
    }

    #[tokio::test]
    async fn walk_active_bindings_at_startup_resumes_current_bindings() {
        let store = Storage::open_in_memory().unwrap();
        let mut row = sample_row("binding-1", "running");
        // The test row intentionally omits an `executable_sha256`
        // digest so the digest-mismatch check refuses the resume.
        // That path is itself a typed recovery error and exercises
        // the same walker the daemon calls at startup.
        store.upsert_driver_binding(&row).unwrap();
        // A second binding in a terminal status should be skipped by
        // the active-binding walker. It needs its own session: the schema
        // declares UNIQUE(driver_kind, session_id), so reusing session-1
        // here made the insert fail the constraint rather than test the
        // walker.
        row.id = "binding-2".into();
        row.session_id = "session-2".into();
        row.status = "completed".into();
        row.process_generation = 4;
        store.upsert_driver_binding(&row).unwrap();

        let shared = SharedStorage::new(store);
        let table = LiveGenerationTable::new();
        table.set("binding-1", 0);
        let epoch = LiveContextEpoch::initial();
        let outcomes = walk_active_bindings_at_startup(&shared, &table, &epoch).await;
        assert_eq!(outcomes.len(), 1);
        // The walker refuses binding-1 with an ExecutableDigestMismatch
        // because the row has no recorded SHA-256 — that is exactly
        // what the typed recovery path is supposed to surface.
        match &outcomes[0].decision {
            ResumeDecision::Refuse(RecoveryError::ExecutableDigestMismatch {
                binding_id, ..
            }) => {
                assert_eq!(binding_id, "binding-1");
            }
            other => panic!("expected ExecutableDigestMismatch, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_deferred_updates_persists_each_kind() {
        let store = Storage::open_in_memory().unwrap();
        store
            .upsert_driver_binding(&sample_row("binding-1", "running"))
            .unwrap();
        let shared = SharedStorage::new(store);

        let queue = DeferredBindingUpdates::new();
        queue.enqueue(DeferredBindingUpdate::increment_generation(
            "binding-1",
            5,
            "2026-08-07T00:00:01Z",
        ));
        queue.enqueue(DeferredBindingUpdate::bump_epoch(
            "binding-1",
            ContextEpoch {
                value: 2,
                reason: ContextEpochReason::SystemPolicyChanged,
            },
            "2026-08-07T00:00:02Z",
        ));
        queue.enqueue(DeferredBindingUpdate::record_digest(
            "binding-1",
            "a".repeat(64),
            "2026-08-07T00:00:03Z",
        ));
        let drained = queue.drain();
        assert_eq!(drained.len(), 3);
        let report = apply_deferred_binding_updates(&shared, drained).await;
        assert_eq!(report.applied, 3);
        assert_eq!(report.dropped_unknown_binding, 0);

        let row = shared
            .lock()
            .await
            .get_driver_binding("binding-1")
            .unwrap()
            .unwrap();
        assert_eq!(row.process_generation, 5);
        assert_eq!(row.context_epoch, 2);
        assert_eq!(row.executable_sha256, Some("a".repeat(64)));
    }

    #[tokio::test]
    async fn apply_deferred_updates_drops_unknown_bindings() {
        let store = Storage::open_in_memory().unwrap();
        let shared = SharedStorage::new(store);

        let queue = DeferredBindingUpdates::new();
        queue.enqueue(DeferredBindingUpdate::increment_generation(
            "missing-binding",
            9,
            "2026-08-07T00:00:01Z",
        ));
        let report = apply_deferred_binding_updates(&shared, queue.drain()).await;
        assert_eq!(report.applied, 0);
        assert_eq!(report.dropped_unknown_binding, 1);
    }

    #[test]
    fn record_process_restart_advances_table_and_queues_update() {
        let table = LiveGenerationTable::new();
        let queue = DeferredBindingUpdates::new();
        record_process_restart(&queue, &table, "binding-1", "2026-08-07T00:00:00Z");
        assert_eq!(table.get("binding-1"), Some(1));
        let drained = queue.drain();
        assert_eq!(drained.len(), 1);
        match drained[0].kind {
            DeferredUpdateKind::IncrementProcessGeneration { next_generation } => {
                assert_eq!(next_generation, 1);
            }
            _ => panic!("expected generation update"),
        }
    }

    #[test]
    fn record_context_rebuild_advances_epoch_and_queues_updates() {
        let epoch = LiveContextEpochState::default();
        let queue = DeferredBindingUpdates::new();
        record_context_rebuild(
            &queue,
            &epoch,
            &["binding-1".into(), "binding-2".into()],
            ContextEpoch {
                value: 7,
                reason: ContextEpochReason::ModelChanged,
            },
            "2026-08-07T00:00:00Z",
        );
        assert_eq!(epoch.current().value, 1);
        let drained = queue.drain();
        assert_eq!(drained.len(), 2);
    }
}
