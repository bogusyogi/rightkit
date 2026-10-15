//! PTY-002 input lease: epoch fencing plus monotonic, acknowledged input
//! sequence so a stale controller can never inject input.
//!
//! Contract (CodeRight canon PTY-002): "Input ownership uses a fenced lease
//! epoch plus monotonic input sequence and acknowledgement; stale-lease writes
//! are rejected, and reconnect explicitly reconciles or rejects
//! unacknowledged bytes rather than replaying them ambiguously." Shape follows
//! CodeRight `ProcessLeaseSnapshot` / `UnacknowledgedInputDecision`
//! (`engine/crates/tools/src/background_shell/process_model.rs`): attach bumps
//! the epoch; a stale epoch is rejected; unacknowledged input is reconciled
//! before new input.
//!
//! Sequence model (one global sequence per lease, continuing across epochs):
//! - `next_input_sequence` is the only sequence accepted as new input.
//! - every sequence `< acked_input_sequence` was delivered and acknowledged.
//! - `[acked_input_sequence, next_input_sequence)` is the unacknowledged
//!   range: input whose delivery is ambiguous (executor failed or panicked
//!   after it may have produced a side effect). While it is non-empty, new
//!   input and plain `acquire` are rejected until the holder or a new
//!   controller explicitly reconciles it.
//!
//! Input rules for a submit `(epoch, sequence)`:
//! 1. lease not attached → rejected (`NotAttached`).
//! 2. `epoch < current` → rejected (`StaleEpoch`): the holder was fenced.
//!    `epoch > current` → rejected (`FutureEpoch`).
//! 3. `sequence < acked_input_sequence` → deduplicated: acknowledged again,
//!    not executed (`InputOutcome::Duplicate`).
//! 4. `sequence` inside the unacknowledged range, or any new sequence while
//!    that range is non-empty → rejected (`UnacknowledgedInput`).
//! 5. `sequence > next_input_sequence` → rejected (`OutOfOrder`); no gap
//!    buffering.
//! 6. `sequence == next_input_sequence` → executed exactly once while the
//!    lease is held, then acknowledged.
//!
//! Submissions are serialized: the lease lock is held across the executor, so
//! an acquisition that fences a holder waits for its in-flight input to
//! settle. Executors must not call back into the same lease.
//!
//! Execution state: every lease carries an [`ExecutionIdentity`] and an
//! [`ExecutionState`]; new input is accepted only while the execution is
//! `Running` (in-memory leases start `Running`, so behaviour is unchanged).
//!
//! Durability (optional, [`InputLease::open`]): every state change is saved
//! to a [`LeaseStore`] before it takes effect. An epoch is only granted after
//! it is durable, and an input sequence is reserved durably before its
//! executor runs, so a crash at any point leaves either the old state or an
//! honest unacknowledged range. Recovery semantics are in
//! [`crate::control::lease_store`]. If saving the post-execute acknowledgement fails,
//! the live lease keeps the acknowledgement and the durable record stays
//! conservative (that input recovers as `Unknown`);
//! [`InputLease::store_degraded`] reports it until the next successful save.
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::control::admission::{
    lock, CancelToken, EffectGate, EffectRequest, ExecError, SettleOutcome,
};
use crate::control::events::{emit, ControlEvent, EventSink};
pub use crate::control::lease_store::{
    ExecutionIdentity, ExecutionState, FileLeaseStore, LeaseRecord, LeaseStore, MemoryLeaseStore,
    StoreError,
};

/// Point-in-time lease state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseSnapshot {
    /// Current epoch; `0` means never acquired.
    pub epoch: u64,
    pub attached: bool,
    pub next_input_sequence: u64,
    /// Exclusive: every sequence below this is acknowledged.
    pub acked_input_sequence: u64,
    /// `Some((from, to))` when `[from, to)` is unacknowledged.
    pub unacknowledged_input: Option<(u64, u64)>,
}

impl LeaseSnapshot {
    /// CodeRight-shaped `last_acked_input_sequence` (highest acked sequence).
    pub fn last_acked_input_sequence(&self) -> Option<u64> {
        self.acked_input_sequence.checked_sub(1)
    }
}

