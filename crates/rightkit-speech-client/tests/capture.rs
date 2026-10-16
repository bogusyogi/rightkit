//! Protocol minor 4: capture ownership rules, their wire frames, and the cancel frame's
//! backward compatibility. Platform independent: no host process is needed.

use rightkit_speech_client::*;

fn owner(role: CaptureRole, id: &str) -> CaptureOwner {
    CaptureOwner {
        role,
        owner_id: id.into(),
        pid: 100,
    }
}

const DICTATION: CaptureRole = CaptureRole::HeardRightDictation;
const PULSE: CaptureRole = CaptureRole::PulseConversation;
const MEETING: CaptureRole = CaptureRole::Meeting;
const BATCH: CaptureRole = CaptureRole::Batch;

#[test]
fn priority_is_dictation_then_pulse_then_batch_and_a_meeting_is_outside_it() {
    assert!(DICTATION.outranks(PULSE) && DICTATION.outranks(BATCH) && PULSE.outranks(BATCH));
    assert!(!BATCH.outranks(PULSE) && !PULSE.outranks(DICTATION) && !PULSE.outranks(PULSE));
    for other in [DICTATION, PULSE, BATCH, MEETING] {
        assert!(!MEETING.outranks(other) && !other.outranks(MEETING));
    }
    for role in [DICTATION, PULSE, MEETING, BATCH] {
        assert_eq!(CaptureRole::from_wire(role.as_str()), Some(role));
        assert_eq!(serde_json::to_value(role).unwrap(), role.as_str());
    }
    assert_eq!(CaptureRole::from_wire("bogus"), None);
}

#[test]
fn decide_preempts_downward_only_and_never_silently_takes_a_meeting() {
    use CaptureDecision::*;
    assert_eq!(decide_capture(None, BATCH, false), Free);
    assert_eq!(decide_capture(Some(PULSE), DICTATION, false), Preempt);
    assert_eq!(decide_capture(Some(BATCH), PULSE, false), Preempt);
    assert_eq!(decide_capture(Some(DICTATION), PULSE, false), Busy);
    assert_eq!(decide_capture(Some(PULSE), PULSE, false), Busy);
    assert_eq!(decide_capture(Some(PULSE), MEETING, true), Busy);
    assert_eq!(
        decide_capture(Some(MEETING), DICTATION, false),
        ConfirmRequired
    );
    assert_eq!(decide_capture(Some(MEETING), DICTATION, true), Preempt);
    for requester in [PULSE, BATCH, MEETING] {
        assert_eq!(decide_capture(Some(MEETING), requester, true), Busy);
    }
}

#[test]
fn dictation_preempts_a_pulse_conversation_and_gets_the_capture_the_moment_pulse_releases() {
    let mut a = CaptureArbiter::new();
    let pulse = owner(PULSE, "pulse-1");
    let dictation = owner(DICTATION, "hr-1");
    assert_eq!(a.acquire(pulse.clone(), false, 0), AcquireOutcome::Granted);
    // One mode at a time: a second Pulse session is refused.
    assert_eq!(
        a.acquire(owner(PULSE, "pulse-2"), false, 10),
        AcquireOutcome::Busy {
            owner: pulse.clone()
        }
    );
    assert_eq!(
        a.acquire(dictation.clone(), false, 20),
        AcquireOutcome::Preempting {
            victim: pulse.clone()
        }
    );
    assert_eq!(a.phase(), Some(CapturePhase::Preempting));
    // Pulse learns it on its next heartbeat and must release at once.
    assert_eq!(a.heartbeat("pulse-1", 30), HeartbeatOutcome::Preempted);
    // A lower-priority requester cannot slip in while the handoff is pending.
    assert_eq!(
        a.acquire(owner(BATCH, "batch-1"), false, 40),
        AcquireOutcome::Busy {
            owner: pulse.clone()
        }
    );
    assert_eq!(
        a.release("pulse-1", 50),
        ReleaseOutcome::Released {
            handoff_to: Some(dictation.clone()),
            discarded_pending: false
        }
    );
    assert_eq!(a.current(), Some(&dictation));
    assert_eq!(a.phase(), Some(CapturePhase::Active));
    assert_eq!(a.heartbeat("hr-1", 60), HeartbeatOutcome::Ok);
}

