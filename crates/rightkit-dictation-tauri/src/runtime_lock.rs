use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::runtime::RuntimeApp;
use crate::telemetry;
use rightkit_dictation_core::state::AppState;

/// The one global dictation runtime, opened on first use.
pub static APP: OnceLock<Result<Mutex<RuntimeApp>, String>> = OnceLock::new();

/// Exposed for commands.rs (Pass 3 — engine_info uses runtime).
pub struct RuntimeLockGuard {
    guard: std::sync::MutexGuard<'static, RuntimeApp>,
}

impl std::ops::Deref for RuntimeLockGuard {
    type Target = RuntimeApp;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl std::ops::DerefMut for RuntimeLockGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

pub static BACKEND_TRANSITION_GATE: OnceLock<Mutex<()>> = OnceLock::new();

pub fn with_backend_transition<T>(
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let gate = BACKEND_TRANSITION_GATE.get_or_init(|| Mutex::new(()));
    let _guard = match gate.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            crate::telemetry::log_message(
                "backend_transition_gate_poisoned",
                &poisoned.to_string(),
            );
            poisoned.into_inner()
        }
    };
    operation()
}

pub fn assert_ui_dispatch_unlocked(_context: &str) {
    crate::pill_dispatch_queue::assert_not_locked();
}

pub fn runtime_lock_for_commands() -> Result<RuntimeLockGuard, String> {
    runtime_lock()
}

// LOCK ORDER (outermost first). Never acquire an earlier lock while holding a later one:
//   1. BACKEND_TRANSITION_GATE        (never while runtime_lock is held)
//   2. HISTORY_PREWARM_GATE           (held only across the slow SQLCipher first open;
//                                      released BEFORE runtime_lock is taken, never nested with it)
//   3. runtime_lock (APP mutex)       (must not perform the slow first open; see below)
//   4. HISTORY_PREOPEN                (leaf: pointer swap only; taken under 2 or 3, never both)
// The SQLCipher first open (key derivation + schema/migration, issue #77 M9-root) runs in
// `prewarm_history_outside_runtime_lock` before runtime_lock is acquired; the locked
// `ensure_history_loaded` only adopts the pre-opened store (cheap) and falls back to an inline
// open when the prewarm failed or the history generation moved.
static HISTORY_PREWARM_GATE: Mutex<()> = Mutex::new(());
static HISTORY_PREOPEN: Mutex<Option<(u64, Box<dyn crate::history::HistoryStore>)>> =
    Mutex::new(None);
static HISTORY_PREWARMED_GEN: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(u64::MAX);

/// Open history before runtime_lock is taken. Called only from `runtime_lock()`, which is not
/// reentrant, so the caller never holds runtime_lock here.
pub fn prewarm_history_outside_runtime_lock() {
    use std::sync::atomic::Ordering;
    let generation = crate::runtime::history_storage_generation();
    if HISTORY_PREWARMED_GEN.load(Ordering::Acquire) == generation {
        return;
    }
    let _gate = HISTORY_PREWARM_GATE
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let generation = crate::runtime::history_storage_generation();
    if HISTORY_PREWARMED_GEN.load(Ordering::Acquire) == generation {
        return;
    }
    // Failure is not retried on every lock; the locked path reopens inline and reports the error.
    if let Ok(store) = crate::runtime::build_history_store() {
        *HISTORY_PREOPEN.lock().unwrap_or_else(|p| p.into_inner()) = Some((generation, store));
    }
    HISTORY_PREWARMED_GEN.store(generation, Ordering::Release);
}

/// Adopt the pre-opened store if it matches `generation`; a stale one is dropped.
pub fn take_prewarmed_history(generation: u64) -> Option<Box<dyn crate::history::HistoryStore>> {
    let taken = HISTORY_PREOPEN
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    taken.and_then(|(g, store)| (g == generation).then_some(store))
}

pub fn runtime_lock() -> Result<RuntimeLockGuard, String> {
    prewarm_history_outside_runtime_lock();
    match APP.get_or_init(|| RuntimeApp::open_default().map(Mutex::new)) {
        Ok(runtime) => runtime
            .lock()
            .map(|guard| RuntimeLockGuard { guard })
            .map_err(|err| format!("runtime lock poisoned: {err}")),
        Err(err) => Err(err.clone()),
    }
}

/// How often the consumer loop wakes to check the transcribe watchdog when no
/// engine events are flowing.
pub const ENGINE_EVENT_WATCHDOG_TICK: Duration = Duration::from_secs(1);
/// How long the pill may sit in `Transcribing` with no engine events before the
/// watchdog force-recovers it. This is a last-ditch UI recovery path, not an ASR
/// timeout; keep it loose so cold model/ANE startup cannot make a valid terminal
/// event look stale.
pub const TRANSCRIBE_STUCK_AFTER: Duration = Duration::from_secs(30);
/// F3 fix (shell state-machine audit): hard wall-clock ceiling on TOTAL time in
/// `Transcribing` for one session, independent of `sidecar_is_transcribing_session`.
/// Past `TRANSCRIBE_STUCK_AFTER` the watchdog defers as long as the engine keeps
/// reporting itself as still transcribing that session — correct for a slow-but-
/// live decode, but it means a wedged finalize that never emits a terminal event
/// (and never flips the engine's own internal state) can defer forever, freezing
/// the pill in "processing" indefinitely. This caps the worst case: once a
/// session has been in `Transcribing` this long, the watchdog stops probing and
/// force-recovers unconditionally.
pub const TRANSCRIBE_HARD_CAP_AFTER: Duration = Duration::from_secs(120);

