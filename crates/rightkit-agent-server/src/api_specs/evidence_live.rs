fn live_spec_evidence(
    state: &DaemonState,
    spec: &FeatureSpec,
    tasks: &[FeatureTask],
) -> Vec<SpecEvidence> {
    let sessions = state
        .sessions
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect::<Vec<_>>();
    let mut evidence = Vec::new();

    for session in sessions {
        if session.workspace_root != spec.workspace_root {
            continue;
        }
        let session_bound_to_spec =
            lock_recover(&session.spec_id, "session.spec_id").as_deref() == Some(spec.id.as_str());
        let artifacts = lock_recover(&session.artifacts, "session.artifacts").clone();
        for row in artifacts {
            let meta = serde_json::from_str::<serde_json::Value>(&row.meta)
                .unwrap_or(serde_json::Value::Null);
            if string_field(&meta, "specId").as_deref() != Some(spec.id.as_str()) {
                continue;
            }
            let task_id = matching_task_id(&meta, tasks);
            evidence.push(SpecEvidence {
                id: row.id.clone(),
                session_id: session.id.clone(),
                task_id,
                evidence_id: string_field(&meta, "evidenceId"),
                path: row.path,
                kind: row.kind,
                content_hash: row.content_hash,
                shippable: row.shippable,
                producer: row.producer,
                provenance: row.provenance,
                verdict_passed: row.verdict.as_ref().map(|verdict| verdict.passed),
                verdict_summary: row.verdict.as_ref().map(|verdict| verdict.summary.clone()),
                created_at: row.created_at,
            });
        }
        evidence.extend(tool_result_spec_evidence(
            &session,
            spec,
            tasks,
            session_bound_to_spec,
        ));
        evidence.extend(tool_approval_spec_evidence(
            &session,
            spec,
            tasks,
            session_bound_to_spec,
        ));
        evidence.extend(verification_event_spec_evidence(
            &session,
            tasks,
            session_bound_to_spec,
        ));
        evidence.extend(task_graph_spec_evidence(
            &session,
            tasks,
            session_bound_to_spec,
        ));
        evidence.extend(orchestration_observation_spec_evidence(
            &session,
            tasks,
            session_bound_to_spec,
        ));
        evidence.extend(session_protocol_provenance_spec_evidence(
            &session,
            session_bound_to_spec,
        ));
    }

    evidence.sort_by(|a, b| a.id.cmp(&b.id));
    evidence
}

fn constitution_decision_spec_evidence(
    spec: &FeatureSpec,
    tasks: &[FeatureTask],
    decisions: &[ConstitutionDecision],
) -> Vec<SpecEvidence> {
    decisions
        .iter()
        .filter(|decision| decision.spec_id == spec.id)
        .map(|decision| {
            let passed = !matches!(decision.verdict, ConstitutionDecisionVerdict::Violation);
            let hash_payload = serde_json::json!({
                "specId": spec.id,
                "decision": decision,
            });
            SpecEvidence {
                id: format!("constitution:{}", decision.id),
                session_id: format!("spec:{}", spec.id),
                task_id: constitution_decision_task_id(decision, tasks),
                evidence_id: decision.evidence_ids.first().cloned(),
                path: format!("constitution:{}/{}", decision.source_path, decision.id),
                kind: "constitution_decision".to_string(),
                content_hash: rightkit_fs::digest::sha256_bytes(
                    hash_payload.to_string().as_bytes(),
                ),
                shippable: passed,
                producer: "constitution_review".to_string(),
                provenance: "spec-constitution-decision".to_string(),
                verdict_passed: Some(passed),
                verdict_summary: Some(format!("{:?}: {}", decision.verdict, decision.rationale)),
                created_at: decision.updated_at.clone(),
            }
        })
        .collect()
}

fn constitution_decision_task_id(
    decision: &ConstitutionDecision,
    tasks: &[FeatureTask],
) -> Option<String> {
    tasks
        .iter()
        .find(|task| {
            task.evidence_ids.iter().any(|task_evidence_id| {
                decision
                    .evidence_ids
                    .iter()
                    .any(|decision_evidence_id| decision_evidence_id == task_evidence_id)
            })
        })
        .map(|task| task.id.clone())
}
