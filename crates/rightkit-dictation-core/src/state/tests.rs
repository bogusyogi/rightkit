use super::*;

fn rec(id: &str) -> AppState {
    AppState::Recording {
        session_id: id.into(),
    }
}

#[test]
fn happy_path_armed_to_pasted() {
    let s = transition(
        AppState::Armed,
        AppEvent::StartRecording {
            session_id: "s1".into(),
        },
    )
    .unwrap()
    .next_state;
    assert_eq!(s, rec("s1"));
    let s = transition(s, AppEvent::StopRecording).unwrap().next_state;
    assert_eq!(
        s,
        AppState::Transcribing {
            session_id: "s1".into()
        }
    );
    let s = transition(
        s,
        AppEvent::TranscriptFinal {
            transcript: "hello".into(),
            send_enter: false,
        },
    )
    .unwrap()
    .next_state;
    assert!(matches!(s, AppState::Pasting { .. }));
    let s = transition(
        s,
        AppEvent::PasteSucceeded {
            delivery_id: "d1".into(),
            send_enter: false,
        },
    )
    .unwrap()
    .next_state;
    assert_eq!(
        s,
        AppState::Pasted {
            delivery_id: "d1".into(),
            transcript: "hello".into(),
            send_enter: false
        }
    );
}

#[test]
fn send_intent_survives_paste_transition() {
    let s = AppState::Transcribing {
        session_id: "s1".into(),
    };
    let s = transition(
        s,
        AppEvent::TranscriptFinal {
            transcript: "send me".into(),
            send_enter: true,
        },
    )
    .unwrap()
    .next_state;
    assert_eq!(
        s,
        AppState::Pasting {
            session_id: "s1".into(),
            transcript: "send me".into(),
            send_enter: true
        }
    );
    let s = transition(
        s,
        AppEvent::PasteSucceeded {
            delivery_id: "d1".into(),
            send_enter: false,
        },
    )
    .unwrap()
    .next_state;
    assert_eq!(
        s,
        AppState::Pasted {
            delivery_id: "d1".into(),
            transcript: "send me".into(),
            send_enter: true
        }
    );
}

#[test]
fn skipped_enter_is_reported_as_pasted() {
    let record = DeliveryRecord::new(
        "d1",
        "send me",
        DeliveryOutcome::Pasted,
        rightkit_transcript_library::delivery::TargetSnapshot::test_target(),
    )
    .with_delivery_timing(
        rightkit_transcript_library::delivery::DeliveryTimings::from_steps(
            300,
            vec![
                rightkit_transcript_library::delivery::DeliveryTimingStep::new(
                    "paste_settle_skipped_enter",
                    300,
                ),
            ],
        ),
    );

    assert_eq!(
        delivery_outcome_event_with_send(&record, true),
        AppEvent::PasteSucceeded {
            delivery_id: "d1".into(),
            send_enter: false,
        }
    );
}

#[test]
fn unconfirmed_paste_preserves_transcript_and_never_submits() {
    let pasting = AppState::Pasting {
        session_id: "s1".into(),
        transcript: "keep me".into(),
        send_enter: true,
    };
    let next = transition(
        pasting,
        AppEvent::PasteSentUnconfirmed {
            delivery_id: "d1".into(),
            reason: "paste_readback_unavailable".into(),
        },
    )
    .unwrap()
    .next_state;
    assert_eq!(
        next,
        AppState::PasteSentUnconfirmed {
            delivery_id: "d1".into(),
            transcript: "keep me".into(),
            reason: "paste_readback_unavailable".into(),
        }
    );

    let record = DeliveryRecord::new(
        "d1",
        "keep me",
        DeliveryOutcome::PasteSentUnconfirmed {
            reason: "paste_readback_unavailable".into(),
        },
        rightkit_transcript_library::delivery::TargetSnapshot::test_target(),
    );
    assert_eq!(
        delivery_outcome_event_with_send(&record, true),
        AppEvent::PasteSentUnconfirmed {
            delivery_id: "d1".into(),
            reason: "paste_readback_unavailable".into(),
        }
    );
}

#[test]
fn empty_transcript_routes_to_copied_fallback() {
    let s = AppState::Transcribing {
        session_id: "s1".into(),
    };
    let next = transition(
        s,
        AppEvent::TranscriptFinal {
            transcript: "   ".into(),
            send_enter: false,
        },
    )
    .unwrap()
    .next_state;
    assert!(matches!(
        next,
        AppState::CopiedFallback {
            reason: CopyFallbackReason::EmptyTranscript,
            ..
        }
    ));
}

#[test]
fn fail_from_pasting_preserves_last_transcript() {
    let s = AppState::Pasting {
        session_id: "s1".into(),
        transcript: "keep me".into(),
        send_enter: false,
    };
    let next = transition(
        s,
        AppEvent::Fail {
            message: "boom".into(),
        },
    )
    .unwrap()
    .next_state;
    match next {
        AppState::Error {
            last_transcript, ..
        } => {
            assert_eq!(last_transcript.as_deref(), Some("keep me"));
        }
        other => panic!("expected Error, got {other:?}"),
    }
}

#[test]
fn fail_from_armed_and_recording_routes_to_error() {
    for start in [AppState::Armed, rec("s1")] {
        let next = transition(
            start,
            AppEvent::Fail {
                message: "boom".into(),
            },
        )
        .unwrap()
        .next_state;
        match next {
            AppState::Error {
                message,
                last_transcript,
            } => {
                assert_eq!(message, "boom");
                assert_eq!(last_transcript, None);
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }
}

#[test]
fn invalid_transition_errors() {
    let r = transition(
        AppState::Armed,
        AppEvent::PasteSucceeded {
            delivery_id: "d".into(),
            send_enter: false,
        },
    );
    assert!(r.is_err());
}

#[test]
fn reset_returns_to_armed() {
    let s = AppState::Pasted {
        delivery_id: "d1".into(),
        transcript: "t".into(),
        send_enter: false,
    };
    let next = transition(s, AppEvent::ResetToArmed).unwrap().next_state;
    assert_eq!(next, AppState::Armed);
}
