use rightkit_agent_engine_protocol::event::{
    EventCorrelationV1, EventEnvelopeV1, ProtocolEvent, EVENT_ENVELOPE_SCHEMA_V1,
};

fn durable(session_id: &str, seq: i64) -> ProtocolEvent {
    ProtocolEvent::SessionStarted {
        session_id: session_id.into(),
        seq,
        created_at: "2026-09-05T00:00:00Z".into(),
        title: None,
    }
}

#[test]
fn public_facade_round_trips_without_rewriting_the_legacy_event_body() {
    let event = durable("session-a", 7);
    let legacy_body = serde_json::to_value(&event).unwrap();
    let correlation = EventCorrelationV1 {
        run_id: Some("run-a".into()),
        turn_id: Some("turn-a".into()),
        task_id: Some("task-a".into()),
        tool_call_id: Some("call-a".into()),
        attempt_id: Some("attempt-a".into()),
        causal_parent_event_id: Some("parent-a".into()),
    };
    let envelope = EventEnvelopeV1::for_durable_event(event, correlation.clone()).unwrap();
    let encoded = serde_json::to_value(&envelope).unwrap();
    assert_eq!(encoded["schemaVersion"], EVENT_ENVELOPE_SCHEMA_V1);
    assert_eq!(encoded["event"], legacy_body);
    let decoded: EventEnvelopeV1 = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.event_id, envelope.event_id);
    assert_eq!(decoded.correlation, correlation);
    assert_eq!(serde_json::to_value(decoded.event).unwrap(), legacy_body);
}

#[test]
fn empty_correlation_is_backward_compatible() {
    let envelope =
        EventEnvelopeV1::for_durable_event(durable("session-a", 1), Default::default()).unwrap();
    let mut encoded = serde_json::to_value(envelope).unwrap();
    encoded.as_object_mut().unwrap().remove("correlation");
    let decoded: EventEnvelopeV1 = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded.correlation, EventCorrelationV1::default());
}

#[test]
fn durable_envelope_constructor_rejects_uncommitted_or_unbound_events() {
    for seq in [0, -1] {
        assert!(
            EventEnvelopeV1::for_durable_event(durable("session-a", seq), Default::default())
                .is_err()
        );
    }
    for session in ["", " "] {
        assert!(
            EventEnvelopeV1::for_durable_event(durable(session, 1), Default::default()).is_err()
        );
    }
}
