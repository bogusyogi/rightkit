use rightkit_agent_engine_protocol::{event, ExternalEffectSettlement};

#[test]
fn settlement_facades_expose_exactly_the_same_type() {
    let through_event: event::ExternalEffectSettlement =
        ExternalEffectSettlement::InterruptedUnknown;
    let through_module: rightkit_agent_engine_protocol::effect_settlement::ExternalEffectSettlement =
        through_event;
    assert_eq!(through_module, ExternalEffectSettlement::InterruptedUnknown);
}

#[test]
fn all_settlement_states_round_trip_without_relaxing_replay_safety() {
    for (state, wire) in [
        (ExternalEffectSettlement::NotStarted, "not_started"),
        (ExternalEffectSettlement::Running, "running"),
        (ExternalEffectSettlement::Succeeded, "succeeded"),
        (ExternalEffectSettlement::Failed, "failed"),
        (
            ExternalEffectSettlement::InterruptedUnknown,
            "interrupted_unknown",
        ),
    ] {
        let encoded = serde_json::to_value(state).unwrap();
        assert_eq!(encoded, serde_json::json!(wire));
        let decoded: ExternalEffectSettlement = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, state);
        assert_eq!(
            decoded.allows_automatic_non_idempotent_replay(),
            state == ExternalEffectSettlement::NotStarted,
        );
    }
}
