use super::*;

#[test]
fn valid_health_frame_passes() {
    let frame = EngineFrame::health("r1", "t1");
    assert!(validate_frame(&frame).is_ok());
    let value = serde_json::to_value(&frame).unwrap();
    assert!(validate_engine_frame(&value).is_ok());
}

#[test]
fn protocol_major_mismatch_rejected() {
    let mut frame = EngineFrame::health("r1", "t1");
    frame.protocol_major = 2;
    assert_eq!(
        validate_frame(&frame),
        Err(EngineContractError::ProtocolMajorMismatch {
            expected: 1,
            actual: 2
        })
    );
}

#[test]
fn ui_concept_leak_rejected() {
    let v = serde_json::json!({
        "protocol_major": 1, "protocol_minor": 0,
        "schema_name": "transcript_final", "schema_version": 1,
        "engine_version": "x", "request_id": "r", "trace_id": "t",
        "session_id": "s",
        "payload": { "kind": "transcript_final", "text": "hi", "pill_state": "leak" }
    });
    match validate_engine_frame(&v) {
        Err(EngineContractError::UiConceptLeaked(field)) => assert_eq!(field, "pill_state"),
        other => panic!("expected UiConceptLeaked, got {other:?}"),
    }
}

#[test]
fn recording_started_requires_session_id() {
    let frame = EngineFrame::base(
        EngineSchemaName::RecordingStarted,
        "r1",
        None, // no session
        "t1",
        Some(EnginePayload::RecordingStarted {
            session_id: "s1".into(),
            last_pasted: None,
        }),
        None,
    );
    assert_eq!(
        validate_frame(&frame),
        Err(EngineContractError::MissingSessionId)
    );
}

#[test]
fn recording_started_frame_without_last_pasted_still_deserializes() {
    // The shape an older shell sends: no `last_pasted` key.
    let old = serde_json::json!({
        "protocol_major": 1, "protocol_minor": 0,
        "schema_name": "recording_started", "schema_version": 1,
        "engine_version": "x", "request_id": "r", "trace_id": "t",
        "session_id": "s1",
        "payload": { "kind": "recording_started", "session_id": "s1" }
    });
    let frame = validate_engine_frame(&old).expect("old frame validates");
    assert!(matches!(
        frame.payload,
        Some(EnginePayload::RecordingStarted {
            last_pasted: None,
            ..
        })
    ));
    // And a frame with no last paste does not write the key, so an older engine sees the old shape.
    let value = serde_json::to_value(&frame).unwrap();
    assert!(value["payload"].get("last_pasted").is_none());
}

#[test]
fn file_transcription_cancel_frame_validates_and_names_the_request() {
    let frame = EngineFrame::base(
        EngineSchemaName::FileTranscriptionCancel,
        "r2",
        None,
        "t2",
        Some(EnginePayload::FileTranscriptionCancel {
            request_id: "transcribe_file:7".into(),
        }),
        None,
    );
    let value = serde_json::to_value(&frame).unwrap();
    assert_eq!(value["schema_name"], "file_transcription_cancel");
    assert!(validate_engine_frame(&value).is_ok());
}

#[test]
fn recording_context_states_preserve_unknown_vs_empty() {
    let unknown = RecordingContext {
        app_name: None,
        window_title: None,
        field_text: None,
        selected_text: None,
        state: RecordingContextState::Unreadable,
        request_attempted: false,
    };
    let empty = RecordingContext {
        state: RecordingContextState::ConfirmedEmpty,
        ..unknown.clone()
    };
    let nonempty = RecordingContext {
        field_text: Some("draft".into()),
        state: RecordingContextState::NonEmpty,
        ..unknown.clone()
    };
    assert_ne!(unknown.state, empty.state);
    assert_ne!(empty.state, nonempty.state);
    for context in [unknown, empty, nonempty] {
        let roundtrip: RecordingContext =
            serde_json::from_value(serde_json::to_value(&context).unwrap()).unwrap();
        assert_eq!(roundtrip, context);
    }
}

