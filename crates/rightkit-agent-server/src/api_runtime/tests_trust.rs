#[cfg(test)]
mod tests_trust {
    use super::*;

    use axum::{
        body::Body,
        http::{Method, Request},
    };

    use tower::ServiceExt;
    async fn lower_trust_runtime_request_status(
        app: axum::Router,
        method: Method,
        uri: &str,
        body: serde_json::Value,
    ) -> StatusCode {
        app.oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-coderight-source-trust", "remoteAgentUntrusted")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
    }

    #[tokio::test]
    async fn skillopt_and_skill_synthesis_routes_reject_lower_trust_sources() {
        let root = std::env::temp_dir().join(format!(
            "coderight-skillopt-trust-gate-{}",
            crate::api::now_millis()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let store_path = root.join("skillopt-runs.json");
        let app = crate::api::build_router(
            DaemonState::new("").with_skillopt_store_path(store_path.clone()),
        );

        let create_body = serde_json::json!({
            "name": "blocked-training",
            "target": {
                "skillId": "review",
                "objective": "improve review quality",
                "optimizer": "skillopt-local"
            }
        });
        let cases = [
            (Method::POST, "/runtime/skillopt/runs", create_body.clone()),
            (Method::POST, "/skills/training-runs", create_body),
            (
                Method::POST,
                "/runtime/skillopt/runs/run-blocked/resume",
                serde_json::json!({"message": "blocked"}),
            ),
            (
                Method::POST,
                "/skills/training-runs/run-blocked/execute",
                serde_json::json!({
                    "proposedBody": "candidate body",
                    "validationScore": 1.0
                }),
            ),
            (
                Method::POST,
                "/runtime/skillopt/runs/run-blocked/propose-candidate",
                serde_json::json!({"summary": "blocked candidate"}),
            ),
            (
                Method::POST,
                "/skills/training-runs/run-blocked/cancel",
                serde_json::json!({"reason": "blocked"}),
            ),
            (
                Method::POST,
                "/runtime/skills/synthesize",
                serde_json::json!({"taskDescription": "write a skill", "template": ""}),
            ),
        ];

        for (method, uri, body) in cases {
            let status = lower_trust_runtime_request_status(app.clone(), method, uri, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
        }
        assert!(
            !store_path.exists(),
            "lower-trust SkillOpt routes must fail before persisting training state"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn runtime_admin_routes_reject_lower_trust_sources() {
        let root = std::env::temp_dir().join(format!(
            "coderight-runtime-admin-trust-gate-{}",
            crate::api::now_millis()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let app = crate::api::build_router(DaemonState::new(""));
        let cases = [
            (
                Method::POST,
                "/runtime/learning/proposal-1/approve",
                serde_json::json!({}),
            ),
            (
                Method::POST,
                "/runtime/learning/proposal-1/reject",
                serde_json::json!({}),
            ),
            (
                Method::POST,
                "/runtime/harness-radar/proposals/harness-radar-awesome-agent-harness-multi-agent/promote",
                serde_json::json!({}),
            ),
            (
                Method::POST,
                "/runtime/harness-radar/schedule",
                serde_json::json!({
                    "workspaceRoot": root,
                    "cronExpr": "0 9 * * 1",
                    "timeZone": "UTC"
                }),
            ),
            (
                Method::PUT,
                "/runtime/middleware/builtin.trace-session/enabled",
                serde_json::json!({"enabled": false}),
            ),
        ];

        for (method, uri, body) in cases {
            let status = lower_trust_runtime_request_status(app.clone(), method, uri, body).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
        }
        assert!(
            !root.join(".coderight").join("schedules.json").exists(),
            "lower-trust Harness Radar scheduling must fail before writing schedules"
        );

        let _ = std::fs::remove_dir_all(root);
    }
}
