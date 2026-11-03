// Shared imports live on the module root (runtime.rs); `use super::*` keeps
// this file on the same namespace the include!-based layout provided.
#[allow(unused_imports)]
use super::*;

/// `Arc<RwLock<AppState>>` — the lock-free half of the runtime state machine.
///
/// `EngineRuntime.state` is a `SharedState`, and the SAME `Arc` is cloned into
/// `SidecarHandle::state`, which `open_sidecar` publishes in
/// `ENGINE_SIDECAR`. Single-writer semantics are preserved: every mutation
/// still runs under the global `Mutex<RuntimeApp>`; the `RwLock` only lets
/// *readers* (`get_app_state`, the transcribe watchdog, future status
/// surfaces) snapshot the state without queuing behind a held lock (e.g.
/// prewarm's config/vocab sync).
///
/// Poison policy: a panic while a writer holds the write lock would poison
/// the `RwLock`; both `get`/`set` recover via `into_inner` — `AppState` is a
/// plain data enum, so a mid-write panic cannot leave it structurally
/// inconsistent (each `set` is one atomic store of a complete value).
#[derive(Clone)]
pub struct SharedState {
    inner: Arc<RwLock<AppState>>,
    /// Bumped on every transition INTO `Error`, under the write lock. A
    /// terminal-error expiry timer is bound to the generation it observed, so
    /// an expiry from an older Error can never reset a newer interaction.
    error_generation: Arc<std::sync::atomic::AtomicU64>,
}