#[test]
fn context_capture_frame_validates_without_text_telemetry() {
    let frame = EngineFrame::base(
        EngineSchemaName::RecordingContextCaptured,
        "r1",
        Some("s1".into()),
        "t1",
        Some(EnginePayload::RecordingContextCaptured {
            session_id: "s1".into(),
            context: RecordingContext {
                app_name: Some("Editor".into()),
                window_title: None,
                field_text: Some("draft".into()),
                selected_text: None,
                state: RecordingContextState::NonEmpty,
                request_attempted: false,
            },
            recording_target: None,
        }),
        None,
    );
    assert!(validate_frame(&frame).is_ok());
    let wire = serde_json::to_string(&frame).unwrap();
    assert!(wire.contains("recording_context_captured"));
}

#[test]
fn recording_delivery_policy_matrix_is_fail_closed() {
    use RecordingDeliveryPolicy::*;
    assert_eq!(
        recording_delivery_policy(Some(RecordingContextState::ConfirmedEmpty), false, false),
        ConfirmedEmptyStop
    );
    assert_eq!(
        recording_delivery_policy(Some(RecordingContextState::ConfirmedEmpty), true, true),
        ConfirmedEmptyStop
    );
    assert_eq!(
        recording_delivery_policy(Some(RecordingContextState::NonEmpty), true, true),
        OriginalBound
    );
    assert_eq!(
        recording_delivery_policy(Some(RecordingContextState::NonEmpty), false, true),
        UnboundStop
    );
    assert_eq!(
        recording_delivery_policy(Some(RecordingContextState::NonEmpty), true, false),
        UnboundStop
    );
    assert_eq!(
        recording_delivery_policy(Some(RecordingContextState::NonEmpty), false, false),
        UnboundStop
    );
    assert_eq!(
        recording_delivery_policy(Some(RecordingContextState::Unreadable), true, true),
        Recovery
    );
    assert_eq!(recording_delivery_policy(None, true, true), Recovery);
}

#[test]
fn file_transcription_frames_validate() {
    let request = EngineFrame::base(
        EngineSchemaName::FileTranscriptionRequest,
        "r1",
        None,
        "t1",
        Some(EnginePayload::FileTranscriptionRequest {
            path: "/tmp/sample.wav".into(),
        }),
        None,
    );
    assert!(validate_frame(&request).is_ok());

    let response = EngineFrame::base(
        EngineSchemaName::FileTranscriptionResult,
        "r1",
        None,
        "t2",
        Some(EnginePayload::FileTranscriptionResult {
            text: "Hello".into(),
            srt: String::new(),
            vtt: String::new(),
            words: Vec::new(),
        }),
        None,
    );
    assert!(validate_frame(&response).is_ok());
}

mod service_contract_tests {
    use super::*;
    use rightkit_speech_client::{FileTranscript, ServiceEnvelope, TimedWord};

    fn core_to_client(frame: EngineFrame) {
        let value = serde_json::to_value(frame).unwrap();
        let envelope: ServiceEnvelope = serde_json::from_value(value).unwrap();
        envelope.validate_envelope().unwrap();
    }

    fn client_to_core(frame: ServiceEnvelope) {
        let value = serde_json::to_value(frame).unwrap();
        validate_engine_frame(&value).unwrap();
    }

