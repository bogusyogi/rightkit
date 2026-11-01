mod camel_case;
mod event_extras;
mod round_trips;
mod user_row;

#[test]
fn subagent_spawn_envelope_carries_immediate_parent_through_replay() {
    let event = crate::ProtocolEvent::SubAgentSpawned {
        session_id: "immediate-parent".into(),
        seq: 17,
        created_at: "2026-08-26T00:00:00Z".into(),
        sub_session_id: "child".into(),
        role: "coder".into(),
        task_title: "repair".into(),
    };
    let wire = serde_json::to_value(&event).unwrap();
    assert_eq!(wire["sessionId"], "immediate-parent");
    assert_eq!(wire["subSessionId"], "child");
    let replayed: crate::ProtocolEvent = serde_json::from_value(wire).unwrap();
    assert!(matches!(
        replayed,
        crate::ProtocolEvent::SubAgentSpawned {
            session_id,
            sub_session_id,
            ..
        } if session_id == "immediate-parent" && sub_session_id == "child"
    ));
}
