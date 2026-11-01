#[cfg(test)]
mod tests_backend_overrides_a {
    #![allow(clippy::await_holding_lock)]
    use super::tests_support::*;
    use super::*;
    use axum::body::Body;
    use axum::http::Request;

    use http_body_util::BodyExt;
    use tower::ServiceExt;
    #[tokio::test]
    async fn execution_backend_default_override_persists_per_workspace() {
        let root = test_workspace("backend-default");
        let root_query = encode_query_value(root.to_str().unwrap());
        let app = crate::api::build_router(DaemonState::new(""));

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!(
                        "/execution/backends/local?workspaceRoot={root_query}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"enabled":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!(
                        "/execution/backends/default?workspaceRoot={root_query}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"backendId":"local"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["defaultBackend"], "local");

        let fresh_app = crate::api::build_router(DaemonState::new(""));
        let resp = fresh_app
            .oneshot(
                Request::builder()
                    .uri(format!("/execution/backends?workspaceRoot={root_query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["defaultBackend"], "local");
    }

    #[tokio::test]
    async fn execution_backend_enabled_override_persists_per_workspace() {
        let root = test_workspace("backend-enabled");
        let root_query = encode_query_value(root.to_str().unwrap());
        let app = crate::api::build_router(DaemonState::new(""));

        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!(
                        "/execution/backends/local?workspaceRoot={root_query}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"enabled":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["backends"]
            .as_array()
            .unwrap()
            .iter()
            .any(|backend| { backend["id"] == "local" && backend["enabled"] == true }));

        let fresh_app = crate::api::build_router(DaemonState::new(""));
        let resp = fresh_app
            .oneshot(
                Request::builder()
                    .uri(format!("/execution/backends?workspaceRoot={root_query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["backends"]
            .as_array()
            .unwrap()
            .iter()
            .any(|backend| { backend["id"] == "local" && backend["enabled"] == true }));
    }

    #[tokio::test]
    async fn execution_backend_resource_limits_override_persists_per_workspace() {
        let root = test_workspace("backend-resource-limits");
        let root_query = encode_query_value(root.to_str().unwrap());
        let app = crate::api::build_router(DaemonState::new(""));

        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!(
                        "/execution/backends/local?workspaceRoot={root_query}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"resourceLimits":{"cpu":3,"memoryMb":6144,"diskMb":65536}}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let backend = json["backends"]
            .as_array()
            .unwrap()
            .iter()
            .find(|backend| backend["id"] == "local")
            .unwrap();
        assert_eq!(backend["resourceLimits"]["cpu"], 3);
        assert_eq!(backend["resourceLimits"]["memoryMb"], 6144);
        assert_eq!(backend["resourceLimits"]["diskMb"], 65536);

        let fresh_app = crate::api::build_router(DaemonState::new(""));
        let resp = fresh_app
            .oneshot(
                Request::builder()
                    .uri(format!("/execution/backends?workspaceRoot={root_query}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let backend = json["backends"]
            .as_array()
            .unwrap()
            .iter()
            .find(|backend| backend["id"] == "local")
            .unwrap();
        assert_eq!(backend["resourceLimits"]["cpu"], 3);
        assert_eq!(backend["resourceLimits"]["memoryMb"], 6144);
        assert_eq!(backend["resourceLimits"]["diskMb"], 65536);

        let resolved = resolve_execution_backend_resource_limits(
            &root,
            &rightkit_sandbox::execution::ExecutionBackendRegistry::default(),
            Some("local"),
        )
        .unwrap()
        .expect("resource limits");
        assert_eq!(resolved.cpu, Some(3));
        assert_eq!(resolved.memory_mb, Some(6144));
        assert_eq!(resolved.disk_mb, Some(65536));
    }
}
