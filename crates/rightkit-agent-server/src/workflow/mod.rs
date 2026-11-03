//! Daemon-owned autonomous workflow control plane (WP-03 — ADR-0021).
//!
//! WP-03 scope: **supervisor, leased queue, lifecycle, restart recovery**. The
//! modules below sit *behind* the workflow feature gate (see
//! `rightkit_workflow::control_plane::prelude::FeatureGate`) and own nothing but the
//! durable outer workflow DAG. They never touch the chat, the conductor, or
//! any bounded inner executor — those live in WP-04 / WP-05.
//!
//! WP-06 lives alongside in the [`triggers`] submodule: the
//! normalized trigger-event inbox and the typed occurrence admission
//! policy sit *in front of* the supervisor, feeding it admitted
//! occurrences rather than raw webhook payloads.
//!
//! Constraint: every lease, claim, heartbeat, completion, and restart goes
//! through [`RunSupervisor`] which serializes SQLite writes through
//! `tokio::task::spawn_blocking`. The reducer remains pure; this module only
//! drives it.
//!
//! Invariants this module enforces:
//! 1. The lease `fence` is monotonic; reclaiming a stolen lease aborts.
//! 2. Every `JournalAppend` runs in one SQLite transaction with the
//!    `workflow_runs` / `workflow_node_states` / `workflow_run_lease`
//!    projection update (the storage crate's `WorkflowJournalWriter` owns
//!    that atomicity).
//! 3. Restart recovers by re-running the deterministic reducer over the
//!    journal — identical to the live path, so live and recovered state
//!    must match.
//! 4. The feature gate short-circuits `run_once` to `RunOnceOutcome::Empty`
//!    so a misconfigured production daemon never dispatches workflows.

#![allow(dead_code, unused_imports)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::needless_return)]

pub mod continuations;
pub mod cron;
pub mod dispatcher;
pub mod executor;
pub mod executors;
pub mod monitor;
pub mod restart;
pub mod supervisor;
pub mod triggers;

pub use continuations::ContinuationWorker;
pub use cron::CronWorker;
pub use dispatcher::{
    DispatcherError, ExecuteNode, NodeDispatchContext, NodeDispatcher, NodeOutput,
};
pub use executor::{
    ExecutorError, NodeExecutor, PassOutcome, ResourceLimiters, ResourceReservation,
};
pub use executors::{A2aAdapter, A2aExecutionPort, AdapterError, ConductorAdapter, NativeAdapter};
pub use monitor::{InternalMonitorEvent, MonitorEventQueue, MonitorWorker};
pub use restart::{RecoveryOutcome, RecoveryWorker};
pub use supervisor::{
    ClaimOutcome, RunOnceOutcome, RunSupervisor, SupervisorError, SupervisorLeaseTtl,
};
pub use triggers::{
    build_normalized_canonical_json, compute_content_hash, derive_dedupe_key, derive_event_id,
    format_unix_ms_as_iso, parse_iso_as_unix_ms, truncate_bytes, InboxAdmission, InboxAuditRow,
    InboxError, NormalizedData, NormalizedDataError, NormalizedTriggerEvent,
    OccurrenceAdmissionLayer, OccurrenceDecision, OccurrenceError, OccurrenceInputs,
    PersistedEventRow, PersistedOccurrence, RawArtifact, TriggerInbox, TrustFinding, TrustInfo,
    TrustLevel, TrustOrigin, TrustSeverity,
};

use rightkit_workflow::control_plane::prelude::{Clock, FeatureGate};
use std::sync::Arc;

/// Convenience wiring used by the daemon main_runtime when it wants to
/// start the supervisor behind the workflow feature gate. Returns `None`
/// when the gate is disabled so the caller can avoid paying the cost of
/// constructing idle state.
pub fn maybe_build_supervisor(
    storage: crate::api::SharedStorage,
    feature: FeatureGate,
    clock: Arc<dyn Clock>,
) -> Option<Arc<RunSupervisor>> {
    if feature.is_enabled() {
        Some(Arc::new(RunSupervisor::new(storage, feature, clock)))
    } else {
        None
    }
}
