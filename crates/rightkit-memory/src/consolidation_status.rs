//! FM-074 roster-status projection for the daily-analyzer consolidation
//! cycle.
//!
//! `engine/host-app/src/learning/daily_analysis.rs` runs the host app's
//! background consolidation cycle: it turns daily-analyzer review candidates
//! into durable memories and repeated-failure-pattern memories without
//! waiting on manual proposal approval (see `consolidate_daily_memory`).
//! Nothing previously projected *that this ran, when, or what it produced*
//! to any status surface a roster panel, TUI, or API caller could read — the
//! cycle was retained but invisible. This module is that projection: a
//! thread-safe last-run snapshot the consolidation cycle records into after
//! every tick, and any caller (API route, roster/status panel, CLI `status`)
//! can read without coupling to daemon internals.
//!
//! Wiring note: the daemon-owned `DaemonState` (hub file, not edited by this
//! lane) must hold one `ConsolidationStatusState` and hand it to
//! `run_daily_analysis`/`consolidate_daily_memory`; see
//! `docs/canon/daemon-master-canon-atoms.md`
//! for the exact hub-file wiring this lane cannot perform itself.

use serde::Serialize;
use std::sync::{Arc, Mutex};

/// Point-in-time projection of the consolidation cycle's most recent run,
/// plus lifetime counters. Serializable so API/TUI callers can render it
/// directly.
#[derive(Debug, Clone, Serialize, Default, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConsolidationRunStatus {
    pub last_run_started_at: Option<String>,
    pub last_run_completed_at: Option<String>,
    pub last_run_ok: bool,
    pub last_error: Option<String>,
    pub review_candidates_seen: u64,
    pub memories_written: u64,
    pub repeated_failure_patterns: u64,
    pub proposals_minted: u64,
    pub total_runs: u64,
    pub total_memories_written: u64,
}

/// One completed consolidation-cycle tick, as reported by the caller
/// (`run_daily_analysis`). Kept separate from `ConsolidationRunStatus` so the
/// recorder owns lifetime-counter accumulation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConsolidationRunOutcome {
    pub started_at: String,
    pub completed_at: String,
    pub review_candidates_seen: u64,
    pub memories_written: u64,
    pub repeated_failure_patterns: u64,
    pub proposals_minted: u64,
    pub error: Option<String>,
}

/// Thread-safe handle to the consolidation-cycle status projection.
/// Clone-friendly for Axum state, mirroring `MemoryRoutingState`'s shape in
/// `engine/host-app/src/memory.rs`.
#[derive(Clone, Default)]
pub struct ConsolidationStatusState {
    inner: Arc<Mutex<ConsolidationRunStatus>>,
}

impl ConsolidationStatusState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one completed consolidation-cycle run. Called once per tick
    /// from `run_daily_analysis`, whether or not the run produced an error.
    pub fn record_run(&self, outcome: ConsolidationRunOutcome) {
        if let Ok(mut status) = self.inner.lock() {
            status.last_run_started_at = Some(outcome.started_at);
            status.last_run_completed_at = Some(outcome.completed_at);
            status.last_run_ok = outcome.error.is_none();
            status.last_error = outcome.error;
            status.review_candidates_seen = outcome.review_candidates_seen;
            status.memories_written = outcome.memories_written;
            status.repeated_failure_patterns = outcome.repeated_failure_patterns;
            status.proposals_minted = outcome.proposals_minted;
            status.total_runs = status.total_runs.saturating_add(1);
            status.total_memories_written = status
                .total_memories_written
                .saturating_add(outcome.memories_written);
        }
    }

    /// Read the current projection. Safe to call before any run has
    /// completed — returns the `Default` (all-empty) snapshot.
    pub fn snapshot(&self) -> ConsolidationRunStatus {
        self.inner
            .lock()
            .map(|status| status.clone())
            .unwrap_or_default()
    }
}
