#[tokio::test]
async fn remaining_control_plane_routes_reject_lower_trust_sources() {
    let workspace = unique_temp_dir("remaining-control-gates");
    let workspace_query = encode_query_value(&workspace.to_string_lossy());
    let app = build_router(state_with_session("s1"));

    let cases = vec![
        (
            "POST",
            format!("/schedules?workspaceRoot={workspace_query}"),
            Some(serde_json::json!({
                "id": "untrusted-schedule",
                "cronExpr": "0 9 * * *",
                "prompt": "run from untrusted"
            })),
            "remoteAgentUntrusted",
        ),
        (
            "PATCH",
            format!("/schedules/untrusted-schedule?workspaceRoot={workspace_query}"),
            Some(serde_json::json!({"paused": true})),
            "toolUntrusted",
        ),
        (
            "PUT",
            format!("/schedules/untrusted-schedule/time-zone?workspaceRoot={workspace_query}"),
            Some(serde_json::json!({"timeZone": "UTC"})),
            "untrusted",
        ),
        (
            "POST",
            format!("/schedules/untrusted-schedule/run?workspaceRoot={workspace_query}"),
            None,
            "remoteAgentUntrusted",
        ),
        (
            "POST",
            format!("/schedules/untrusted-schedule/pause?workspaceRoot={workspace_query}"),
            None,
            "toolUntrusted",
        ),
        (
            "DELETE",
            format!("/schedules/untrusted-schedule?workspaceRoot={workspace_query}"),
            None,
            "untrusted",
        ),
        (
            "POST",
            "/specs".to_string(),
            Some(serde_json::json!({
                "workspaceRoot": workspace.to_string_lossy(),
                "title": "Untrusted spec",
                "description": "Ship an untrusted change"
            })),
            "remoteAgentUntrusted",
        ),
        (
            "PATCH",
            "/specs/spec-1".to_string(),
            Some(serde_json::json!({"title": "mutated"})),
            "toolUntrusted",
        ),
        (
            "POST",
            "/specs/spec-1/plan".to_string(),
            Some(serde_json::json!({})),
            "untrusted",
        ),
        (
            "POST",
            "/specs/spec-1/tasks".to_string(),
            Some(serde_json::json!({
                "tasks": [{"description": "Do a thing"}]
            })),
            "remoteAgentUntrusted",
        ),
        (
            "POST",
            "/specs/spec-1/constitution/decisions".to_string(),
            Some(serde_json::json!({
                "rule": "No untrusted policy changes",
                "verdict": "compliant",
                "rationale": "route should reject first"
            })),
            "toolUntrusted",
        ),
        (
            "POST",
            "/specs/spec-1/checklist".to_string(),
            Some(serde_json::json!({})),
            "untrusted",
        ),
        (
            "POST",
            "/specs/spec-1/analyze".to_string(),
            None,
            "remoteAgentUntrusted",
        ),
        (
            "POST",
            "/specs/spec-1/implement".to_string(),
            Some(serde_json::json!({})),
            "toolUntrusted",
        ),
        (
            "PATCH",
            format!("/execution/backends/default?workspaceRoot={workspace_query}"),
            Some(serde_json::json!({"backendId": "local"})),
            "untrusted",
        ),
        (
            "PATCH",
            format!("/execution/backends/local?workspaceRoot={workspace_query}"),
            Some(serde_json::json!({"enabled": true})),
            "remoteAgentUntrusted",
        ),
        (
            "PUT",
            format!("/execution/network-profiles/locked?workspaceRoot={workspace_query}"),
            Some(serde_json::json!({"default": "deny"})),
            "toolUntrusted",
        ),
        (
            "DELETE",
            format!("/execution/network-profiles/locked?workspaceRoot={workspace_query}"),
            None,
            "untrusted",
        ),
        (
            "POST",
            format!("/execution/workspaces?workspaceRoot={workspace_query}"),
            Some(serde_json::json!({"backendId": "local"})),
            "remoteAgentUntrusted",
        ),
        (
            "POST",
            "/execution/processes/proc-1/stdin".to_string(),
            Some(serde_json::json!({"input": "yes"})),
            "toolUntrusted",
        ),
        (
            "POST",
            "/execution/processes/proc-1/kill".to_string(),
            None,
            "untrusted",
        ),
        (
            "POST",
            format!("/execution/backends/local/doctor?workspaceRoot={workspace_query}"),
            None,
            "remoteAgentUntrusted",
        ),
        (
            "POST",
            "/runtime/benchmark".to_string(),
            Some(serde_json::json!({
                "sessionId": "s1",
                "role": "coder",
                "goal": "measure this"
            })),
            "toolUntrusted",
        ),
        (
            "POST",
            "/runtime/promote".to_string(),
            Some(serde_json::json!({
                "sessionId": "s1",
                "role": "coder",
                "challengerModel": "example/model",
                "goal": "promote this"
            })),
            "untrusted",
        ),
        (
            "POST",
            "/runtime/terminal-bench/run".to_string(),
            Some(serde_json::json!({
                "sessionId": "s1",
                "taskId": "tb-001",
                "agent": "coder"
            })),
            "remoteAgentUntrusted",
        ),
        (
            "POST",
            "/runtime/compare".to_string(),
            Some(serde_json::json!({
                "prompt": "compare",
                "models": ["model-a", "model-b"]
            })),
            "toolUntrusted",
        ),
        (
            "POST",
            "/runtime/compare/judge".to_string(),
            Some(serde_json::json!({"id": "cmp-1", "winner": "A"})),
            "untrusted",
        ),
        (
            "POST",
            "/commands/execute".to_string(),
            Some(serde_json::json!({"input": "/help"})),
            "remoteAgentUntrusted",
        ),
        (
            "PUT",
            "/lsp".to_string(),
            Some(serde_json::json!({
                "workspaceRoot": workspace.to_string_lossy(),
                "servers": []
            })),
            "toolUntrusted",
        ),
        (
            "DELETE",
            "/teams/untrusted-team".to_string(),
            None,
            "untrusted",
        ),
        (
            "POST",
            "/tool-invocations/tc-1/cancel".to_string(),
            Some(serde_json::json!({"reason": "untrusted cancel"})),
            "remoteAgentUntrusted",
        ),
        (
            "POST",
            format!("/notifications/notification-1/dismiss?workspaceRoot={workspace_query}"),
            None,
            "toolUntrusted",
        ),
    ];

    for (method, uri, body, trust) in cases {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri.as_str())
            .header("x-coderight-source-trust", trust);
        let body = if let Some(body) = body {
            builder = builder.header("content-type", "application/json");
            Body::from(body.to_string())
        } else {
            Body::empty()
        };
        let response = app
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "{method} {uri} with {trust} should reject before route-specific work"
        );
    }
}