/// What a controller receives when it acquires the lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseGrant {
    pub epoch: u64,
    /// First sequence this holder must send.
    pub next_input_sequence: u64,
    /// Epoch of the attached holder this grant fenced, if any.
    pub fenced_epoch: Option<u64>,
}

/// Explicit decision about an unacknowledged range (CodeRight
/// `UnacknowledgedInputDecision`). There is no implicit option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnacknowledgedInputDecision {
    /// Caller confirmed delivery (e.g. observed the effect): acknowledge it.
    ReconcileAsDelivered,
    /// Caller treats it as lost: roll the cursor back so the input is resent
    /// under fresh, unambiguous sequence numbers.
    ReconcileAsLost,
}

/// Acknowledgement for one input sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputAck {
    pub epoch: u64,
    pub sequence: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InputOutcome<T> {
    /// Executed once and acknowledged.
    Applied { ack: InputAck, value: T },
    /// Already acknowledged; not executed again.
    Duplicate { ack: InputAck },
}

impl<T> InputOutcome<T> {
    pub fn ack(&self) -> InputAck {
        match self {
            Self::Applied { ack, .. } | Self::Duplicate { ack } => *ack,
        }
    }
}

/// Fencing / sequencing refusal. Nothing executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseError {
    NotAttached,
    StaleEpoch {
        presented: u64,
        current: u64,
    },
    FutureEpoch {
        presented: u64,
        current: u64,
    },
    UnacknowledgedInput {
        from: u64,
        to: u64,
    },
    OutOfOrder {
        expected: u64,
        got: u64,
    },
    /// New input while the execution is not `Running`.
    ExecutionNotRunning {
        state: ExecutionState,
    },
    /// `acknowledge_input` named a sequence that was never sent.
    UnsentSequence {
        sequence: u64,
        next: u64,
    },
    /// `acknowledge_input` went backwards past an acknowledged sequence.
    NonMonotonicAck {
        sequence: u64,
        acked: u64,
    },
    /// `set_execution_state` attempted a transition out of `Exited`.
    InvalidExecutionTransition {
        from: ExecutionState,
        to: ExecutionState,
    },
    /// The durable store refused the change; nothing changed.
    Store {
        reason: String,
    },
}

impl From<StoreError> for LeaseError {
    fn from(e: StoreError) -> Self {
        Self::Store {
            reason: e.to_string(),
        }
    }
}

impl LeaseError {
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotAttached => "not_attached",
            Self::StaleEpoch { .. } => "stale_epoch",
            Self::FutureEpoch { .. } => "future_epoch",
            Self::UnacknowledgedInput { .. } => "unacknowledged_input",
            Self::OutOfOrder { .. } => "out_of_order",
            Self::ExecutionNotRunning { .. } => "execution_not_running",
            Self::UnsentSequence { .. } => "unsent_sequence",
            Self::NonMonotonicAck { .. } => "non_monotonic_ack",
            Self::InvalidExecutionTransition { .. } => "invalid_execution_transition",
            Self::Store { .. } => "store",
        }
    }
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAttached => write!(f, "input lease is not held; acquire before input"),
            Self::StaleEpoch { presented, current } => {
                write!(f, "stale input lease epoch {presented} (current {current})")
            }
            Self::FutureEpoch { presented, current } => {
                write!(f, "unknown input lease epoch {presented} (current {current})")
            }
            Self::UnacknowledgedInput { from, to } => write!(
                f,
                "input lease has unacknowledged input in sequence range [{from}, {to}); reconcile it explicitly before new input"
            ),
            Self::OutOfOrder { expected, got } => {
                write!(f, "input sequence must be {expected}, got {got}")
            }
            Self::ExecutionNotRunning { state } => {
                write!(f, "execution is {}; input requires running", state.label())
            }
            Self::UnsentSequence { sequence, next } => write!(
                f,
                "cannot acknowledge unsent input sequence {sequence} (next {next})"
            ),
            Self::NonMonotonicAck { sequence, acked } => write!(
                f,
                "input acknowledgement must be monotonic: {sequence} is below acknowledged {acked}"
            ),
            Self::InvalidExecutionTransition { from, to } => write!(
                f,
                "execution state cannot change from {} to {}",
                from.label(),
                to.label()
            ),
            Self::Store { reason } => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for LeaseError {}

