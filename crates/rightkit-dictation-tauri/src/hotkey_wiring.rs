//! What a dictation hotkey does once it fires: the onboarding and launch gates, the trigger guard,
//! the optimistic feedback and the call into the dictation commands.
//!
//! The shell owns the bindings (which keys, which taps, which windows) and the policy gates
//! ([`crate::host::DictationHost::onboarding_dictation_hotkeys_enabled`] and the others); this module
//! owns the order of the checks and the log trail. The shell registers the closures that call the
//! `on_*` functions here.

use crate::dictation_control::{
    stop_dictation_and_send_inner, stop_dictation_inner, toggle_dictation_inner,
};
use crate::host::host;
use crate::shell::{DictationShell, Shell, ShellEvent};
use crate::telemetry;

use std::sync::atomic::{AtomicBool, Ordering};

static HOTKEYS_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Why a dictation hotkey is discarded right now: the engine is still warming up, or onboarding is
/// not finished. Log text only.
fn ignored_reason() -> &'static str {
    if !host().launch_finished() {
        "engine_not_ready"
    } else {
        "onboarding_incomplete"
    }
}

/// Light the onboarding shortcut card for `kind` while the shortcut test owns the hotkeys.
pub fn emit_onboarding_shortcut(shell: &dyn DictationShell, kind: &'static str) {
    if host().onboarding_shortcut_test_hotkeys_enabled() {
        let _ = shell.emit_event(ShellEvent::OnboardingShortcut(
            serde_json::json!({ "kind": kind }),
        ));
    }
}

/// The global toggle hotkey fired.
pub fn on_toggle_hotkey(shell: &Shell) {
    let host = host();
    if host.onboarding_shortcut_test_hotkeys_enabled() {
        emit_onboarding_shortcut(shell.as_ref(), "toggle");
        return;
    }
    if !host.onboarding_dictation_hotkeys_enabled() {
        telemetry::log_message("hotkey_dictation_ignored", ignored_reason());
        return;
    }
    if let Err(err) = toggle_dictation_inner(shell) {
        telemetry::log_message(
            "hotkey_dictation_error",
            crate::dictation_error::error_log_code(&err),
        );
    }
}

/// The global push-to-talk hotkey was released.
pub fn on_ptt_release_hotkey(shell: &Shell) {
    let host = host();
    if host.onboarding_shortcut_test_hotkeys_enabled() {
        return;
    }
    if !host.onboarding_dictation_hotkeys_enabled() {
        telemetry::log_message("hotkey_ptt_release_ignored", ignored_reason());
        return;
    }
    if let Err(err) = stop_dictation_inner(shell.as_ref()) {
        telemetry::log_message(
            "hotkey_ptt_release_error",
            crate::dictation_error::error_log_code(&err),
        );
    }
}

/// The global stop-and-send hotkey fired.
pub fn on_send_hotkey(shell: &Shell) {
    let host = host();
    if host.onboarding_shortcut_test_hotkeys_enabled() {
        emit_onboarding_shortcut(shell.as_ref(), "send");
        return;
    }
    if !host.onboarding_dictation_hotkeys_enabled() {
        telemetry::log_message("hotkey_send_ignored", ignored_reason());
        return;
    }
    if let Err(err) = stop_dictation_and_send_inner(shell.as_ref()) {
        telemetry::log_message(
            "hotkey_send_error",
            crate::dictation_error::error_log_code(&err),
        );
    }
}

