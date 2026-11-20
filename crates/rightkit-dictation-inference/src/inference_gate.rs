//! Serialization boundary for main on-device ASR inference.
//!
//! Final, file, and committed-window decodes share one accelerator and take
//! this lease. CPU KWS owns a separate model and worker, so it never
//! contends on this gate.

use std::sync::OnceLock;
use std::time::Instant;

use parking_lot::{Mutex, MutexGuard};

static GATE: OnceLock<Mutex<()>> = OnceLock::new();

fn gate() -> &'static Mutex<()> {
    GATE.get_or_init(|| Mutex::new(()))
}

pub(crate) struct InferenceLease {
    _guard: MutexGuard<'static, ()>,
    owner: &'static str,
    acquired_at: Instant,
}

impl Drop for InferenceLease {
    fn drop(&mut self) {
        tracing::debug!(
            target: "inference_gate",
            owner = self.owner,
            held_ms = self.acquired_at.elapsed().as_millis() as u64,
            "inference_released"
        );
    }
}

/// Error text of an alignment that stopped because its cooperative deadline passed. The control
/// cleanup treats any alignment `Err` as "no accepted boundary", so the caller delivers its
/// direct onset cut (flagged unresolved) instead.
pub(crate) const ALIGNMENT_DEADLINE_ERROR: &str = "control alignment deadline passed";

/// True when an optional cooperative `deadline` has passed. `None` never expires. One monotonic
/// clock read; the alignment loops call it once per joint step.
pub(crate) fn deadline_passed(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

/// Deadline-bound acquisition, for OPTIONAL work (control-alignment cleanup) that runs on a
/// worker that must not block past its own deadline. Waits at most until `deadline` (a deadline
/// already passed makes one non-blocking attempt) and returns `None` when another decode still
/// holds the gate, so the caller delivers without the optional work.
pub(crate) fn lease_until(owner: &'static str, deadline: Instant) -> Option<InferenceLease> {
    let wait_started = Instant::now();
    let guard = gate().try_lock_until(deadline);
    let waited = wait_started.elapsed();
    crate::timeline_recorder::add_duration_ms(
        |t| &mut t.inference_wait_ms,
        waited.as_secs_f64() * 1_000.0,
    );
    let Some(guard) = guard else {
        tracing::info!(
            target: "inference_gate",
            owner,
            wait_ms = waited.as_millis() as u64,
            "inference_wait_deadline_passed"
        );
        return None;
    };
    if waited.as_millis() > 0 {
        tracing::info!(
            target: "inference_gate",
            owner,
            wait_ms = waited.as_millis() as u64,
            "inference_waited"
        );
    }
    Some(InferenceLease {
        _guard: guard,
        owner,
        acquired_at: Instant::now(),
    })
}

/// Blocking acquisition, for decodes that MUST run (final, committed windows).
/// Logs a wait only when there was one, so quiet operation stays quiet.
pub(crate) fn lease(owner: &'static str) -> InferenceLease {
    let wait_started = Instant::now();
    let guard = gate().lock();
    let wait_ms = wait_started.elapsed().as_millis() as u64;
    crate::timeline_recorder::add_duration_ms(
        |t| &mut t.inference_wait_ms,
        wait_started.elapsed().as_secs_f64() * 1_000.0,
    );
    if wait_ms > 0 {
        tracing::info!(
            target: "inference_gate",
            owner,
            wait_ms,
            "inference_waited"
        );
    }
    InferenceLease {
        _guard: guard,
        owner,
        acquired_at: Instant::now(),
    }
}
