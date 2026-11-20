//! The flow around showing a hint card: the pending hint slot, and the two ways a host queues a
//! hint after an event: claim-once (a one-time hint whose "already shown" flag the host owns) and
//! feature tips (the pending kind is set now, the card shown a moment later).
//!
//! Moved from HeardRight `pill_overlays.rs` (`PILL_HINT_PENDING`, `set_pending_pill_hint`,
//! `take_pill_hint_payload`, `show_screenshot_notice`, `maybe_show_cancel_saved_hint`,
//! `note_completed_transcript_for_hints`). Kinds, delays, event names and what "show" does are
//! the host's. Every delay goes through a [`Generation`], so a delayed show that was cancelled or
//! superseded does nothing.

use std::sync::Mutex;
use std::time::Duration;

use crate::delayed::Generation;
use crate::host_glue::Sink;

struct PendingState {
    kind: Option<String>,
    /// A retired lifecycle accepts no new pending kind (until `restart`).
    retired: bool,
}

/// The hint kind waiting to be shown (declare as a `static`).
pub struct PendingHint(Mutex<PendingState>);

impl Default for PendingHint {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingHint {
    pub const fn new() -> PendingHint {
        PendingHint(Mutex::new(PendingState {
            kind: None,
            retired: false,
        }))
    }

    /// Set the pending kind. Logs `<set_event> ok`, `<error_event> outcome=lock_poisoned`, or
    /// `<error_event> outcome=retired` (nothing is stored) after [`PendingHint::retire`].
    pub fn set(&self, sink: Sink, set_event: &str, error_event: &str, kind: &str) {
        match self.0.lock() {
            Ok(mut slot) => {
                if slot.retired {
                    sink(error_event, "outcome=retired");
                    return;
                }
                slot.kind = Some(kind.to_string());
                sink(set_event, "ok");
            }
            Err(_) => sink(error_event, "outcome=lock_poisoned"),
        }
    }

    /// Take the pending kind; `Err` when the lock is poisoned.
    pub fn take(&self) -> Result<Option<String>, String> {
        self.0
            .lock()
            .map(|mut slot| slot.kind.take())
            .map_err(|err| format!("pending hint lock poisoned: {err}"))
    }

    /// Drop the pending kind (a poisoned lock is recovered: the value is dropped all the same).
    pub fn clear(&self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).kind = None;
    }

    /// End this lifecycle: drop the pending kind and refuse a new one until `restart`.
    pub fn retire(&self) {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        slot.kind = None;
        slot.retired = true;
    }

    /// Begin a new lifecycle: nothing is pending, and a kind is accepted again.
    pub fn restart(&self) {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        slot.kind = None;
        slot.retired = false;
    }
}

/// Cancel every delayed hint of the host and close the pending slot (shutdown). A delayed show
/// that has not run never runs, a show already running finishes first (see [`Generation`]), the
/// pending kind is dropped, and a hint queued afterwards is refused until [`restart`].
pub fn retire(delays: &[&'static Generation], pending: &'static PendingHint) {
    for generation in delays {
        generation.retire();
    }
    pending.retire();
}

/// Cancel every delayed hint and drop the pending kind without closing the lifecycle (the host
/// cancels a hint flow that may start again at once).
pub fn cancel(delays: &[&'static Generation], pending: &'static PendingHint) {
    for generation in delays {
        generation.cancel();
    }
    pending.clear();
}

/// Open the next lifecycle (the pill is installed again): every delay token moves its epoch, so no
/// show queued before the restart can run, and the pending slot is empty and open.
pub fn restart(delays: &[&'static Generation], pending: &'static PendingHint) {
    for generation in delays {
        generation.restart();
    }
    pending.restart();
}

/// The names the pending slot logs under.
#[derive(Clone, Copy, Debug)]
pub struct PendingEvents {
    pub set: &'static str,
    pub set_error: &'static str,
}

/// The events of a claim-once hint.
#[derive(Clone, Copy, Debug)]
pub struct ClaimEvents {
    /// `outcome=failed` when claiming failed (the error text is never logged).
    pub claim_error: &'static str,
    /// `outcome=failed` when showing failed.
    pub show_error: &'static str,
    /// `queued` once the card was handed over.
    pub shown: &'static str,
}

/// Queue a one-time hint. `claim` is the host's atomic "not shown before" gate: `Ok(true)` means
/// this call owns the hint, an `Err` is logged and reads as `false`. Nothing is claimed once `delays`
/// is retired. When owned, after `delay` (and only if `delays` is still current) the kind is made pending and `show` runs.
#[allow(clippy::too_many_arguments)]
pub fn claim_once_then_show<E1, E2>(
    sink: Sink,
    delays: &'static Generation,
    pending: &'static PendingHint,
    pending_events: PendingEvents,
    events: ClaimEvents,
    kind: &'static str,
    delay: Duration,
    claim: impl FnOnce() -> Result<bool, E1>,
    show: impl FnOnce() -> Result<(), E2> + Send + 'static,
) {
    // A retired lifecycle claims nothing: the host's "already shown" flag must not be spent on a
    // hint that can never show.
    if delays.is_retired() {
        return;
    }
    let owned = match claim() {
        Ok(owned) => owned,
        Err(_) => {
            sink(events.claim_error, "outcome=failed");
            false
        }
    };
    if !owned {
        return;
    }
    delays.schedule(delay, move || {
        pending.set(sink, pending_events.set, pending_events.set_error, kind);
        if show().is_err() {
            sink(events.show_error, "outcome=failed");
        } else {
            sink(events.shown, "queued");
        }
    });
}

/// The events of a feature tip.
#[derive(Clone, Copy, Debug)]
pub struct TipEvents {
    pub show_error: &'static str,
    pub shown: &'static str,
}

/// Queue a feature tip: the kind is pending now, and after `delay` (if `delays` is still current)
/// `show` runs. `detail` is the host's detail text for the `shown` event.
#[allow(clippy::too_many_arguments)]
pub fn queue_tip<E>(
    sink: Sink,
    delays: &'static Generation,
    pending: &'static PendingHint,
    pending_events: PendingEvents,
    events: TipEvents,
    kind: &str,
    detail: String,
    delay: Duration,
    show: impl FnOnce() -> Result<(), E> + Send + 'static,
) {
    pending.set(sink, pending_events.set, pending_events.set_error, kind);
    delays.schedule(delay, move || {
        if show().is_err() {
            sink(events.show_error, "outcome=failed");
        } else {
            sink(events.shown, &detail);
        }
    });
}
