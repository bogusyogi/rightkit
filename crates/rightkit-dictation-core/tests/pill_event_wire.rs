//! Wire-compatibility fixtures for `rightkit_dictation_core::pill::PillEvent`.
//!
//! Fixtures were written from the host's checked-in serde derive (`tag = "kind"`, `rename_all =
//! "snake_case"`), rather than captured from a running build. Serialising must reproduce the
//! fixture byte for byte (trailing newline trimmed), and parsing the fixture must give back the
//! same event.

use rightkit_dictation_core::pill::{state_for_event, PillEvent, PillState};

fn cases() -> Vec<(&'static str, &'static str, PillEvent)> {
    vec![
        (
            "hide",
            include_str!("../fixtures/pill_event/hide.json"),
            PillEvent::Hide,
        ),
        (
            "idle",
            include_str!("../fixtures/pill_event/idle.json"),
            PillEvent::Idle,
        ),
        (
            "starting",
            include_str!("../fixtures/pill_event/starting.json"),
            PillEvent::Starting,
        ),
        (
            "recording",
            include_str!("../fixtures/pill_event/recording.json"),
            PillEvent::Recording,
        ),
        (
            "voice",
            include_str!("../fixtures/pill_event/voice.json"),
            PillEvent::Voice { level: 0.5 },
        ),
        (
            "processing_cancellable",
            include_str!("../fixtures/pill_event/processing_cancellable.json"),
            PillEvent::Processing { cancellable: true },
        ),
        (
            "processing_final_only",
            include_str!("../fixtures/pill_event/processing_final_only.json"),
            PillEvent::Processing { cancellable: false },
        ),
        (
            "success",
            include_str!("../fixtures/pill_event/success.json"),
            PillEvent::Success,
        ),
        (
            "status",
            include_str!("../fixtures/pill_event/status.json"),
            PillEvent::Status {
                label: "Saved".to_string(),
            },
        ),
        (
            "error",
            include_str!("../fixtures/pill_event/error.json"),
            PillEvent::Error {
                label: "Microphone unavailable".to_string(),
            },
        ),
    ]
}

#[test]
fn every_variant_has_a_fixture() {
    // Nine variants; `processing` has two fixtures (both values of `cancellable`).
    assert_eq!(cases().len(), 10);
    let mut kinds: Vec<String> = cases()
        .iter()
        .map(|(_, json, _)| {
            serde_json::from_str::<serde_json::Value>(json).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    kinds.sort();
    kinds.dedup();
    assert_eq!(
        kinds,
        [
            "error",
            "hide",
            "idle",
            "processing",
            "recording",
            "starting",
            "status",
            "success",
            "voice"
        ]
    );
}

#[test]
fn serialises_to_the_fixture_exactly() {
    for (name, json, event) in cases() {
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            json.trim(),
            "{name}"
        );
    }
}

#[test]
fn parses_the_fixture() {
    for (name, json, event) in cases() {
        let parsed: PillEvent = serde_json::from_str(json).unwrap();
        assert_eq!(parsed, event, "{name}");
    }
}

#[test]
fn round_trips() {
    for (name, _, event) in cases() {
        let text = serde_json::to_string(&event).unwrap();
        let back: PillEvent = serde_json::from_str(&text).unwrap();
        assert_eq!(back, event, "{name}");
        let value = serde_json::to_value(&event).unwrap();
        let back: PillEvent = serde_json::from_value(value).unwrap();
        assert_eq!(back, event, "{name}");
    }
}

#[test]
fn rejects_unknown_kind_and_missing_fields() {
    assert!(serde_json::from_str::<PillEvent>(r#"{"kind":"paused"}"#).is_err());
    assert!(serde_json::from_str::<PillEvent>(r#"{"kind":"voice"}"#).is_err());
    assert!(serde_json::from_str::<PillEvent>(r#"{"kind":"status"}"#).is_err());
    assert!(serde_json::from_str::<PillEvent>(r#"{"kind":"Hide"}"#).is_err());
    assert!(serde_json::from_str::<PillEvent>(r#"{}"#).is_err());
}

#[test]
fn render_state_mapping() {
    assert_eq!(state_for_event(&PillEvent::Hide), None);
    assert_eq!(state_for_event(&PillEvent::Voice { level: 0.2 }), None);
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
        state_for_event(&PillEvent::Processing { cancellable: true }),
        Some(PillState::Processing)
    );
    assert_eq!(
        state_for_event(&PillEvent::Success),
        Some(PillState::Success)
    );
    assert_eq!(
        state_for_event(&PillEvent::Status {
            label: String::new()
        }),
        Some(PillState::Success)
    );
    assert_eq!(
        state_for_event(&PillEvent::Error {
            label: String::new()
        }),
        Some(PillState::Error)
    );
}
