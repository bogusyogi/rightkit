fn verification_event_spec_evidence(
    session: &crate::api::LiveSession,
    tasks: &[FeatureTask],
    session_bound_to_spec: bool,
) -> Vec<SpecEvidence> {
    if !session_bound_to_spec {
        return Vec::new();
    }

    let default_task_id = single_verification_task_id(tasks);
    let mut evidence = Vec::new();
    for event in session.conduit.events_after(0) {
        let ProtocolEvent::VerificationFinished {
            seq,
            created_at,
            summary,
            passed,
            tool,
            issue_count,
            ..
        } = event
        else {
            continue;
        };
        let producer = if tool.trim().is_empty() {
            "verification".to_string()
        } else {
            tool.trim().to_string()
        };
        let hash_payload = serde_json::json!({
            "sessionId": session.id,
            "seq": seq,
            "summary": summary,
            "passed": passed,
            "tool": producer,
            "issueCount": issue_count,
        });
        evidence.push(SpecEvidence {
            id: format!("verification:{seq}"),
            session_id: session.id.clone(),
            task_id: default_task_id.clone(),
            evidence_id: None,
            path: format!("verification:{producer}/{seq}"),
            kind: "verification_event".to_string(),
            content_hash: rightkit_fs::digest::sha256_bytes(hash_payload.to_string().as_bytes()),
            shippable: passed,
            producer,
            provenance: "bound-session-verification-event".to_string(),
            verdict_passed: Some(passed),
            verdict_summary: Some(if issue_count > 0 {
                format!("{summary} ({issue_count} issues)")
            } else {
                summary
            }),
            created_at,
        });
    }
    evidence
}
