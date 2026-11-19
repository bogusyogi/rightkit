//! The dictation lifecycle around the runtime: the trigger double-fire guard, the optimistic
//! feedback of a trigger, and the order in which a committed state reaches the mirrors, the pill and
//! the history notification.
//!
//! # Trigger latency and double fire
//!
//! The audible start sound is gated behind focused-text-input probing, record-target capture and
//! the engine handshake, all of which happen deep inside the start path and can take several
//! seconds. Hearing nothing, a user can press the trigger again while the first press's slow path
//! is still in flight, opening a second session if nothing drops the duplicate. The trigger call
//! sites therefore (1) emit the pill and sound feedback synchronously, before the slow path, (2) hold
//! a guard for the duration of the slow path so a second trigger is dropped, not queued, and (3) log
//! when feedback still exceeds a 300 ms budget so a regression shows in telemetry.

use rightkit_transcript_library::delivery::TargetSnapshot;

use crate::host::host;
use crate::runtime::{self, PreparedStop};
use crate::runtime_lock::runtime_lock;
use crate::shell::{DictationShell, ShellEvent};
use crate::state::AppState;
use crate::state_emission::{self, EmissionHost};
use crate::telemetry;

#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "macos")]
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
static DICTATION_TRIGGER_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
/// Lock-free mirror written whenever authoritative application state is emitted. Trigger callbacks
/// use it only for immediate optimistic feedback.
#[cfg(target_os = "macos")]
static PRESENTATION_RECORDING: AtomicBool = AtomicBool::new(false);

/// Budget from raw input event to audible and visual feedback. Not enforced, only logged.
#[cfg(target_os = "macos")]
pub const INPUT_LATENCY_BUDGET: Duration = Duration::from_millis(300);

/// RAII guard for one in-flight dictation trigger. `Drop` clears the flag on every exit path of
/// the closure that holds it (success, `Err` or an early `return`), so a failed start can never
/// permanently wedge later triggers.
#[cfg(target_os = "macos")]
pub struct DictationTriggerGuard;

#[cfg(target_os = "macos")]
impl DictationTriggerGuard {
    /// Claim the trigger. `None` means a previous trigger's start or stop is still in flight; the
    /// caller must drop this event outright rather than queue it as a toggle or a stop.
    pub fn acquire(source: &str) -> Option<Self> {
        if DICTATION_TRIGGER_IN_FLIGHT.swap(true, Ordering::SeqCst) {
            telemetry::log_message(
                "input_trigger_ignored",
                &format!("reason=start_in_flight source={source}"),
            );
            return None;
        }
        Some(Self)
    }
}

#[cfg(target_os = "macos")]
impl Drop for DictationTriggerGuard {
    fn drop(&mut self) {
        DICTATION_TRIGGER_IN_FLIGHT.store(false, Ordering::SeqCst);
        host().notify_runtime_idle();
    }
}

/// Best-effort peek at whether a session is currently recording, used only to pick which optimistic
/// pill look to show immediately. The authoritative state event from the inner call corrects it
/// shortly after if the guess was wrong.
#[cfg(target_os = "macos")]
pub fn currently_recording() -> bool {
    PRESENTATION_RECORDING.load(Ordering::SeqCst)
}

/// Record whether the authoritative state is `Recording`. Call from the shell's state mirror, once
/// per committed state, before the other mirrors.
#[cfg(target_os = "macos")]
pub fn note_presentation_state(recording: bool) {
    PRESENTATION_RECORDING.store(recording, Ordering::SeqCst);
    if !recording {
        host().notify_runtime_idle();
    }
}

/// Lock-free background-work gate. Cloud sync uses this before every local history merge or
/// snapshot so recording state always wins over maintenance.
#[cfg(target_os = "macos")]
pub fn dictation_interaction_active() -> bool {
    PRESENTATION_RECORDING.load(Ordering::SeqCst)
        || DICTATION_TRIGGER_IN_FLIGHT.load(Ordering::SeqCst)
}

/// Emit pill feedback for a trigger SYNCHRONOUSLY, before any of the slow work that lives inside
/// the inner dictation calls: that work must never gate the user's acknowledgment. `recording` is
/// true for a trigger that begins a new capture, false for one that ends or cancels one.
#[cfg(target_os = "macos")]
pub fn emit_immediate_trigger_feedback(source: &str, trigger_started: Instant, recording: bool) {
    let host = host();
    if host.onboarding_surfaces_enabled() {
        host.show_pill_cue(if recording {
            crate::shell::PillCue::Recording
        } else {
            crate::shell::PillCue::Processing
        });
    }
    let elapsed = trigger_started.elapsed();
    if elapsed > INPUT_LATENCY_BUDGET {
        telemetry::log_message(
            "input_latency_exceeded",
            &format!("ms={} source={source}", elapsed.as_millis()),
        );
    }
}

// Emission serialization is `state_emission`; this module supplies the values.
static EMISSION_HOST: EmissionHost = EmissionHost {
    is_current: runtime::state_is_current,
    event_sink: telemetry::log_message,
    log_state: log_state_line,
};

