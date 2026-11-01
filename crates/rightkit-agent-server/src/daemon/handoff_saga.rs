//! PL-008 — orchestrator-side root-controller handoff saga.
//!
//! `rightkit_agent_engine_storage::handoff` (tagged PL-008) owns three durable
//! checkpoints per session — `quiescing` -> `source_quiesced` -> `resumed` ->
//! a terminal phase — gated by a strictly-increasing `controller_epoch`
//! (compare-and-swap). This module is the missing orchestrator half: it
//! drives the finer-grained saga steps the canon requires (claim, fence
//! intake, preflight target, drain/interrupt, stop source, resume, publish
//! handle+generation) around those checkpoints, and it is restart-resumable
//! because every re-entry re-derives its next action from the durable
//! checkpoint alone — never from saga state kept only in memory.
//!
//! # Distinct from FM-167 / ENG-66
//!
//! `rightkit_agent_tools::coding_tools::handoff::HandoffController` fences a *different*
//! mechanism: client-surface session continuity across `Local`/`Cloud`/
//! `Computer` execution targets (FM-167). The canon amendment at the bottom
//! of `docs/canon/daemon-master-canon-atoms.md`'s pre-lock section is
//! explicit that FM-167 "remains distinct from PL-008 root-controller
//! replacement" — so this module owns its own intake fence ([`IntakeFence`])
//! rather than reusing that registry, and the two are never allowed to
//! observe each other's state.
//!
//! # Proven-quiescence gating (AO-04)
//!
//! [`evaluate_quiescence`] never returns [`QuiescenceOutcome::ReadyToHandoff`]
//! unless the caller has actually observed the session
//! ([`QuiescenceSignals::observed`]); an unobserved session yields
//! [`QuiescenceOutcome::Unverified`], and [`start`] refuses to touch the
//! source runtime for any outcome other than `ReadyToHandoff`. This is what
//! makes "untouched source when unproven" true structurally rather than by
//! caller discipline.
//!
//! # Rollback-safe cancellation
//!
//! A preflight/drain/stop callback failure after `begin()` has durably
//! recorded the `quiescing` checkpoint is rolled back through
//! [`HandoffStore::finish`] with [`HandoffTerminalState::Cancelled`] and the
//! intake fence is released — the saga never leaves a durable row stuck in a
//! non-terminal phase after a caller-visible refusal.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use rightkit_agent_engine_storage::{
    HandoffAdvance, HandoffRow, HandoffTerminalState, QuiescenceOutcome,
};

/// Durable checkpoint operations the saga drives. Implemented for
/// [`rightkit_agent_engine_storage::Storage`] in production; unit tests below use an
/// in-memory fake so saga-phase logic is verifiable without a database.
pub trait HandoffStore {
    fn state(&self, session_id: &str) -> Result<Option<HandoffRow>, String>;
    fn begin(&self, row: HandoffRow) -> Result<HandoffAdvance, String>;
    fn quiesce(&self, session_id: &str, epoch: u64, now: &str) -> Result<HandoffAdvance, String>;
    fn resume(
        &self,
        session_id: &str,
        epoch: u64,
        cursor: u64,
        now: &str,
    ) -> Result<HandoffAdvance, String>;
    fn finish(
        &self,
        session_id: &str,
        epoch: u64,
        terminal: HandoffTerminalState,
        detail: Option<&str>,
        now: &str,
    ) -> Result<HandoffAdvance, String>;
}

impl HandoffStore for rightkit_agent_engine_storage::Storage {
    fn state(&self, session_id: &str) -> Result<Option<HandoffRow>, String> {
        self.handoff_state(session_id)
            .map_err(|error| error.to_string())
    }
    fn begin(&self, row: HandoffRow) -> Result<HandoffAdvance, String> {
        self.begin_handoff(row).map_err(|error| error.to_string())
    }
    fn quiesce(&self, session_id: &str, epoch: u64, now: &str) -> Result<HandoffAdvance, String> {
        self.quiesce_handoff(session_id, epoch, now)
            .map_err(|error| error.to_string())
    }
    fn resume(
        &self,
        session_id: &str,
        epoch: u64,
        cursor: u64,
        now: &str,
    ) -> Result<HandoffAdvance, String> {
        self.resume_handoff(session_id, epoch, cursor, now)
            .map_err(|error| error.to_string())
    }
    fn finish(
        &self,
        session_id: &str,
        epoch: u64,
        terminal: HandoffTerminalState,
        detail: Option<&str>,
        now: &str,
    ) -> Result<HandoffAdvance, String> {
        self.finish_handoff(session_id, epoch, terminal, detail, now)
            .map_err(|error| error.to_string())
    }
}

