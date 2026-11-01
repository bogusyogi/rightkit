struct ProtocolProvenanceEvidence {
    seq: i64,
    created_at: String,
    path: String,
    kind: String,
    producer: String,
    shippable: bool,
    verdict_passed: Option<bool>,
    verdict_summary: String,
    payload: serde_json::Value,
}

fn session_protocol_provenance_spec_evidence(
    session: &crate::api::LiveSession,
    session_bound_to_spec: bool,
) -> Vec<SpecEvidence> {
    if !session_bound_to_spec {
        return Vec::new();
    }

    let mut evidence = Vec::new();
    for event in session.conduit.events_after(0) {
        let row = protocol_provenance_evidence_row(session, event);

        if let Some(row) = row {
            evidence.push(SpecEvidence {
                id: format!("protocol:{}:{}", row.kind, row.seq),
                session_id: session.id.clone(),
                task_id: None,
                evidence_id: None,
                path: row.path,
                kind: row.kind,
                content_hash: rightkit_fs::digest::sha256_bytes(row.payload.to_string().as_bytes()),
                shippable: row.shippable,
                producer: row.producer,
                provenance: "bound-session-protocol-event".to_string(),
                verdict_passed: row.verdict_passed,
                verdict_summary: Some(row.verdict_summary),
                created_at: row.created_at,
            });
        }
    }
    evidence
}