impl SharedState {
    pub fn new(state: AppState) -> Self {
        Self {
            inner: Arc::new(RwLock::new(state)),
            error_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Generation of the most recent transition into `Error`.
    pub fn error_generation(&self) -> u64 {
        self.error_generation
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Terminal-error recovery decision: the state must still be `Error`
    /// and, when the caller carries a generation, it must be the CURRENT
    /// error's. `None` is the legacy unbound caller (state check only).
    pub fn terminal_error_recovery_permits(&self, expected: Option<u64>) -> bool {
        let guard = self
            .inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        matches!(*guard, AppState::Error { .. })
            && expected.is_none_or(|g| g == self.error_generation())
    }

    pub fn get(&self) -> AppState {
        self.inner
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn set(&self, state: AppState) {
        let mut guard = self
            .inner
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(state, AppState::Error { .. }) {
            self.error_generation
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        }
        *guard = state;
    }

    /// Emission-authority decision: `candidate` may emit only while it is
    /// still the authoritative value — session-bound, because `AppState`
    /// equality carries the session id, so a superseded `Recording` or
    /// terminal snapshot can never paint over the state that replaced it.
    /// Called inside the serialized emission critical section in
    /// `dictation_lifecycle` — pairing this read with the emit is what makes
    /// terminal-write-wins atomic.
    pub fn emission_permits(&self, candidate: &AppState) -> bool {
        self.get() == *candidate
    }
}

/// The sidecar half of the running runtime, registered once by
/// `RuntimeApp::open_sidecar`. `None` until the runtime has been opened — every
/// lock-free facade falls back to the locked path then.
static ENGINE_SIDECAR: OnceLock<SidecarHandle> = OnceLock::new();

pub fn register_engine_sidecar(handle: &SidecarHandle) {
    // APP itself is OnceLock-initialized, so this fires exactly once; a second
    // set would mean a second runtime was constructed, which must not happen.
    if ENGINE_SIDECAR.set(handle.clone()).is_err() {
        crate::telemetry::log_message(
            "engine_sidecar_register_duplicate",
            "ENGINE_SIDECAR already initialized; keeping the first handle",
        );
    }
}

pub fn engine_sidecar() -> Option<&'static SidecarHandle> {
    ENGINE_SIDECAR.get()
}

/// Lock-free `RuntimeApp::app_state()` for the sidecar runtime. `None` when
/// there is no published sidecar handle (the runtime has not
/// been opened yet) — callers fall back to `runtime_lock()?.app_state()`.
pub fn app_state_lock_free() -> Option<AppState> {
    engine_sidecar().map(|sidecar| sidecar.state.get())
}

/// `true` while `state` is still the authoritative runtime state. With no
/// authority (pre-init) no racing writer exists and
/// emission proceeds, preserving pre-guard behavior. The caller must hold the
/// emission critical section — pairing this check with the emit is what makes
/// terminal-write-wins atomic. A bare read without that serialization is the
/// non-atomic reread the 2026-10-01 review rejected.
pub fn state_is_current(state: &AppState) -> bool {
    engine_sidecar().is_none_or(|sidecar| sidecar.state.emission_permits(state))
}

/// Lock-free `RuntimeApp::engine_info()` for the sidecar runtime
/// (audit §2.1 — the Hub footer's `get_engine_info` must never queue behind
/// prewarm's model load or its under-lock config/vocab sync).
///
/// Semantics vs the old `EngineRuntime::engine_info`: identical fields
/// (`mode="sidecar"`, no cold-load timing, mirrored engine version). The old
/// version also *drove* readiness via `ensure_engine_ready` once onboarding
/// completed — including restarting a dead sidecar, which can block for the
/// full model load. That side effect is preserved asynchronously: a dead
/// sidecar gets a detached `ensure_started` kick while this query returns the
/// (stale-version) snapshot immediately. `None` before
/// `APP` init — callers fall back to the locked path.
pub fn engine_info_lock_free() -> Option<EngineInfoResult> {
    let sidecar = engine_sidecar()?;
    if crate::settings::onboarding_complete() && !sidecar.supervisor.is_running() {
        let sidecar = sidecar.clone();
        let _ = std::thread::Builder::new()
            .name("engine-info-restart".to_string())
            .spawn(move || {
                if let Err(err) = sidecar.ensure_started() {
                    crate::telemetry::log_message("engine_info_restart_error", &err);
                }
            });
    }
    let engine_version = sidecar
        .engine_version
        .lock()
        .ok()
        .and_then(|slot| slot.clone());
    Some(EngineInfoResult {
        mode: "sidecar".to_string(),
        cold_load_s: None,
        engine_version,
    })
}

/// Lock-free sibling of `RuntimeApp::sidecar_is_transcribing_session` — the
/// watchdog's "is the engine still decoding this session" probe. Bounded by
/// `SIDECAR_STATE_PROBE_TIMEOUT` on the supervisor's own channel, never by
/// the runtime lock. `None` = no sidecar runtime → caller falls back.
pub fn sidecar_is_transcribing_session_lock_free(session_id: &str) -> Option<Result<bool, String>> {
    engine_sidecar().map(|sidecar| sidecar.is_transcribing_session(session_id))
}

/// Lock-free watchdog revocation (#149). `None` = no sidecar runtime.
pub fn sidecar_revoke_session_lock_free(
    session_id: &str,
) -> Option<engine_supervisor::RevocationOutcome> {
    engine_sidecar().map(|sidecar| sidecar.revoke_session_acked(session_id))
}

/// Whether the audio-length-scaled final-transcript wait (#159) for this
/// session has run out. `Some(false)` while armed and in budget; `None` when
/// no wait is armed (or no sidecar runtime) so the caller uses its fixed cap.
pub fn sidecar_final_wait_expired_lock_free(session_id: &str) -> Option<bool> {
    engine_sidecar().and_then(|sidecar| {
        sidecar
            .supervisor
            .final_wait_armed(session_id)
            .then(|| sidecar.supervisor.final_wait_expired(session_id))
    })
}

/// Staged prewarm, half 1 (audit §2.1): process spawn + ready-wait + the
/// `EngineInfo` handshake — the parts that can take the full model-load time —
/// WITHOUT the global runtime lock. `None` before `APP` init.
///
/// Half 2 is `RuntimeApp::sync_sidecar_runtime_snapshot_if_running` under a
/// short `runtime_lock()` — it only pushes the cached config/vocab diff.
pub fn prewarm_handshake_lock_free() -> Option<Result<(), String>> {
    engine_sidecar().map(|sidecar| sidecar.warm_handshake())
}

/// Generation of the current Error for the pill's expiry timer (`None` before `APP` init).
pub fn terminal_error_generation_lock_free() -> Option<u64> {
    engine_sidecar().map(|sidecar| sidecar.state.error_generation())
}