pub type TranscribeWatchdogSince = Option<(String, Instant)>;

pub fn transcribe_watchdog_due(
    state: &AppState,
    since: &mut TranscribeWatchdogSince,
    now: Instant,
) -> Option<String> {
    let AppState::Transcribing { session_id } = state else {
        *since = None;
        return None;
    };
    let started = match since {
        Some((tracked, started)) if tracked == session_id => *started,
        _ => {
            *since = Some((session_id.clone(), now));
            now
        }
    };
    (now.duration_since(started) >= TRANSCRIBE_STUCK_AFTER).then(|| session_id.clone())
}

/// Whether `event` is progress for the session the watchdog is tracking.
/// Recording levels, wake fires and events for other sessions are not.
pub fn watchdog_progress_event(
    since: &TranscribeWatchdogSince,
    event: &crate::engine_process::EngineEvent,
) -> bool {
    use crate::engine_process::EngineEvent;
    let Some((tracked, _)) = since.as_ref() else {
        return false;
    };
    let session_id = match event {
        EngineEvent::DeliveryCompleted { session_id, .. }
        | EngineEvent::TranscriptFinal { session_id, .. }
        | EngineEvent::TranscriptPartial { session_id, .. }
        | EngineEvent::SessionReset { session_id, .. }
        | EngineEvent::TranscribingStarted { session_id, .. }
        | EngineEvent::EngineError { session_id, .. } => session_id,
        EngineEvent::RecordingLevel { .. } | EngineEvent::WakeFired { .. } => return false,
    };
    session_id == tracked
}

/// One watchdog tick (fired when the consumer loop's recv times out). If the
/// runtime has been stuck in `Transcribing` past `TRANSCRIBE_STUCK_AFTER` with no
/// events, force the pill back to `Armed` so it can never freeze in Processing
/// forever (the prior behaviour when a terminal event was dropped/never sent).
pub fn watchdog_tick(since: &mut TranscribeWatchdogSince, emit_observed: impl FnOnce(&[AppState])) {
    // Lock-free first (audit §2.1): the watchdog must still fire while the
    // runtime lock is held (e.g. during prewarm's config/vocab sync).
    let state = crate::runtime::app_state_lock_free()
        .or_else(|| runtime_lock().ok().map(|runtime| runtime.app_state()))
        .unwrap_or(AppState::Armed);
    let Some(session_id) = transcribe_watchdog_due(&state, since, Instant::now()) else {
        return;
    };

    // `since` is guaranteed `Some((session_id, started))` here: `transcribe_watchdog_due`
    // only returns `Some` for the session it just tracked/kept in `since`.
    let total_elapsed = match since.as_ref() {
        Some((tracked, started)) if tracked == &session_id => Some(started.elapsed()),
        _ => None,
    };
    // #159: the ceiling is the audio-length-scaled final-transcript budget the
    // supervisor armed at Stop; the fixed cap only applies when none is armed.
    let hard_capped = match crate::runtime::sidecar_final_wait_expired_lock_free(&session_id) {
        Some(expired) => expired,
        None => total_elapsed.is_some_and(|elapsed| elapsed >= TRANSCRIBE_HARD_CAP_AFTER),
    };

    if hard_capped {
        // Past the hard cap — stop deferring even if the engine still claims
        // this session is transcribing; force the recovery path unconditionally.
        telemetry::log_message(
            "transcribe_watchdog_hard_cap_forced",
            &format!(
                "session={session_id} cap_s={}",
                TRANSCRIBE_HARD_CAP_AFTER.as_secs()
            ),
        );
    } else {
        // Lock-free probe first — bounded by SIDECAR_STATE_PROBE_TIMEOUT on
        // the supervisor channel, not by the runtime lock.
        let probe = match crate::runtime::sidecar_is_transcribing_session_lock_free(&session_id) {
            Some(result) => result,
            None => runtime_lock()
                .and_then(|runtime| runtime.sidecar_is_transcribing_session(&session_id)),
        };
        match probe {
            Ok(true) => {
                telemetry::log_message(
                    "transcribe_watchdog_deferred",
                    &format!("session={session_id}"),
                );
                return;
            }
            Ok(false) => {}
            Err(err) => {
                telemetry::log_message(
                    "transcribe_watchdog_probe_error",
                    &format!("session={session_id} error={err}"),
                );
            }
        }
    }

    // #149: re-arm only after the sidecar acknowledged revoking this session
    // (or was restarted because it could not). Lock-free: the watchdog must
    // never queue behind the runtime lock it exists to outlive.
    let revocation = crate::runtime::sidecar_revoke_session_lock_free(&session_id);
    if let Some(crate::engine_supervisor::RevocationOutcome::Failed { reason }) = &revocation {
        telemetry::log_message(
            "transcribe_watchdog_sidecar_restarted",
            &format!("session={session_id} reason={reason}"),
        );
    }

    let mut observed = Vec::new();
    let _ = with_backend_transition(|| {
        let mut runtime = runtime_lock()?;
        if revocation.is_some() {
            runtime.recover_to_armed_after_revocation(&session_id, |state| {
                observed.push(state.clone())
            });
        } else {
            let prepared = runtime.force_recover_to_armed(|state| observed.push(state.clone()));
            drop(runtime);
            if let Some(prepared) = prepared {
                let _ = prepared.send();
            }
        }
        Ok(())
    });
    emit_observed(&observed);
    telemetry::log_message(
        "transcribe_watchdog_recovered",
        &format!(
            "session={session_id} threshold_s={}",
            TRANSCRIBE_STUCK_AFTER.as_secs(),
        ),
    );
    *since = None;
}
