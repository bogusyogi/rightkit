#[cfg(test)]
mod tests_process_lifecycle_b {
    #![allow(clippy::await_holding_lock)]
    use super::tests_support::*;
    use super::*;
    use axum::body::Body;
    use axum::http::Request;

    use http_body_util::BodyExt;
    use tower::ServiceExt;
    #[tokio::test]
    async fn execution_process_detail_404s_unknown_process() {
        let app = crate::api::build_router(DaemonState::new(""));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/execution/processes/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execution_process_create_route_starts_local_terminal_and_accepts_stdin() {
        let root = test_workspace("terminal-create");
        let state = DaemonState::new("");
        state.create_session(
            "s-terminal",
            None,
            &root.to_string_lossy(),
            "Terminal session",
            None,
        );
        let app = crate::api::build_router(state);

        let create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/execution/processes")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "sessionId": "s-terminal",
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::OK);
        let body = create.into_body().collect().await.unwrap().to_bytes();
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(created["process"]["kind"], "terminalSession");
        assert_eq!(created["process"]["sessionId"], "s-terminal");
        assert_eq!(created["process"]["backendId"], "local");
        let shell_id = created["process"]["id"]
            .as_str()
            .expect("terminal process id")
            .to_string();
        let lease_epoch = created["process"]["leaseEpoch"]
            .as_u64()
            .expect("terminal process leaseEpoch");
        let input_sequence = created["process"]["nextInputSequence"]
            .as_u64()
            .expect("terminal process nextInputSequence");

        let input = if cfg!(windows) {
            "Write-Output 'terminal-api'"
        } else {
            "printf 'terminal-api\\n'"
        };
        let write = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/execution/processes/{shell_id}/stdin"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "input": input,
                            "appendNewline": true,
                            "leaseEpoch": lease_epoch,
                            "inputSequence": input_sequence,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(write.status(), StatusCode::OK);

        let mut logs = serde_json::Value::Null;
        // A cold PowerShell start can take well past 12s when the whole
        // workspace suite is running in parallel on a loaded Windows host,
        // which showed up as an intermittent empty-log failure here. The
        // assertion below is unchanged -- this only widens the wait so a slow
        // start is not reported as missing output.
        for _ in 0..300 {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!(
                            "/execution/processes/{shell_id}/logs?tailBytes=1024"
                        ))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            logs = serde_json::from_slice(&body).unwrap();
            if logs["logs"]["output"]
                .as_str()
                .is_some_and(|output| output.contains("terminal-api"))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            logs["logs"]["output"]
                .as_str()
                .is_some_and(|output| output.contains("terminal-api")),
            "terminal output missing from logs: {logs}"
        );

        let kill = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/execution/processes/{shell_id}/kill"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(kill.status(), StatusCode::OK);

        let _ = std::fs::remove_dir_all(root);
    }
}
