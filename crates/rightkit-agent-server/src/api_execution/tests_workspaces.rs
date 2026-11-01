#[cfg(test)]
mod tests_workspaces {
    #![allow(clippy::await_holding_lock)]
    use super::tests_support::*;
    use super::*;
    use axum::body::Body;
    use axum::http::Request;

    use http_body_util::BodyExt;
    use tower::ServiceExt;
    #[tokio::test]
    async fn execution_backend_doctor_reports_local_workspace() {
        let app = crate::api::build_router(DaemonState::new(""));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/execution/backends/local/doctor?workspaceRoot=.")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["backendId"], "local");
        assert!(json["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| { check["id"] == "local-workspace-root" && check["verdict"] == "pass" }));
    }

    #[tokio::test]
    async fn execution_backend_doctor_404s_unknown_backend() {
        let app = crate::api::build_router(DaemonState::new(""));
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/execution/backends/missing/doctor")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn execution_workspace_routes_create_and_list_backend_workspace() {
        let root = test_workspace("workspaces");
        let root_query = encode_query_value(&root.to_string_lossy());
        let app = crate::api::build_router(DaemonState::new(""));

        let create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/execution/workspaces?workspaceRoot={root_query}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "backendId": "local",
                            "workspaceId": "audit-run",
                            "name": "Audit run"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::CREATED);
        let body = create.into_body().collect().await.unwrap().to_bytes();
        let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(created["workspace"]["id"], "audit-run");
        assert_eq!(created["workspace"]["backendId"], "local");
        assert_eq!(created["workspace"]["name"], "Audit run");
        assert_eq!(created["workspace"]["status"], "ready");
        let layout_root = PathBuf::from(created["workspace"]["paths"]["root"].as_str().unwrap());
        assert!(layout_root.join("uploads").is_dir());
        assert!(layout_root.join("workspace").is_dir());
        assert!(layout_root.join("outputs").is_dir());
        assert!(layout_root.join("logs").is_dir());
        assert!(layout_root.join("skills").is_dir());

        let list = app
            .oneshot(
                Request::builder()
                    .uri(format!("/execution/workspaces?workspaceRoot={root_query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);
        let body = list.into_body().collect().await.unwrap().to_bytes();
        let listed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(listed["workspaces"].as_array().unwrap().len(), 1);
        assert_eq!(listed["workspaces"][0]["id"], "audit-run");
        assert!(PathBuf::from(listed["storePath"].as_str().unwrap()).exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn execution_workspace_create_rejects_unknown_backend() {
        let root = test_workspace("unknown-backend");
        let root_query = encode_query_value(&root.to_string_lossy());
        let app = crate::api::build_router(DaemonState::new(""));

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/execution/workspaces?workspaceRoot={root_query}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "backendId": "missing",
                            "workspaceId": "audit-run"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(root);
    }
}
