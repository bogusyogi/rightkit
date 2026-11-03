#![allow(unused_imports)]
#[allow(unused_imports)]
use super::*;
#[allow(unused_imports)]
use super::{dispatch::*, server::*};

pub(crate) fn spawn_worker_event_pump(
    runtime: Arc<Mutex<EngineRuntime>>,
    writer: Arc<Mutex<io::Stdout>>,
) -> Result<(), String> {
    let Some(worker_rx) = runtime.lock().worker_event_rx() else {
        return Err("worker event receiver is unavailable".to_string());
    };
    std::thread::Builder::new()
        .name("hr-engine-worker-events".to_string())
        .spawn(move || loop {
            let event = {
                let rx = worker_rx.lock();
                rx.recv()
            };
            let Ok(event) = event else { break };
            match event {
                crate::worker::WorkerEvent::WakeFired {
                    ts,
                    fire_count,
                    threshold,
                } => {
                    emit_event(
                        &writer,
                        EngineSchemaName::WakeEvent,
                        None,
                        Some(EnginePayload::WakeFired {
                            ts,
                            fire_count: Some(fire_count),
                            model: Some("kws-hey-zephyr".into()),
                            threshold: Some(threshold),
                            score: None,
                        }),
                    );
                }
                crate::worker::WorkerEvent::StartFailed { message } => {
                    let frame = error_frame(&next_id(), &next_id(), None, &message);
                    emit_frame(&writer, &frame);
                    break;
                }
                crate::worker::WorkerEvent::RecordingLevel { session_id, level } => {
                    emit_event(
                        &writer,
                        EngineSchemaName::RecordingLevel,
                        Some(session_id),
                        Some(EnginePayload::RecordingLevel { level }),
                    );
                }
                crate::worker::WorkerEvent::StreamCommitted {
                    session_id,
                    text,
                    revision,
                    audio_secs,
                } => {
                    emit_event(
                        &writer,
                        EngineSchemaName::TranscriptPartial,
                        Some(session_id),
                        Some(EnginePayload::TranscriptPartial {
                            text,
                            revision,
                            audio_secs: Some(audio_secs),
                            diagnostics: None,
                        }),
                    );
                }
                crate::worker::WorkerEvent::CaptureFailed {
                    session_id,
                    message,
                } => {
                    runtime.lock().fail_recording_capture(&session_id);
                    let frame = error_frame(&next_id(), &next_id(), Some(&session_id), &message);
                    emit_frame(&writer, &frame);
                }
                crate::worker::WorkerEvent::TranscriptFinal {
                    session_id,
                    result,
                    capture_warning,
                    audio_secs,
                    mut stage_metrics,
                } => {
                    // Capture -> unlock -> commit: finalize does not hold the
                    // runtime mutex across stop-time UIA, cloud L1/L2/L3
                    // polish HTTP, or delivery — see
                    // `crate::runtime::finalize_unlocked`'s doc comment.
                    let polish_started = std::time::Instant::now();
                    rightkit_dictation_inference::timeline_recorder::mark(|t| {
                        &mut t.finalize_started_ms
                    });
                    let finalized = crate::runtime::finalize_unlocked_typed(
                        &runtime,
                        &session_id,
                        result,
                        audio_secs,
                        None,
                        None,
                    );
                    stage_metrics.polish_elapsed_ms =
                        Some(polish_started.elapsed().as_millis() as u64);
                    emit_finalized(
                        &writer,
                        session_id,
                        finalized,
                        stage_metrics,
                        capture_warning,
                    );
                }
                crate::worker::WorkerEvent::AutoStop {
                    session_id,
                    result,
                    audio_secs,
                    send_enter,
                    standalone_action,
                    mut stage_metrics,
                } => {
                    // AutoStopBegin normally moved the runtime to Transcribing
                    // before final ASR. finalize_unlocked repeats begin_stop
                    // idempotently so a reordered/missing begin signal cannot fail
                    // the stale guard. `send_enter` carries the Enter intent for a
                    // stripped `zephyr send` tail. Polish/delivery runs unlocked.
                    let polish_started = std::time::Instant::now();
                    rightkit_dictation_inference::timeline_recorder::mark(|t| {
                        &mut t.finalize_started_ms
                    });
                    let finalized = crate::runtime::finalize_unlocked_typed(
                        &runtime,
                        &session_id,
                        result,
                        audio_secs,
                        Some(send_enter),
                        standalone_action,
                    );
                    stage_metrics.polish_elapsed_ms =
                        Some(polish_started.elapsed().as_millis() as u64);
                    emit_finalized(&writer, session_id, finalized, stage_metrics, None);
                }
                crate::worker::WorkerEvent::AutoStopBegin {
                    session_id,
                    send_enter,
                    standalone_action: _,
                } => {
                    // Hands-free control tail recognized; the full transcribe is about
                    // to run. Move the runtime to Transcribing NOW so stop timing
                    // includes final ASR, and push TranscribingStarted so the shell
                    // leaves Recording immediately. The later AutoStop begin_stop is
                    // an idempotent no-op.
                    // Hands-free auto-stop never carries the cancel-to-history
                    // intent (that only originates from an explicit shell
                    // `StopDictation { local_only: true }` request), so `false`.
                    let stop_target = {
                        let mut r = runtime.lock();
                        let _ = r.begin_stop(&session_id, send_enter, false);
                        r.captured_stop_target()
                    };
                    emit_event(
                        &writer,
                        EngineSchemaName::TranscribingStarted,
                        Some(session_id),
                        Some(EnginePayload::TranscribingStarted {
                            stop_kind: None,
                            stop_target,
                        }),
                    );
                    let drain = std::time::Duration::from_millis(60);
                    tracing::info!(
                        drain_ms = drain.as_millis() as u64,
                        "autostop begin: draining shell PTT hook before command dispatch"
                    );
                    if !drain.is_zero() {
                        std::thread::sleep(drain);
                    }
                }
                crate::worker::WorkerEvent::AutoCancelBegin { session_id } => {
                    // Spoken `zephyr cancel`: latch the cancel-to-history stop
                    // kind BEFORE the worker's full-buffer decode lands as a
                    // normal TranscriptFinal. `begin_stop` is idempotent once
                    // Transcribing, so the latch cannot be clobbered later.
                    let stop_target = {
                        let mut r = runtime.lock();
                        let _ = r.begin_stop(&session_id, false, true);
                        r.captured_stop_target()
                    };
                    emit_event(
                        &writer,
                        EngineSchemaName::TranscribingStarted,
                        Some(session_id),
                        Some(EnginePayload::TranscribingStarted {
                            stop_kind: Some(
                                rightkit_dictation_core::engine::StopKind::CancelToHistory,
                            ),
                            stop_target,
                        }),
                    );
                }
                crate::worker::WorkerEvent::RunawayDiscard { session_id } => {
                    // No-voice backstop: drop the recording, reset to idle.
                    runtime.lock().cancel(&session_id);
                    emit_event(
                        &writer,
                        EngineSchemaName::TranscriptFinal,
                        Some(session_id),
                        Some(EnginePayload::TranscriptFinal {
                            text: String::new(),
                            confidence: Some(1.0),
                            diagnostics: diagnostics_value(DiagnosticsPayload {
                                reset_to_armed: Some(true),
                                ..Default::default()
                            }),
                        }),
                    );
                }
            }
        })
        .map(|_| ())
        .map_err(|error| format!("spawn worker event pump: {error}"))
}