#[test]
fn stop_hands_the_capture_to_the_waiting_preemptor_who_can_itself_be_preempted() {
    let mut a = CaptureArbiter::new();
    a.acquire(owner(BATCH, "b"), false, 0);
    a.acquire(owner(PULSE, "p"), false, 1);
    // The Batch owner stops with the preemptor's request already recorded: it goes to Pulse.
    match a.cancel("b", 2) {
        ReleaseOutcome::Released {
            handoff_to: Some(next),
            discarded_pending: true,
        } => assert_eq!(next.owner_id, "p"),
        other => panic!("{other:?}"),
    }
    // Dictation outranks the held Pulse capture and preempts it in turn.
    assert!(matches!(
        a.acquire(owner(DICTATION, "d"), false, 3),
        AcquireOutcome::Preempting { .. }
    ));
}

#[test]
fn a_higher_priority_requester_replaces_a_waiting_preemptor() {
    let mut a = CaptureArbiter::new();
    a.acquire(owner(BATCH, "b"), false, 0);
    assert!(matches!(
        a.acquire(owner(PULSE, "p"), false, 1),
        AcquireOutcome::Preempting { .. }
    ));
    assert!(matches!(
        a.acquire(owner(DICTATION, "d"), false, 2),
        AcquireOutcome::Preempting { .. }
    ));
    match a.release("b", 3) {
        ReleaseOutcome::Released {
            handoff_to: Some(next),
            ..
        } => assert_eq!(next.owner_id, "d"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_meeting_is_never_silently_preempted() {
    let mut a = CaptureArbiter::new();
    let meeting = owner(MEETING, "m");
    a.acquire(meeting.clone(), false, 0);
    for role in [PULSE, BATCH, MEETING] {
        assert_eq!(
            a.acquire(owner(role, "x"), true, 1),
            AcquireOutcome::Busy {
                owner: meeting.clone()
            }
        );
    }
    assert_eq!(
        a.acquire(owner(DICTATION, "d"), false, 2),
        AcquireOutcome::ConfirmRequired {
            owner: meeting.clone()
        }
    );
    assert_eq!(a.current(), Some(&meeting));
    assert_eq!(a.phase(), Some(CapturePhase::Active));
    assert_eq!(a.heartbeat("m", 3), HeartbeatOutcome::Ok);
    assert!(matches!(
        a.acquire(owner(DICTATION, "d"), true, 4),
        AcquireOutcome::Preempting { victim } if victim == meeting
    ));
}

#[test]
fn heartbeat_expiry_fences_the_owner_and_only_proven_release_frees_the_capture() {
    let mut a = CaptureArbiter::new();
    let pulse = owner(PULSE, "pulse-1");
    a.acquire(pulse.clone(), false, 0);
    assert_eq!(
        a.heartbeat("pulse-1", CAPTURE_HEARTBEAT_MS),
        HeartbeatOutcome::Ok
    );
    let silent_until = CAPTURE_HEARTBEAT_MS + CAPTURE_LEASE_MS;
    assert_eq!(
        a.heartbeat("pulse-1", silent_until - 1),
        HeartbeatOutcome::Ok,
        "a heartbeat inside the lease keeps it"
    );
    let lapsed = silent_until - 1 + CAPTURE_LEASE_MS;
    a.tick(lapsed);
    assert_eq!(a.phase(), Some(CapturePhase::Fenced));
    // A late heartbeat does not revive the owner.
    assert_eq!(a.heartbeat("pulse-1", lapsed + 1), HeartbeatOutcome::Fenced);
    // Nobody else gets the mic, whatever their priority.
    for (i, role) in [DICTATION, PULSE, BATCH].into_iter().enumerate() {
        assert_eq!(
            a.acquire(owner(role, &format!("x{i}")), true, lapsed + 2),
            AcquireOutcome::Fenced {
                owner: pulse.clone()
            }
        );
    }
    // The fenced owner's own release frame is not proof.
    assert_eq!(
        a.release("pulse-1", lapsed + 3),
        ReleaseOutcome::StillFenced {
            owner: pulse.clone()
        }
    );
    assert_eq!(
        a.cancel("pulse-1", lapsed + 3),
        ReleaseOutcome::StillFenced {
            owner: pulse.clone()
        }
    );
    assert_eq!(a.current(), Some(&pulse));
    // Proof (process gone, device closed) frees it.
    assert_eq!(
        a.prove_release("pulse-1", lapsed + 4),
        ReleaseOutcome::Released {
            handoff_to: None,
            discarded_pending: true
        }
    );
    assert_eq!(a.current(), None);
    assert_eq!(
        a.acquire(owner(DICTATION, "d"), false, lapsed + 5),
        AcquireOutcome::Granted
    );
}

#[test]
fn a_preemptor_waits_through_a_fence_and_gets_the_capture_on_proof() {
    let mut a = CaptureArbiter::new();
    a.acquire(owner(PULSE, "p"), false, 0);
    assert!(matches!(
        a.acquire(owner(DICTATION, "d"), false, 1),
        AcquireOutcome::Preempting { .. }
    ));
    // Pulse never answers the preemption: the lease runs from the moment it was told.
    a.tick(1 + CAPTURE_LEASE_MS);
    assert_eq!(a.phase(), Some(CapturePhase::Fenced));
    assert!(matches!(
        a.acquire(owner(DICTATION, "d"), false, 2 + CAPTURE_LEASE_MS),
        AcquireOutcome::Fenced { .. }
    ));
    // The reservation lapsed with the lease; proof of release frees the capture for anyone.
    match a.prove_release("p", 3 + CAPTURE_LEASE_MS) {
        ReleaseOutcome::Released {
            handoff_to: None, ..
        } => {}
        other => panic!("{other:?}"),
    }
}

#[test]
fn release_cancel_and_heartbeat_from_a_non_owner_change_nothing() {
    let mut a = CaptureArbiter::new();
    assert_eq!(a.release("nobody", 0), ReleaseOutcome::NotOwner);
    assert_eq!(a.heartbeat("nobody", 0), HeartbeatOutcome::NotOwner);
    a.acquire(owner(PULSE, "p"), false, 0);
    assert_eq!(a.cancel("other", 1), ReleaseOutcome::NotOwner);
    assert_eq!(a.prove_release("other", 1), ReleaseOutcome::NotOwner);
    assert_eq!(a.current().map(|o| o.owner_id.as_str()), Some("p"));
    // Asking again as the owner is idempotent and refreshes the lease.
    assert_eq!(
        a.acquire(owner(PULSE, "p"), false, 2),
        AcquireOutcome::Granted
    );
}

#[test]
fn stop_discards_pending_uploads_and_a_plain_release_does_not() {
    let mut a = CaptureArbiter::new();
    a.acquire(owner(PULSE, "p"), false, 0);
    assert_eq!(
        a.cancel("p", 1),
        ReleaseOutcome::Released {
            handoff_to: None,
            discarded_pending: true
        }
    );
    a.acquire(owner(PULSE, "p"), false, 2);
    assert_eq!(
        a.release("p", 3),
        ReleaseOutcome::Released {
            handoff_to: None,
            discarded_pending: false
        }
    );
}

#[test]
fn capture_commands_and_replies_round_trip_through_frames() {
    let commands = [
        CaptureCommand::Acquire {
            owner: owner(PULSE, "p"),
            confirm_preempt: false,
        },
        CaptureCommand::Acquire {
            owner: owner(DICTATION, "d"),
            confirm_preempt: true,
        },
        CaptureCommand::Heartbeat {
            owner_id: "p".into(),
        },
        CaptureCommand::Release {
            owner_id: "p".into(),
        },
        CaptureCommand::Cancel {
            owner_id: "p".into(),
        },
    ];
    for command in commands {
        let frame = ServiceEnvelope::capture_request("r1", "t", &command);
        assert_eq!(frame.schema_name, SCHEMA_CAPTURE);
        assert_eq!(frame.validate_envelope(), Ok(()));
        assert_eq!(
            frame.request_extended(),
            Ok(ExtendedRequest::Capture(command))
        );
    }
    let reply = CaptureReply {
        status: CaptureStatus::Preempting,
        owner: Some(owner(PULSE, "p")),
    };
    let frame = ServiceEnvelope::capture_state_frame("r1", "t", &reply);
    assert_eq!(frame.capture_reply(), Some(reply));
    let bare = ServiceEnvelope::capture_state_frame(
        "r1",
        "t",
        &CaptureReply {
            status: CaptureStatus::Ok,
            owner: None,
        },
    );
    assert_eq!(bare.capture_reply().map(|r| r.owner), Some(None));
    // Every status survives its wire spelling.
    for status in [
        CaptureStatus::Granted,
        CaptureStatus::Preempting,
        CaptureStatus::ConfirmRequired,
        CaptureStatus::Busy,
        CaptureStatus::Fenced,
        CaptureStatus::Ok,
        CaptureStatus::Preempted,
        CaptureStatus::Released,
        CaptureStatus::NotOwner,
    ] {
        assert_eq!(CaptureStatus::from_wire(status.as_str()), Some(status));
    }
}

#[test]
fn malformed_capture_and_cancel_frames_are_rejected_not_guessed() {
    let mut frame = ServiceEnvelope::capture_request(
        "r",
        "t",
        &CaptureCommand::Heartbeat {
            owner_id: "p".into(),
        },
    );
    frame.payload = Some(serde_json::json!({"kind": "capture_heartbeat", "owner_id": ""}));
    assert_eq!(frame.request_extended(), Err("invalid capture command"));
    frame.payload = Some(serde_json::json!({"kind": "capture_dance", "owner_id": "p"}));
    assert_eq!(frame.request_extended(), Err("invalid capture command"));
    frame.payload = Some(serde_json::json!({
        "kind": "capture_acquire", "role": "bogus", "owner_id": "p", "pid": 1
    }));
    assert_eq!(frame.request_extended(), Err("invalid capture command"));
    let mut cancel = ServiceEnvelope::cancel_job("r", "t", "job-1");
    assert_eq!(
        cancel.request_extended(),
        Ok(ExtendedRequest::Cancel {
            job_id: "job-1".into()
        })
    );
    cancel.payload = Some(serde_json::json!({"kind": "request_cancel"}));
    assert_eq!(cancel.request_extended(), Err("missing job id"));
}

#[test]
fn the_new_schemas_are_minor_4_only_and_old_allowlists_are_unchanged() {
    assert_eq!(SERVICE_PROTOCOL_MINOR_CAPTURE, 4);
    assert_eq!(SERVICE_ALLOWED_SCHEMAS.len(), 5);
    for schema in [SCHEMA_CANCEL, SCHEMA_CAPTURE] {
        assert!(!is_allowed_schema(schema));
        assert!(!is_allowed_schema_for_minor(schema, 3));
        assert!(is_allowed_schema_for_minor(schema, 4));
    }
    for schema in SERVICE_ALLOWED_SCHEMAS {
        assert!(is_allowed_schema_for_minor(schema, 0));
    }
    // Both are plain capabilities: a host that lacks them reports neither.
    let old = ServiceEnvelope::capabilities_with("r", "t", &[], &[CAPABILITY_QUEUE])
        .capabilities_info()
        .unwrap();
    assert!(!old.supports_cancel() && !old.supports_capture() && old.supports_queue());
    let new = ServiceEnvelope::capabilities_with(
        "r",
        "t",
        &[],
        &[CAPABILITY_QUEUE, CAPABILITY_CANCEL, CAPABILITY_CAPTURE],
    )
    .capabilities_info()
    .unwrap();
    assert!(new.supports_cancel() && new.supports_capture());
}
