use rightkit_agent_engine_protocol::{ProtocolEvent, ProtocolEventMeta};

#[test]
fn dispatch_decided_uses_protocol_event_envelope() {
    let event = ProtocolEvent::DispatchDecided {
        session_id: "s1".into(),
        seq: 42,
        created_at: "2026-06-29T00:00:00Z".into(),
        strategy: "multi-agent".into(),
        rationale: "repo-wide work benefits from decomposition".into(),
    };

    assert_eq!(
        event.meta(),
        ProtocolEventMeta {
            session_id: "s1".into(),
            seq: 42,
            created_at: "2026-06-29T00:00:00Z".into(),
        }
    );

    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains(r#""$kind":"dispatchDecided""#), "{json}");
    assert!(json.contains(r#""strategy":"multi-agent""#), "{json}");

    let round_trip: ProtocolEvent = serde_json::from_str(&json).unwrap();
    assert!(matches!(
        round_trip,
        ProtocolEvent::DispatchDecided {
            strategy,
            rationale,
            ..
        } if strategy == "multi-agent" && rationale.contains("decomposition")
    ));
}
