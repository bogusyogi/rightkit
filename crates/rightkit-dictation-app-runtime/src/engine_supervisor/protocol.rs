// Sidecar frame ↔ shell event translation + request-frame construction.

use rightkit_dictation_core::engine::{
    DiagnosticsPayload, EngineFrame, EnginePayload, EngineSchemaName,
};

use crate::engine_process::EngineEvent;

/// Map a sidecar frame into the existing `EngineEvent` flow the Tauri
/// event-consumer thread already drives. The Tauri shell doesn't need to
/// care about the sidecar's internal worker/event details.
pub(crate) fn frame_to_engine_event(frame: &EngineFrame) -> Option<EngineEvent> {
    // Error frames carry an `error` field, not a `payload` — map them BEFORE the
    // payload guard below, otherwise `payload.as_ref()?` silently drops every
    // engine error (e.g. the empty/silent-dictation "no text" result), leaving the
    // pill stuck in Processing until the watchdog. (Diagnosed via frame trace.)
    if matches!(frame.schema_name, EngineSchemaName::EngineError) {
        let message = frame
            .error
            .as_ref()
            .map(|e| e.message.clone())
            .unwrap_or_else(|| "engine error".to_string());
        return Some(EngineEvent::EngineError {
            session_id: frame.session_id.clone().unwrap_or_default(),
            message,
        });
    }
    let payload = frame.payload.as_ref()?;
    match (frame.schema_name.clone(), payload) {
        (
            EngineSchemaName::WakeEvent,
            EnginePayload::WakeFired {
                ts,
                fire_count,
                model,
                threshold,
                score,
            },
        ) => Some(EngineEvent::WakeFired {
            ts: *ts,
            fire_count: *fire_count,
            model: model.clone(),
            threshold: *threshold,
            score: *score,
        }),
        (EngineSchemaName::RecordingStarted, EnginePayload::RecordingStarted { .. }) => None,
        (
            EngineSchemaName::TranscribingStarted,
            EnginePayload::TranscribingStarted { stop_target, .. },
        ) => Some(EngineEvent::TranscribingStarted {
            session_id: frame.session_id.clone().unwrap_or_default(),
            stop_target: stop_target.clone(),
        }),
        (EngineSchemaName::RecordingLevel, EnginePayload::RecordingLevel { level }) => {
            Some(EngineEvent::RecordingLevel {
                session_id: frame.session_id.clone().unwrap_or_default(),
                level: *level,
            })
        }
        (
            EngineSchemaName::TranscriptPartial,
            EnginePayload::TranscriptPartial {
                text,
                revision,
                audio_secs,
                ..
            },
        ) => Some(EngineEvent::TranscriptPartial {
            session_id: frame.session_id.clone().unwrap_or_default(),
            text: text.clone(),
            revision: *revision,
            audio_secs: *audio_secs,
        }),
        (EngineSchemaName::TranscriptFinal, EnginePayload::TranscriptFinal { .. }) => {
            let text = match payload {
                EnginePayload::TranscriptFinal { text, .. } => text.clone(),
                _ => String::new(),
            };
            let diag_value = frame.payload.as_ref().and_then(|payload| match payload {
                EnginePayload::TranscriptFinal {
                    diagnostics: Some(diagnostics),
                    ..
                } => Some(diagnostics),
                _ => None,
            });
            // Protocol v2: prefer the typed `DiagnosticsPayload` when the
            // sender's `diagnostics` value deserializes into it cleanly; fall
            // back to the legacy string-keyed extraction for frames from an
            // older engine build that still sends bespoke `json!({...})`
            // shapes. See `DiagnosticsPayload` doc comment + dispatch #9/#10.
            let typed_diag: Option<DiagnosticsPayload> = diag_value
                .and_then(|value| serde_json::from_value::<DiagnosticsPayload>(value.clone()).ok());
            let delivery_record_value = typed_diag
                .as_ref()
                .and_then(|d| d.delivery_record.clone())
                .or_else(|| diag_value.and_then(|d| d.get("delivery_record").cloned()));
            let send_enter = typed_diag
                .as_ref()
                .and_then(|d| d.send_enter)
                .or_else(|| diag_value.and_then(|d| d.get("send_enter").and_then(|v| v.as_bool())))
                .unwrap_or(false);
            if let Some(record) = delivery_record_value.and_then(|value| {
                serde_json::from_value::<crate::delivery::DeliveryRecord>(value).ok()
            }) {
                return Some(EngineEvent::DeliveryCompleted {
                    session_id: frame.session_id.clone().unwrap_or_default(),
                    record,
                    send_enter,
                    timeline: typed_diag.as_ref().and_then(|d| d.timeline.clone()),
                });
            }

            let reset_to_armed = typed_diag
                .as_ref()
                .and_then(|d| d.reset_to_armed)
                .or_else(|| {
                    diag_value.and_then(|d| d.get("reset_to_armed").and_then(|v| v.as_bool()))
                })
                .unwrap_or(false);
            if reset_to_armed {
                let command_dispatched = typed_diag
                    .as_ref()
                    .and_then(|d| d.command_dispatched.clone())
                    .or_else(|| {
                        diag_value
                            .and_then(|d| d.get("command_dispatched").and_then(|v| v.as_str()))
                            .map(str::to_string)
                    });
                let command_detail = typed_diag
                    .as_ref()
                    .and_then(|d| d.command_detail.clone())
                    .or_else(|| {
                        diag_value
                            .and_then(|d| d.get("command_detail").and_then(|v| v.as_str()))
                            .map(str::to_string)
                    });
                let command_failed = typed_diag
                    .as_ref()
                    .and_then(|d| d.command_failed.clone())
                    .or_else(|| {
                        diag_value
                            .and_then(|d| d.get("command_failed").and_then(|v| v.as_str()))
                            .map(str::to_string)
                    });
                return Some(EngineEvent::SessionReset {
                    session_id: frame.session_id.clone().unwrap_or_default(),
                    command_dispatched,
                    command_detail,
                    command_failed,
                });
            }

            let send_enter = typed_diag
                .as_ref()
                .and_then(|d| d.send_enter)
                .or_else(|| diag_value.and_then(|d| d.get("send_enter").and_then(|v| v.as_bool())))
                .unwrap_or(false);
            // Pre-AI-polish text + clip duration, carried for undo/redo + clip length.
            let raw_text = typed_diag
                .as_ref()
                .and_then(|d| d.raw_text.clone())
                .or_else(|| {
                    diag_value
                        .and_then(|d| d.get("raw_text").and_then(|v| v.as_str()))
                        .map(str::to_string)
                });
            let recording_ms = typed_diag
                .as_ref()
                .and_then(|d| d.recording_ms)
                .or_else(|| {
                    diag_value.and_then(|d| d.get("recording_ms").and_then(|v| v.as_u64()))
                });
            // Engine-recognized cancel (spoken `zephyr cancel`): the shell
            // must store this to history instead of delivering, even without
            // its own `cancel_to_history_session` latch.
            let cancelled = typed_diag
                .as_ref()
                .and_then(|d| d.cancelled)
                .or_else(|| diag_value.and_then(|d| d.get("cancelled").and_then(|v| v.as_bool())))
                .unwrap_or(false);
            let context_request_attempted = typed_diag
                .as_ref()
                .and_then(|d| d.context_request_attempted)
                .or_else(|| {
                    diag_value
                        .and_then(|d| d.get("context_request_attempted").and_then(|v| v.as_bool()))
                });
            let context_original_binding = typed_diag
                .as_ref()
                .and_then(|d| d.context_original_binding)
                .or_else(|| {
                    diag_value
                        .and_then(|d| d.get("context_original_binding").and_then(|v| v.as_bool()))
                });
            Some(EngineEvent::TranscriptFinal {
                session_id: frame.session_id.clone().unwrap_or_default(),
                text,
                send_enter,
                raw_text,
                recording_ms,
                cancelled,
                context_request_attempted,
                context_original_binding,
                timeline: typed_diag.as_ref().and_then(|d| d.timeline.clone()),
                capture_warning: typed_diag.and_then(|d| d.capture_warning),
            })
        }
        _ => None,
    }
}

/// Convenience: build a request frame to send to the sidecar.
pub fn build_request(
    request_id: &str,
    schema_name: EngineSchemaName,
    session_id: Option<String>,
    payload: Option<EnginePayload>,
) -> EngineFrame {
    EngineFrame::base(
        schema_name,
        request_id,
        session_id,
        "sidecar-1",
        payload,
        None,
    )
}