/// Side-effecting saga steps around the durable checkpoints. Callbacks must
/// be idempotent: a restart re-entering mid-saga replays whichever of these
/// were not yet durably checkpointed.
pub trait HandoffCallbacks {
    fn preflight_target(
        &self,
        session_id: &str,
        source: &str,
        destination: &str,
    ) -> Result<(), String>;
    fn drain_source(&self, session_id: &str) -> Result<(), String>;
    fn stop_source(&self, session_id: &str) -> Result<(), String>;
}

/// The published handle+generation the destination runtime resumes with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffHandle {
    pub session_id: String,
    pub controller_epoch: u64,
    pub source_target: String,
    pub destination_target: String,
    pub phase: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SagaOutcome {
    Published(HandoffHandle),
    Refused(SagaRefusal),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SagaRefusal {
    /// Quiescence was not proven `ReadyToHandoff`; the source was never
    /// touched.
    QuiescenceUnproven(QuiescenceOutcome),
    /// This module's own PL-008 intake fence is already held for this
    /// session by another in-flight attempt.
    IntakeAlreadyFenced,
    ActiveHandoff,
    StaleEpoch,
    InvalidPhase,
    Missing,
    PreflightFailed(String),
    DrainFailed(String),
    StopFailed(String),
    StorageError(String),
}

/// PL-008's own intake fence, deliberately separate from FM-167's
/// `HandoffController` registry (see module docs). Held for the duration of
/// one saga attempt; a session with no fence entry accepts new daemon
/// intake normally.
#[derive(Default)]
pub struct IntakeFence {
    inflight: Mutex<HashMap<String, u64>>,
}

impl IntakeFence {
    pub fn new() -> Self {
        Self::default()
    }

    /// True while a claim is held for `session_id`, at any epoch.
    pub fn is_fenced(&self, session_id: &str) -> bool {
        self.inflight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(session_id)
    }

    fn claim(&self, session_id: &str, epoch: u64) -> Result<(), SagaRefusal> {
        let mut map = self
            .inflight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if map.contains_key(session_id) {
            return Err(SagaRefusal::IntakeAlreadyFenced);
        }
        map.insert(session_id.to_string(), epoch);
        Ok(())
    }

    fn release(&self, session_id: &str, epoch: u64) {
        let mut map = self
            .inflight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if map.get(session_id) == Some(&epoch) {
            map.remove(session_id);
        }
    }
}

static GLOBAL_INTAKE_FENCE: OnceLock<IntakeFence> = OnceLock::new();

/// The process-wide PL-008 intake fence used by the live daemon route.
pub fn global_intake_fence() -> &'static IntakeFence {
    GLOBAL_INTAKE_FENCE.get_or_init(IntakeFence::new)
}

/// Signals used to compute [`QuiescenceOutcome`]. `observed` gates every
/// other check: an un-observed session can never be reported ready.
pub trait QuiescenceSignals {
    fn observed(&self) -> bool;
    fn has_pending_decision(&self) -> bool;
    fn has_draft(&self) -> bool;
    fn has_active_work(&self) -> bool;
}

/// Proven-quiescence gating (AO-04). Precedence when multiple signals are
/// true: an operator decision blocks handoff before an unsent draft, which
/// blocks before ordinary active work — the most human-blocking condition is
/// reported first so a caller surfaces the right refusal.
pub fn evaluate_quiescence(signals: &impl QuiescenceSignals) -> QuiescenceOutcome {
    if !signals.observed() {
        return QuiescenceOutcome::Unverified;
    }
    if signals.has_pending_decision() {
        return QuiescenceOutcome::DecisionPending;
    }
    if signals.has_draft() {
        return QuiescenceOutcome::DraftPresent;
    }
    if signals.has_active_work() {
        return QuiescenceOutcome::ActiveWork;
    }
    QuiescenceOutcome::ReadyToHandoff
}

fn is_terminal(phase: &str) -> bool {
    matches!(phase, "completed" | "cancelled" | "failed")
}

