#[cfg(test)]
mod tests_process_lifecycle_a {
    #![allow(clippy::await_holding_lock)]
    use super::tests_support::*;
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use rightkit_agent_tools::coding_tools::Tool;
    use tower::ServiceExt;
    #[tokio::test(flavor = "multi_thread")]
    async fn execution_process_routes_list_detail_and_kill_live_background_shell() {
        let root = test_workspace("processes");
        let state = DaemonState::new("");
        let live = state.create_session(
            "s-process",
            None,
            &root.to_string_lossy(),
            "Process session",
            None,
        );
        let command = if cfg!(windows) {
            "Write-Output process-ready; Start-Sleep -Seconds 30"
        } else {
            "printf 'process-ready\\n'; sleep 30"
        };
        let shell_id =
            rightkit_agent_tools::coding_tools::background_shell::spawn_background_with_target(
                command.to_string(),
                &root,
                &live.background_shells,
                None,
                rightkit_agent_tools::coding_tools::background_shell::BgExecutionTarget {
                    backend_id: Some("local".to_string()),
                    workspace_id: Some("process-workspace".to_string()),
                    network_profile: Some("egress-restricted".to_string()),
                },
            )
            .expect("background admission");
        let app = crate::api::build_router(state);
        let root_query = encode_query_value(&root.to_string_lossy());

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
        let body = list.into_body().collect().await.unwrap().to_bytes();
        let listed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(listed["processes"].as_array().unwrap().len(), 1);
        assert_eq!(listed["processes"][0]["id"], shell_id);
        assert_eq!(listed["processes"][0]["sessionId"], "s-process");
        assert_eq!(listed["processes"][0]["backendId"], "local");
        assert_eq!(
            listed["processes"][0]["executionWorkspaceId"],
            "process-workspace"
        );
        assert_eq!(
            listed["processes"][0]["networkProfile"],
            "egress-restricted"
        );
        assert_eq!(listed["processes"][0]["status"], "running");

        let detail = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/execution/processes/{shell_id}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail.status(), StatusCode::OK);
        let body = detail.into_body().collect().await.unwrap().to_bytes();
        let detail: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(detail["process"]["id"], shell_id);
        assert_eq!(detail["process"]["command"], command);
        assert_eq!(
            detail["process"]["executionWorkspaceId"],
            "process-workspace"
        );
        assert_eq!(detail["process"]["networkProfile"], "egress-restricted");
        assert!(detail["process"]["output"].is_string());

        let kill = app
            .clone()
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
        let body = kill.into_body().collect().await.unwrap().to_bytes();
        let killed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(killed["process"]["id"], shell_id);
        assert!(matches!(
            killed["process"]["status"].as_str(),
            Some("running" | "killed")
        ));
        assert_eq!(
            killed["process"]["executionWorkspaceId"],
            "process-workspace"
        );
        assert_eq!(killed["process"]["networkProfile"], "egress-restricted");

        let fresh_app = crate::api::build_router(DaemonState::new(""));
        let historical = fresh_app
            .oneshot(
                Request::builder()
                    .uri(format!("/execution/processes?workspaceRoot={root_query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(historical.status(), StatusCode::OK);
        let body = historical.into_body().collect().await.unwrap().to_bytes();
        let historical: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(historical["processes"][0]["id"], shell_id);
        assert_eq!(historical["processes"][0]["live"], false);
        assert_eq!(
            historical["processes"][0]["executionWorkspaceId"],
            "process-workspace"
        );
        assert_eq!(
            historical["processes"][0]["networkProfile"],
            "egress-restricted"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execution_process_routes_list_completed_foreground_shell() {
        let root = test_workspace("foreground-processes");
        let state = DaemonState::new("");
        let live = state.create_session(
            "s-foreground",
            None,
            &root.to_string_lossy(),
            "Foreground process session",
            None,
        );
        let ctx = rightkit_agent_tools::coding_tools::ToolContext::new(
            "s-foreground",
            &root,
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_background_shells(live.background_shells.clone())
        .with_execution_backend("local")
        .with_execution_workspace_id("foreground-workspace")
        .with_execution_network_profile("local-host");
        let tool = rightkit_agent_tools::coding_tools::shell::ShellTool;
        let result = tool
            .execute(serde_json::json!({"command": "echo foreground-api"}), &ctx)
            .await;
        assert!(!result.is_error, "{}", result.content);

        let app = crate::api::build_router(state);
        let root_query = encode_query_value(&root.to_string_lossy());
        let list = app
            .oneshot(
                Request::builder()
                    .uri(format!("/execution/processes?workspaceRoot={root_query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);
        let body = list.into_body().collect().await.unwrap().to_bytes();
        let listed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let row = listed["processes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["kind"] == "foregroundShell")
            .expect("foreground shell process row");
        assert_eq!(row["sessionId"], "s-foreground");
        assert_eq!(row["backendId"], "local");
        assert_eq!(row["executionWorkspaceId"], "foreground-workspace");
        assert_eq!(row["networkProfile"], "local-host");
        assert_eq!(row["status"], "exited");
        assert_eq!(row["statusLabel"], "exited (code 0)");
        assert!(row["id"].as_str().unwrap().starts_with("fg-"));

        let _ = std::fs::remove_dir_all(root);
    }
}
