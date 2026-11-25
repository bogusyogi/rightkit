//! Host-neutral glue for the engine's cache and model checks that wrap existing primitives:
//! the composed "clear caches and restart the resident engine" command body, the "selected
//! backend has no model" check and the streaming-insertion conversion the runtime host needs.

use rightkit_dictation_command_host::streaming::ReconcileOutcome;
use rightkit_dictation_models::install_flow::{rebuild_resident, EngineLock};

use crate::host::StreamingFinal;

/// Clear the persistent engine caches and restart the resident engine, serialized against backend
/// transitions. Order (kept): take the backend-transition gate, run `ensure_model_active` (the
/// restart spawns the resident engine, so this process must have activated the shared model first,
/// before any lock and before the caches are cleared), then
/// [`rebuild_resident`] with `clear`, then `on_rebuilt` (set the result message, log). The host
/// supplies `engine` (its engine lock), `busy_message` (dictation in progress), `clear`, and
/// `restart_failed` (wording for a failed restart). The host runs this on a blocking thread.
pub fn recompile_resident_cache<T>(
    engine: &dyn EngineLock,
    busy_message: &str,
    ensure_model_active: impl FnOnce() -> Result<(), String>,
    clear: impl FnOnce() -> Result<T, String>,
    restart_failed: &dyn Fn(&str) -> String,
    on_rebuilt: impl FnOnce(&mut T),
) -> Result<T, String> {
    crate::runtime_lock::with_backend_transition(|| {
        ensure_model_active()?;
        let mut result = rebuild_resident(engine, busy_message, clear, restart_failed)?;
        on_rebuilt(&mut result);
        Ok(result)
    })
}

/// True when `backend` is `model_backend_id` (the backend id that needs a downloaded model) and
/// no model directory resolves for it. The host passes the selected backend and that id.
pub fn selected_model_missing(backend: &str, model_backend_id: &str) -> bool {
    use rightkit_dictation_models::model_paths::{resolve_model_dir, subdir_for_backend};
    backend == model_backend_id && resolve_model_dir(subdir_for_backend(backend)).is_none()
}

/// Convert the streaming-insertion reconcile outcome into the runtime's [`StreamingFinal`]. The
/// log `tag` is taken before the outcome is consumed, as the runtime host did.
pub fn streaming_final_from(outcome: ReconcileOutcome) -> StreamingFinal {
    let tag = outcome.tag();
    match outcome {
        ReconcileOutcome::NothingWritten => StreamingFinal::NothingWritten,
        ReconcileOutcome::Reconciled { remainder } => StreamingFinal::Reconciled { remainder },
        ReconcileOutcome::StoppedWritesRemain { cause, .. } => {
            StreamingFinal::Stopped { tag, cause }
        }
        ReconcileOutcome::StoppedCountUncertain { cause } => StreamingFinal::Stopped { tag, cause },
    }
}