fn advance_refusal(advance: HandoffAdvance) -> SagaRefusal {
    match advance {
        HandoffAdvance::Applied(_) => {
            unreachable!("advance_refusal is only called on a non-Applied outcome")
        }
        HandoffAdvance::Missing => SagaRefusal::Missing,
        HandoffAdvance::StaleEpoch => SagaRefusal::StaleEpoch,
        HandoffAdvance::InvalidPhase => SagaRefusal::InvalidPhase,
        HandoffAdvance::ActiveHandoff => SagaRefusal::ActiveHandoff,
    }
}

fn cancel<S: HandoffStore>(
    store: &S,
    fence: &IntakeFence,
    session_id: &str,
    epoch: u64,
    detail: &str,
    now: &str,
) {
    // Best-effort: if this also fails the row is left non-terminal, but the
    // fence release below still restores intake so a later attempt is not
    // starved by this one's failure.
    let _ = store.finish(
        session_id,
        epoch,
        HandoffTerminalState::Cancelled,
        Some(detail),
        now,
    );
    fence.release(session_id, epoch);
}

/// Claim a brand-new saga attempt. Proven-quiescence gating happens before
/// anything durable or in-process is touched: any outcome other than
/// `ReadyToHandoff` refuses without claiming the fence or writing storage.
#[allow(clippy::too_many_arguments)]
pub fn start<S: HandoffStore, C: HandoffCallbacks>(
    store: &S,
    fence: &IntakeFence,
    callbacks: &C,
    session_id: &str,
    source_target: &str,
    destination_target: &str,
    epoch: u64,
    quiescence: QuiescenceOutcome,
    now: &str,
) -> SagaOutcome {
    if quiescence != QuiescenceOutcome::ReadyToHandoff {
        return SagaOutcome::Refused(SagaRefusal::QuiescenceUnproven(quiescence));
    }
    if let Err(refusal) = fence.claim(session_id, epoch) {
        return SagaOutcome::Refused(refusal);
    }

    let row = HandoffRow {
        session_id: session_id.to_string(),
        controller_epoch: epoch,
        source_target: source_target.to_string(),
        destination_target: destination_target.to_string(),
        phase: "quiescing".to_string(),
        replay_cursor: None,
        terminal_detail: None,
        created_at: now.to_string(),
        updated_at: now.to_string(),
    };
    let advance = match store.begin(row) {
        Ok(advance) => advance,
        Err(error) => {
            fence.release(session_id, epoch);
            return SagaOutcome::Refused(SagaRefusal::StorageError(error));
        }
    };
    let applied = match advance {
        HandoffAdvance::Applied(row) => row,
        other => {
            fence.release(session_id, epoch);
            return SagaOutcome::Refused(advance_refusal(other));
        }
    };
    drive_from_quiescing(store, fence, callbacks, applied, now)
}

/// Restart-resumable re-entry. Reads the durable checkpoint and drives
/// forward from there; never trusts any in-memory saga state. Callbacks are
/// replayed for a `quiescing` restart because their effects (drain/stop) are
/// not themselves durable — only the checkpoint transition is.
pub fn resume_in_progress<S: HandoffStore, C: HandoffCallbacks>(
    store: &S,
    fence: &IntakeFence,
    callbacks: &C,
    session_id: &str,
    now: &str,
) -> SagaOutcome {
    let row = match store.state(session_id) {
        Ok(Some(row)) => row,
        Ok(None) => return SagaOutcome::Refused(SagaRefusal::Missing),
        Err(error) => return SagaOutcome::Refused(SagaRefusal::StorageError(error)),
    };
    if is_terminal(&row.phase) {
        return SagaOutcome::Refused(SagaRefusal::InvalidPhase);
    }
    if fence.claim(session_id, row.controller_epoch).is_err() {
        // A still-live in-process attempt already owns this exact epoch;
        // avoid double-driving the same saga concurrently.
        return SagaOutcome::Refused(SagaRefusal::IntakeAlreadyFenced);
    }
    match row.phase.as_str() {
        "quiescing" => drive_from_quiescing(store, fence, callbacks, row, now),
        "source_quiesced" => drive_from_source_quiesced(store, fence, row, now),
        "resumed" => drive_from_resumed(store, fence, row, now),
        _ => {
            fence.release(session_id, row.controller_epoch);
            SagaOutcome::Refused(SagaRefusal::InvalidPhase)
        }
    }
}

