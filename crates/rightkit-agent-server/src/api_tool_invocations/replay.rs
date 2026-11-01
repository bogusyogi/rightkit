fn records_from_events(
    state: &DaemonState,
    events: Vec<ProtocolEvent>,
) -> Vec<ToolInvocationRecord> {
    builders_from_events(state, events)
        .into_values()
        .map(InvocationBuilder::into_record)
        .collect()
}

/// Reconcile a pre-effect origin that has no durable settlement after a
/// process restart. `ToolCallStarted` is committed immediately before the
/// effect on the mutating path; if no `ToolResultAdded` exists, the effect may
/// have executed and automatic replay is unsafe. Recovery closes that gap with
/// an explicit durable interrupted/unknown result and mirrors the same state in
/// the tool-invocation ledger. Re-running this function is idempotent because
/// the synthetic result becomes the durable settlement for the origin.
pub(crate) fn reconcile_interrupted_effects(
    store: &mut rightkit_agent_engine_storage::Storage,
    session_id: &str,
    events: &[rightkit_agent_engine_storage::EventRow],
) -> Result<usize, String> {
    let mut starts = BTreeMap::<String, (i64, String, ToolCall)>::new();
    let mut settled = std::collections::HashSet::<String>::new();
    for row in events {
        let Ok(event) = serde_json::from_str::<ProtocolEvent>(&row.payload_json) else {
            continue;
        };
        match event {
            ProtocolEvent::ToolCallStarted {
                session_id: sid,
                created_at,
                call,
                ..
            } if sid == session_id => {
                starts.insert(call.id.clone(), (row.seq, created_at, call));
            }
            ProtocolEvent::ToolResultAdded {
                session_id: sid,
                result: TranscriptMessage::Tool { tool_call_id, .. },
                ..
            } if sid == session_id => {
                settled.insert(tool_call_id);
            }
            _ => {}
        }
    }
    let mut next_seq = events.last().map(|row| row.seq).unwrap_or(0) + 1;
    let mut reconciled = 0usize;
    for (call_id, (_origin_seq, started_at, call)) in starts {
        if settled.contains(&call_id) {
            continue;
        }
        let created_at = rightkit_agent_engine_protocol::now_iso();
        let summary = "External effect settlement is interrupted/unknown after process recovery; automatic replay is forbidden until reconciled.".to_string();
        let event = ProtocolEvent::ToolResultAdded {
            session_id: session_id.to_string(),
            seq: next_seq,
            created_at: created_at.clone(),
            result: TranscriptMessage::Tool {
                id: format!("recovery:{call_id}"),
                created_at: created_at.clone(),
                tool_call_id: call_id.clone(),
                summary: summary.clone(),
                output: None,
                trust: None,
                is_error: true,
                pinned: true,
                process_id: None,
            },
        };
        let payload = serde_json::to_string(&event).map_err(|error| error.to_string())?;
        crate::api::append_event_via_storage_authority(
            store,
            session_id,
            next_seq,
            "toolResultAdded",
            &payload,
        )?;
        let mut row = store
            .get_tool_invocation(&call_id)
            .map_err(|error| error.to_string())?
            .unwrap_or_else(|| {
                let mut builder = builder_from_call(session_id, &call, &HashMap::new());
                builder.started_at = Some(started_at.clone());
                storage_row_from_builder(builder, "interrupted_unknown", &created_at)
            });
        row.lifecycle_state = "interrupted_unknown".to_string();
        row.completed_at = None;
        row.output_hash = None;
        row.error_class = Some("interrupted_unknown".to_string());
        row.updated_at = created_at.clone();
        store
            .upsert_tool_invocation(&row)
            .map_err(|error| error.to_string())?;
        store
            .append_tool_invocation_event(&storage_event_row(
                session_id,
                &call_id,
                next_seq,
                "effectSettlementInterruptedUnknown",
                &created_at,
                serde_json::json!({
                    "settlement": rightkit_agent_engine_protocol::ExternalEffectSettlement::InterruptedUnknown,
                    "automaticReplayAllowed": false
                }),
            ))
            .map_err(|error| error.to_string())?;
        next_seq += 1;
        reconciled += 1;
    }
    Ok(reconciled)
}

