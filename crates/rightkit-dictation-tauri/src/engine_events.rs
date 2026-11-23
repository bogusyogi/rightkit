//! The engine event consumer: the thread that receives the engine sidecar's events and routes each
//! to the runtime and the shell, the transcribe watchdog tick that rides on it, and the engine
//! prewarm that boots it.
//!
//! The shell supplies the pill looks ([`crate::shell::PillCue`]), the renderer events
//! ([`crate::shell::ShellEvent`]) and the per-app operations of [`crate::shell::DictationShell`];
//! the order of runtime calls, state emission and notices is here.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use rightkit_transcript_library::delivery::{DeliveryRecord, TargetSnapshot};

use crate::dictation_control::start_dictation_inner;
use crate::engine::EngineTimeline;
use crate::engine_event_consumer::spawn_receiver_consumer;
use crate::engine_process::EngineEvent;
use crate::host::host;
use crate::lifecycle::{
    emit_delivery_recorded, emit_observed_states, emit_pill_state, emit_sidecar_observed_states,
    emit_state, observe_state,
};
use crate::runtime_lock::{
    runtime_lock, watchdog_progress_event, watchdog_tick, ENGINE_EVENT_WATCHDOG_TICK,
};
use crate::shell::{PillCue, Shell, ShellEvent};
use crate::start_sequence::{note_capture_failed, note_capture_live};
use crate::state::AppState;
use crate::telemetry;

type EngineEventReceiver = std::sync::mpsc::Receiver<EngineEvent>;

static ENGINE_EVENT_CONSUMER_STARTED: AtomicBool = AtomicBool::new(false);

// A failed OS thread spawn must not consume the only receiver. Keep ownership here until a later
// startup retry can hand it to a successfully spawned consumer.
static ENGINE_EVENT_RECEIVER_RETRY: OnceLock<Mutex<Option<EngineEventReceiver>>> = OnceLock::new();

fn engine_event_receiver_retry_slot() -> &'static Mutex<Option<EngineEventReceiver>> {
    ENGINE_EVENT_RECEIVER_RETRY.get_or_init(|| Mutex::new(None))
}

fn take_engine_event_receiver_for_consumer() -> Option<EngineEventReceiver> {
    if let Ok(mut retry) = engine_event_receiver_retry_slot().lock() {
        if retry.is_some() {
            return retry.take();
        }
    }
    let mut runtime = runtime_lock().ok();
    runtime
        .as_mut()
        .and_then(|r| r.take_engine_event_receiver())
}

fn restore_engine_event_receiver_for_retry(receiver: EngineEventReceiver) {
    let mut retry = match engine_event_receiver_retry_slot().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if retry.is_none() {
        *retry = Some(receiver);
    } else {
        telemetry::log_message(
            "engine_event_consumer_receiver_restore_skipped",
            "retry receiver already present",
        );
    }
}

fn handle_wake_fired(
    shell: &Shell,
    ts: f64,
    fire_count: Option<u64>,
    model: Option<String>,
    threshold: Option<f32>,
    score: Option<f32>,
) {
    telemetry::log_message(
        "wake_fired",
        &format!(
            "ts={ts:.3} fire_count={fire_count:?} \
             model_present={} threshold={threshold:?} score={score:?}",
            model.is_some()
        ),
    );
    let _ = shell.emit_event(ShellEvent::WakeFired(serde_json::json!({
        "ts": ts,
        "fire_count": fire_count,
        "model": model,
        "threshold": threshold,
        "score": score,
    })));
    if !host().onboarding_surfaces_enabled() {
        telemetry::log_message("wake_dictation_ignored", "onboarding_incomplete");
        return;
    }
    // Off the consumer thread: the start waits for the first RecordingLevel, which only this
    // consumer can deliver.
    let shell = shell.clone();
    host().spawn_blocking(Box::new(move || {
        if let Err(err) = start_dictation_inner(&shell) {
            telemetry::log_message(
                "wake_dictation_error",
                crate::dictation_error::error_log_code(&err),
            );
        }
    }));
}