fn drive_from_quiescing<S: HandoffStore, C: HandoffCallbacks>(
    store: &S,
    fence: &IntakeFence,
    callbacks: &C,
    row: HandoffRow,
    now: &str,
) -> SagaOutcome {
    let session_id = row.session_id.clone();
    let epoch = row.controller_epoch;

    if let Err(error) =
        callbacks.preflight_target(&session_id, &row.source_target, &row.destination_target)
    {
        cancel(
            store,
            fence,
            &session_id,
            epoch,
            &format!("preflight failed: {error}"),
            now,
        );
        return SagaOutcome::Refused(SagaRefusal::PreflightFailed(error));
    }
    if let Err(error) = callbacks.drain_source(&session_id) {
        cancel(
            store,
            fence,
            &session_id,
            epoch,
            &format!("drain failed: {error}"),
            now,
        );
        return SagaOutcome::Refused(SagaRefusal::DrainFailed(error));
    }
    if let Err(error) = callbacks.stop_source(&session_id) {
        cancel(
            store,
            fence,
            &session_id,
            epoch,
            &format!("stop failed: {error}"),
            now,
        );
        return SagaOutcome::Refused(SagaRefusal::StopFailed(error));
    }

    match store.quiesce(&session_id, epoch, now) {
        Ok(HandoffAdvance::Applied(row)) => drive_from_source_quiesced(store, fence, row, now),
        Ok(other) => {
            fence.release(&session_id, epoch);
            SagaOutcome::Refused(advance_refusal(other))
        }
        Err(error) => {
            fence.release(&session_id, epoch);
            SagaOutcome::Refused(SagaRefusal::StorageError(error))
        }
    }
}

fn drive_from_source_quiesced<S: HandoffStore>(
    store: &S,
    fence: &IntakeFence,
    row: HandoffRow,
    now: &str,
) -> SagaOutcome {
    let session_id = row.session_id.clone();
    let epoch = row.controller_epoch;
    match store.resume(&session_id, epoch, 0, now) {
        Ok(HandoffAdvance::Applied(row)) => drive_from_resumed(store, fence, row, now),
        Ok(other) => {
            fence.release(&session_id, epoch);
            SagaOutcome::Refused(advance_refusal(other))
        }
        Err(error) => {
            fence.release(&session_id, epoch);
            SagaOutcome::Refused(SagaRefusal::StorageError(error))
        }
    }
}

