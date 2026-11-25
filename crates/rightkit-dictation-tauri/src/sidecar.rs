//! Engine sidecar helpers for the settings, vocabulary, BYOK-key and entitlement commands: the
//! best-effort runtime snapshot sync, and the restart that applies an L3 or ASR-compute change to
//! the engine.
//!
//! An L3 change (AI polish, BYOK keys) is applied live when the engine advertises it. An ASR
//! compute change always restarts the engine, and so does a live apply that is unsupported or
//! failed. A change that arrives while a session is busy (recording, transcribing, pasting) is
//! queued, and a watcher applies it as soon as the runtime goes idle. The watcher's poll interval
//! and give-up deadline come from [`crate::host::DictationHost::sidecar_restart_timing`].
//!
//! Log labels are this module's own, as in the rest of the package. The Tauri entry points stay in
//! the application.

use std::sync::Mutex;
use std::time::Instant;

use crate::host::host;
use crate::runtime::RuntimeApp;
use crate::runtime_lock::{runtime_lock_for_commands, with_backend_transition, APP};
use crate::state::AppState;
use crate::telemetry;

// Pending flags & watcher ownership move together, including the exit/rearm boundary.
#[derive(Default)]
struct RestartState {
    pending: bool,
    full_required: bool,
    watcher_running: bool,
}
static RESTART: Mutex<RestartState> = Mutex::new(RestartState {
    pending: false,
    full_required: false,
    watcher_running: false,
});

// A queued rewarm cannot be lost when a restart races with the prior worker's exit.
struct RewarmState {
    pending: Option<String>,
    running: bool,
}
static REWARM: Mutex<RewarmState> = Mutex::new(RewarmState {
    pending: None,
    running: false,
});

/// Best-effort runtime snapshot sync after a mutation the engine should observe (vocabulary,
/// settings, ...). `context` names the caller in the log.
pub fn sync_sidecar_runtime_snapshot_best_effort(context: &str) {
    match runtime_lock_for_commands() {
        Ok(mut runtime) => {
            if let Err(err) = runtime.sync_sidecar_runtime_snapshot_if_running() {
                telemetry::log_message("sidecar_runtime_sync", &format!("{context}: {err}"));
            }
        }
        Err(err) => {
            telemetry::log_message("sidecar_runtime_sync_lock", &format!("{context}: {err}"));
        }
    }
}

/// Apply an L3 change: live when the engine is idle and supports it, otherwise a restart.
pub fn restart_sidecar_for_l3_env(context: &str) {
    apply_sidecar_change(context, false);
}

/// Apply an ASR-compute change. The profile is baked in at model load, so it always restarts.
pub fn restart_sidecar_for_asr_compute(context: &str) {
    apply_sidecar_change(context, true);
}

/// Whether a session is in flight: the states the restart must not tear down under.
fn busy_state(state: &AppState) -> bool {
    matches!(
        state,
        AppState::Recording { .. } | AppState::Transcribing { .. } | AppState::Pasting { .. }
    )
}

/// Whether no dictation is recording, transcribing or pasting. A runtime that cannot be locked
/// is not idle: the caller keeps waiting rather than act under an unknown state.
pub fn dictation_idle() -> bool {
    match runtime_lock_for_commands() {
        Ok(runtime) => !busy_state(&runtime.app_state()),
        Err(_) => false,
    }
}

/// Log-safe variant label. Deliberately not the `Debug` impl: `Pasting`, `Pasted` and
/// `CopiedFallback` embed the dictated transcript, and the logs are a support-bundle target.
fn state_label(state: &AppState) -> &'static str {
    match state {
        AppState::Armed => "armed",
        AppState::Recording { .. } => "recording",
        AppState::Transcribing { .. } => "transcribing",
        AppState::Pasting { .. } => "pasting",
        AppState::Pasted { .. } => "pasted",
        AppState::PasteSentUnconfirmed { .. } => "paste_sent_unconfirmed",
        AppState::CopiedFallback { .. } => "copied_fallback",
        AppState::Cancelled { .. } => "cancelled",
        AppState::Error { .. } => "error",
    }
}

/// Apply a change to an idle runtime: a live L3 reconfigure when the engine supports it and no
/// full restart is needed, otherwise a full restart, followed by a rewarm when the engine was up.
fn apply_to_idle_runtime(runtime: &mut RuntimeApp, context: &str, full_restart: bool) {
    if !full_restart {
        let outcome = runtime
            .engine_supervisor_handle()
            .map(|supervisor| supervisor.reconfigure_l3_live());
        match outcome {
            Some(Ok(true)) => {
                telemetry::log_message("sidecar_l3_live_reconfigure", context);
                return;
            }
            Some(Ok(false)) => telemetry::log_message(
                "sidecar_l3_live_reconfigure_unsupported",
                &format!("{context}; falling back to restart"),
            ),
            Some(Err(err)) => telemetry::log_message(
                "sidecar_l3_live_reconfigure_failed",
                &format!("{context}: {err}; falling back to restart"),
            ),
            None => {}
        }
    }
    // Only an engine that was up comes back: a change made before onboarding (or before the first
    // dictation) must not start one.
    let was_running = runtime
        .engine_supervisor_handle()
        .is_some_and(|supervisor| supervisor.is_running());
    runtime.shutdown_resident_backend();
    telemetry::log_message("sidecar_l3_env_restart", context);
    if was_running {
        spawn_engine_rewarm(context);
    }
}