fn handle_recording_level(shell: &Shell, session_id: String, level: f32) {
    // Validated PCM reached the worker: capture is live for this session.
    note_capture_live(&session_id);
    let lvl = level.clamp(0.0, 1.0);
    let host = host();
    if host.recording_level_meter_active() {
        let _ = shell.emit_event(ShellEvent::RecordingLevel(serde_json::json!({
            "session_id": session_id,
            "level": lvl,
        })));
    }
    // Drive the pill's reactive bars (the level was once only sent to the webview meter, never to
    // the pill).
    if host.onboarding_surfaces_enabled() {
        host.show_pill_cue(PillCue::Voice { level: lvl });
    }
}

fn handle_transcript_partial(session_id: String, text: String, revision: u64) {
    // Session authority and the write share the runtime mutex with stop/cancel/start.
    // Never dispatch to the host before this guard: a stale session could replace
    // command-host's current binding, including its stopped/uncertain state.
    let outcome = {
        let mut runtime = match runtime_lock() {
            Ok(r) => r,
            Err(e) => {
                telemetry::log_message(
                    "draft_upsert_lock_error",
                    crate::dictation_error::error_log_code(&e),
                );
                return;
            }
        };
        runtime.stream_transcript_partial(&session_id, &text, revision);
        // Recovery drafts remain eligible after stop while final transcription drains.
        runtime.upsert_transcript_draft(session_id, text, revision)
    };
    if let Err(err) = outcome {
        telemetry::log_message(
            "draft_upsert_error",
            crate::dictation_error::error_log_code(&err),
        );
    }
}

fn handle_transcribing_started(
    shell: &Shell,
    session_id: String,
    stop_target: Option<TargetSnapshot>,
) {
    // Hands-free auto-stop: the sidecar recognized the command and is transcribing. Leave
    // Recording and show "processing" NOW, so the pill stops the instant the command fires instead
    // of after the ~1s decode. The transcript follows via TranscriptFinal.
    let mut observed = Vec::new();
    let outcome = {
        let mut runtime = match runtime_lock() {
            Ok(r) => r,
            Err(e) => {
                telemetry::log_message(
                    "sidecar_transcribing_lock_error",
                    crate::dictation_error::error_log_code(&e),
                );
                return;
            }
        };
        runtime.accept_sidecar_transcribing_started(session_id, stop_target, |state| {
            observe_state(&mut observed, state)
        })
    };
    emit_observed_states(shell.as_ref(), &observed);
    if let Err(err) = outcome {
        telemetry::log_message(
            "sidecar_transcribing_error",
            crate::dictation_error::error_log_code(&err),
        );
    }
}

fn handle_engine_error(shell: &Shell, session_id: String, message: String) {
    note_capture_failed(&session_id, &message);
    let mut observed = Vec::new();
    let outcome = {
        let mut runtime = match runtime_lock() {
            Ok(r) => r,
            Err(e) => {
                telemetry::log_message(
                    "sidecar_error_lock_error",
                    crate::dictation_error::error_log_code(&e),
                );
                return;
            }
        };
        runtime.accept_sidecar_error(session_id, message, |state| {
            observe_state(&mut observed, state)
        })
    };
    emit_observed_states(shell.as_ref(), &observed);
    if let Err(err) = outcome {
        telemetry::log_message(
            "sidecar_error_apply_error",
            crate::dictation_error::error_log_code(&err),
        );
    }
    crate::wake_lab::start_configured(shell.clone());
}

