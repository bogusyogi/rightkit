#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{delivery_history::*, engine::*, engine_impl::*, finalize_transcript::*};

impl Default for EngineRuntime {
    fn default() -> Self {
        Self::new(PathBuf::from("."))
    }
}

pub(crate) fn delivery_id_for(s: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

pub(crate) fn trace_stop_to_engine_outcome(started: Option<Instant>, outcome: &'static str) {
    if let Some(started) = started {
        tracing::info!(
            stop_to_engine_outcome_ms = started.elapsed().as_millis() as u64,
            outcome,
            "stop_to_engine_outcome"
        );
    }
}

/// Public helper exposed to tests: the captured-target snapshot. The Tauri
/// shell uses the same helper via `delivery::snapshot_target` for the in-
/// process engine path; this re-export keeps the contract symmetric.
pub fn snapshot_current_target() -> TargetSnapshot {
    snapshot_target()
}