fn drive_from_resumed<S: HandoffStore>(
    store: &S,
    fence: &IntakeFence,
    row: HandoffRow,
    now: &str,
) -> SagaOutcome {
    let session_id = row.session_id.clone();
    let epoch = row.controller_epoch;
    match store.finish(
        &session_id,
        epoch,
        HandoffTerminalState::Completed,
        None,
        now,
    ) {
        Ok(HandoffAdvance::Applied(final_row)) => {
            fence.release(&session_id, epoch);
            SagaOutcome::Published(HandoffHandle {
                session_id: final_row.session_id,
                controller_epoch: final_row.controller_epoch,
                source_target: final_row.source_target,
                destination_target: final_row.destination_target,
                phase: final_row.phase,
            })
        }
        Ok(other) => {
            fence.release(&session_id, epoch);
            SagaOutcome::Refused(advance_refusal(other))
        }
        Err(error) => {
            fence.release(&session_id, epoch);
            SagaOutcome::Refused(SagaRefusal::StorageError(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct FakeStore {
        rows: RefCell<HashMap<String, HandoffRow>>,
    }

    impl FakeStore {
        fn seed(&self, row: HandoffRow) {
            self.rows.borrow_mut().insert(row.session_id.clone(), row);
        }

        fn transition(
            &self,
            session_id: &str,
            epoch: u64,
            expected: &str,
            next: &str,
            now: &str,
        ) -> Result<HandoffAdvance, String> {
            let mut rows = self.rows.borrow_mut();
            let Some(existing) = rows.get(session_id).cloned() else {
                return Ok(HandoffAdvance::Missing);
            };
            if existing.controller_epoch != epoch {
                return Ok(HandoffAdvance::StaleEpoch);
            }
            if existing.phase != expected {
                return Ok(HandoffAdvance::InvalidPhase);
            }
            let mut updated = existing;
            updated.phase = next.to_string();
            updated.updated_at = now.to_string();
            rows.insert(session_id.to_string(), updated.clone());
            Ok(HandoffAdvance::Applied(updated))
        }
    }

    impl HandoffStore for FakeStore {
        fn state(&self, session_id: &str) -> Result<Option<HandoffRow>, String> {
            Ok(self.rows.borrow().get(session_id).cloned())
        }
        fn begin(&self, row: HandoffRow) -> Result<HandoffAdvance, String> {
            let mut rows = self.rows.borrow_mut();
            if let Some(existing) = rows.get(&row.session_id) {
                if !is_terminal(&existing.phase) {
                    return Ok(HandoffAdvance::ActiveHandoff);
                }
                if row.controller_epoch <= existing.controller_epoch {
                    return Ok(HandoffAdvance::StaleEpoch);
                }
            }
            rows.insert(row.session_id.clone(), row.clone());
            Ok(HandoffAdvance::Applied(row))
        }
        fn quiesce(
            &self,
            session_id: &str,
            epoch: u64,
            now: &str,
        ) -> Result<HandoffAdvance, String> {
            self.transition(session_id, epoch, "quiescing", "source_quiesced", now)
        }
        fn resume(
            &self,
            session_id: &str,
            epoch: u64,
            _cursor: u64,
            now: &str,
        ) -> Result<HandoffAdvance, String> {
            self.transition(session_id, epoch, "source_quiesced", "resumed", now)
        }
        fn finish(
            &self,
            session_id: &str,
            epoch: u64,
            terminal: HandoffTerminalState,
            detail: Option<&str>,
            now: &str,
        ) -> Result<HandoffAdvance, String> {
            let next = match terminal {
                HandoffTerminalState::Completed => "completed",
                HandoffTerminalState::Cancelled => "cancelled",
                HandoffTerminalState::Failed => "failed",
            };
            let mut rows = self.rows.borrow_mut();
            let Some(existing) = rows.get(session_id).cloned() else {
                return Ok(HandoffAdvance::Missing);
            };
            if existing.controller_epoch != epoch {
                return Ok(HandoffAdvance::StaleEpoch);
            }
            if is_terminal(&existing.phase) {
                return Ok(HandoffAdvance::InvalidPhase);
            }
            let mut updated = existing;
            updated.phase = next.to_string();
            updated.terminal_detail = detail.map(str::to_string);
            updated.updated_at = now.to_string();
            rows.insert(session_id.to_string(), updated.clone());
            Ok(HandoffAdvance::Applied(updated))
        }
    }

    #[derive(Default)]
    struct RecordingCallbacks {
        preflight_err: Option<String>,
        drain_err: Option<String>,
        stop_err: Option<String>,
        calls: RefCell<Vec<&'static str>>,
    }

    impl HandoffCallbacks for RecordingCallbacks {
        fn preflight_target(&self, _s: &str, _src: &str, _dst: &str) -> Result<(), String> {
            self.calls.borrow_mut().push("preflight");
            match &self.preflight_err {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }
        fn drain_source(&self, _s: &str) -> Result<(), String> {
            self.calls.borrow_mut().push("drain");
            match &self.drain_err {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }
        fn stop_source(&self, _s: &str) -> Result<(), String> {
            self.calls.borrow_mut().push("stop");
            match &self.stop_err {
                Some(error) => Err(error.clone()),
                None => Ok(()),
            }
        }
    }

    struct FakeSignals {
        observed: bool,
        decision: bool,
        draft: bool,
        active: bool,
    }

    impl QuiescenceSignals for FakeSignals {
        fn observed(&self) -> bool {
            self.observed
        }
        fn has_pending_decision(&self) -> bool {
            self.decision
        }
        fn has_draft(&self) -> bool {
            self.draft
        }
        fn has_active_work(&self) -> bool {
            self.active
        }
    }

    // --- quiescence outcomes (AO-04) ---

    #[test]
    fn quiescence_unverified_when_never_observed() {
        let signals = FakeSignals {
            observed: false,
            decision: true,
            draft: true,
            active: true,
        };
        assert_eq!(evaluate_quiescence(&signals), QuiescenceOutcome::Unverified);
    }

    #[test]
    fn quiescence_decision_pending_outranks_draft_and_active() {
        let signals = FakeSignals {
            observed: true,
            decision: true,
            draft: true,
            active: true,
        };
        assert_eq!(
            evaluate_quiescence(&signals),
            QuiescenceOutcome::DecisionPending
        );
    }

    #[test]
    fn quiescence_draft_present_outranks_active_work() {
        let signals = FakeSignals {
            observed: true,
            decision: false,
            draft: true,
            active: true,
        };
        assert_eq!(
            evaluate_quiescence(&signals),
            QuiescenceOutcome::DraftPresent
        );
    }

    #[test]
    fn quiescence_active_work_when_only_active() {
        let signals = FakeSignals {
            observed: true,
            decision: false,
            draft: false,
            active: true,
        };
        assert_eq!(evaluate_quiescence(&signals), QuiescenceOutcome::ActiveWork);
    }

    #[test]
    fn quiescence_ready_when_all_clear() {
        let signals = FakeSignals {
            observed: true,
            decision: false,
            draft: false,
            active: false,
        };
        assert_eq!(
            evaluate_quiescence(&signals),
            QuiescenceOutcome::ReadyToHandoff
        );
    }

    // --- saga phase ordering / publish ---

    #[test]
    fn unproven_quiescence_leaves_storage_and_fence_untouched() {
        let store = FakeStore::default();
        let fence = IntakeFence::new();
        let callbacks = RecordingCallbacks::default();
        let outcome = start(
            &store,
            &fence,
            &callbacks,
            "s1",
            "native",
            "claude",
            1,
            QuiescenceOutcome::DecisionPending,
            "t0",
        );
        assert_eq!(
            outcome,
            SagaOutcome::Refused(SagaRefusal::QuiescenceUnproven(
                QuiescenceOutcome::DecisionPending
            ))
        );
        assert!(store.state("s1").unwrap().is_none());
        assert!(!fence.is_fenced("s1"));
        assert!(callbacks.calls.borrow().is_empty());
    }

    #[test]
    fn full_saga_runs_phases_in_order_and_publishes() {
        let store = FakeStore::default();
        let fence = IntakeFence::new();
        let callbacks = RecordingCallbacks::default();
        let outcome = start(
            &store,
            &fence,
            &callbacks,
            "s1",
            "native",
            "claude",
            1,
            QuiescenceOutcome::ReadyToHandoff,
            "t0",
        );
        match outcome {
            SagaOutcome::Published(handle) => {
                assert_eq!(handle.session_id, "s1");
                assert_eq!(handle.controller_epoch, 1);
                assert_eq!(handle.source_target, "native");
                assert_eq!(handle.destination_target, "claude");
                assert_eq!(handle.phase, "completed");
            }
            other => panic!("expected Published, got {other:?}"),
        }
        assert_eq!(
            *callbacks.calls.borrow(),
            vec!["preflight", "drain", "stop"]
        );
        assert!(!fence.is_fenced("s1"), "fence must release after publish");
        assert_eq!(store.state("s1").unwrap().unwrap().phase, "completed");
    }

    #[test]
    fn concurrent_claim_is_refused_while_fenced() {
        let store = FakeStore::default();
        let fence = IntakeFence::new();
        fence.claim("s1", 99).unwrap();
        let callbacks = RecordingCallbacks::default();
        let outcome = start(
            &store,
            &fence,
            &callbacks,
            "s1",
            "native",
            "claude",
            1,
            QuiescenceOutcome::ReadyToHandoff,
            "t0",
        );
        assert_eq!(
            outcome,
            SagaOutcome::Refused(SagaRefusal::IntakeAlreadyFenced)
        );
        assert!(callbacks.calls.borrow().is_empty());
    }

    #[test]
    fn stale_epoch_against_a_terminal_row_is_rejected() {
        let store = FakeStore::default();
        store.seed(HandoffRow {
            session_id: "s1".into(),
            controller_epoch: 5,
            source_target: "native".into(),
            destination_target: "claude".into(),
            phase: "completed".into(),
            replay_cursor: None,
            terminal_detail: None,
            created_at: "t0".into(),
            updated_at: "t0".into(),
        });
        let fence = IntakeFence::new();
        let callbacks = RecordingCallbacks::default();
        let outcome = start(
            &store,
            &fence,
            &callbacks,
            "s1",
            "native",
            "claude",
            3,
            QuiescenceOutcome::ReadyToHandoff,
            "t1",
        );
        assert_eq!(outcome, SagaOutcome::Refused(SagaRefusal::StaleEpoch));
        assert!(
            !fence.is_fenced("s1"),
            "a rejected claim must release the fence"
        );
    }

    #[test]
    fn active_handoff_is_rejected_without_disturbing_the_existing_row() {
        let store = FakeStore::default();
        store.seed(HandoffRow {
            session_id: "s1".into(),
            controller_epoch: 5,
            source_target: "native".into(),
            destination_target: "claude".into(),
            phase: "quiescing".into(),
            replay_cursor: None,
            terminal_detail: None,
            created_at: "t0".into(),
            updated_at: "t0".into(),
        });
        let fence = IntakeFence::new();
        let callbacks = RecordingCallbacks::default();
        let outcome = start(
            &store,
            &fence,
            &callbacks,
            "s1",
            "native",
            "claude",
            9,
            QuiescenceOutcome::ReadyToHandoff,
            "t1",
        );
        assert_eq!(outcome, SagaOutcome::Refused(SagaRefusal::ActiveHandoff));
        assert_eq!(store.state("s1").unwrap().unwrap().controller_epoch, 5);
    }

    #[test]
    fn drain_failure_rolls_back_to_cancelled_and_releases_fence() {
        let store = FakeStore::default();
        let fence = IntakeFence::new();
        let callbacks = RecordingCallbacks {
            drain_err: Some("worker wedged".to_string()),
            ..Default::default()
        };
        let outcome = start(
            &store,
            &fence,
            &callbacks,
            "s1",
            "native",
            "claude",
            1,
            QuiescenceOutcome::ReadyToHandoff,
            "t0",
        );
        assert_eq!(
            outcome,
            SagaOutcome::Refused(SagaRefusal::DrainFailed("worker wedged".to_string()))
        );
        assert_eq!(*callbacks.calls.borrow(), vec!["preflight", "drain"]);
        assert_eq!(store.state("s1").unwrap().unwrap().phase, "cancelled");
        assert!(!fence.is_fenced("s1"));
    }

    #[test]
    fn preflight_failure_never_reaches_drain_or_stop() {
        let store = FakeStore::default();
        let fence = IntakeFence::new();
        let callbacks = RecordingCallbacks {
            preflight_err: Some("target unreachable".to_string()),
            ..Default::default()
        };
        let outcome = start(
            &store,
            &fence,
            &callbacks,
            "s1",
            "native",
            "claude",
            1,
            QuiescenceOutcome::ReadyToHandoff,
            "t0",
        );
        assert_eq!(
            outcome,
            SagaOutcome::Refused(SagaRefusal::PreflightFailed(
                "target unreachable".to_string()
            ))
        );
        assert_eq!(*callbacks.calls.borrow(), vec!["preflight"]);
        assert_eq!(store.state("s1").unwrap().unwrap().phase, "cancelled");
    }

    #[test]
    fn stop_failure_after_a_successful_drain_still_cancels_cleanly() {
        let store = FakeStore::default();
        let fence = IntakeFence::new();
        let callbacks = RecordingCallbacks {
            stop_err: Some("stop timed out".to_string()),
            ..Default::default()
        };
        let outcome = start(
            &store,
            &fence,
            &callbacks,
            "s1",
            "native",
            "claude",
            1,
            QuiescenceOutcome::ReadyToHandoff,
            "t0",
        );
        assert_eq!(
            outcome,
            SagaOutcome::Refused(SagaRefusal::StopFailed("stop timed out".to_string()))
        );
        assert_eq!(
            *callbacks.calls.borrow(),
            vec!["preflight", "drain", "stop"]
        );
        assert_eq!(store.state("s1").unwrap().unwrap().phase, "cancelled");
        assert!(!fence.is_fenced("s1"));
    }

    // --- restart-resumable phases ---

    #[test]
    fn restart_resume_from_quiescing_replays_callbacks_and_completes() {
        let store = FakeStore::default();
        store.seed(HandoffRow {
            session_id: "s1".into(),
            controller_epoch: 7,
            source_target: "native".into(),
            destination_target: "claude".into(),
            phase: "quiescing".into(),
            replay_cursor: None,
            terminal_detail: None,
            created_at: "t0".into(),
            updated_at: "t0".into(),
        });
        let fence = IntakeFence::new();
        let callbacks = RecordingCallbacks::default();
        let outcome = resume_in_progress(&store, &fence, &callbacks, "s1", "t1");
        match outcome {
            SagaOutcome::Published(handle) => assert_eq!(handle.phase, "completed"),
            other => panic!("expected Published, got {other:?}"),
        }
        assert_eq!(
            *callbacks.calls.borrow(),
            vec!["preflight", "drain", "stop"]
        );
        assert!(!fence.is_fenced("s1"));
    }

    #[test]
    fn restart_resume_from_source_quiesced_skips_callbacks() {
        let store = FakeStore::default();
        store.seed(HandoffRow {
            session_id: "s1".into(),
            controller_epoch: 7,
            source_target: "native".into(),
            destination_target: "claude".into(),
            phase: "source_quiesced".into(),
            replay_cursor: None,
            terminal_detail: None,
            created_at: "t0".into(),
            updated_at: "t0".into(),
        });
        let fence = IntakeFence::new();
        let callbacks = RecordingCallbacks::default();
        let outcome = resume_in_progress(&store, &fence, &callbacks, "s1", "t1");
        match outcome {
            SagaOutcome::Published(handle) => assert_eq!(handle.phase, "completed"),
            other => panic!("expected Published, got {other:?}"),
        }
        assert!(
            callbacks.calls.borrow().is_empty(),
            "a checkpoint past quiescing must not replay preflight/drain/stop"
        );
    }

    #[test]
    fn restart_resume_from_resumed_only_finishes() {
        let store = FakeStore::default();
        store.seed(HandoffRow {
            session_id: "s1".into(),
            controller_epoch: 7,
            source_target: "native".into(),
            destination_target: "claude".into(),
            phase: "resumed".into(),
            replay_cursor: Some(0),
            terminal_detail: None,
            created_at: "t0".into(),
            updated_at: "t0".into(),
        });
        let fence = IntakeFence::new();
        let callbacks = RecordingCallbacks::default();
        let outcome = resume_in_progress(&store, &fence, &callbacks, "s1", "t1");
        match outcome {
            SagaOutcome::Published(handle) => assert_eq!(handle.phase, "completed"),
            other => panic!("expected Published, got {other:?}"),
        }
        assert!(callbacks.calls.borrow().is_empty());
    }

    #[test]
    fn restart_resume_on_missing_session_refuses() {
        let store = FakeStore::default();
        let fence = IntakeFence::new();
        let callbacks = RecordingCallbacks::default();
        let outcome = resume_in_progress(&store, &fence, &callbacks, "ghost", "t1");
        assert_eq!(outcome, SagaOutcome::Refused(SagaRefusal::Missing));
        assert!(!fence.is_fenced("ghost"));
    }

    #[test]
    fn restart_resume_on_terminal_phase_refuses_without_claiming_fence() {
        let store = FakeStore::default();
        store.seed(HandoffRow {
            session_id: "s1".into(),
            controller_epoch: 7,
            source_target: "native".into(),
            destination_target: "claude".into(),
            phase: "completed".into(),
            replay_cursor: None,
            terminal_detail: None,
            created_at: "t0".into(),
            updated_at: "t0".into(),
        });
        let fence = IntakeFence::new();
        let callbacks = RecordingCallbacks::default();
        let outcome = resume_in_progress(&store, &fence, &callbacks, "s1", "t1");
        assert_eq!(outcome, SagaOutcome::Refused(SagaRefusal::InvalidPhase));
        assert!(!fence.is_fenced("s1"));
    }

    #[test]
    fn epoch_must_strictly_increase_across_two_full_sagas_on_one_session() {
        let store = FakeStore::default();
        let fence = IntakeFence::new();
        let callbacks = RecordingCallbacks::default();
        let first = start(
            &store,
            &fence,
            &callbacks,
            "s1",
            "native",
            "claude",
            1,
            QuiescenceOutcome::ReadyToHandoff,
            "t0",
        );
        assert!(matches!(first, SagaOutcome::Published(_)));

        // Replaying the same epoch after completion must be refused, never
        // silently re-applied (idempotent delivery without double effects).
        let replay = start(
            &store,
            &fence,
            &callbacks,
            "s1",
            "native",
            "claude",
            1,
            QuiescenceOutcome::ReadyToHandoff,
            "t2",
        );
        assert_eq!(replay, SagaOutcome::Refused(SagaRefusal::StaleEpoch));

        let second = start(
            &store,
            &fence,
            &callbacks,
            "s1",
            "claude",
            "native",
            2,
            QuiescenceOutcome::ReadyToHandoff,
            "t3",
        );
        match second {
            SagaOutcome::Published(handle) => assert_eq!(handle.controller_epoch, 2),
            other => panic!("expected Published, got {other:?}"),
        }
    }
}