fn handle_delivery_completed(
    shell: &Shell,
    session_id: String,
    record: DeliveryRecord,
    send_enter: bool,
    timeline: Option<EngineTimeline>,
) {
    telemetry::utterance_timeline::final_received(&session_id, timeline);
    let timeline_session = session_id.clone();
    let mut observed = Vec::new();
    let outcome = {
        let mut runtime = match runtime_lock() {
            Ok(r) => r,
            Err(e) => {
                telemetry::log_message(
                    "sidecar_delivery_lock_error",
                    crate::dictation_error::error_log_code(&e),
                );
                return;
            }
        };
        // AutoStop desync: a hands-free auto-fire ends recording on the worker thread, so the
        // shell can sit in Recording for the whole decode window with the LL hook's
        // RECORDING_ACTIVE still set, swallowing Ctrl+Space and Ctrl+Esc. The sidecar has now
        // delivered, so recording is definitively over: clear the flag immediately.
        #[cfg(target_os = "windows")]
        if runtime.is_active_session(&session_id)
            && matches!(runtime.app_state(), AppState::Recording { .. })
        {
            host().set_recording_active_hook(false);
        }
        runtime.accept_sidecar_delivery(session_id, record, send_enter, |state| {
            observe_state(&mut observed, state)
        })
    };
    // Processing may bridge Recording -> Transcribing -> Pasting. Emit it after runtime_lock drops,
    // but hold terminal Pasted/Sent until history notification has been published below.
    let terminal_pill_state = emit_sidecar_observed_states(shell.as_ref(), &observed);
    telemetry::utterance_timeline::emit(&timeline_session);
    let surfaces = host().onboarding_surfaces_enabled();
    match outcome {
        Ok(state) => {
            emit_delivery_recorded(shell.as_ref(), &state);
            if surfaces {
                emit_pill_state(&state);
                shell.note_completed_transcript();
            }
        }
        Err(err) => {
            // OS delivery may already have succeeded before a later history write fails. Do not
            // strand the pill in Processing or falsely report delivery failure when the terminal
            // runtime state proves the paste or send completed.
            if surfaces {
                if let Some(state) = terminal_pill_state.as_ref() {
                    emit_pill_state(state);
                }
            }
            telemetry::log_message(
                "sidecar_delivery_error",
                crate::dictation_error::error_log_code(&err),
            );
        }
    }
    crate::wake_lab::start_configured(shell.clone());
}

fn handle_transcript_final(
    shell: &Shell,
    session_id: String,
    text: String,
    send_enter: bool,
    raw_text: Option<String>,
    recording_ms: Option<u64>,
    cancelled: bool,
    context_request_attempted: Option<bool>,
    context_original_binding: Option<bool>,
    capture_warning: Option<String>,
    timeline: Option<EngineTimeline>,
) {
    telemetry::utterance_timeline::final_received(&session_id, timeline);
    let timeline_session = session_id.clone();
    telemetry::log_transcript_final(&session_id, &text, raw_text.as_deref(), recording_ms);
    let mut observed = Vec::new();
    let mut delivery_inserted = false;
    let outcome = {
        let mut runtime = match runtime_lock() {
            Ok(r) => r,
            Err(e) => {
                telemetry::log_message(
                    "sidecar_transcript_lock_error",
                    crate::dictation_error::error_log_code(&e),
                );
                return;
            }
        };
        // AutoStop desync (see DeliveryCompleted): clear the stale LL-hook recording flag the
        // moment the sidecar reports the final transcript.
        #[cfg(target_os = "windows")]
        if runtime.is_active_session(&session_id)
            && matches!(runtime.app_state(), AppState::Recording { .. })
        {
            host().set_recording_active_hook(false);
        }
        runtime.accept_sidecar_transcript(
            session_id,
            text,
            send_enter,
            raw_text,
            recording_ms,
            cancelled,
            capture_warning,
            context_request_attempted,
            context_original_binding,
            || {
                delivery_inserted = true;
                telemetry::utterance_timeline::mark(|t| &mut t.paste_confirmed_ms);
            },
            |state| observe_state(&mut observed, state),
        )
    };
    let host = host();
    if delivery_inserted {
        telemetry::log_message("delivery_progress", "phase=inserted");
        if host.onboarding_surfaces_enabled() {
            host.show_pill_cue(PillCue::Pasted);
        }
        telemetry::utterance_timeline::mark(|t| &mut t.pill_pasted_ms);
    }
    // One `utterance_timeline` per dictation, for every outcome (pasted, unconfirmed, copy
    // fallback) with whatever offsets exist.
    telemetry::utterance_timeline::emit(&timeline_session);
    let terminal_pill_state = emit_sidecar_observed_states(shell.as_ref(), &observed);
    match outcome {
        Ok(state) => {
            emit_delivery_recorded(shell.as_ref(), &state);
            if host.onboarding_surfaces_enabled() {
                emit_pill_state(&state);
            }
            if delivery_inserted && host.onboarding_surfaces_enabled() {
                shell.note_completed_transcript();
            }
            // One-time cancel hint: a cancel that just finalized to history is the trigger point.
            if matches!(state, AppState::Cancelled { .. }) {
                shell.maybe_show_cancel_saved_hint();
            }
        }
        Err(err) => {
            if host.onboarding_surfaces_enabled() {
                if let Some(state) = terminal_pill_state.as_ref() {
                    emit_pill_state(state);
                }
            }
            telemetry::log_message(
                "sidecar_transcript_error",
                crate::dictation_error::error_log_code(&err),
            );
        }
    }
    crate::wake_lab::start_configured(shell.clone());
}

