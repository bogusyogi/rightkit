/// `POST /qualification/matrix/run` — OPS-006 inside-out capability matrix.
///
/// This file is `include!`d into `api_runtime::implementation` (like
/// `resource_metrics.rs`), so it shares that module's imports and must not
/// restate them.
///
/// Every row is a probe sent over the live session's own governed executor
/// (`LiveSession::driver_tool_tx` -> `SessionEngine::authorize_external_tool_call`),
/// the same channel the CLI/MCP capability path uses. Probes are permission-only:
/// no tool body runs, so the matrix has no side effects. Each outcome is read
/// from the engine's response; nothing is preset. A probe that waits on an
/// approval is withdrawn after a bounded wait, which the engine treats as a
/// revoked approval.
const QUALIFICATION_MATRIX_VERSION: &str = "ops006.v1";
const QUALIFICATION_MAX_CAPABILITIES: usize = 64;
const QUALIFICATION_PROBE_TIMEOUT_SECS: u64 = 5;

/// Versioned rows: (row id, capability, probe arguments JSON, expected outcome).
/// An empty expected outcome records the observation without a pass/fail gate.
const QUALIFICATION_MATRIX_V1: &[(&str, &str, &str, &str)] = &[
    (
        "read_file.allowed",
        "read_file",
        r#"{"path":"ops006-probe.txt"}"#,
        "allowed",
    ),
    (
        "absent.unavailable",
        "ops006_absent_capability",
        "{}",
        "unavailable",
    ),
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QualificationMatrixRequest {
    session_id: String,
    #[serde(default)]
    capabilities: Vec<String>,
}

/// Classify one permission-only engine response into a matrix outcome.
fn qualification_outcome(
    result: &rightkit_agent_tools::coding_tools::ToolResult,
) -> (&'static str, String) {
    let outcome = if !result.is_error {
        "allowed"
    } else if result.content.contains("unknown tool") {
        // The engine reports a missing tool with this exact prefix; valid JSON
        // arguments are always supplied by the matrix, so no other cause maps here.
        "unavailable"
    } else if result.content.contains("permission denied") {
        "denied"
    } else {
        "rejected"
    };
    (outcome, result.content.clone())
}

async fn post_qualification_matrix_run(
    headers: HeaderMap,
    State(state): State<DaemonState>,
    Json(body): Json<QualificationMatrixRequest>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let session_id = body.session_id.trim().to_string();
    if session_id.is_empty() || body.capabilities.len() > QUALIFICATION_MAX_CAPABILITIES {
        return Err(StatusCode::BAD_REQUEST);
    }
    let live = state
        .get_session(&session_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    let sender = live
        .driver_tool_tx
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;

    let requested: Vec<String> = if body.capabilities.is_empty() {
        QUALIFICATION_MATRIX_V1
            .iter()
            .map(|row| row.1.to_string())
            .collect()
    } else {
        body.capabilities.clone()
    };

    let mut cases = Vec::with_capacity(requested.len());
    let mut failed = 0usize;
    for (index, capability) in requested.iter().enumerate() {
        let (row_id, arguments, expected): (String, &str, &str) = match QUALIFICATION_MATRIX_V1
            .iter()
            .find(|row| row.1 == capability.as_str())
        {
            Some(row) => (row.0.to_string(), row.2, row.3),
            None => (format!("{capability}.observed"), "{}", ""),
        };

        let (response, receiver) = tokio::sync::oneshot::channel();
        let probe = crate::api::DriverToolRequest {
            call: rightkit_agent_engine_protocol::ToolCall {
                id: format!("ops006:{session_id}:{index}"),
                name: capability.clone(),
                arguments_json: arguments.to_string(),
            },
            permission_only: true,
            response,
        };
        let (outcome, reason) = if sender.send(probe).await.is_err() {
            (
                "unavailable",
                "governed session executor stopped".to_string(),
            )
        } else {
            match tokio::time::timeout(
                std::time::Duration::from_secs(QUALIFICATION_PROBE_TIMEOUT_SECS),
                receiver,
            )
            .await
            {
                // Dropping the receiver withdraws the probe; the engine revokes it.
                Err(_) => (
                    "approval-required",
                    "permission decision still pending; probe withdrawn".to_string(),
                ),
                Ok(Err(_)) => (
                    "unavailable",
                    "governed session executor dropped its result".to_string(),
                ),
                Ok(Ok(result)) => qualification_outcome(&result),
            }
        };

        let gated = !expected.is_empty();
        let passed = gated.then(|| outcome == expected);
        if passed == Some(false) {
            failed += 1;
        }
        cases.push(json!({
            "id": row_id,
            "capability": capability,
            "arguments": arguments,
            "expected": if gated { json!(expected) } else { Value::Null },
            "outcome": outcome,
            "passed": passed,
            "reason": reason,
            "executionPath": "session-engine",
            "permissionOnly": true,
        }));
    }

    Ok(Json(json!({
        "schema": crate::host_identity::schema("qualification-matrix-run.v1"),
        "matrixVersion": QUALIFICATION_MATRIX_VERSION,
        "executionPath": "session-engine",
        "hostMode": "service",
        "sessionId": session_id,
        "sampledAt": now_iso(),
        "cases": cases,
        "passed": failed == 0,
    })))
}