fn log_state_line(state: &AppState) {
    telemetry::log_state(state);
}

/// Mirror emission for a committed runtime state. Guarded: the state emits only while it is still
/// the authoritative `SharedState`.
pub fn emit_state(shell: &dyn DictationShell, state: &AppState) {
    state_emission::emit_mirrors(&EMISSION_HOST, state, |state| {
        shell.emit_state_mirrors(state)
    });
}

fn show_pill_for_state(state: &AppState) {
    let host = host();
    if host.onboarding_surfaces_enabled() {
        host.show_pill_for_state(state);
    }
}

/// Pill-only emission for a committed runtime state, for command paths that already emitted
/// mirrors through [`emit_observed_states`] and for the sidecar handlers' deferred terminal pushes.
pub fn emit_pill_state(state: &AppState) {
    state_emission::emit_pill(&EMISSION_HOST, state, show_pill_for_state);
}

/// Mirrors then pill for every state a sidecar event produced; the last terminal state is returned
/// so the caller pushes its final pill look after the batch.
pub fn emit_sidecar_observed_states(
    shell: &dyn DictationShell,
    observed: &[AppState],
) -> Option<AppState> {
    state_emission::emit_observed_sidecar(
        &EMISSION_HOST,
        observed,
        |state| shell.emit_state_mirrors(state),
        show_pill_for_state,
    )
}

/// Record a state a runtime call produced; the caller emits it after the runtime lock drops.
pub fn observe_state(observed: &mut Vec<AppState>, state: &AppState) {
    observed.push(state.clone());
}

/// Send a prepared stop to the backend, then (when the stop captures a destination) capture it.
pub fn send_prepared_stop(
    prepared: PreparedStop,
    observed: &mut Vec<AppState>,
) -> Result<AppState, String> {
    let capture_destination = prepared.captures_destination();
    match prepared.send() {
        Ok(state) => {
            // Stop reaches the capture backend first. AX or UIA destination capture happens only
            // after audio stop dispatch succeeds.
            if capture_destination {
                runtime_lock()?.capture_stop_destination();
                telemetry::utterance_timeline::mark(|t| &mut t.destination_captured_ms);
            }
            Ok(state)
        }
        Err(error) => {
            let message = error.message;
            match runtime_lock() {
                Ok(mut runtime) => {
                    let state =
                        runtime.fail_prepared_stop(&error.session_id, message.clone(), |state| {
                            observe_state(observed, state)
                        });
                    if matches!(state, AppState::Cancelled { .. }) {
                        return Ok(state);
                    }
                }
                Err(lock_error) => telemetry::log_message(
                    "stop_failure_rollback_lock_error",
                    &format!(
                        "session={} code={}",
                        error.session_id,
                        crate::dictation_error::error_log_code(&lock_error)
                    ),
                ),
            }
            Err(message)
        }
    }
}

/// Full runtime-state emission: the forensic `state_changed` line is unconditional, mirrors and the
/// mapped pill emit only while the state is authoritative.
pub fn emit_runtime_state(shell: &dyn DictationShell, state: &AppState) {
    state_emission::emit_full(
        &EMISSION_HOST,
        state,
        |state| shell.emit_state_mirrors(state),
        show_pill_for_state,
    );
}

/// [`emit_runtime_state`] for each observed state, in order.
pub fn emit_observed_states(shell: &dyn DictationShell, observed: &[AppState]) {
    for state in observed {
        emit_runtime_state(shell, state);
    }
}

/// Announce a delivery recorded in history and, when the state leaves the transcript on the
/// clipboard without a confirmed insertion, offer the clipboard-recovery popup.
pub fn emit_delivery_recorded(shell: &dyn DictationShell, state: &AppState) {
    if let Some(delivery_id) = crate::state_decisions::delivered_record_id(state) {
        emit_delivery_recorded_from_id(shell, delivery_id, None);
    }
    // The mini-clipboard fallback popup is NOT gated on onboarding: if the pill is alive, the
    // clipboard fallback is fully active (a dropped dictation must always be recoverable).
    // Suppressed only while the onboarding dictation test is running, so that flow can own the
    // screen.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    if !host().onboarding_dictation_test_active() {
        if let Some(transcript) = crate::state_decisions::clipboard_recovery_transcript(state) {
            if let Err(err) = shell.show_clipboard_recovery(transcript) {
                telemetry::log_message(
                    "pill_clipboard_fallback_error",
                    crate::dictation_error::error_log_code(&err),
                );
            }
        }
    }
}

/// Announce a delivery recorded in history to the renderer.
pub fn emit_delivery_recorded_from_id(
    shell: &dyn DictationShell,
    delivery_id: &str,
    target: Option<&TargetSnapshot>,
) {
    telemetry::log_message("ui_delivery_recorded", delivery_id);
    host().note_delivery_recorded(target);
    let payload = crate::start_sequence::delivery_recorded_payload(delivery_id, target);
    if let Err(err) = shell.emit_event(ShellEvent::DeliveryRecorded(payload)) {
        telemetry::log_message(
            "emit_delivery_recorded_error",
            crate::dictation_error::error_log_code(&err),
        );
    }
}