    #[test]
    fn service_v1_cross_parser_health_info_file_result_and_error() {
        core_to_client(EngineFrame::health("health", "trace"));
        core_to_client(EngineFrame::base(
            EngineSchemaName::EngineInfo,
            "info",
            None,
            "trace",
            Some(EnginePayload::EngineInfo {
                mode: "speech_service".into(),
                cold_load_s: Some(0.2),
                engine_version: Some("v1".into()),
            }),
            None,
        ));
        core_to_client(EngineFrame::base(
            EngineSchemaName::FileTranscriptionResult,
            "file",
            None,
            "trace",
            Some(EnginePayload::FileTranscriptionResult {
                text: "hi".into(),
                srt: "s".into(),
                vtt: "v".into(),
                words: vec![],
            }),
            None,
        ));
        core_to_client(EngineFrame::base(
            EngineSchemaName::EngineError,
            "error",
            None,
            "trace",
            None,
            Some(EngineErrorPayload {
                code: "E".into(),
                message: "m".into(),
                retryable: false,
                diagnostics: None,
            }),
        ));

        client_to_core(ServiceEnvelope::health("health", "trace"));
        client_to_core(ServiceEnvelope::info(
            "info",
            "trace",
            Some(0.2),
            Some("v1".into()),
        ));
        client_to_core(ServiceEnvelope::file_result(
            "file",
            "trace",
            FileTranscript {
                text: "hi".into(),
                srt: "s".into(),
                vtt: "v".into(),
                words: vec![TimedWord::new("hi", 0, 10)],
            },
        ));
        client_to_core(ServiceEnvelope::error("error", "trace", "E", "m", false));
    }

    #[test]
    fn service_v1_rejects_malformed_allowed_requests() {
        let bad_payload: ServiceEnvelope = serde_json::from_value(serde_json::json!({
                "protocol_major": 1, "protocol_minor": 0, "schema_name": "engine_health", "schema_version": 1,
                "engine_version": "v", "request_id": "r", "session_id": null, "trace_id": "t",
                "payload": { "kind": "health", "status": 4 }
            })).unwrap();
        assert!(bad_payload.request().is_err());
        let missing_payload = ServiceEnvelope {
            payload: None,
            ..ServiceEnvelope::health("r", "t")
        };
        assert!(missing_payload.request().is_err());
        let mut wrong_major = ServiceEnvelope::health("r", "t");
        wrong_major.protocol_major = 2;
        assert!(wrong_major.validate_envelope().is_err());
        assert!(validate_engine_frame(&serde_json::to_value(&wrong_major).unwrap()).is_err());
        let mut wrong_version = ServiceEnvelope::health("r", "t");
        wrong_version.schema_version = 2;
        assert!(wrong_version.validate_envelope().is_err());
        assert!(validate_engine_frame(&serde_json::to_value(&wrong_version).unwrap()).is_err());
        let unexpected_error = serde_json::json!({
            "protocol_major": 1, "protocol_minor": 0, "schema_name": "engine_health", "schema_version": 1,
            "engine_version": "v", "request_id": "r", "session_id": null, "trace_id": "t",
            "payload": { "kind": "health", "status": "ok" },
            "error": { "code": "E", "message": "m", "retryable": false }
        });
        assert!(validate_engine_frame(&unexpected_error).is_err());
        let unexpected_error: ServiceEnvelope = serde_json::from_value(unexpected_error).unwrap();
        assert!(unexpected_error.request().is_err());
    }
}

#[test]
fn error_frame_requires_error_and_no_payload() {
    let mut frame = EngineFrame::base(EngineSchemaName::EngineError, "r1", None, "t1", None, None);
    assert_eq!(
        validate_frame(&frame),
        Err(EngineContractError::MissingError)
    );
    frame.error = Some(EngineErrorPayload {
        code: "E".into(),
        message: "m".into(),
        retryable: false,
        diagnostics: None,
    });
    assert!(validate_frame(&frame).is_ok());
}

#[test]
fn free_tier_blocks_over_ten_minutes() {
    let err = check_duration_limit(11 * 60, false).unwrap_err();
    assert!(err.to_lowercase().contains("pro"), "got: {err}");
}

#[test]
fn free_tier_allows_ten_minutes_exactly() {
    assert!(check_duration_limit(FREE_TRANSCRIBE_MAX_SECS, false).is_ok());
}

#[test]
fn pro_has_no_length_limit() {
    assert!(check_duration_limit(90 * 60, true).is_ok());
    assert!(check_duration_limit(100 * 3600, true).is_ok()); // 100h — unlimited
}

// ---- Protocol v2 (dispatch #9/#10): StopKind + DiagnosticsPayload ----

