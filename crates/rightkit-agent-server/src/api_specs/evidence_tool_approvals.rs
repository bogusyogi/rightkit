#[derive(Debug, Clone)]
struct ToolApprovalEvidenceSeed {
    tool_name: String,
    args: serde_json::Value,
    risk_label: String,
    policy_source: Option<String>,
    proposed_change_hash: Option<String>,
    requested_at: String,
}

fn tool_approval_spec_evidence(
    session: &crate::api::LiveSession,
    spec: &FeatureSpec,
    tasks: &[FeatureTask],
    session_bound_to_spec: bool,
) -> Vec<SpecEvidence> {
    let mut requested: HashMap<String, ToolApprovalEvidenceSeed> = HashMap::new();
    let mut evidence = Vec::new();

    for event in session.conduit.events_after(0) {
        match event {
            ProtocolEvent::ToolApprovalRequested {
                created_at,
                call,
                risk_label,
                proposed_change,
                policy_source,
                ..
            } => {
                let args =
                    serde_json::from_str(&call.arguments_json).unwrap_or(serde_json::Value::Null);
                let explicit_spec_id = string_field(&args, "specId");
                if explicit_spec_id
                    .as_deref()
                    .is_some_and(|id| id != spec.id.as_str())
                {
                    continue;
                }
                if !session_bound_to_spec && explicit_spec_id.as_deref() != Some(spec.id.as_str()) {
                    continue;
                }
                requested.insert(
                    call.id.clone(),
                    ToolApprovalEvidenceSeed {
                        tool_name: call.name,
                        args,
                        risk_label,
                        policy_source,
                        proposed_change_hash: proposed_change
                            .as_ref()
                            .map(|change| rightkit_fs::digest::sha256_bytes(change.as_bytes())),
                        requested_at: created_at,
                    },
                );
            }
            ProtocolEvent::ToolApprovalResolved {
                seq,
                created_at,
                tool_call_id,
                approved,
                ..
            } => {
                let Some(seed) = requested.get(&tool_call_id).cloned() else {
                    continue;
                };
                let hash_payload = serde_json::json!({
                    "sessionId": session.id,
                    "seq": seq,
                    "toolCallId": tool_call_id,
                    "tool": seed.tool_name,
                    "riskLabel": seed.risk_label,
                    "policySource": seed.policy_source,
                    "proposedChangeHash": seed.proposed_change_hash,
                    "requestedAt": seed.requested_at,
                    "resolvedAt": created_at,
                    "approved": approved,
                });
                evidence.push(SpecEvidence {
                    id: format!("approval:{tool_call_id}"),
                    session_id: session.id.clone(),
                    task_id: matching_task_id(&seed.args, tasks),
                    evidence_id: string_field(&seed.args, "evidenceId"),
                    path: format!("approval:{}/{tool_call_id}", seed.tool_name),
                    kind: "tool_approval".to_string(),
                    content_hash: rightkit_fs::digest::sha256_bytes(
                        hash_payload.to_string().as_bytes(),
                    ),
                    shippable: approved,
                    producer: seed.tool_name.clone(),
                    provenance: "bound-session-tool-approval".to_string(),
                    verdict_passed: Some(approved),
                    verdict_summary: Some(if approved {
                        format!(
                            "Tool approval granted for {} ({})",
                            seed.tool_name, seed.risk_label
                        )
                    } else {
                        format!(
                            "Tool approval denied for {} ({})",
                            seed.tool_name, seed.risk_label
                        )
                    }),
                    created_at,
                });
            }
            _ => {}
        }
    }

    evidence
}
