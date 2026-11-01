use super::{
    run_feedback_before_action, should_abort_ptt_chord, should_accept_fn_chord, should_arm_ptt,
    should_clear_ptt_hold_suppression, PttEnd, PttHandoff,
};

#[test]
fn ptt_chord_abort_expires_one_second_after_hold_begins() {
    assert!(should_abort_ptt_chord(0));
    assert!(should_abort_ptt_chord(999));
    assert!(!should_abort_ptt_chord(1_000));
    assert!(!should_abort_ptt_chord(7_639));
}

#[test]
fn ptt_does_not_arm_over_an_existing_recording() {
    assert!(should_arm_ptt(true, false, false));
    assert!(!should_arm_ptt(true, true, false));
    assert!(!should_arm_ptt(true, false, true));
    assert!(!should_arm_ptt(false, false, false));
}

#[test]
fn ptt_hold_suppression_survives_injected_modifier_flags() {
    assert!(!should_clear_ptt_hold_suppression(false, true));
    assert!(!should_clear_ptt_hold_suppression(true, false));
    assert!(should_clear_ptt_hold_suppression(false, false));
}

#[test]
fn fn_chords_reject_stale_or_synthetic_secondary_fn_flags() {
    assert!(should_accept_fn_chord(true, true, true));
    assert!(!should_accept_fn_chord(true, false, true));
    assert!(!should_accept_fn_chord(false, true, true));
    assert!(!should_accept_fn_chord(true, true, false));
}

#[test]
fn optimistic_feedback_precedes_slow_action_even_when_action_fails() {
    let order = std::cell::RefCell::new(Vec::new());
    let result: Result<(), &'static str> = run_feedback_before_action(
        || order.borrow_mut().push("feedback"),
        || {
            order.borrow_mut().push("action");
            Err("backend")
        },
    );
    assert_eq!(result, Err("backend"));
    assert_eq!(*order.borrow(), ["feedback", "action"]);
}

#[test]
fn release_during_start_waits_until_start_completes() {
    let handoff = PttHandoff::new();
    assert!(handoff.begin_start());
    assert_eq!(handoff.request_end(PttEnd::Stop, None), None);
    assert_eq!(handoff.complete_start(false), Some((PttEnd::Stop, None)));
    assert!(!handoff.is_active());
}

#[test]
fn release_after_start_owns_one_end_transition() {
    let handoff = PttHandoff::new();
    assert!(handoff.begin_start());
    assert_eq!(handoff.complete_start(true), None);
    assert_eq!(
        handoff.request_end(PttEnd::Send, None),
        Some((PttEnd::Send, None))
    );
    assert_eq!(handoff.request_end(PttEnd::Send, None), None);
}

#[test]
fn chord_during_start_defers_cancel() {
    let handoff = PttHandoff::new();
    assert!(handoff.begin_start());
    assert_eq!(handoff.request_end(PttEnd::Cancel, None), None);
    assert_eq!(handoff.complete_start(false), Some((PttEnd::Cancel, None)));
    assert!(!handoff.is_active());
}

#[test]
fn cancel_dominates_release_while_start_is_in_flight() {
    let handoff = PttHandoff::new();
    assert!(handoff.begin_start());
    assert_eq!(handoff.request_end(PttEnd::Stop, None), None);
    assert_eq!(handoff.request_end(PttEnd::Cancel, None), None);
    assert_eq!(handoff.complete_start(false), Some((PttEnd::Cancel, None)));
}

#[test]
fn deferred_cancel_preserves_exact_abort_cause() {
    let handoff = PttHandoff::new();
    assert!(handoff.begin_start());
    let cause = super::PttAbortCause {
        kind: super::PttAbortKind::ModifierJoined,
        key_code: None,
        modifiers: super::PttModifiers(super::PttModifiers::CTRL | super::PttModifiers::SHIFT),
        joined: super::PttModifiers(super::PttModifiers::SHIFT),
        phase: super::PttPhase::Starting,
        swallowed: false,
    };
    assert_eq!(handoff.request_end(PttEnd::Cancel, Some(cause)), None);
    assert_eq!(
        handoff.complete_start(false),
        Some((PttEnd::Cancel, Some(cause)))
    );
}

#[test]
fn abort_telemetry_is_normalized_without_user_content() {
    let line = super::format_abort(
        "macos",
        super::PttAbortCause {
            kind: super::PttAbortKind::KeyDown,
            key_code: Some(8),
            modifiers: super::PttModifiers(super::PttModifiers::CTRL),
            joined: super::PttModifiers::default(),
            phase: super::PttPhase::Arming,
            swallowed: false,
        },
    );
    assert_eq!(
            line,
            "platform=macos kind=key_down key_code=8 modifiers=ctrl:true,shift:false,alt:false,command_or_win:false,fn:false joined=ctrl:false,shift:false,alt:false,command_or_win:false,fn:false phase=arming swallowed=false"
        );
}