#[test]
fn stop_kind_round_trips_all_variants() {
    for kind in [
        StopKind::Stop,
        StopKind::SendEnter,
        StopKind::Cancel,
        StopKind::CancelToHistory,
    ] {
        let value = serde_json::to_value(kind).unwrap();
        let back: StopKind = serde_json::from_value(value).unwrap();
        assert_eq!(back, kind);
    }
}

#[test]
fn transcribing_started_frame_with_stop_kind_round_trips() {
    let frame = EngineFrame::base(
        EngineSchemaName::TranscribingStarted,
        "stop:1:send_enter",
        Some("s1".to_string()),
        "t1",
        Some(EnginePayload::TranscribingStarted {
            stop_kind: Some(StopKind::SendEnter),
            stop_target: None,
        }),
        None,
    );
    assert!(validate_frame(&frame).is_ok());
    let value = serde_json::to_value(&frame).unwrap();
    // stop_kind must actually be on the wire when Some(..).
    assert_eq!(
        value["payload"]["stop_kind"],
        serde_json::json!("send_enter")
    );
    let round_tripped = validate_engine_frame(&value).unwrap();
    assert_eq!(round_tripped, frame);
}

#[test]
fn legacy_transcribing_started_frame_without_stop_kind_still_parses() {
    // Simulates a frame from an OLDER binary that predates the `stop_kind`
    // field entirely — the JSON simply doesn't have the key. This MUST
    // still deserialize (backward compatibility is the invariant for
    // this pass) with stop_kind defaulting to None.
    let legacy_json = serde_json::json!({
        "protocol_major": 1, "protocol_minor": 0,
        "schema_name": "transcribing_started", "schema_version": 1,
        "engine_version": "x", "request_id": "stop:1:stop", "trace_id": "t1",
        "session_id": "s1",
        "payload": { "kind": "transcribing_started" }
    });
    let frame = validate_engine_frame(&legacy_json).unwrap();
    match frame.payload {
        Some(EnginePayload::TranscribingStarted { stop_kind, .. }) => {
            assert_eq!(stop_kind, None);
        }
        other => panic!("expected TranscribingStarted payload, got {other:?}"),
    }
}

#[test]
fn diagnostics_payload_round_trips_delivery_shape() {
    let payload = DiagnosticsPayload {
        delivery_record: Some(serde_json::json!({"delivery_id": "d1"})),
        ..Default::default()
    };
    let value = serde_json::to_value(&payload).unwrap();
    // Only the populated field should be on the wire — the rest are
    // skipped via skip_serializing_if, keeping old readers unaffected.
    assert_eq!(value.as_object().unwrap().len(), 1);
    let back: DiagnosticsPayload = serde_json::from_value(value).unwrap();
    assert_eq!(back, payload);
}

#[test]
fn diagnostics_payload_round_trips_transcript_shape() {
    let payload = DiagnosticsPayload {
        shell_delivery: Some(true),
        send_enter: Some(true),
        raw_text: Some("raw hello".into()),
        recording_ms: Some(1234),
        ..Default::default()
    };
    let value = serde_json::to_value(&payload).unwrap();
    let back: DiagnosticsPayload = serde_json::from_value(value).unwrap();
    assert_eq!(back, payload);
}

#[test]
fn diagnostics_payload_round_trips_command_failed_shape() {
    let payload = DiagnosticsPayload {
        reset_to_armed: Some(true),
        command_failed: Some("dispatch failed".into()),
        ..Default::default()
    };
    let value = serde_json::to_value(&payload).unwrap();
    let back: DiagnosticsPayload = serde_json::from_value(value).unwrap();
    assert_eq!(back, payload);
}

#[test]
fn legacy_diagnostics_json_map_without_typed_fields_still_parses() {
    // A frame from an OLDER engine build that still sends a bespoke
    // json!({...}) map instead of the typed struct. All fields are
    // optional/defaulted, so this must parse into a DiagnosticsPayload
    // with the matching fields populated and everything else None.
    let legacy = serde_json::json!({ "reset_to_armed": true, "command_failed": "oops" });
    let parsed: DiagnosticsPayload = serde_json::from_value(legacy).unwrap();
    assert_eq!(parsed.reset_to_armed, Some(true));
    assert_eq!(parsed.command_failed, Some("oops".to_string()));
    assert_eq!(parsed.delivery_record, None);
}