/// How an executor failed, from the lease's point of view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputFailure {
    /// Definitely no side effect (e.g. denied by admission): the sequence is
    /// released and may be reused.
    NotDelivered(String),
    /// A side effect may have happened: the sequence stays unacknowledged
    /// until explicitly reconciled.
    Ambiguous(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputError {
    Lease(LeaseError),
    NotDelivered {
        reason: String,
    },
    Ambiguous {
        reason: String,
        unacknowledged: (u64, u64),
    },
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lease(e) => e.fmt(f),
            Self::NotDelivered { reason } => write!(f, "input not delivered: {reason}"),
            Self::Ambiguous {
                reason,
                unacknowledged: (a, b),
            } => write!(
                f,
                "input delivery ambiguous ({reason}); sequence range [{a}, {b}) is unacknowledged"
            ),
        }
    }
}

impl std::error::Error for InputError {}

impl From<LeaseError> for InputError {
    fn from(e: LeaseError) -> Self {
        Self::Lease(e)
    }
}

/// Delivery status of one input sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputStatus {
    Acknowledged,
    /// Delivery ambiguous in this host run; reconcile explicitly.
    Unacknowledged,
    /// Was unacknowledged when the host crashed/restarted; resolve explicitly.
    Unknown,
    /// Never sent (at or beyond `next_input_sequence`).
    Unsent,
}

#[derive(Debug, Clone)]
struct State {
    epoch: u64,
    attached: bool,
    next: u64,
    acked: u64,
    exec_id: ExecutionIdentity,
    exec_state: ExecutionState,
    /// The unacknowledged range was inherited from a crash/restart.
    unknown: bool,
}

impl State {
    fn fresh(exec_id: ExecutionIdentity) -> Self {
        Self {
            epoch: 0,
            attached: false,
            next: 0,
            acked: 0,
            exec_id,
            exec_state: ExecutionState::Running,
            unknown: false,
        }
    }
    fn recovered(r: LeaseRecord) -> Self {
        let exec_state = match r.execution_state {
            ExecutionState::Running => ExecutionState::Unknown,
            other => other,
        };
        Self {
            // Recovery fence: every pre-restart holder is now stale.
            epoch: r.epoch.saturating_add(1),
            attached: false,
            next: r.next_input_sequence,
            acked: r.acked_input_sequence,
            exec_id: r.execution_id,
            exec_state,
            unknown: r.next_input_sequence > r.acked_input_sequence,
        }
    }
    fn normalize(&mut self) {
        if self.unacked().is_none() {
            self.unknown = false;
        }
    }
    fn record(&self, name: &str) -> LeaseRecord {
        LeaseRecord {
            name: name.to_string(),
            execution_id: self.exec_id.clone(),
            execution_state: self.exec_state,
            epoch: self.epoch,
            holder_epoch: self.attached.then_some(self.epoch),
            next_input_sequence: self.next,
            acked_input_sequence: self.acked,
            unknown_input: self.unacked().filter(|_| self.unknown),
        }
    }
    fn check_epoch(&self, epoch: u64) -> Result<(), LeaseError> {
        if epoch < self.epoch {
            return Err(LeaseError::StaleEpoch {
                presented: epoch,
                current: self.epoch,
            });
        }
        if epoch > self.epoch {
            return Err(LeaseError::FutureEpoch {
                presented: epoch,
                current: self.epoch,
            });
        }
        Ok(())
    }
    fn unacked(&self) -> Option<(u64, u64)> {
        (self.next > self.acked).then_some((self.acked, self.next))
    }
    fn snapshot(&self) -> LeaseSnapshot {
        LeaseSnapshot {
            epoch: self.epoch,
            attached: self.attached,
            next_input_sequence: self.next,
            acked_input_sequence: self.acked,
            unacknowledged_input: self.unacked(),
        }
    }
    fn check_holder(&self, epoch: u64) -> Result<(), LeaseError> {
        if !self.attached {
            return Err(LeaseError::NotAttached);
        }
        self.check_epoch(epoch)
    }
}

