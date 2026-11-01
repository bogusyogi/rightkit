use super::*;

#[test]
fn fullscreen_suppresses_only_the_resting_pill() {
    assert!(pill_should_be_visible(true, false, false));
    assert!(pill_should_be_visible(false, true, false));
    assert!(pill_should_be_visible(true, true, true));
    assert!(pill_should_be_visible(true, false, true));
    assert!(!pill_should_be_visible(false, true, true));
    assert!(!pill_should_be_visible(false, false, false));
}

#[test]
fn event_for_message_dispatches() {
    assert_eq!(event_for_message(""), PillEvent::Hide);
    assert_eq!(event_for_message("Listening…"), PillEvent::Recording);
    assert_eq!(
        event_for_message("Transcribing"),
        PillEvent::Processing { cancellable: false }
    );
    assert_eq!(
        event_for_message("Pasting"),
        PillEvent::Processing { cancellable: false }
    );
    assert!(matches!(
        event_for_message("Error: foo"),
        PillEvent::Error { .. }
    ));
    assert_eq!(event_for_message("hello world"), PillEvent::Idle);
}

#[test]
fn pill_event_serde_matches_renderer_union() {
    // The TSX union keys on `kind` (snake_case). Lock the wire shape so the
    // renderer and Rust never drift.
    let cases = [
        (PillEvent::Hide, r#"{"kind":"hide"}"#),
        (PillEvent::Idle, r#"{"kind":"idle"}"#),
        (PillEvent::Starting, r#"{"kind":"starting"}"#),
        (PillEvent::Recording, r#"{"kind":"recording"}"#),
        (
            PillEvent::Voice { level: 0.5 },
            r#"{"kind":"voice","level":0.5}"#,
        ),
        (
            PillEvent::Processing { cancellable: false },
            r#"{"kind":"processing","cancellable":false}"#,
        ),
        (PillEvent::Success, r#"{"kind":"success"}"#),
        (
            PillEvent::Status {
                label: "Saved".into(),
            },
            r#"{"kind":"status","label":"Saved"}"#,
        ),
        (
            PillEvent::Error {
                label: "Error".into(),
            },
            r#"{"kind":"error","label":"Error"}"#,
        ),
    ];
    for (ev, json) in cases {
        assert_eq!(serde_json::to_string(&ev).unwrap(), json, "ser {ev:?}");
        assert_eq!(
            serde_json::from_str::<PillEvent>(json).unwrap(),
            ev,
            "de {json}"
        );
    }
}

#[test]
fn pill_hover_hitbox_rejects_popup_area_above_capsule() {
    // Prompt window: tall WebView, with the actual pill capsule down near
    // the bottom. A cursor over the prompt must not expand the pill.
    assert!(!pill_hover_hitbox_contains(
        118.0, 92.0, 52.0, 10.0, 140.0, 30.0
    ));
    assert!(pill_hover_hitbox_contains(
        118.0, 92.0, 52.0, 10.0, 140.0, 96.0
    ));
}

#[test]
fn pill_hover_hitbox_uses_exact_rendered_bounds() {
    assert!(pill_hover_hitbox_contains(
        20.0, 40.0, 52.0, 10.0, 20.0, 40.0
    ));
    assert!(!pill_hover_hitbox_contains(
        20.0, 40.0, 52.0, 10.0, 19.99, 40.0
    ));
    assert!(!pill_hover_hitbox_contains(
        20.0, 40.0, 52.0, 10.0, 72.01, 50.0
    ));
}

#[test]
fn pill_hover_pass_gap_is_pinned() {
    assert_eq!(PILL_HOVER_ACTIVE_POLL_MS, 40);
}

#[test]
fn windows_hover_gate_input_is_stable_window_rect_only() {
    assert!(windows_hover_gate_input(true));
    assert!(!windows_hover_gate_input(false));
}

#[test]
fn state_for_event_maps_modes() {
    assert_eq!(state_for_event(&PillEvent::Idle), Some(PillState::Idle));
    assert_eq!(
        state_for_event(&PillEvent::Starting),
        Some(PillState::Processing)
    );
    assert_eq!(
        state_for_event(&PillEvent::Recording),
        Some(PillState::Recording)
    );
    assert_eq!(
        state_for_event(&PillEvent::Processing { cancellable: false }),
        Some(PillState::Processing)
    );
    assert_eq!(
        state_for_event(&PillEvent::Success),
        Some(PillState::Success)
    );
    assert_eq!(
        state_for_event(&PillEvent::Status {
            label: "Saved".into()
        }),
        Some(PillState::Success)
    );
    assert_eq!(
        state_for_event(&PillEvent::Error { label: "x".into() }),
        Some(PillState::Error)
    );
    // Hide + Voice never change the render mode.
    assert_eq!(state_for_event(&PillEvent::Hide), None);
    assert_eq!(state_for_event(&PillEvent::Voice { level: 0.7 }), None);
}