fn handle_session_reset(
    shell: &Shell,
    session_id: String,
    command_dispatched: Option<String>,
    command_detail: Option<String>,
    command_failed: Option<String>,
) {
    let command_result = command_dispatched.is_some() || command_failed.is_some();
    let screenshot_command_result = command_dispatched.as_deref() == Some("screenshot")
        || command_failed.as_deref().is_some_and(|message| {
            let message = message.to_ascii_lowercase();
            message.contains("screenshot")
                || message.contains("printscreen")
                || message.contains("print screen")
        });
    telemetry::log_message(
        "sidecar_session_reset",
        &format!(
            "session={session_id} command_result={command_result} screenshot_command_result={screenshot_command_result}"
        ),
    );
    let mut observed = Vec::new();
    let outcome = {
        let mut runtime = match runtime_lock() {
            Ok(r) => r,
            Err(e) => {
                telemetry::log_message(
                    "sidecar_reset_lock_error",
                    crate::dictation_error::error_log_code(&e),
                );
                return;
            }
        };
        crate::media_control::restore_for_session(&session_id, "session_reset");
        if !runtime.is_active_session(&session_id) {
            telemetry::log_message(
                "sidecar_reset_dropped_stale",
                &format!("session={session_id}"),
            );
            return;
        }
        runtime.accept_sidecar_reset(session_id, |state| observe_state(&mut observed, state))
    };
    for state in &observed {
        telemetry::log_state(state);
        emit_state(shell.as_ref(), state);
        if !command_result {
            emit_pill_state(state);
        } else {
            telemetry::log_message("pill_reset_suppressed", "reason=command_result");
        }
    }
    if let Err(err) = outcome {
        telemetry::log_message(
            "sidecar_reset_error",
            crate::dictation_error::error_log_code(&err),
        );
    }
    let host = host();
    if let Some(action) = command_dispatched.as_deref() {
        telemetry::log_message(
            "command_success_dispatched",
            // The detail names an app, a shortcut or a saved file: only whether there was one is
            // logged.
            &format!(
                "action_present=true detail_present={}",
                command_detail.is_some()
            ),
        );
        if action == "screenshot" {
            shell.show_screenshot_notice(command_detail.as_deref());
        } else if host.onboarding_surfaces_enabled() {
            host.show_pill_cue(PillCue::CommandSucceeded {
                action: action.to_string(),
                detail: command_detail.clone(),
            });
        }
    }
    // A recognized command that failed to dispatch must be VISIBLE: flash the pill error AFTER the
    // reset so the armed/idle event does not overwrite it.
    if let Some(message) = command_failed {
        // The message can name an app or a shortcut: log a code, show the label (the shell's).
        let accessibility_denied = message.contains("accessibility");
        let code = if accessibility_denied {
            "accessibility"
        } else {
            crate::dictation_error::error_log_code(&message)
        };
        telemetry::log_message("command_dispatch_failed", &format!("code={code}"));
        if host.onboarding_surfaces_enabled() {
            host.show_pill_cue(PillCue::CommandFailed {
                accessibility_denied,
            });
        }
    }
    crate::wake_lab::start_configured(shell.clone());
}