/// One exclusive input lease (per PTY, window, or control target).
pub struct InputLease {
    name: String,
    state: Mutex<State>,
    events: Option<EventSink>,
    store: Option<Arc<dyn LeaseStore>>,
    degraded: AtomicBool,
}

impl std::fmt::Debug for InputLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputLease")
            .field("name", &self.name)
            .field("state", &self.snapshot())
            .field("durable", &self.store.is_some())
            .finish()
    }
}

impl InputLease {
    /// In-memory lease (the default): nothing survives the process.
    pub fn new(name: impl Into<String>) -> Self {
        Self::from_state(
            name.into(),
            State::fresh(ExecutionIdentity::generate()),
            None,
        )
    }

    /// Durable lease backed by `store`. A first open creates a fresh record
    /// (new [`ExecutionIdentity`], `Running`, epoch 0); a later open recovers
    /// it (see [`crate::control::lease_store`]): epoch fenced forward, detached,
    /// `Running` becomes `Unknown`, unacknowledged input becomes `Unknown`
    /// input. Fails closed on an unreadable/corrupt record, a record for a
    /// different lease name, or a failed save.
    pub fn open(name: impl Into<String>, store: Arc<dyn LeaseStore>) -> Result<Self, LeaseError> {
        Self::open_inner(name.into(), store, None)
    }

    /// As [`Self::open`], using `identity` when no record exists yet. An
    /// existing record keeps its own identity; a different one is refused.
    pub fn open_with_identity(
        name: impl Into<String>,
        store: Arc<dyn LeaseStore>,
        identity: ExecutionIdentity,
    ) -> Result<Self, LeaseError> {
        Self::open_inner(name.into(), store, Some(identity))
    }

    fn open_inner(
        name: String,
        store: Arc<dyn LeaseStore>,
        identity: Option<ExecutionIdentity>,
    ) -> Result<Self, LeaseError> {
        let state = match store.load()? {
            None => State::fresh(identity.unwrap_or_else(ExecutionIdentity::generate)),
            Some(r) => {
                if r.name != name {
                    return Err(StoreError::Invalid(format!(
                        "record belongs to lease {:?}, not {name:?}",
                        r.name
                    ))
                    .into());
                }
                if let Some(id) = identity.filter(|id| *id != r.execution_id) {
                    return Err(StoreError::Invalid(format!(
                        "record holds execution {}, not {id}",
                        r.execution_id
                    ))
                    .into());
                }
                State::recovered(r)
            }
        };
        store.save(&state.record(&name))?;
        Ok(Self::from_state(name, state, Some(store)))
    }

    fn from_state(name: String, state: State, store: Option<Arc<dyn LeaseStore>>) -> Self {
        Self {
            name,
            state: Mutex::new(state),
            events: None,
            store,
            degraded: AtomicBool::new(false),
        }
    }

