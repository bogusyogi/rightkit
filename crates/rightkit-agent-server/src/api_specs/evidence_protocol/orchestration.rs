fn orchestration_observation_spec_evidence(
    session: &crate::api::LiveSession,
    tasks: &[FeatureTask],
    session_bound_to_spec: bool,
) -> Vec<SpecEvidence> {
    if !session_bound_to_spec {
        return Vec::new();
    }

    let mut evidence = Vec::new();
    for event in session.conduit.events_after(0) {
        let ProtocolEvent::OrchestrationObservationRecorded {
            seq,
            created_at,
            observation,
            ..
        } = event
        else {
            continue;
        };
        let fields = observation_fields_json(&observation.fields);
        let verdict_passed = string_field(&fields, "verdictPassed")
            .or_else(|| string_field(&fields, "passed"))
            .and_then(|value| value.parse::<bool>().ok());
        let hash_payload = serde_json::json!({
            "sessionId": session.id,
            "seq": seq,
            "observation": observation,
        });
        let kind = if observation.kind.trim().is_empty() {
            "observation".to_string()
        } else {
            observation.kind.trim().to_string()
        };
        evidence.push(SpecEvidence {
            id: format!("orchestration:{seq}:{}", observation.id),
            session_id: session.id.clone(),
            task_id: matching_task_id(&fields, tasks),
            evidence_id: string_field(&fields, "evidenceId"),
            path: format!("orchestration:{kind}/{}", observation.id),
            kind: "orchestration_observation".to_string(),
            content_hash: rightkit_fs::digest::sha256_bytes(hash_payload.to_string().as_bytes()),
            shippable: verdict_passed.unwrap_or(false),
            producer: kind,
            provenance: "bound-session-orchestration-observation".to_string(),
            verdict_passed,
            verdict_summary: Some(observation.summary),
            created_at,
        });
    }
    evidence
}