#[test]
fn transcript_final_frame_with_typed_diagnostics_round_trips() {
    let diagnostics = serde_json::to_value(DiagnosticsPayload {
        shell_delivery: Some(true),
        send_enter: Some(false),
        raw_text: Some("raw".into()),
        recording_ms: Some(500),
        ..Default::default()
    })
    .unwrap();
    let frame = EngineFrame::base(
        EngineSchemaName::TranscriptFinal,
        "r1",
        Some("s1".to_string()),
        "t1",
        Some(EnginePayload::TranscriptFinal {
            text: "hello".into(),
            confidence: Some(1.0),
            diagnostics: Some(diagnostics),
        }),
        None,
    );
    assert!(validate_frame(&frame).is_ok());
    let value = serde_json::to_value(&frame).unwrap();
    let round_tripped = validate_engine_frame(&value).unwrap();
    assert_eq!(round_tripped, frame);
}

#[test]
fn unreadable_start_requires_negative_binding_receipt_and_frozen_stop_destination() {
    use super::{
        recording_delivery_policy_with_stop_proof as resolve, RecordingContextState as State,
        RecordingDeliveryPolicy as Policy,
    };
    for state in [None, Some(State::Unreadable)] {
        for request in [None, Some(false), Some(true)] {
            for bound in [None, Some(false), Some(true)] {
                for safe in [false, true] {
                    // The binding receipt decides: a polish request that carried
                    // NO field context (request true, bound false) binds nothing.
                    // A missing binding receipt still fails closed.
                    let allowed = bound == Some(false) && safe;
                    assert_eq!(
                        resolve(state, request, bound, safe),
                        if allowed {
                            Policy::UnboundStop
                        } else {
                            Policy::Recovery
                        }
                    );
                }
            }
        }
    }
    assert_eq!(
        resolve(Some(State::NonEmpty), Some(true), Some(true), false),
        Policy::OriginalBound
    );
    assert_eq!(
        resolve(Some(State::NonEmpty), Some(true), Some(true), true),
        Policy::OriginalBound
    );
    assert_eq!(
        resolve(Some(State::ConfirmedEmpty), Some(false), Some(false), false),
        Policy::ConfirmedEmptyStop
    );
}

#[test]
fn unreadable_start_with_unbound_polish_uses_stop_destination() {
    use super::{
        recording_delivery_policy_with_stop_proof as resolve, RecordingContextState as State,
        RecordingDeliveryPolicy as Policy,
    };
    // 0.1.194 incident: no start proof, polish ran without field context,
    // but the shell captured a valid native stop destination.
    assert_eq!(
        resolve(Some(State::Unreadable), Some(true), Some(false), true),
        Policy::UnboundStop
    );
    assert_eq!(
        resolve(Some(State::Unreadable), Some(true), Some(false), false),
        Policy::Recovery
    );
    assert_eq!(
        resolve(Some(State::Unreadable), Some(true), None, true),
        Policy::Recovery
    );
    assert_eq!(
        resolve(Some(State::Unreadable), Some(true), Some(true), true),
        Policy::Recovery
    );
}

#[test]
fn unbound_receipt_with_stop_target_delivers_to_stop_for_every_start_state() {
    use super::{
        recording_delivery_policy_with_stop_proof as resolve, RecordingContextState as State,
        RecordingDeliveryPolicy as Policy,
    };
    for state in [Some(State::Unreadable), None, Some(State::NonEmpty)] {
        for bound in [None, Some(false), Some(true)] {
            for stop in [false, true] {
                let got = resolve(state, Some(true), bound, stop);
                let expected = match (state, bound, stop) {
                    (Some(State::NonEmpty), Some(true), _) => Policy::OriginalBound,
                    (Some(State::NonEmpty), _, _) => Policy::UnboundStop,
                    (_, Some(false), true) => Policy::UnboundStop,
                    _ => Policy::Recovery,
                };
                assert_eq!(got, expected, "{state:?} {bound:?} stop={stop}");
            }
        }
    }
}

