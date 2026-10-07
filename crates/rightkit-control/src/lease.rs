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
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use crate::admission::{lock, CancelToken, EffectGate, EffectRequest, ExecError, SettleOutcome};
use crate::events::{emit, ControlEvent, EventSink};

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
    StaleEpoch { presented: u64, current: u64 },
    FutureEpoch { presented: u64, current: u64 },
    UnacknowledgedInput { from: u64, to: u64 },
    OutOfOrder { expected: u64, got: u64 },
}

impl LeaseError {
    pub fn label(&self) -> &'static str {
        match self {
            Self::NotAttached => "not_attached",
            Self::StaleEpoch { .. } => "stale_epoch",
            Self::FutureEpoch { .. } => "future_epoch",
            Self::UnacknowledgedInput { .. } => "unacknowledged_input",
            Self::OutOfOrder { .. } => "out_of_order",
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

#[derive(Debug, Default)]
struct State {
    epoch: u64,
    attached: bool,
    next: u64,
    acked: u64,
}

impl State {
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
}

/// One exclusive input lease (per PTY, window, or control target).
pub struct InputLease {
    name: String,
    state: Mutex<State>,
    events: Option<EventSink>,
}

impl std::fmt::Debug for InputLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputLease")
            .field("name", &self.name)
            .field("state", &self.snapshot())
            .finish()
    }
}

impl InputLease {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            state: Mutex::new(State::default()),
            events: None,
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

    /// Take the lease: bump the epoch (fencing any attached holder). Rejected
    /// while unacknowledged input is outstanding; use
    /// [`Self::acquire_reconciling`].
    pub fn acquire(&self) -> Result<LeaseGrant, LeaseError> {
        let mut s = lock(&self.state);
        if let Some((from, to)) = s.unacked() {
            return Err(LeaseError::UnacknowledgedInput { from, to });
        }
        Ok(self.take(&mut s))
    }

    /// Resolve any unacknowledged range with an explicit decision, then take
    /// the lease (bumping the epoch).
    pub fn acquire_reconciling(&self, decision: UnacknowledgedInputDecision) -> LeaseGrant {
        let mut s = lock(&self.state);
        reconcile_state(&mut s, decision);
        self.take(&mut s)
    }

    fn take(&self, s: &mut State) -> LeaseGrant {
        let fenced = s.attached.then_some(s.epoch);
        s.epoch = s.epoch.saturating_add(1);
        s.attached = true;
        if let Some(old) = fenced {
            emit(
                &self.events,
                ControlEvent::LeaseFenced {
                    lease: self.name.clone(),
                    old_epoch: old,
                    new_epoch: s.epoch,
                },
            );
        }
        emit(
            &self.events,
            ControlEvent::LeaseAcquired {
                lease: self.name.clone(),
                epoch: s.epoch,
            },
        );
        LeaseGrant {
            epoch: s.epoch,
            next_input_sequence: s.next,
            fenced_epoch: fenced,
        }
    }

    /// The current holder resolves its own unacknowledged range in place.
    pub fn reconcile(
        &self,
        epoch: u64,
        decision: UnacknowledgedInputDecision,
    ) -> Result<LeaseSnapshot, LeaseError> {
        let mut s = lock(&self.state);
        s.check_holder(epoch)?;
        reconcile_state(&mut s, decision);
        Ok(s.snapshot())
    }

    /// Release by the current holder only; a fenced holder cannot release.
    pub fn release(&self, epoch: u64) -> Result<LeaseSnapshot, LeaseError> {
        let mut s = lock(&self.state);
        s.check_holder(epoch)?;
        s.attached = false;
        emit(
            &self.events,
            ControlEvent::LeaseReleased {
                lease: self.name.clone(),
                epoch,
            },
        );
        Ok(s.snapshot())
    }

    /// Check fencing and sequencing without executing. Returns `Ok(true)` for
    /// new input at `next_input_sequence`, `Ok(false)` for a duplicate.
    pub fn check(&self, epoch: u64, sequence: u64) -> Result<bool, LeaseError> {
        let s = lock(&self.state);
        classify(&s, epoch, sequence).map(|c| c == Class::New)
    }

    /// Submit input `(epoch, sequence)`. `execute` runs at most once, only for
    /// new in-order input from the current holder.
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
        match catch_unwind(AssertUnwindSafe(execute)) {
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
        }
    }
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