/// Serialize a typed `DiagnosticsPayload` into the wire `Option<Value>` slot.
/// The `TranscriptFinal.diagnostics` field itself stays untyped (protocol v2
/// is additive, not a schema-shape break) — this is purely a sender-side
/// convenience so the engine builds diagnostics from a typed struct instead
/// of ad-hoc `json!({...})` maps. Falls back to `None` only if serialization
/// itself fails (should not happen for this plain-data struct).
pub(crate) fn diagnostics_value(payload: DiagnosticsPayload) -> Option<Value> {
    serde_json::to_value(payload).ok()
}

/// Stamp `result_sent_ms` and take the session's engine timeline. Called
/// immediately before the `TranscriptFinal` frame is built and written, so the
/// stamp is the last engine-side offset.
fn take_timeline_for_send(
    session_id: &str,
) -> Option<rightkit_dictation_core::engine::EngineTimeline> {
    rightkit_dictation_inference::timeline_recorder::mark(|t| &mut t.result_sent_ms);
    rightkit_dictation_inference::timeline_recorder::take(session_id)
}

/// Emit the engine event for a `finalize_transcript` outcome. Shared by the
/// manual-stop `TranscriptFinal` path and the hands-free `AutoStop` path.
pub(crate) fn emit_finalized(
    writer: &Arc<Mutex<io::Stdout>>,
    session_id: String,
    finalized: Result<FinalizeOutcome, String>,
    stage_metrics: rightkit_dictation_core::engine::RecordingStageMetrics,
    capture_warning: Option<String>,
) {
    let finalized = match (finalized, capture_warning.as_ref()) {
        (Ok(FinalizeOutcome::ResetToArmed | FinalizeOutcome::NoOp), Some(message)) => {
            Err(message.clone())
        }
        (outcome, _) => outcome,
    };
    match finalized {
        Ok(FinalizeOutcome::Delivery { record, send_enter }) => {
            let record_value = serde_json::to_value(&record).ok();
            let timeline = take_timeline_for_send(&session_id);
            emit_event(
                writer,
                EngineSchemaName::TranscriptFinal,
                Some(session_id),
                Some(EnginePayload::TranscriptFinal {
                    text: record.transcript.clone(),
                    confidence: Some(1.0),
                    diagnostics: diagnostics_value(DiagnosticsPayload {
                        delivery_record: record_value,
                        send_enter: Some(send_enter),
                        recording_stage_metrics: Some(stage_metrics),
                        timeline,
                        ..Default::default()
                    }),
                }),
            )
        }
        Ok(FinalizeOutcome::Transcript {
            text,
            send_enter,
            raw_text,
            recording_ms,
            cancelled,
            context_request_attempted,
            context_original_binding,
        }) => {
            let timeline = take_timeline_for_send(&session_id);
            // Receipt is deliberately content-free: it records whether the
            // actual serialized request attempted context & whether sanitized
            // nonempty context was accepted for original binding.
            tracing::info!(
                session_id = %session_id,
                request_attempted = ?context_request_attempted,
                original_binding = ?context_original_binding,
                "context_request_receipt"
            );
            emit_event(
                writer,
                EngineSchemaName::TranscriptFinal,
                Some(session_id),
                Some(EnginePayload::TranscriptFinal {
                    text,
                    confidence: Some(1.0),
                    diagnostics: diagnostics_value(DiagnosticsPayload {
                        shell_delivery: Some(true),
                        send_enter: Some(send_enter),
                        raw_text,
                        recording_ms,
                        cancelled: cancelled.then_some(true),
                        capture_warning,
                        context_request_attempted,
                        context_original_binding,
                        recording_stage_metrics: Some(stage_metrics),
                        timeline,
                        ..Default::default()
                    }),
                }),
            )
        }
        Ok(FinalizeOutcome::ResetToArmed | FinalizeOutcome::NoOp) => emit_event(
            writer,
            EngineSchemaName::TranscriptFinal,
            Some(session_id),
            Some(EnginePayload::TranscriptFinal {
                text: String::new(),
                confidence: Some(1.0),
                diagnostics: diagnostics_value(DiagnosticsPayload {
                    reset_to_armed: Some(true),
                    ..Default::default()
                }),
            }),
        ),
        Ok(FinalizeOutcome::CommandDispatched { action, detail }) => emit_event(
            writer,
            EngineSchemaName::TranscriptFinal,
            Some(session_id),
            Some(EnginePayload::TranscriptFinal {
                text: String::new(),
                confidence: Some(1.0),
                diagnostics: diagnostics_value(DiagnosticsPayload {
                    reset_to_armed: Some(true),
                    command_dispatched: Some(action),
                    command_detail: Some(detail),
                    ..Default::default()
                }),
            }),
        ),
        // Command classified but dispatch failed: still resets to armed, but the
        // shell gets the failure to surface on the pill.
        Ok(FinalizeOutcome::CommandFailed { message }) => emit_event(
            writer,
            EngineSchemaName::TranscriptFinal,
            Some(session_id),
            Some(EnginePayload::TranscriptFinal {
                text: String::new(),
                confidence: Some(1.0),
                diagnostics: diagnostics_value(DiagnosticsPayload {
                    reset_to_armed: Some(true),
                    command_failed: Some(message),
                    ..Default::default()
                }),
            }),
        ),
        Err(message) => {
            let frame = error_frame(&next_id(), &next_id(), Some(&session_id), &message);
            emit_frame(writer, &frame);
        }
    }
}

pub(crate) use std::sync::atomic::AtomicU64;

pub(crate) static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_id() -> String {
    let n = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("req-{n}")
}
