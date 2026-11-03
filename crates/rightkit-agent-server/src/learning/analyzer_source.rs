struct DaemonAnalyzerSource<'a> {
    state: &'a crate::api::DaemonState,
}

impl rightkit_adapt::AnalyzerSource for DaemonAnalyzerSource<'_> {
    fn recent_sessions(&self) -> Vec<SessionSummary> {
        let Some(storage) = &self.state.storage else {
            return vec![];
        };
        let Ok(store) = storage.try_lock() else {
            return vec![];
        };
        let Ok(sessions) = store.list_sessions() else {
            return vec![];
        };
        let mut summaries = sessions
            .into_iter()
            .map(|row| {
                let events = store.all_events(&row.id).unwrap_or_default();
                session_summary_from_events(&row.id, &row.status, &events)
            })
            .collect::<Vec<_>>();
        drop(store);

        let observations = self
            .state
            .runtime_middleware
            .lock()
            .map(|registry| registry.observations(None))
            .unwrap_or_default();
        for summary in &mut summaries {
            summary
                .notable_decisions
                .extend(execution_observation_evidence(observations.iter().filter(
                    |observation| observation.session_id == summary.session_id,
                )));
            summary.notable_decisions.sort();
            summary.notable_decisions.dedup();
        }
        summaries
    }
}

/// Validated execution observations are descriptive learning evidence only.
/// They never carry attributes into learning, never alter outcome/error counts,
/// & enter existing proposal flow as candidate context rather than authority.
fn execution_observation_evidence<'a>(
    observations: impl Iterator<Item = &'a rightkit_agent_engine_runtime_middleware::RuntimeObservation>,
) -> Vec<String> {
    observations
        .filter_map(|observation| {
            let payload = &observation.payload;
            let schema = payload
                .get("schemaVersion")
                .and_then(|value| value.as_str())?;
            let event_id = payload.get("eventId").and_then(|value| value.as_str())?;
            let session_id = payload.get("sessionId").and_then(|value| value.as_str())?;
            let phase = payload.get("phase").and_then(|value| value.as_str())?;
            let status = payload.get("status").and_then(|value| value.as_str())?;
            let created_at = payload.get("createdAt").and_then(|value| value.as_str())?;
            if schema != "coderight.execution-observation.v1"
                || event_id != observation.span_id
                || session_id != observation.session_id
                || created_at != observation.created_at
            {
                return None;
            }
            let parent = match payload.get("causalParentId") {
                Some(serde_json::Value::String(parent)) => parent.as_str(),
                Some(serde_json::Value::Null) | None => "root",
                _ => return None,
            };
            // "failed" is deliberately normalized here: this is provenance,
            // not an error event, so it must not trigger learning's frustration
            // heuristics or grant an observation decision-making force.
            let status = if status.eq_ignore_ascii_case("failed") {
                "unsuccessful"
            } else {
                status
            };
            Some(format!(
                "execution observation: phase={phase}, status={status}, parent={parent}"
            ))
        })
        .take(16)
        .collect()
}

fn session_summary_from_events(
    session_id: &str,
    status: &str,
    events: &[rightkit_agent_engine_storage::EventRow],
) -> SessionSummary {
    let mut succeeded = !matches!(status, "error" | "failed");
    let mut tool_calls = 0u32;
    let mut errors = Vec::new();
    let mut notable_decisions = Vec::new();

    for event in events {
        let value = serde_json::from_str::<serde_json::Value>(&event.payload_json)
            .unwrap_or(serde_json::Value::Null);
        match event.kind.as_str() {
            "toolCallStarted" => {
                tool_calls = tool_calls.saturating_add(1);
            }
            "toolResultAdded"
                if value
                    .pointer("/result/isError")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false) =>
            {
                errors.push(
                    extract_text(&value, &["/result/summary"])
                        .unwrap_or_else(|| "tool result error without summary".to_string()),
                );
            }
            "runFailed" => {
                succeeded = false;
                let mut text =
                    extract_text(&value, &["/error"]).unwrap_or_else(|| "run failed".to_string());
                if let Some(hint) = extract_text(&value, &["/hint"]) {
                    text.push_str(": ");
                    text.push_str(&hint);
                }
                errors.push(text);
            }
            "verificationFinished" => {
                let passed = value
                    .get("passed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                if !passed {
                    succeeded = false;
                    errors.push(
                        extract_text(&value, &["/summary"])
                            .unwrap_or_else(|| "verification failed".to_string()),
                    );
                    if let Some(issues) = value.get("issues").and_then(|v| v.as_array()) {
                        for issue in issues.iter().take(3) {
                            if let Some(message) = extract_text(issue, &["/message"]) {
                                errors.push(message);
                            }
                        }
                    }
                }
            }
            "budgetExceeded" => {
                succeeded = false;
                errors.push(
                    extract_text(&value, &["/hint"])
                        .unwrap_or_else(|| "budget exceeded".to_string()),
                );
            }
            "toolApprovalResolved"
                if value
                    .get("approved")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false) =>
            {
                if let Some(tool_call_id) = extract_text(&value, &["/toolCallId"]) {
                    notable_decisions.push(format!("approved tool call {tool_call_id}"));
                }
            }
            "humanSteeringInjected" => {
                if let Some(preview) = extract_text(&value, &["/contentPreview"]) {
                    notable_decisions.push(format!("human steering: {preview}"));
                }
            }
            "sessionCompacted" => {
                if let Some(summary) = extract_text(&value, &["/summary"]) {
                    notable_decisions.push(format!("compacted context: {summary}"));
                }
            }
            "providerRouteUpdated" => {
                if let Some(detail) = extract_text(&value, &["/detail"]) {
                    notable_decisions.push(format!("provider route update: {detail}"));
                }
            }
            "orchestrationObservationRecorded" => {
                if let Some(summary) = extract_text(&value, &["/observation/summary"]) {
                    notable_decisions.push(format!("observation: {summary}"));
                }
            }
            _ => {}
        }
    }

    errors.sort();
    errors.dedup();
    notable_decisions.sort();
    notable_decisions.dedup();

    SessionSummary {
        session_id: session_id.to_string(),
        succeeded,
        tool_calls,
        errors,
        notable_decisions,
    }
}

fn extract_text(value: &serde_json::Value, pointers: &[&str]) -> Option<String> {
    pointers.iter().find_map(|pointer| {
        value
            .pointer(pointer)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    })
}
