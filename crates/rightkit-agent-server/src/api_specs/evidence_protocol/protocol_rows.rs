fn protocol_provenance_evidence_row(
    session: &crate::api::LiveSession,
    event: ProtocolEvent,
) -> Option<ProtocolProvenanceEvidence> {
    match event {
            ProtocolEvent::SessionStarted {
                seq,
                created_at,
                title,
                ..
            } => Some(ProtocolProvenanceEvidence {
                seq,
                created_at,
                path: format!("session:started/{seq}"),
                kind: "session_started".to_string(),
                producer: "session_runtime".to_string(),
                shippable: false,
                verdict_passed: None,
                verdict_summary: title
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| format!("Session started: {value}"))
                    .unwrap_or_else(|| "Session started".to_string()),
                payload: serde_json::json!({
                    "sessionId": session.id,
                    "seq": seq,
                    "title": title,
                }),
            }),
            ProtocolEvent::RunInterrupted {
                seq,
                created_at,
                reason,
                ..
            } => Some(ProtocolProvenanceEvidence {
                seq,
                created_at,
                path: format!("session:run_interrupted/{seq}"),
                kind: "run_interrupted".to_string(),
                producer: "session_runtime".to_string(),
                shippable: false,
                verdict_passed: Some(false),
                verdict_summary: reason
                    .clone()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or_else(|| "Run interrupted".to_string()),
                payload: serde_json::json!({
                    "sessionId": session.id,
                    "seq": seq,
                    "reason": reason,
                }),
            }),
            ProtocolEvent::RunFailed {
                seq,
                created_at,
                error,
                kind,
                hint,
                ..
            } => Some(ProtocolProvenanceEvidence {
                seq,
                created_at,
                path: format!("session:run_failed/{seq}"),
                kind: "run_failed".to_string(),
                producer: "session_runtime".to_string(),
                shippable: false,
                verdict_passed: Some(false),
                verdict_summary: if hint.trim().is_empty() {
                    error.clone()
                } else {
                    format!("{error} ({hint})")
                },
                payload: serde_json::json!({
                    "sessionId": session.id,
                    "seq": seq,
                    "error": error,
                    "kind": kind,
                    "hint": hint,
                }),
            }),
            ProtocolEvent::SessionCompacted {
                seq,
                created_at,
                summary,
                dropped_count,
                ..
            } => Some(ProtocolProvenanceEvidence {
                seq,
                created_at,
                path: format!("session:compacted/{seq}"),
                kind: "session_compacted".to_string(),
                producer: "compaction".to_string(),
                shippable: false,
                verdict_passed: None,
                verdict_summary: format!("Session compacted; dropped {dropped_count} message(s)"),
                payload: serde_json::json!({
                    "sessionId": session.id,
                    "seq": seq,
                    "summary": summary,
                    "droppedCount": dropped_count,
                }),
            }),
            ProtocolEvent::UsageUpdated {
                seq,
                created_at,
                prompt_tokens,
                completion_tokens,
                cost_usd,
                context_tokens,
                context_percent,
                cache_read_tokens,
                cache_creation_tokens,
                ..
            } => Some(ProtocolProvenanceEvidence {
                seq,
                created_at,
                path: format!("session:usage/{seq}"),
                kind: "usage_updated".to_string(),
                producer: "usage_meter".to_string(),
                shippable: false,
                verdict_passed: None,
                verdict_summary: format!(
                    "Usage updated: {prompt_tokens} prompt tokens, {completion_tokens} completion tokens, ${cost_usd:.4}"
                ),
                payload: serde_json::json!({
                    "sessionId": session.id,
                    "seq": seq,
                    "promptTokens": prompt_tokens,
                    "completionTokens": completion_tokens,
                    "costUsd": cost_usd,
                    "contextTokens": context_tokens,
                    "contextPercent": context_percent,
                    "cacheReadTokens": cache_read_tokens,
                    "cacheCreationTokens": cache_creation_tokens,
                }),
            }),
            ProtocolEvent::BudgetExceeded {
                seq,
                created_at,
                scope,
                current_usd,
                ceiling_usd,
                hint,
                ..
            } => Some(ProtocolProvenanceEvidence {
                seq,
                created_at,
                path: format!("session:budget_exceeded/{seq}"),
                kind: "budget_exceeded".to_string(),
                producer: "session_budget".to_string(),
                shippable: false,
                verdict_passed: Some(false),
                verdict_summary: format!(
                    "Budget exceeded for {}: ${:.4} of ${:.4}",
                    if scope.trim().is_empty() {
                        "session"
                    } else {
                        scope.as_str()
                    },
                    current_usd,
                    ceiling_usd
                ),
                payload: serde_json::json!({
                    "sessionId": session.id,
                    "seq": seq,
                    "scope": scope,
                    "currentUsd": current_usd,
                    "ceilingUsd": ceiling_usd,
                    "hint": hint,
                }),
            }),
            ProtocolEvent::ProviderRouteUpdated {
                seq,
                created_at,
                state,
                provider,
                model,
                failure_kind,
                detail,
                ..
            } => {
                let failed = !failure_kind.trim().is_empty();
                Some(ProtocolProvenanceEvidence {
                    seq,
                    created_at,
                    path: format!("session:provider_route/{seq}"),
                    kind: "provider_route_event".to_string(),
                    producer: "provider_router".to_string(),
                    shippable: false,
                    verdict_passed: failed.then_some(false),
                    verdict_summary: if detail.trim().is_empty() {
                        format!("Provider route {state}: {provider}/{model}")
                    } else {
                        format!("Provider route {state}: {provider}/{model} ({detail})")
                    },
                    payload: serde_json::json!({
                        "sessionId": session.id,
                        "seq": seq,
                        "state": state,
                        "provider": provider,
                        "model": model,
                        "failureKind": failure_kind,
                        "detail": detail,
                    }),
                })
            }
        _ => protocol_provenance_coordination_evidence_row(session, event),
    }
}