fn builders_from_events(
    state: &DaemonState,
    events: Vec<ProtocolEvent>,
) -> BTreeMap<String, InvocationBuilder> {
    let schema_versions = tool_schema_versions(state);
    let mut out: BTreeMap<String, InvocationBuilder> = BTreeMap::new();
    for event in events {
        match event {
            ProtocolEvent::ToolCallStarted {
                session_id,
                seq,
                created_at,
                call,
            } => {
                let builder = out
                    .entry(call.id.clone())
                    .or_insert_with(|| builder_from_call(&session_id, &call, &schema_versions));
                builder.lifecycle_state = "requested".to_string();
                builder.started_at = Some(created_at.clone());
                builder.events.push(invocation_event_row(
                    &session_id,
                    &call.id,
                    seq,
                    "toolCallStarted",
                    &created_at,
                    serde_json::json!({
                        "call": redacted_tool_call(&call),
                    }),
                ));
            }
            ProtocolEvent::ToolApprovalRequested {
                session_id,
                seq,
                created_at,
                call,
                risk_label,
                proposed_change,
                policy_source,
                middleware_decisions,
            } => {
                let builder = out
                    .entry(call.id.clone())
                    .or_insert_with(|| builder_from_call(&session_id, &call, &schema_versions));
                builder.lifecycle_state = "approvalRequested".to_string();
                builder.approval_id = Some(call.id.clone());
                builder.policy_verdict_id = policy_source.clone();
                builder.events.push(invocation_event_row(
                    &session_id,
                    &call.id,
                    seq,
                    "toolApprovalRequested",
                    &created_at,
                    serde_json::json!({
                        "call": redacted_tool_call(&call),
                        "riskLabel": risk_label,
                        "proposedChange": proposed_change,
                        "policySource": policy_source,
                        "middlewareDecisions": middleware_decisions,
                    }),
                ));
            }
            ProtocolEvent::ToolApprovalResolved {
                session_id,
                seq,
                created_at,
                tool_call_id,
                approved,
            } => {
                if let Some(builder) = out.get_mut(&tool_call_id) {
                    builder.lifecycle_state = if approved {
                        "approved".to_string()
                    } else {
                        "denied".to_string()
                    };
                    builder.approval_id = Some(tool_call_id.clone());
                    builder.events.push(invocation_event_row(
                        &session_id,
                        &tool_call_id,
                        seq,
                        "toolApprovalResolved",
                        &created_at,
                        serde_json::json!({
                            "toolCallId": tool_call_id,
                            "approved": approved,
                        }),
                    ));
                }
            }
            ProtocolEvent::ToolResultAdded {
                session_id,
                seq,
                created_at,
                result:
                    TranscriptMessage::Tool {
                        tool_call_id,
                        summary,
                        output,
                        is_error,
                        process_id,
                        ..
                    },
            } => {
                if let Some(builder) = out.get_mut(&tool_call_id) {
                    builder.lifecycle_state = if is_error {
                        "failed".to_string()
                    } else {
                        "completed".to_string()
                    };
                    builder.completed_at = Some(created_at.clone());
                    builder.output_hash = Some(output_hash(&summary, output.as_ref()));
                    builder.error_class = if is_error {
                        Some(error_class(&summary))
                    } else {
                        None
                    };
                    builder.process_id = process_id.clone();
                    builder.events.push(invocation_event_row(
                        &session_id,
                        &tool_call_id,
                        seq,
                        "toolResultAdded",
                        &created_at,
                        serde_json::json!({
                            "toolCallId": tool_call_id,
                            "summary": summary,
                            "output": output,
                            "isError": is_error,
                        }),
                    ));
                }
            }
            _ => {}
        }
    }
    merge_cancellation_events(state, &mut out);
    out
}

fn merge_cancellation_events(
    state: &DaemonState,
    builders: &mut BTreeMap<String, InvocationBuilder>,
) {
    let cancellations = lock_recover(
        &state.tool_invocation_cancellations,
        "state.tool_invocation_cancellations",
    )
    .clone();
    for cancellation in cancellations {
        let Some(builder) = builders.get_mut(&cancellation.invocation_id) else {
            continue;
        };
        if builder.session_id != cancellation.session_id
            || builder
                .events
                .iter()
                .any(|event| event.kind == "cancelRequested")
        {
            continue;
        }
        builder.lifecycle_state = "cancelled".to_string();
        let seq = builder
            .events
            .iter()
            .map(|event| event.seq)
            .max()
            .unwrap_or(0)
            + 1;
        builder.events.push(invocation_event_row(
            &cancellation.session_id,
            &cancellation.invocation_id,
            seq,
            "cancelRequested",
            &cancellation.created_at,
            serde_json::json!({
                "toolCallId": cancellation.invocation_id,
                "reason": cancellation.reason,
            }),
        ));
    }
}

fn builder_from_call(
    session_id: &str,
    call: &ToolCall,
    schema_versions: &HashMap<String, String>,
) -> InvocationBuilder {
    let args = serde_json::from_str::<Value>(&call.arguments_json).unwrap_or(Value::Null);
    InvocationBuilder {
        id: call.id.clone(),
        session_id: session_id.to_string(),
        tool_name: call.name.clone(),
        schema_version: schema_versions
            .get(&call.name)
            .cloned()
            .unwrap_or_else(|| "unknown".to_string()),
        args_hash: format!(
            "sha256:{}",
            rightkit_fs::digest::sha256_bytes(call.arguments_json.as_bytes())
        ),
        args_redacted_json: redact_json(args),
        lifecycle_state: "requested".to_string(),
        ..InvocationBuilder::default()
    }
}