/// Register the global hotkeys once. `register` performs the OS registration; `publish_conflict`
/// receives `None` when the hotkeys are registered and `Some(error)` with the registration error
/// when they are not (the shell builds and shows its own conflict from it). Returns the
/// registration result.
pub fn register_hotkeys_if_needed(
    register: impl FnOnce() -> Result<(), String>,
    publish_conflict: impl Fn(Option<&str>),
) -> Result<(), String> {
    if HOTKEYS_REGISTERED.load(Ordering::SeqCst) {
        publish_conflict(None);
        return Ok(());
    }
    match register() {
        Ok(()) => {
            HOTKEYS_REGISTERED.store(true, Ordering::SeqCst);
            publish_conflict(None);
            Ok(())
        }
        Err(error) => {
            publish_conflict(Some(&error));
            Err(error)
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    use crate::dictation_control::{
        cancel_dictation_inner, start_dictation_inner, stop_dictation_and_send_inner,
        stop_dictation_inner, toggle_dictation_inner,
    };
    use crate::host::host;
    use crate::lifecycle::{
        currently_recording, emit_immediate_trigger_feedback, DictationTriggerGuard,
    };
    use crate::runtime::CancelSource;
    use crate::shell::{Shell, ShellEvent};
    use crate::telemetry;

    use super::{emit_onboarding_shortcut, ignored_reason};

    static MACOS_FN_HOTKEY_STARTED: AtomicBool = AtomicBool::new(false);

    /// Wires the fn-chord callbacks (toggle, stop-and-send, cancel, fn-alone).
    fn wire_fn_chord_callbacks(shell: &Shell) {
        let toggle_shell = shell.clone();
        let stop_send_shell = shell.clone();
        let fn_shell = shell.clone();
        let cancel_shell = shell.clone();
        host().fn_hotkey_set_chord_callbacks(
            Arc::new(move || {
                let host = host();
                if host.onboarding_shortcut_test_hotkeys_enabled() {
                    emit_onboarding_shortcut(toggle_shell.as_ref(), "toggle");
                    return;
                }
                if !host.onboarding_dictation_hotkeys_enabled() {
                    telemetry::log_message("fn_hotkey_dictation_ignored", ignored_reason());
                    return;
                }
                let trigger_started = Instant::now();
                let Some(_guard) = DictationTriggerGuard::acquire("fn_space_toggle") else {
                    return;
                };
                telemetry::log_input_trigger("fn_space_toggle");
                let will_record = !currently_recording();
                let result = rightkit_hotkeys::ptt::run_feedback_before_action(
                    || {
                        emit_immediate_trigger_feedback(
                            "fn_space_toggle",
                            trigger_started,
                            will_record,
                        )
                    },
                    || toggle_dictation_inner(&toggle_shell),
                );
                match result {
                    Ok(_) => {}
                    Err(err) => telemetry::log_message(
                        "fn_hotkey_dictation_error",
                        crate::dictation_error::error_log_code(&err),
                    ),
                }
            }),
            Arc::new(move || {
                let host = host();
                if host.onboarding_shortcut_test_hotkeys_enabled() {
                    emit_onboarding_shortcut(stop_send_shell.as_ref(), "send");
                    return;
                }
                if !host.onboarding_dictation_hotkeys_enabled() {
                    telemetry::log_message("fn_hotkey_stop_send_ignored", "onboarding_incomplete");
                    return;
                }
                if let Err(err) = stop_dictation_and_send_inner(stop_send_shell.as_ref()) {
                    telemetry::log_message(
                        "fn_hotkey_stop_send_error",
                        crate::dictation_error::error_log_code(&err),
                    );
                }
            }),
            Arc::new(move || {
                let host = host();
                if host.onboarding_shortcut_test_hotkeys_enabled() {
                    emit_onboarding_shortcut(cancel_shell.as_ref(), "cancel");
                    return;
                }
                if !host.onboarding_dictation_hotkeys_enabled() {
                    telemetry::log_message("fn_hotkey_cancel_ignored", "onboarding_incomplete");
                    return;
                }
                if let Err(err) =
                    cancel_dictation_inner(cancel_shell.as_ref(), CancelSource::FnEscape)
                {
                    telemetry::log_message(
                        "fn_hotkey_cancel_error",
                        crate::dictation_error::error_log_code(&err),
                    );
                }
            }),
            // fn (alone) press and release: only surfaced to the onboarding chord UI.
            Arc::new(move |down: bool| {
                if host().onboarding_shortcut_test_hotkeys_enabled() {
                    let _ = fn_shell.emit_event(ShellEvent::OnboardingFn(
                        serde_json::json!({ "down": down }),
                    ));
                }
            }),
        );
    }

    /// Wires the push-to-talk (Control or Option hold) callbacks.
    fn wire_ptt_callbacks(shell: &Shell) {
        let start_shell = shell.clone();
        let stop_shell = shell.clone();
        let stop_send_shell = shell.clone();
        let cancel_shell = shell.clone();
        host().fn_hotkey_set_ptt_callbacks(
            Arc::new(move || {
                let host = host();
                if !host.push_to_talk_enabled() {
                    return;
                }
                if !host.ptt_dictation_allowed() {
                    return;
                }
                if let Err(err) = start_dictation_inner(&start_shell) {
                    telemetry::log_message(
                        "ptt_start_error",
                        crate::dictation_error::error_log_code(&err),
                    );
                }
            }),
            Arc::new(move || {
                let host = host();
                if !host.push_to_talk_enabled() {
                    return;
                }
                if !host.ptt_dictation_allowed() {
                    return;
                }
                telemetry::log_input_trigger("ptt_release");
                if let Err(err) = stop_dictation_inner(stop_shell.as_ref()) {
                    telemetry::log_message(
                        "ptt_stop_error",
                        crate::dictation_error::error_log_code(&err),
                    );
                }
            }),
            Arc::new(move || {
                let host = host();
                if !host.push_to_talk_enabled() {
                    return;
                }
                if !host.ptt_dictation_allowed() {
                    return;
                }
                let trigger_started = Instant::now();
                let Some(_guard) = DictationTriggerGuard::acquire("ptt_send") else {
                    return;
                };
                telemetry::log_input_trigger("ptt_send");
                match stop_dictation_and_send_inner(stop_send_shell.as_ref()) {
                    Ok(_) => emit_immediate_trigger_feedback("ptt_send", trigger_started, false),
                    Err(err) => telemetry::log_message(
                        "ptt_stop_send_error",
                        crate::dictation_error::error_log_code(&err),
                    ),
                }
            }),
            // Abort: a chord formed mid-hold (Ctrl+C and the like). Preserve audio through
            // CancelToHistory while letting the user's chord pass through.
            Arc::new(move || {
                let trigger_started = Instant::now();
                let Some(_guard) = DictationTriggerGuard::acquire("ptt_cancel") else {
                    return;
                };
                telemetry::log_input_trigger("ptt_cancel");
                match cancel_dictation_inner(cancel_shell.as_ref(), CancelSource::PttChordAbort) {
                    Ok(_) => emit_immediate_trigger_feedback("ptt_cancel", trigger_started, false),
                    Err(err) => telemetry::log_message(
                        "ptt_cancel_error",
                        crate::dictation_error::error_log_code(&err),
                    ),
                }
            }),
        );
        host().fn_hotkey_apply_ptt_settings();
    }

    /// Wires the middle-mouse toggle callback.
    fn wire_middle_mouse_callback(shell: &Shell) {
        // Middle-mouse tap: spawned once; enabled state checked per click.
        let mm_shell = shell.clone();
        host().middle_mouse_set_toggle_callback(Arc::new(move || {
            let host = host();
            if !host.middle_mouse_recording_enabled() {
                return;
            }
            // During the onboarding control test the native tap suppresses the click, so the
            // webview cannot see it: emit an event to light the card instead (mirrors the fn-chord
            // onboarding signal).
            if host.onboarding_shortcut_test_hotkeys_enabled() {
                let _ = mm_shell.emit_event(ShellEvent::OnboardingMiddleClick);
                return;
            }
            if !host.onboarding_dictation_hotkeys_enabled() {
                return;
            }
            let trigger_started = Instant::now();
            let Some(_guard) = DictationTriggerGuard::acquire("middle_mouse") else {
                return;
            };
            telemetry::log_input_trigger("middle_mouse");
            let will_record = !currently_recording();
            let result = rightkit_hotkeys::ptt::run_feedback_before_action(
                || emit_immediate_trigger_feedback("middle_mouse", trigger_started, will_record),
                || toggle_dictation_inner(&mm_shell),
            );
            match result {
                Ok(_) => {}
                Err(err) => telemetry::log_message(
                    "middle_mouse_toggle_error",
                    crate::dictation_error::error_log_code(&err),
                ),
            }
        }));
        host().middle_mouse_start();
    }

    /// Start the macOS fn-chord, push-to-talk and middle-mouse taps once Accessibility is granted.
    /// Returns false while the grant is missing; a repeat call after the taps started only
    /// re-enables the fn tap.
    pub fn activate_macos_fn_hotkey(shell: &Shell) -> bool {
        let host = host();
        if MACOS_FN_HOTKEY_STARTED.load(Ordering::SeqCst) {
            host.fn_hotkey_set_enabled(true);
            return true;
        }
        if !host.accessibility_trusted() {
            telemetry::log_message("fn_hotkey", "deferred until Accessibility is granted");
            return false;
        }
        if MACOS_FN_HOTKEY_STARTED.swap(true, Ordering::SeqCst) {
            host.fn_hotkey_set_enabled(true);
            return true;
        }
        host.fn_hotkey_set_enabled(true);

        wire_fn_chord_callbacks(shell);
        wire_ptt_callbacks(shell);
        host.fn_hotkey_start();
        wire_middle_mouse_callback(shell);

        true
    }
}

#[cfg(target_os = "macos")]
pub use macos::activate_macos_fn_hotkey;
