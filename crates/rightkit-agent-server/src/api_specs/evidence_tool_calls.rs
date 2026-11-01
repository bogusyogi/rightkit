#[derive(Debug, Clone)]
struct ToolCallEvidenceSeed {
    tool_name: String,
    args: serde_json::Value,
}

fn tool_result_spec_evidence(
    session: &crate::api::LiveSession,
    spec: &FeatureSpec,
    tasks: &[FeatureTask],
    session_bound_to_spec: bool,
) -> Vec<SpecEvidence> {
    let mut calls: HashMap<String, ToolCallEvidenceSeed> = HashMap::new();
    let mut evidence = Vec::new();

    for event in session.conduit.events_after(0) {
        match event {
            ProtocolEvent::ToolCallStarted { call, .. }
            | ProtocolEvent::ToolApprovalRequested { call, .. } => {
                calls
                    .entry(call.id.clone())
                    .or_insert_with(|| ToolCallEvidenceSeed {
                        tool_name: call.name,
                        args: serde_json::from_str(&call.arguments_json)
                            .unwrap_or(serde_json::Value::Null),
                    });
            }
            ProtocolEvent::ToolResultAdded {
                created_at,
                result:
                    TranscriptMessage::Tool {
                        tool_call_id,
                        summary,
                        output,
                        is_error,
                        ..
                    },
                ..
            } => {
                let seed = calls
                    .get(&tool_call_id)
                    .cloned()
                    .unwrap_or(ToolCallEvidenceSeed {
                        tool_name: "unknown".to_string(),
                        args: serde_json::Value::Null,
                    });
                let explicit_spec_id = string_field(&seed.args, "specId");
                if explicit_spec_id
                    .as_deref()
                    .is_some_and(|id| id != spec.id.as_str())
                {
                    continue;
                }
                if !session_bound_to_spec && explicit_spec_id.as_deref() != Some(spec.id.as_str()) {
                    continue;
                }

                let output_id = output
                    .as_ref()
                    .map(|out| out.id.as_str())
                    .unwrap_or("summary");
                let content_hash = output
                    .as_ref()
                    .and_then(|out| out.content_hash.clone())
                    .unwrap_or_else(|| rightkit_fs::digest::sha256_bytes(summary.as_bytes()));
                let kind = if is_error {
                    "tool_error"
                } else {
                    "tool_invocation"
                };
                evidence.push(SpecEvidence {
                    id: format!("tool:{tool_call_id}"),
                    session_id: session.id.clone(),
                    task_id: matching_task_id(&seed.args, tasks),
                    evidence_id: string_field(&seed.args, "evidenceId"),
                    path: format!("tool:{}/{tool_call_id}/{output_id}", seed.tool_name),
                    kind: kind.to_string(),
                    content_hash,
                    shippable: false,
                    producer: seed.tool_name,
                    provenance: "bound-session-tool-result".to_string(),
                    verdict_passed: Some(!is_error),
                    verdict_summary: Some(summary),
                    created_at,
                });
            }
            _ => {}
        }
    }

    evidence
}
