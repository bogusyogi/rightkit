#[cfg(test)]
mod tests_network_profiles {
    #![allow(clippy::await_holding_lock)]
    use super::tests_support::*;
    use super::*;
    use axum::body::Body;
    use axum::http::Request;

    use http_body_util::BodyExt;
    use tower::ServiceExt;
    #[tokio::test]
    async fn execution_network_profile_definition_rejects_bad_host() {
        let root = test_workspace("backend-network-profile-bad-host");
        let root_query = encode_query_value(root.to_str().unwrap());
        let app = crate::api::build_router(DaemonState::new(""));

        let resp = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!(
                        "/execution/network-profiles/workspace-egress?workspaceRoot={root_query}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"default":"deny","allowHosts":["https://github.com"]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["error"],
            "network profile allow hosts must be hostnames or host:port values"
        );
    }

    #[tokio::test]
    async fn execution_network_profile_definition_delete_rejects_in_use() {
        let root = test_workspace("backend-network-profile-delete-in-use");
        let root_query = encode_query_value(root.to_str().unwrap());
        let app = crate::api::build_router(DaemonState::new(""));

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!(
                        "/execution/network-profiles/workspace-egress?workspaceRoot={root_query}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"name":"Workspace egress","default":"deny","allowHosts":["github.com"]}"#,
                    ))
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
                        "/execution/backends/local?workspaceRoot={root_query}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"networkProfile":"workspace-egress"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!(
                        "/execution/network-profiles/workspace-egress?workspaceRoot={root_query}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json["error"],
            "network profile is still assigned to an execution backend"
        );
    }

    #[tokio::test]
    async fn execution_network_profile_definition_delete_rejects_builtin_profile() {
        let root = test_workspace("backend-network-profile-delete-builtin");
        let root_query = encode_query_value(root.to_str().unwrap());
        let app = crate::api::build_router(DaemonState::new(""));

        let resp = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!(
                        "/execution/network-profiles/local-host?workspaceRoot={root_query}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "built-in network profiles cannot be deleted");
    }

    #[tokio::test]
    async fn execution_backend_default_override_rejects_disabled_backend() {
        let root = test_workspace("backend-default-disabled");
        let root_query = encode_query_value(root.to_str().unwrap());
        let app = crate::api::build_router(DaemonState::new(""));

        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!(
                        "/execution/backends/default?workspaceRoot={root_query}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"backendId":"missing-backend"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn execution_backend_enabled_override_rejects_disabling_default() {
        let root = test_workspace("backend-disable-default");
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
                    .body(Body::from(r#"{"enabled":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn execution_backend_default_override_rejects_unknown_backend() {
        let root = test_workspace("backend-default-missing");
        let root_query = encode_query_value(root.to_str().unwrap());
        let app = crate::api::build_router(DaemonState::new(""));

        let resp = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!(
                        "/execution/backends/default?workspaceRoot={root_query}"
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"backendId":"missing"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
