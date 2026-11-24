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

/// The hook's native release sample as the stop/send mark: translated EXACTLY ONCE here, from
/// `rightkit_hotkeys::release_edge::NativeReleaseEdge` to the audio crate's `CaptureEdge`
/// (`version: 1`, the matching clock, seconds and nanos unchanged) and on to the wire edge
/// (`OriginalReleaseMark::edge_from_audio`, a rename of the same value). `None` (the hook's clock
/// sample failed) is `OriginalReleaseMark::missing()`: the engine fails closed, the stop is never
/// re-sampled at dispatch time and never falls back to receipt age.
#[cfg(any(target_os = "macos", target_os = "windows"))]
pub fn release_mark_from_native(
    edge: Option<rightkit_hotkeys::release_edge::NativeReleaseEdge>,
) -> crate::runtime::OriginalReleaseMark {
    use crate::runtime::OriginalReleaseMark;
    // Wake starts deliberately bind legacy identity. Select that route before constructing
    // any exact mark; absence of an epoch never selects this branch.
    if OriginalReleaseMark::current_capture_is_legacy() {
        return OriginalReleaseMark::legacy_receipt();
    }
    use rightkit_audio::capture::{CaptureClock, CaptureEdge};
    use rightkit_hotkeys::release_edge::NativeReleaseClock;
    let Some(edge) = edge else {
        return OriginalReleaseMark::missing();
    };
    let audio = CaptureEdge {
        version: 1,
        clock: match edge.clock {
            NativeReleaseClock::WasapiQpc100nsV1 => CaptureClock::WasapiQpc100nsV1,
            NativeReleaseClock::CoreAudioHostNsV1 => CaptureClock::CoreAudioHostNsV1,
        },
        seconds: edge.seconds,
        nanos: edge.nanos,
    };
    OriginalReleaseMark::from_edge(OriginalReleaseMark::edge_from_audio(audio))
}

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
        stop_dictation_and_send_inner_at, stop_dictation_inner_at, toggle_dictation_inner,
    };
    use crate::host::host;
    use crate::lifecycle::{
        currently_recording, emit_immediate_trigger_feedback, DictationTriggerGuard,
    };
    use crate::runtime::CancelSource;
    use crate::shell::{Shell, ShellEvent};
    use crate::telemetry;

    use super::{emit_onboarding_shortcut, ignored_reason, release_mark_from_native};

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
    fn wire_ptt_callbacks(shell: &Shell, exact: bool, exact_ready: bool) {
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
                if exact && !exact_ready {
                    return;
                }
                let started = if exact {
                    start_dictation_inner(&start_shell)
                } else {
                    crate::dictation_control::start_dictation_legacy_inner(&start_shell)
                };
                if let Err(err) = started {
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
                if let Err(err) = stop_dictation_inner_at(
                    stop_shell.as_ref(),
                    if exact {
                        crate::runtime::OriginalReleaseMark::missing()
                    } else {
                        crate::runtime::OriginalReleaseMark::legacy_receipt()
                    },
                ) {
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
                match stop_dictation_and_send_inner_at(
                    stop_send_shell.as_ref(),
                    if exact {
                        crate::runtime::OriginalReleaseMark::missing()
                    } else {
                        crate::runtime::OriginalReleaseMark::legacy_receipt()
                    },
                ) {
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

    /// Wires the physical push-to-talk Stop/Send release with the hook's original native edge
    /// (`FnHotkeyHolder::set_ptt_release_callback`). Must run BEFORE `fn_hotkey_start()`: the tap
    /// reads it when it is created. It replaces only the physical Stop/Send callbacks above (the
    /// same gates and logging); start, cancel and a `Cancel` end are not routed here. The mark is
    /// built once from the hook's sample and handed to the `*_at` stop/send, so nothing samples a
    /// replacement at dispatch time. Needs the host method `fn_hotkey_set_ptt_release_callback`
    /// (see the HANDOFF; host.rs is outside this donor).
    fn wire_ptt_release_callback(shell: &Shell) -> bool {
        let stop_shell = shell.clone();
        let stop_send_shell = shell.clone();
        host().fn_hotkey_set_ptt_release_callback(Some(Arc::new(
            move |end: rightkit_hotkeys::ptt::PttEnd,
                  edge: Option<rightkit_hotkeys::release_edge::NativeReleaseEdge>| {
                use rightkit_hotkeys::ptt::PttEnd;
                match end {
                    PttEnd::Stop => {
                        let released = release_mark_from_native(edge);
                        let host = host();
                        if !host.push_to_talk_enabled() || !host.ptt_dictation_allowed() {
                            return;
                        }
                        telemetry::log_input_trigger("ptt_release");
                        if let Err(err) = stop_dictation_inner_at(stop_shell.as_ref(), released) {
                            telemetry::log_message(
                                "ptt_stop_error",
                                crate::dictation_error::error_log_code(&err),
                            );
                        }
                    }
                    PttEnd::Send => {
                        let released = release_mark_from_native(edge);
                        let host = host();
                        if !host.push_to_talk_enabled() || !host.ptt_dictation_allowed() {
                            return;
                        }
                        let trigger_started = Instant::now();
                        let Some(_guard) = DictationTriggerGuard::acquire("ptt_send") else {
                            return;
                        };
                        telemetry::log_input_trigger("ptt_send");
                        match stop_dictation_and_send_inner_at(stop_send_shell.as_ref(), released) {
                            Ok(_) => {
                                emit_immediate_trigger_feedback("ptt_send", trigger_started, false)
                            }
                            Err(err) => telemetry::log_message(
                                "ptt_stop_send_error",
                                crate::dictation_error::error_log_code(&err),
                            ),
                        }
                    }
                    // Cancel keeps its own callback.
                    _ => {}
                }
            },
        )))
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
        activate_macos_fn_hotkey_route(shell, false)
    }

    /// Select before first activation. False means exact callback was not registered.
    pub fn activate_macos_fn_hotkey_exact(shell: &Shell) -> bool {
        activate_macos_fn_hotkey_route(shell, true)
    }

    static MACOS_EXACT_ROUTE: AtomicBool = AtomicBool::new(false);

    fn activate_macos_fn_hotkey_route(shell: &Shell, exact: bool) -> bool {
        let host = host();
        if MACOS_FN_HOTKEY_STARTED.load(Ordering::SeqCst) {
            host.fn_hotkey_set_enabled(true);
            return !exact || MACOS_EXACT_ROUTE.load(Ordering::SeqCst);
        }
        if !host.accessibility_trusted() {
            telemetry::log_message("fn_hotkey", "deferred until Accessibility is granted");
            return false;
        }
        let exact_ready = exact && wire_ptt_release_callback(shell);
        if exact && !exact_ready {
            telemetry::log_message(
                "fn_hotkey",
                "original PTT release callback unsupported; exact PTT disabled",
            );
        }
        if MACOS_FN_HOTKEY_STARTED.swap(true, Ordering::SeqCst) {
            host.fn_hotkey_set_enabled(true);
            return !exact || MACOS_EXACT_ROUTE.load(Ordering::SeqCst);
        }
        host.fn_hotkey_set_enabled(true);

        wire_fn_chord_callbacks(shell);
        MACOS_EXACT_ROUTE.store(exact_ready, Ordering::SeqCst);
        wire_ptt_callbacks(shell, exact, exact_ready);
        host.fn_hotkey_start();
        wire_middle_mouse_callback(shell);

        !exact || exact_ready
    }
}

#[cfg(target_os = "macos")]
pub use macos::{activate_macos_fn_hotkey, activate_macos_fn_hotkey_exact};

/// Windows: the low-level keyboard and mouse hooks and what each event does to dictation.
///
/// The hooks, their message pump, the off-thread dispatch and the push-to-talk and middle-click
/// state machines are `rightkit_hotkeys::windows::{input_hooks, ptt_hook, mouse_hook}`. This
/// module owns the dictation side: the Ctrl+Escape cancel, and what a push-to-talk start, release,
/// send, chord abort and onboarding preview do. The hook thread never runs dictation work: every
/// event below arrives on the hooks package's own dispatch thread.
///
/// The host owns the key choices, the timings (inside its `KeyboardState` and `MiddleClickState`),
/// the middle-click re-injection signature and the thread name.
#[cfg(target_os = "windows")]
pub mod windows {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::thread;

    use rightkit_hotkeys::windows::input_hooks::{
        InputHooks, InputHooksConfig, KeyboardHookConfig, MouseHookConfig,
    };
    use rightkit_hotkeys::windows::mouse_hook::MiddleClickState;
    use rightkit_hotkeys::windows::ptt_hook::{KeyboardState, PttEvent};

    use super::release_mark_from_native;
    use crate::dictation_control::{
        cancel_dictation_inner, send_dictation_detached_at, start_dictation_detached,
        stop_dictation_detached_at,
    };
    use crate::host::host;
    use crate::runtime::CancelSource;
    use crate::shell::{OnboardingPttCompletion, Shell, ShellEvent};
    use crate::telemetry;

    static HOOK_THREAD_STARTED: AtomicBool = AtomicBool::new(false);
    static HOOKS: Mutex<Option<InputHooks>> = Mutex::new(None);

    /// What the host supplies to install the hooks.
    pub struct HookSetup {
        /// The push-to-talk and Ctrl+Escape state, built by the host with its timings and sink.
        pub keyboard: &'static KeyboardState,
        /// The middle-click state, built by the host with its re-injection signature.
        pub mouse: &'static MiddleClickState,
        /// Name of the thread that installs the hooks.
        pub thread_name: &'static str,
        /// The host's telemetry sink for the hooks package.
        pub event_sink: fn(&str, &str),
    }

    /// Installs the keyboard and mouse hooks together, once. Later calls do nothing. The install
    /// runs on its own thread; a failed install leaves no hooks and is reported by the hooks
    /// package through `event_sink`.
    pub fn install_input_hooks(shell: Shell, setup: HookSetup) {
        install_input_hooks_route(shell, setup, false);
    }

    /// Explicit original-release route; select before first hook installation.
    pub fn install_input_hooks_exact(shell: Shell, setup: HookSetup) {
        install_input_hooks_route(shell, setup, true);
    }

    fn install_input_hooks_route(shell: Shell, setup: HookSetup, exact: bool) {
        if HOOK_THREAD_STARTED.swap(true, Ordering::SeqCst) {
            return;
        }
        thread::Builder::new()
            .name(setup.thread_name.to_string())
            .spawn(move || {
                let cancel_shell = shell.clone();
                let release_shell = shell.clone();
                // The release callback receives the hook's native edge, sampled on the hook
                // thread before the dispatch queue; it replaces `on_ptt` only for physical
                // Stop/Send. Start, forced ends, cancel and preview stay on `on_ptt`.
                let config = InputHooksConfig {
                    keyboard: Some(KeyboardHookConfig {
                        state: setup.keyboard,
                        on_cancel: Box::new(move || {
                            let _ = cancel_dictation_inner(
                                cancel_shell.as_ref(),
                                CancelSource::KeyboardCancelHook,
                            );
                        }),
                        on_ptt: Box::new(move |event| dispatch_ptt(&shell, event, exact)),
                    }),
                    mouse: Some(MouseHookConfig { state: setup.mouse }),
                    event_sink: setup.event_sink,
                };
                let installed = if exact {
                    InputHooks::install_with_ptt_release_edges(
                        config,
                        Box::new(move |event, edge| {
                            dispatch_ptt_release(&release_shell, event, edge)
                        }),
                    )
                } else {
                    InputHooks::install(config)
                };
                if let Ok(hooks) = installed {
                    *HOOKS.lock().unwrap_or_else(|e| e.into_inner()) = Some(hooks);
                }
            })
            .ok();
    }

    /// A physical push-to-talk Stop/Send with the hook's original native edge (`None`: the hook's
    /// clock sample failed, `missing()`, the engine fails closed). The mark is built once and
    /// carried through the `*_at` stop/send; nothing here samples at dispatch time. Any other
    /// event (not delivered here by the hooks package) goes through `dispatch_ptt`.
    fn dispatch_ptt_release(
        shell: &Shell,
        event: PttEvent,
        edge: Option<rightkit_hotkeys::release_edge::NativeReleaseEdge>,
    ) {
        match event {
            PttEvent::Stop => {
                telemetry::log_input_trigger("ptt_release");
                if let Err(err) = stop_dictation_detached_at(shell, release_mark_from_native(edge))
                {
                    telemetry::log_message(
                        "ptt_stop_error",
                        crate::dictation_error::error_log_code(&err),
                    );
                }
            }
            PttEvent::Send => {
                telemetry::log_input_trigger("ptt_send");
                if let Err(err) = send_dictation_detached_at(shell, release_mark_from_native(edge))
                {
                    telemetry::log_message(
                        "ptt_stop_send_error",
                        crate::dictation_error::error_log_code(&err),
                    );
                }
            }
            other => dispatch_ptt(shell, other, true),
        }
    }

    /// Push-to-talk events from the hook (arm timer, chord, cancel, preview, start). Runs on the
    /// hooks package's dispatch thread, never on the hook thread. Physical Stop/Send arrive through
    /// `dispatch_ptt_release` with their native edge.
    fn dispatch_ptt(shell: &Shell, event: PttEvent, exact: bool) {
        match event {
            PttEvent::Start => {
                if host().ptt_dictation_allowed() {
                    telemetry::log_input_trigger("ptt_start");
                    // Returns at once (the start runs on its own thread): this dispatch thread is
                    // serial, so a press that stayed in the start would hold the release back.
                    let started = if exact {
                        start_dictation_detached(shell)
                    } else {
                        crate::dictation_control::start_dictation_legacy_detached(shell)
                    };
                    if let Err(err) = started {
                        telemetry::log_message(
                            "ptt_start_error",
                            crate::dictation_error::error_log_code(&err),
                        );
                    }
                }
            }
            PttEvent::Stop => {
                telemetry::log_input_trigger("ptt_release");
                // The release seals the recording and returns; the key-gated stop runs on its own
                // thread so the next press is not held behind it.
                // Not reached through the release callback; fail closed rather than re-sample.
                if let Err(err) = stop_dictation_detached_at(
                    shell,
                    if exact {
                        crate::runtime::OriginalReleaseMark::missing()
                    } else {
                        crate::runtime::OriginalReleaseMark::legacy_receipt()
                    },
                ) {
                    telemetry::log_message(
                        "ptt_stop_error",
                        crate::dictation_error::error_log_code(&err),
                    );
                }
            }
            PttEvent::Send => {
                telemetry::log_input_trigger("ptt_send");
                if let Err(err) = send_dictation_detached_at(
                    shell,
                    if exact {
                        crate::runtime::OriginalReleaseMark::missing()
                    } else {
                        crate::runtime::OriginalReleaseMark::legacy_receipt()
                    },
                ) {
                    telemetry::log_message(
                        "ptt_stop_send_error",
                        crate::dictation_error::error_log_code(&err),
                    );
                }
            }
            PttEvent::Cancel(cause) => {
                telemetry::log_input_trigger("ptt_cancel");
                telemetry::log_message(
                    "ptt_abort",
                    &rightkit_hotkeys::ptt::format_abort("windows", cause),
                );
                let _ = cancel_dictation_inner(shell.as_ref(), CancelSource::PttChordAbort);
            }
            PttEvent::Preview(down) => {
                // `Ok` only means the delivery was scheduled: the shell reports the actual emit
                // through `preview_completed`. Only a failed schedule is logged here.
                if let Err(error) = shell.emit_event(ShellEvent::OnboardingPtt {
                    down,
                    complete: OnboardingPttCompletion(preview_completed),
                }) {
                    telemetry::log_message("onboarding_ptt_error", &error);
                }
            }
        }
    }

    /// The shell's report of an onboarding push-to-talk preview delivery: success is logged only
    /// after the renderer emit actually succeeded.
    fn preview_completed(down: bool, result: Result<(), String>) {
        match result {
            Ok(()) => telemetry::log_message("onboarding_ptt", if down { "down" } else { "up" }),
            Err(error) => telemetry::log_message("onboarding_ptt_error", &error),
        }
    }
}
