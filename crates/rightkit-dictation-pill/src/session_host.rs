//! The session around the pill: one guardian batch, and installing and shutting the session down
//! (hints, guardian and the host's native pill, in a fixed order). Platform-neutral: every
//! platform difference arrives as a function the host supplies.
//!
//! The guardian is event-driven (`guardian_worker`). Its worker blocks on a channel with no timeout
//! and wakes only when the OS reports a change that can alter what is on screen (macOS: app
//! activation, active-Space and Accessibility window notifications; Windows: WinEvent foreground
//! and minimise/restore events, plus location events for the foreground window's process).
//! Each coalesced batch re-evaluates full screen once and applies it ([`apply_guardian_batch`]);
//! foreground, Space and minimise events also run the visibility invariant (and, where the host
//! has them, re-assert the auxiliary overlays topmost). One settle pass repeats both after the
//! last such event, because a full-screen transition and a freshly activated window's z-order
//! finish after the event that announced them.
//!
//! UNPROVEN RECOVERY: the Windows "pill buried in the z-order" self-heal was once retried every
//! 2.5 s whatever the cause. It now runs only on those events, so a pill buried with no
//! foreground or minimise event stays buried until the next one or an explicit visibility pass by
//! the host. Only if no OS event source could be installed does the worker fall back to its
//! degraded interval. Do not add levers on top of this without evidence.
//!
//! Moved from HeardRight `pill/state.rs` (`apply_guardian_batch`, `spawn_visibility_guardian`,
//! `stop_visibility_guardian`, `install`, `shutdown`). Event names and detail text are unchanged.

use std::time::Duration;

use crate::event_pipeline::Task;
use crate::guardian::{Due, GuardianTimings};
use crate::guardian_worker::{GuardianNames, GuardianSlot};
use crate::hint_session::HintSet;
use crate::host_glue::Sink;

/// Where the full-screen verdict is read.
#[derive(Clone, Copy)]
pub enum FullscreenSample {
    /// Read on the guardian's worker thread, then applied on the main thread (a probe that makes
    /// synchronous IPC into another application, such as an Accessibility read, must never make
    /// the main thread wait for it).
    OffMain(fn() -> bool),
    /// Read on the main thread, together with applying it.
    OnMain(fn() -> bool),
}

/// What one guardian pass reads and runs, all supplied by the host.
#[derive(Clone, Copy)]
pub struct GuardianBatch {
    /// Whether the host can run a batch yet (the app handle exists). A batch before that does
    /// nothing, and the full-screen probe is not read.
    pub ready: fn() -> bool,
    /// Runs a task on the main thread. `None`: there is no main thread to run it (nothing is
    /// applied); `Some(Err(()))`: scheduling failed (ignored, the next pass tries again).
    pub run_on_main: fn(Task) -> Option<Result<(), ()>>,
    /// The full-screen probe.
    pub fullscreen: FullscreenSample,
    /// Applies a full-screen verdict (on the main thread).
    pub update_fullscreen: fn(bool),
    /// The visibility invariant; called with `"guardian"` (on the main thread) when the batch
    /// carries a focus-class event.
    pub enforce: fn(&str),
    /// Re-assert the auxiliary overlays topmost, right after `enforce` (on the main thread);
    /// `None` where the host has none.
    pub reassert_aux: Option<fn()>,
}

/// One guardian pass: sample full screen (see [`FullscreenSample`]), hop to the main thread, apply
/// the verdict and, when the batch carries a focus-class event (`Due::invariant`), run the
/// visibility invariant and re-assert the auxiliary overlays. The screenshot sidecar hides the pill
/// on purpose for a short while: a pass landing inside that window re-shows it, as the old timer
/// did; a focus event during a capture is rare.
pub fn apply_guardian_batch(batch: &GuardianBatch, due: Due) {
    if !(batch.ready)() {
        return;
    }
    let batch = *batch;
    let sampled = match batch.fullscreen {
        FullscreenSample::OffMain(sample) => Some(sample()),
        FullscreenSample::OnMain(_) => None,
    };
    let run_invariant = due.invariant;
    let _ = (batch.run_on_main)(Box::new(move || {
        let fullscreen = sampled.unwrap_or_else(|| match batch.fullscreen {
            FullscreenSample::OnMain(sample) | FullscreenSample::OffMain(sample) => sample(),
        });
        (batch.update_fullscreen)(fullscreen);
        if run_invariant {
            (batch.enforce)("guardian");
            if let Some(reassert) = batch.reassert_aux {
                reassert();
            }
        }
    }));
}

/// Everything [`install`] and [`shutdown`] drive, supplied by the host.
pub struct SessionHost {
    /// The one guardian slot (declare it as a `static`).
    pub guardian: &'static GuardianSlot,
    pub names: GuardianNames,
    pub timings: GuardianTimings,
    /// How long [`shutdown`] waits for the guardian worker.
    pub stop_timeout: Duration,
    /// What one guardian pass does.
    pub batch: GuardianBatch,
    /// The delayed hints of this lifecycle (declare it as a `static`).
    pub hints: &'static HintSet,
    pub log: Sink,
    /// Bring the native pill up (a no-op where it comes up by itself).
    pub native_init: fn(),
    /// Destroy the native pill (bounded by the host) and stop its owner thread.
    pub native_shutdown: fn(),
}

/// Start the guardian unless one runs. A failure is never silent: it is logged as
/// `pill_guardian_start_error` (`reason=<code>`), because without a guardian nothing suppresses the
/// pill for a full-screen app and nothing repairs a buried pill until the next explicit pass.
pub fn start_guardian(host: &SessionHost) {
    let batch = host.batch;
    let started = host.guardian.start_once(
        host.names,
        host.timings,
        batch.ready,
        move |due| apply_guardian_batch(&batch, due),
        host.log,
    );
    if let Err(error) = started {
        (host.log)(
            "pill_guardian_start_error",
            &format!("reason={}", error.code()),
        );
    }
}

/// Wake the guardian worker for shutdown (it drops its OS event sources and exits) and wait at most
/// `stop_timeout` for it. A worker still finishing a batch is logged as
/// `pill_guardian_stop_timeout`.
pub fn stop_guardian(host: &SessionHost) {
    if host.guardian.stop(host.stop_timeout) == Some(false) {
        (host.log)(
            "pill_guardian_stop_timeout",
            "worker still finishing a batch; it exits when that returns",
        );
    }
}

/// Open a new lifecycle: the epoch of every delayed hint moves (nothing queued before this call
/// can run), then the native pill comes up, then the guardian starts. The host stores its app
/// handle before calling.
pub fn install(host: &SessionHost) {
    host.hints.restart();
    (host.native_init)();
    start_guardian(host);
}

/// Close the lifecycle: delayed hints are retired first (a hint that has not shown yet must not
/// show after the pill is gone, a settle step already queued on the main thread re-checks and does
/// nothing, and nothing new is accepted until [`install`]), then the guardian stops, then the
/// native pill is destroyed.
pub fn shutdown(host: &SessionHost) {
    host.hints.retire();
    stop_guardian(host);
    (host.native_shutdown)();
}