#[test]
fn explicit_negative_binding_receipt_survives_the_wire() {
    use super::DiagnosticsPayload;
    // A polish request without field context must reach the shell as an
    // explicit Some(false), never as an omitted (None) receipt.
    let wire = serde_json::to_value(DiagnosticsPayload {
        context_request_attempted: Some(true),
        context_original_binding: Some(false),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(wire["context_request_attempted"], true);
    assert_eq!(wire["context_original_binding"], false);
    let back: DiagnosticsPayload = serde_json::from_value(wire).unwrap();
    assert_eq!(back.context_original_binding, Some(false));
    let omitted = serde_json::to_value(DiagnosticsPayload::default()).unwrap();
    assert!(omitted.get("context_original_binding").is_none());
}

#[test]
fn reconfigure_l3_frame_round_trips_and_validates() {
    let frame = EngineFrame::base(
        EngineSchemaName::ReconfigureL3,
        "reconfigure_l3:1",
        None,
        "t1",
        Some(EnginePayload::ReconfigureL3 {
            ai_polish_enabled: true,
            provider_state: "ready".into(),
            provider_ready: true,
        }),
        None,
    );
    assert!(validate_frame(&frame).is_ok());
    let wire = serde_json::to_value(&frame).unwrap();
    assert_eq!(wire["schema_name"], "reconfigure_l3");
    assert!(validate_engine_frame(&wire).is_ok());
    assert_eq!(CAPABILITY_LIVE_L3_RECONFIGURE, "live_l3_reconfigure");
}

#[test]
fn only_a_real_paste_is_handed_to_the_engine_as_the_last_paste() {
    use rightkit_transcript_library::delivery::{
        CopyFallbackReason, DeliveryOutcome, DeliveryRecord, TargetSnapshot,
    };
    let record =
        |outcome| DeliveryRecord::new("d", "hello world", outcome, TargetSnapshot::empty());
    let handed = last_pasted_for_engine(&record(DeliveryOutcome::Pasted)).expect("pasted");
    assert_eq!(handed.transcript, "hello world");
    assert!(
        last_pasted_for_engine(&record(DeliveryOutcome::PasteSentUnconfirmed {
            reason: "readback unavailable".into()
        }))
        .is_some()
    );
    for outcome in [
        DeliveryOutcome::CopiedFallback {
            reason: CopyFallbackReason::NoTextField,
        },
        DeliveryOutcome::Cancelled,
        DeliveryOutcome::Draft,
        DeliveryOutcome::Error {
            code: "x".into(),
            message: "y".into(),
        },
    ] {
        assert!(
            last_pasted_for_engine(&record(outcome)).is_none(),
            "only a paste qualifies"
        );
    }
}

#[test]
fn recording_started_with_a_last_paste_round_trips() {
    use rightkit_transcript_library::delivery::{DeliveryOutcome, DeliveryRecord, TargetSnapshot};
    let record = DeliveryRecord::new(
        "d",
        "hello world",
        DeliveryOutcome::Pasted,
        TargetSnapshot::empty(),
    );
    let frame = EngineFrame::base(
        EngineSchemaName::RecordingStarted,
        "r1",
        Some("s1".to_string()),
        "t1",
        Some(EnginePayload::RecordingStarted {
            session_id: "s1".into(),
            last_pasted: Some(record.clone()),
        }),
        None,
    );
    let value = serde_json::to_value(&frame).unwrap();
    let back = validate_engine_frame(&value).expect("valid");
    match back.payload {
        Some(EnginePayload::RecordingStarted { last_pasted, .. }) => {
            assert_eq!(last_pasted, Some(record));
        }
        other => panic!("unexpected payload {other:?}"),
    }
}