    pub fn with_events(mut self, sink: EventSink) -> Self {
        self.events = Some(sink);
        self
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn snapshot(&self) -> LeaseSnapshot {
        lock(&self.state).snapshot()
    }
    /// True when backed by a [`LeaseStore`].
    pub fn is_durable(&self) -> bool {
        self.store.is_some()
    }
    /// True when the last best-effort save (after an executor ran) failed;
    /// cleared by the next successful save.
    pub fn store_degraded(&self) -> bool {
        self.degraded.load(Ordering::SeqCst)
    }
    /// The durable projection (identity, execution state, epoch, holder,
    /// sequences, unknown input), whether or not a store backs this lease.
    pub fn record(&self) -> LeaseRecord {
        lock(&self.state).record(&self.name)
    }
    pub fn execution_identity(&self) -> ExecutionIdentity {
        lock(&self.state).exec_id.clone()
    }
    pub fn execution_state(&self) -> ExecutionState {
        lock(&self.state).exec_state
    }
    /// `[from, to)` inherited unacknowledged from a crash/restart, if any.
    pub fn unknown_input(&self) -> Option<(u64, u64)> {
        let s = lock(&self.state);
        s.unacked().filter(|_| s.unknown)
    }
    pub fn input_status(&self, sequence: u64) -> InputStatus {
        let s = lock(&self.state);
        if sequence < s.acked {
            InputStatus::Acknowledged
        } else if sequence >= s.next {
            InputStatus::Unsent
        } else if s.unknown {
            InputStatus::Unknown
        } else {
            InputStatus::Unacknowledged
        }
    }

    /// Host-side, explicit execution-state resolution (e.g. after a restart
    /// the host verified the target is alive → `Running`, or observed it
    /// ended → `Exited`). `Exited` is terminal.
    pub fn set_execution_state(&self, state: ExecutionState) -> Result<LeaseSnapshot, LeaseError> {
        self.mutate(|s| {
            if s.exec_state == ExecutionState::Exited && state != ExecutionState::Exited {
                return Err(LeaseError::InvalidExecutionTransition {
                    from: s.exec_state,
                    to: state,
                });
            }
            s.exec_state = state;
            Ok(s.snapshot())
        })
    }

    /// Apply `f` to a copy of the state, save it, then commit. A failed `f`
    /// or save leaves the lease unchanged.
    fn mutate<R>(
        &self,
        f: impl FnOnce(&mut State) -> Result<R, LeaseError>,
    ) -> Result<R, LeaseError> {
        let mut guard = lock(&self.state);
        let mut next = guard.clone();
        let out = f(&mut next)?;
        next.normalize();
        self.persist(&next)?;
        *guard = next;
        Ok(out)
    }

    fn persist(&self, s: &State) -> Result<(), LeaseError> {
        if let Some(store) = &self.store {
            store.save(&s.record(&self.name))?;
            self.degraded.store(false, Ordering::SeqCst);
        }
        Ok(())
    }

    fn persist_best_effort(&self, s: &State) {
        if self.persist(s).is_err() {
            self.degraded.store(true, Ordering::SeqCst);
        }
    }

    /// Take the lease: bump the epoch (fencing any attached holder). Rejected
    /// while unacknowledged input is outstanding; use
    /// [`Self::acquire_reconciling`]. On a durable lease the new epoch is
    /// saved before it is granted.
    pub fn acquire(&self) -> Result<LeaseGrant, LeaseError> {
        let (grant, fenced) = self.mutate(|s| {
            if let Some((from, to)) = s.unacked() {
                return Err(LeaseError::UnacknowledgedInput { from, to });
            }
            Ok(take(s))
        })?;
        self.announce(grant, fenced);
        Ok(grant)
    }

    /// Resolve any unacknowledged (or crash-`Unknown`) range with an explicit
    /// decision, then take the lease (bumping the epoch).
    ///
    /// # Panics
    /// On a durable lease whose store refuses the save (an epoch that is not
    /// durable is never granted). Durable callers should use
    /// [`Self::try_acquire_reconciling`]; an in-memory lease never panics.
    pub fn acquire_reconciling(&self, decision: UnacknowledgedInputDecision) -> LeaseGrant {
        match self.try_acquire_reconciling(decision) {
            Ok(g) => g,
            Err(e) => panic!("input lease {:?}: {e}", self.name),
        }
    }

    /// Fallible [`Self::acquire_reconciling`]: errs only when the store
    /// refuses the save, in which case nothing changed.
    pub fn try_acquire_reconciling(
        &self,
        decision: UnacknowledgedInputDecision,
    ) -> Result<LeaseGrant, LeaseError> {
        let (grant, fenced) = self.mutate(|s| {
            reconcile_state(s, decision);
            Ok(take(s))
        })?;
        self.announce(grant, fenced);
        Ok(grant)
    }

    fn announce(&self, grant: LeaseGrant, fenced: Option<u64>) {
        if let Some(old) = fenced {
            emit(
                &self.events,
                ControlEvent::LeaseFenced {
                    lease: self.name.clone(),
                    old_epoch: old,
                    new_epoch: grant.epoch,
                },
            );
        }
        emit(
            &self.events,
            ControlEvent::LeaseAcquired {
                lease: self.name.clone(),
                epoch: grant.epoch,
            },
        );
    }

    /// The current holder resolves its own unacknowledged range in place.
    pub fn reconcile(
        &self,
        epoch: u64,
        decision: UnacknowledgedInputDecision,
    ) -> Result<LeaseSnapshot, LeaseError> {
        self.mutate(|s| {
            s.check_holder(epoch)?;
            reconcile_state(s, decision);
            Ok(s.snapshot())
        })
    }

    /// Explicitly acknowledge delivery of every sequence up to and including
    /// `sequence` (CodeRight `acknowledge_input`). This is how a client that
    /// observed the effect resolves unacknowledged or crash-`Unknown` input
    /// one prefix at a time. `epoch` must be the current epoch; the lease
    /// need not be attached (after a restart it is detached at the recovery
    /// epoch, which only the host can read). Re-acknowledging the highest
    /// acknowledged sequence is idempotent.
    pub fn acknowledge_input(
        &self,
        epoch: u64,
        sequence: u64,
    ) -> Result<LeaseSnapshot, LeaseError> {
        self.mutate(|s| {
            s.check_epoch(epoch)?;
            if sequence >= s.next {
                return Err(LeaseError::UnsentSequence {
                    sequence,
                    next: s.next,
                });
            }
            if sequence.saturating_add(1) < s.acked {
                return Err(LeaseError::NonMonotonicAck {
                    sequence,
                    acked: s.acked,
                });
            }
            s.acked = s.acked.max(sequence + 1);
            Ok(s.snapshot())
        })
    }

    /// Release by the current holder only; a fenced holder cannot release.
    pub fn release(&self, epoch: u64) -> Result<LeaseSnapshot, LeaseError> {
        let snap = self.mutate(|s| {
            s.check_holder(epoch)?;
            s.attached = false;
            Ok(s.snapshot())
        })?;
        emit(
            &self.events,
            ControlEvent::LeaseReleased {
                lease: self.name.clone(),
                epoch,
            },
        );
        Ok(snap)
    }

    /// Check fencing and sequencing without executing. Returns `Ok(true)` for
    /// new input at `next_input_sequence`, `Ok(false)` for a duplicate.
    pub fn check(&self, epoch: u64, sequence: u64) -> Result<bool, LeaseError> {
        let s = lock(&self.state);
        classify(&s, epoch, sequence).map(|c| c == Class::New)
    }

    /// Submit input `(epoch, sequence)`. `execute` runs at most once, only for
    /// new in-order input from the current holder while the execution is
    /// `Running`. On a durable lease the sequence is reserved durably before
    /// `execute` runs; if that save fails nothing runs (`NotDelivered`).
    pub fn submit<T>(
        &self,
        epoch: u64,
        sequence: u64,
        execute: impl FnOnce() -> Result<T, InputFailure>,
    ) -> Result<InputOutcome<T>, InputError> {
        let mut s = lock(&self.state);
        match classify(&s, epoch, sequence) {
            Err(e) => {
                emit(
                    &self.events,
                    ControlEvent::InputRejected {
                        lease: self.name.clone(),
                        epoch,
                        sequence,
                        reason: e.label(),
                    },
                );
                return Err(e.into());
            }
            Ok(Class::Duplicate) => {
                return Ok(InputOutcome::Duplicate {
                    ack: InputAck { epoch, sequence },
                })
            }
            Ok(Class::New) => {}
        }
        // Reserve the sequence before the side effect: if we crash mid-way the
        // range is visibly unacknowledged, never silently reusable.
        s.next = sequence.saturating_add(1);
        if let Err(e) = self.persist(&s) {
            s.next = sequence;
            return Err(InputError::NotDelivered {
                reason: e.to_string(),
            });
        }
        let result = match catch_unwind(AssertUnwindSafe(execute)) {
            Ok(Ok(value)) => {
                s.acked = s.next;
                Ok(InputOutcome::Applied {
                    ack: InputAck { epoch, sequence },
                    value,
                })
            }
            Ok(Err(InputFailure::NotDelivered(reason))) => {
                s.next = sequence;
                Err(InputError::NotDelivered { reason })
            }
            Ok(Err(InputFailure::Ambiguous(reason))) => Err(InputError::Ambiguous {
                reason,
                unacknowledged: (s.acked, s.next),
            }),
            Err(_) => Err(InputError::Ambiguous {
                reason: "input executor panicked".into(),
                unacknowledged: (s.acked, s.next),
            }),
        };
        s.normalize();
        self.persist_best_effort(&s);
        result
    }
}

fn take(s: &mut State) -> (LeaseGrant, Option<u64>) {
    let fenced = s.attached.then_some(s.epoch);
    s.epoch = s.epoch.saturating_add(1);
    s.attached = true;
    (
        LeaseGrant {
            epoch: s.epoch,
            next_input_sequence: s.next,
            fenced_epoch: fenced,
        },
        fenced,
    )
}

#[derive(PartialEq, Eq)]
enum Class {
    New,
    Duplicate,
}

fn classify(s: &State, epoch: u64, sequence: u64) -> Result<Class, LeaseError> {
    s.check_holder(epoch)?;
    if sequence < s.acked {
        return Ok(Class::Duplicate);
    }
    if s.exec_state != ExecutionState::Running {
        return Err(LeaseError::ExecutionNotRunning {
            state: s.exec_state,
        });
    }
    if let Some((from, to)) = s.unacked() {
        return Err(LeaseError::UnacknowledgedInput { from, to });
    }
    if sequence != s.next {
        return Err(LeaseError::OutOfOrder {
            expected: s.next,
            got: sequence,
        });
    }
    Ok(Class::New)
}

fn reconcile_state(s: &mut State, decision: UnacknowledgedInputDecision) {
    if s.unacked().is_some() {
        match decision {
            UnacknowledgedInputDecision::ReconcileAsDelivered => s.acked = s.next,
            UnacknowledgedInputDecision::ReconcileAsLost => s.next = s.acked,
        }
    }
    s.normalize();
}

/// A lease-fenced, admission-gated control session: input must first pass
/// PTY-002 fencing/sequencing (a stale controller never even reaches the
/// hook), then EFF-001 admission, then executes and settles.
#[derive(Debug, Clone)]
pub struct ControlSession {
    pub lease: Arc<InputLease>,
    pub gate: Arc<EffectGate>,
}

impl ControlSession {
    pub fn new(lease: Arc<InputLease>, gate: Arc<EffectGate>) -> Self {
        Self { lease, gate }
    }

    /// Lease-checked, admission-gated input. Denials and pre-execute
    /// cancellation release the sequence (`NotDelivered`); a failure, panic
    /// or cancellation after the executor started leaves it unacknowledged
    /// (`Ambiguous`).
    pub fn input<T>(
        &self,
        epoch: u64,
        sequence: u64,
        request: EffectRequest,
        cancel: &CancelToken,
        execute: impl FnOnce(&CancelToken) -> Result<T, ExecError>,
    ) -> Result<InputOutcome<T>, InputError> {
        self.lease.submit(epoch, sequence, || {
            let effect = self.gate.admit(request, cancel, execute);
            let st = &effect.settlement;
            let reason = format!(
                "{}: {}",
                st.outcome.label(),
                st.reason.clone().unwrap_or_default()
            );
            match (st.outcome, st.executed) {
                (SettleOutcome::Ok, _) => effect.value.ok_or(InputFailure::Ambiguous(reason)),
                (_, false) => Err(InputFailure::NotDelivered(reason)),
                (_, true) => Err(InputFailure::Ambiguous(reason)),
            }
        })
    }
}