/// Start the engine event consumer once. It owns the receiver for the process; a failed thread
/// spawn keeps the receiver so a later boot can retry.
pub fn start_engine_event_consumer(shell: Shell) {
    if ENGINE_EVENT_CONSUMER_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let event_receiver = take_engine_event_receiver_for_consumer();
    let Some(rx) = event_receiver else {
        ENGINE_EVENT_CONSUMER_STARTED.store(false, Ordering::SeqCst);
        return;
    };

    let spawn_result = spawn_receiver_consumer(
        rx,
        move |rx| {
            // Watchdog: if the runtime sits in `Transcribing` with no engine events for
            // TRANSCRIBE_STUCK_AFTER, a terminal event never arrived (supersession race, dropped
            // event, or a sidecar crash mid-decode): recover the pill. Any real event resets
            // `transcribing_since`, so a normal multi-second decode is never force-reset. Only an
            // event for the tracked transcribing session counts as progress, and the tick also runs
            // while other traffic (levels, wake, stale sessions) keeps arriving, so that traffic
            // cannot postpone recovery indefinitely.
            let mut transcribing_since: Option<(String, Instant)> = None;
            let mut last_watchdog_tick = Instant::now();
            loop {
                let event = match rx.recv_timeout(ENGINE_EVENT_WATCHDOG_TICK) {
                    Ok(event) => {
                        if watchdog_progress_event(&transcribing_since, &event) {
                            transcribing_since = None;
                        }
                        if last_watchdog_tick.elapsed() >= ENGINE_EVENT_WATCHDOG_TICK {
                            watchdog_tick(&mut transcribing_since, |observed| {
                                emit_observed_states(shell.as_ref(), observed)
                            });
                            last_watchdog_tick = Instant::now();
                        }
                        event
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        watchdog_tick(&mut transcribing_since, |observed| {
                            emit_observed_states(shell.as_ref(), observed)
                        });
                        last_watchdog_tick = Instant::now();
                        continue;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                };
                match event {
                    EngineEvent::WakeFired {
                        ts,
                        fire_count,
                        model,
                        threshold,
                        score,
                    } => {
                        handle_wake_fired(&shell, ts, fire_count, model, threshold, score);
                    }
                    EngineEvent::RecordingLevel { session_id, level } => {
                        handle_recording_level(&shell, session_id, level);
                    }
                    EngineEvent::TranscriptPartial {
                        session_id,
                        text,
                        revision,
                        ..
                    } => {
                        handle_transcript_partial(session_id, text, revision);
                    }
                    EngineEvent::TranscribingStarted {
                        session_id,
                        stop_target,
                    } => {
                        handle_transcribing_started(&shell, session_id, stop_target);
                    }
                    EngineEvent::EngineError {
                        session_id,
                        message,
                    } => {
                        handle_engine_error(&shell, session_id, message);
                    }
                    EngineEvent::DeliveryCompleted {
                        session_id,
                        record,
                        send_enter,
                        timeline,
                    } => {
                        handle_delivery_completed(&shell, session_id, record, send_enter, timeline);
                    }
                    EngineEvent::TranscriptFinal {
                        session_id,
                        text,
                        send_enter,
                        raw_text,
                        recording_ms,
                        cancelled,
                        context_request_attempted,
                        context_original_binding,
                        capture_warning,
                        timeline,
                    } => {
                        handle_transcript_final(
                            &shell,
                            session_id,
                            text,
                            send_enter,
                            raw_text,
                            recording_ms,
                            cancelled,
                            context_request_attempted,
                            context_original_binding,
                            capture_warning,
                            timeline,
                        );
                    }
                    EngineEvent::SessionReset {
                        session_id,
                        command_dispatched,
                        command_detail,
                        command_failed,
                        ..
                    } => {
                        handle_session_reset(
                            &shell,
                            session_id,
                            command_dispatched,
                            command_detail,
                            command_failed,
                        );
                    }
                }
            }
            ENGINE_EVENT_CONSUMER_STARTED.store(false, Ordering::SeqCst);
        },
        |job| {
            std::thread::Builder::new()
                .name("engine-event-consumer".to_string())
                .spawn(job)
                .map(|_| ())
                .map_err(|err| err.to_string())
        },
    );
    if let Err((err, receiver)) = spawn_result {
        restore_engine_event_receiver_for_retry(receiver);
        // panic=abort would make a spawn failure fatal; degrade instead: reset the started flag so
        // a later ensure_engine_runtime_booted can retry, and leave a telemetry trail.
        ENGINE_EVENT_CONSUMER_STARTED.store(false, Ordering::SeqCst);
        telemetry::log_message(
            "engine_event_consumer_spawn_failed",
            crate::dictation_error::error_log_code(&err.to_string()),
        );
    }
}

/// Boot the engine for this process: start the consumer, then (off the calling thread) activate the
/// shared model, warm the sidecar without the runtime lock, push the config snapshot, and finish the
/// launch. `source` names the caller in the log.
pub fn ensure_engine_runtime_booted(shell: &Shell, source: &str) {
    start_engine_event_consumer(shell.clone());
    // prewarm() is heavy ONNX/VAD session init (seconds). Run it OFF the main thread: called from
    // setup() it would block the run loop, so the launch splash and pill webviews could not paint
    // until it finishes (the ~4s dead gap). The launch splash stays up until prewarm finishes: only
    // when the engine is fully warm do we reveal the pill and fly the splash into the tray.
    let source = source.to_string();
    let shell = shell.clone();
    let spawned = std::thread::Builder::new()
        .name("engine-prewarm".to_string())
        .spawn(move || {
            let host = host();
            // Activate the shared copy only when this process has not verified it yet. This runs
            // again for every `register_hotkeys_now` (a hotkey-conflict retry, a finished model
            // download): activation shuts the resident engine down and re-hashes ~700 MB, so
            // repeating it dropped a warm engine for a cold load, and a lease conflict on the
            // repeat lost an activation that was already good.
            if rightkit_dictation_models::shared::is_verified_active() {
                host.splash_note_progress();
            } else if let Err(err) = host.prepare_shared_model() {
                telemetry::log_message(
                    "shared_model_activation_failed",
                    crate::dictation_error::error_log_code(&err),
                );
                // No Parakeet model resolves (denied after a failed verification, the Parakeet
                // lease busy with no activation record): a spawn would only die before hello.
                // Leave the splash up with its recovery actions instead of a doomed engine.
                if host.parakeet_model_missing() {
                    host.splash_note_failure();
                    telemetry::log_message(
                        "engine_prewarm_skipped",
                        &format!(
                            "source={source} no Parakeet model resolves after activation failed"
                        ),
                    );
                    return;
                }
                // A model still resolves (the recorded copy, a retired fallback): the prewarm
                // below decides whether the launch failed.
            } else {
                host.splash_note_progress();
            }
            // The splash MUST stay up for the whole warm-up. Revealing the pill first looks instant
            // but ships a pill that could not record for the entire ~30s cold Neural Engine load,
            // and it raced `pill://intro` ahead of the pill webview's mount so the launch bloom was
            // emitted to nobody and never played. An honest "preparing" splash beats a dead-looking
            // pill: finish_launch stays AFTER prewarm, so the pill appears only when it works.
            //
            // Warm the sidecar WITHOUT holding the global runtime lock. `warm_handshake` covers
            // process start + ready-wait AND the up-to-120s EngineInfo handshake; all of it runs on
            // the internally-synchronized `SidecarHandle` published in `runtime::ENGINE_SIDECAR`,
            // so none of the model-load wait holds runtime_lock. `runtime.prewarm()` then
            // re-acquires the lock only for the fast config/vocab snapshot push: the start is
            // "already-running" and `info_ready` is already set.
            if let Some(handshake) = crate::runtime::prewarm_handshake_lock_free() {
                if let Err(err) = handshake {
                    telemetry::log_message(
                        "engine_prewarm_error",
                        &format!(
                            "source={source} phase=handshake code={}",
                            crate::dictation_error::error_log_code(&err)
                        ),
                    );
                    host.splash_note_failure();
                    return;
                } else {
                    host.splash_note_progress();
                }
            }
            if let Err(err) = runtime_lock().and_then(|mut runtime| runtime.prewarm()) {
                telemetry::log_message(
                    "engine_prewarm_error",
                    &format!(
                        "source={source} code={}",
                        crate::dictation_error::error_log_code(&err)
                    ),
                );
                host.splash_note_failure();
                return;
            }
            telemetry::log_message("engine_prewarmed", &format!("source={source}"));
            crate::wake_lab::start_configured(shell.clone());
            // Fly the splash down into the pill, then reveal the pill (bloom + chime).
            shell.finish_launch();
        });
    if let Err(err) = spawned {
        telemetry::log_message("engine_prewarm_error", &format!("worker spawn: {err}"));
        host().splash_note_failure();
    }
}
