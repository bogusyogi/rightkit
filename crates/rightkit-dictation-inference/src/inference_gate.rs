//! Serialization boundary for main on-device ASR inference.
//!
//! Final, file, and committed-window decodes share one accelerator and take
//! this lease. CPU Sherpa KWS owns a separate model and worker, so it never
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Two sequential leases must not deadlock, guarding against an accidental
    /// reentrant/self-blocking regression in the gate.
    #[test]
    fn sequential_leases_do_not_deadlock() {
        drop(lease("first"));
        drop(lease("second"));
    }
}
