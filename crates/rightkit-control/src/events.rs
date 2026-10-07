//! Content-free lifecycle events for the caller's journal.
//!
//! Mirrors `rightkit_service::ServiceEvent` / `EventFn`: one `Arc<dyn Fn>` sink
//! the host installs. Events never carry request payloads (typed text, JS,
//! coordinates); only ids, kinds, method names, outcomes and lease numbers.
use std::sync::Arc;

use crate::admission::{EffectAdmissionStage, EffectKind, SettleOutcome};

/// Lifecycle event emitted by [`crate::admission::EffectGate`] and
/// [`crate::lease::InputLease`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlEvent {
    /// The request passed admission and its executor is about to run.
    EffectStarted {
        id: u64,
        kind: EffectKind,
        method: String,
    },
    /// The executor returned (ok/failed/cancelled), or a cancelled request was
    /// settled before execution (`executed == false`).
    EffectStopped {
        id: u64,
        kind: EffectKind,
        method: String,
        outcome: SettleOutcome,
        executed: bool,
    },
    /// The executor panicked. Settled as `Failed`; the panic is contained.
    EffectCrashed {
        id: u64,
        kind: EffectKind,
        method: String,
    },
    /// Validation or approval refused the request; nothing executed.
    EffectDenied {
        id: u64,
        kind: EffectKind,
        method: String,
        stage: EffectAdmissionStage,
        reason: String,
    },
    /// A controller acquired the input lease at `epoch`.
    LeaseAcquired { lease: String, epoch: u64 },
    /// An attached holder at `old_epoch` was fenced by a new acquisition.
    LeaseFenced {
        lease: String,
        old_epoch: u64,
        new_epoch: u64,
    },
    /// The current holder released the lease.
    LeaseReleased { lease: String, epoch: u64 },
    /// Input was refused by lease fencing / sequencing (never executed).
    InputRejected {
        lease: String,
        epoch: u64,
        sequence: u64,
        reason: &'static str,
    },
}

/// Callback for [`ControlEvent`]s. A panicking sink is contained and ignored.
pub type EventSink = Arc<dyn Fn(&ControlEvent) + Send + Sync>;

pub(crate) fn emit(sink: &Option<EventSink>, event: ControlEvent) {
    if let Some(sink) = sink {
        let sink = sink.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || sink(&event)));
    }
}