/// Bring the engine back after an idle full restart, so the next dictation does not pay a cold
/// model load and the speech service returns. Runs on its own thread, so it starts once the
/// caller's runtime lock is released.
fn spawn_engine_rewarm(context: &str) {
    let mut state = REWARM.lock().unwrap_or_else(|p| p.into_inner());
    state.pending = Some(context.to_owned());
    if state.running {
        telemetry::log_message("engine_rewarm_coalesced", "a rewarm is already running");
        return;
    }
    state.running = true;
    drop(state);
    let spawned = std::thread::Builder::new()
        .name("engine-rewarm".into())
        .spawn(|| loop {
            let context = {
                let mut state = REWARM.lock().unwrap_or_else(|p| p.into_inner());
                match state.pending.take() {
                    Some(context) => context,
                    None => {
                        state.running = false;
                        return;
                    }
                }
            };
            rewarm_engine(&context);
        });
    if let Err(error) = spawned {
        REWARM.lock().unwrap_or_else(|p| p.into_inner()).running = false;
        telemetry::log_message("engine_prewarm_error", &format!("rewarm spawn: {error}"));
    }
}

/// The staged, lock-free warm of the engine boot, without the launch reveal: the shared-model
/// activation first (a no-op once the model is verified active in this process, so an
/// effective-backend flip cannot start an engine with no model), then process start and handshake,
/// then the config and vocabulary push under a short runtime lock.
fn rewarm_engine(context: &str) {
    let result = with_backend_transition(|| {
        host()
            .ensure_shared_model_active()
            .map_err(|err| format!("model: {err}"))?;
        if let Some(handshake) = crate::runtime::prewarm_handshake_lock_free() {
            handshake.map_err(|err| format!("handshake: {err}"))?;
        }
        runtime_lock_for_commands()
            .map_err(|err| format!("lock: {err}"))?
            .prewarm()
    });
    match result {
        Ok(()) => {
            telemetry::log_message("engine_rewarmed", context);
            // Schedule only after releasing the backend transition gate and the runtime lock.
            crate::wake_lab::restore_configured_listener();
        }
        Err(err) => telemetry::log_message(
            "engine_prewarm_error",
            &format!("source=rewarm:{context} {err}"),
        ),
    }
}

fn apply_sidecar_change(context: &str, full_restart: bool) {
    let result = with_backend_transition(|| {
        let mut runtime = runtime_lock_for_commands()?;
        let mut restart = RESTART.lock().unwrap_or_else(|p| p.into_inner());
        if busy_state(&runtime.app_state()) {
            restart.pending = true;
            restart.full_required |= full_restart;
            telemetry::log_message(
                "sidecar_l3_env_restart_deferred",
                &format!("{context}; state={}", state_label(&runtime.app_state())),
            );
            drop(restart);
            drop(runtime);
            spawn_l3_restart_watcher_if_needed();
            return Ok(());
        }
        let full = full_restart || restart.full_required;
        restart.pending = false;
        restart.full_required = false;
        drop(restart);
        apply_to_idle_runtime(&mut runtime, context, full);
        Ok(())
    });
    if let Err(err) = result {
        telemetry::log_message("sidecar_l3_env_restart_lock", &format!("{context}: {err}"));
    }
}

/// One bounded watcher. Try-locks avoid waiting past its deadline behind an active transition
/// or runtime operation. Ownership is released with pending state under the same mutex.
fn spawn_l3_restart_watcher_if_needed() {
    let mut restart = RESTART.lock().unwrap_or_else(|p| p.into_inner());
    if restart.watcher_running {
        return;
    }
    restart.watcher_running = true;
    drop(restart);
    let timing = host().sidecar_restart_timing();
    let spawned = std::thread::Builder::new()
        .name("sidecar-restart".into())
        .spawn(move || {
            let started = Instant::now();
            loop {
                let remaining = timing.give_up_after.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    let mut restart = RESTART.lock().unwrap_or_else(|p| p.into_inner());
                    restart.pending = false;
                    restart.full_required = false;
                    restart.watcher_running = false;
                    telemetry::log_message(
                        "sidecar_l3_env_restart_deferred_timeout",
                        &format!(
                            "gave up after {}s waiting for idle; pending restart cleared",
                            timing.give_up_after.as_secs()
                        ),
                    );
                    return;
                }
                std::thread::sleep(
                    timing
                        .poll
                        .max(std::time::Duration::from_millis(1))
                        .min(remaining),
                );
                let gate =
                    crate::runtime_lock::BACKEND_TRANSITION_GATE.get_or_init(|| Mutex::new(()));
                let Ok(_transition) = gate.try_lock() else {
                    continue;
                };
                let Some(Ok(runtime_mutex)) = APP.get() else {
                    continue;
                };
                let Ok(mut runtime) = runtime_mutex.try_lock() else {
                    continue;
                };
                let mut restart = RESTART.lock().unwrap_or_else(|p| p.into_inner());
                if !restart.pending {
                    restart.watcher_running = false;
                    return;
                }
                if !busy_state(&runtime.app_state()) {
                    let full = restart.full_required;
                    restart.pending = false;
                    restart.full_required = false;
                    restart.watcher_running = false;
                    drop(restart);
                    apply_to_idle_runtime(&mut runtime, "deferred_change", full);
                    telemetry::log_message("sidecar_l3_env_restart", "deferred_restart_executed");
                    return;
                }
            }
        });
    if let Err(error) = spawned {
        RESTART
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .watcher_running = false;
        telemetry::log_message(
            "sidecar_l3_env_restart_lock",
            &format!("watcher spawn: {error}"),
        );
    }
}
