fn task_graph_spec_evidence(
    session: &crate::api::LiveSession,
    tasks: &[FeatureTask],
    session_bound_to_spec: bool,
) -> Vec<SpecEvidence> {
    if !session_bound_to_spec {
        return Vec::new();
    }

    let task_ids = tasks
        .iter()
        .map(|task| task.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut evidence = Vec::new();
    for event in session.conduit.events_after(0) {
        let ProtocolEvent::TaskGraphUpdated {
            seq,
            created_at,
            summary,
            tasks: graph_tasks,
            ..
        } = event
        else {
            continue;
        };
        for task in graph_tasks {
            if !task_ids.contains(task.id.as_str()) {
                continue;
            }
            if !matches!(
                task.state,
                TaskState::Done | TaskState::Failed | TaskState::Cancelled
            ) {
                continue;
            }
            let passed = task.state == TaskState::Done;
            let hash_payload = serde_json::json!({
                "sessionId": session.id,
                "seq": seq,
                "summary": summary,
                "task": task,
            });
            evidence.push(SpecEvidence {
                id: format!("task_graph:{seq}:{}", task.id),
                session_id: session.id.clone(),
                task_id: Some(task.id.clone()),
                evidence_id: None,
                path: format!("task_graph:{seq}/{}", task.id),
                kind: "task_graph_event".to_string(),
                content_hash: rightkit_fs::digest::sha256_bytes(
                    hash_payload.to_string().as_bytes(),
                ),
                shippable: passed,
                producer: "task_graph".to_string(),
                provenance: "bound-session-task-graph-event".to_string(),
                verdict_passed: Some(passed),
                verdict_summary: Some(format!("{summary}: {}", task.title)),
                created_at: created_at.clone(),
            });
        }
    }
    evidence
}
