#[cfg(test)]
mod tests_process_logs_stdin {
    #![allow(clippy::await_holding_lock)]
    use super::tests_support::*;
    use super::*;
    use axum::body::Body;
    use axum::http::Request;

    use http_body_util::BodyExt;
    use tower::ServiceExt;
    #[tokio::test(flavor = "multi_thread")]
    async fn execution_process_logs_route_returns_tail_snapshot() {
        let root = test_workspace("process-logs");
        let state = DaemonState::new("");
        let live = state.create_session(
            "s-logs",
            None,
            &root.to_string_lossy(),
            "Logs session",
            None,
        );
        let command = if cfg!(windows) {
            "Write-Output log-alpha; Write-Output log-beta; Start-Sleep -Seconds 10"
        } else {
            "printf 'log-alpha\\nlog-beta\\n'; sleep 10"
        };
        let shell_id = rightkit_agent_tools::coding_tools::background_shell::spawn_background(
            command.to_string(),
            &root,
            &live.background_shells,
            None,
        )
        .expect("background admission");
        let app = crate::api::build_router(state);

        let mut logs = serde_json::Value::Null;
        for _ in 0..600 {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/execution/processes/{shell_id}/logs?tailBytes=9"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            logs = serde_json::from_slice(&body).unwrap();
            if logs["logs"]["truncated"] == true
                && logs["logs"]["output"]
                    .as_str()
                    .is_some_and(|output| output == "log-beta\n")
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        assert_eq!(logs["process"]["id"], shell_id);
        assert_eq!(logs["logs"]["processId"], shell_id);
        assert_eq!(logs["logs"]["tailBytes"], 9);
        assert_eq!(logs["logs"]["truncated"], true);
        assert_eq!(logs["logs"]["output"], "log-beta\n");

        let _ = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/execution/processes/{shell_id}/kill"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execution_process_list_persists_history_for_workspace_reload() {
        let root = test_workspace("process-history");
        let root_query = encode_query_value(&root.to_string_lossy());
        let state = DaemonState::new("");
        let live = state.create_session(
            "s-history",
            None,
            &root.to_string_lossy(),
            "History session",
            None,
        );
        let command = if cfg!(windows) {
            "Write-Output history-ready; Start-Sleep -Seconds 10"
        } else {
            "printf 'history-ready\\n'; sleep 10"
        };
        let shell_id = rightkit_agent_tools::coding_tools::background_shell::spawn_background(
            command.to_string(),
            &root,
            &live.background_shells,
            None,
        )
        .expect("background admission");
        let app = crate::api::build_router(state);

        let list = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/execution/processes?workspaceRoot={root_query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);

        let reloaded = crate::api::build_router(DaemonState::new(""));
        let history = reloaded
            .oneshot(
                Request::builder()
                    .uri(format!("/execution/processes?workspaceRoot={root_query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(history.status(), StatusCode::OK);
        let body = history.into_body().collect().await.unwrap().to_bytes();
        let history: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(history["processes"].as_array().unwrap().len(), 1);
        assert_eq!(history["processes"][0]["id"], shell_id);
        assert_eq!(history["processes"][0]["sessionId"], "s-history");
        assert_eq!(history["processes"][0]["command"], command);
        assert_eq!(history["processes"][0]["live"], false);
        assert_eq!(history["processes"][0]["status"], "unavailable");
        assert!(PathBuf::from(history["storePath"].as_str().unwrap()).exists());

        let _ = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/execution/processes/{shell_id}/kill"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execution_process_stdin_route_writes_to_live_background_shell() {
        let root = test_workspace("process-stdin");
        let state = DaemonState::new("");
        let live = state.create_session(
            "s-stdin",
            None,
            &root.to_string_lossy(),
            "Stdin session",
            None,
        );
        let command = if cfg!(windows) {
            "$line = [Console]::In.ReadLine(); Write-Output \"stdin:$line\"; Start-Sleep -Seconds 10"
        } else {
            "IFS= read -r line; printf 'stdin:%s\\n' \"$line\"; sleep 10"
        };
        let shell_id = rightkit_agent_tools::coding_tools::background_shell::spawn_background(
            command.to_string(),
            &root,
            &live.background_shells,
            None,
        )
        .expect("background admission");
        let spawn_started = std::time::Instant::now();
        let app = crate::api::build_router(state);
        let probe = live
            .background_shells
            .lock()
            .unwrap()
            .get(&shell_id)
            .cloned();
        if let Some(probe) = probe {
            while probe.pid().is_none()
                && spawn_started.elapsed() < std::time::Duration::from_secs(30)
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }
        eprintln!(
            "confined background spawn took {:?}",
            spawn_started.elapsed()
        );

        let write = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/execution/processes/{shell_id}/stdin"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "input": "typed-from-api",
                            "appendNewline": true,
                            "leaseEpoch": 1,
                            "inputSequence": 0
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let status = write.status();
        let body = write.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let written: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(written["process"]["id"], shell_id);

        let mut detail = serde_json::Value::Null;
        for _ in 0..100 {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!("/execution/processes/{shell_id}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let body = resp.into_body().collect().await.unwrap().to_bytes();
            detail = serde_json::from_slice(&body).unwrap();
            if detail["process"]["output"]
                .as_str()
                .is_some_and(|output| output.contains("stdin:typed-from-api"))
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(
            detail["process"]["output"]
                .as_str()
                .is_some_and(|output| output.contains("stdin:typed-from-api")),
            "detail output did not contain echoed stdin: {detail:#}"
        );

        let _ = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/execution/processes/{shell_id}/kill"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        let _ = std::fs::remove_dir_all(root);
    }
}
