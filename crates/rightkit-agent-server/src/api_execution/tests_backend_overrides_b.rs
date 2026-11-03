#[cfg(test)]
mod tests_backend_overrides_b {
    #![allow(clippy::await_holding_lock)]
    use super::tests_support::*;
    use super::*;
    use axum::body::Body;
    use axum::http::Request;

    use http_body_util::BodyExt;
    use tower::ServiceExt;
    #[tokio::test]
    async fn execution_backend_resource_limits_reject_zero_values() {
        let root = test_workspace("backend-resource-limits-zero");
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
                    .body(Body::from(r#"{"resourceLimits":{"memoryMb":0}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "resource limits must be positive values");
    }

    #[tokio::test]
    async fn execution_backend_network_profile_override_persists_per_workspace() {
        let root = test_workspace("backend-network-profile");
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
                    .body(Body::from(r#"{"networkProfile":"local-host"}"#))
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
        assert_eq!(backend["networkProfile"], "local-host");

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
        assert_eq!(backend["networkProfile"], "local-host");
    }

    #[tokio::test]
    async fn execution_backend_network_profile_rejects_unknown_profile() {
        let root = test_workspace("backend-network-profile-missing");
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
                    .body(Body::from(r#"{"networkProfile":"missing"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "unknown network profile");
    }

    #[tokio::test]
    async fn execution_network_profile_definition_persists_per_workspace() {
        let root = test_workspace("backend-network-profile-definition");
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
                        r#"{"name":"Workspace egress","default":"deny","allowHosts":["github.com","api.openrouter.ai","github.com"],"allowPrivateNetworks":false,"denyPrivateNetworks":true}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let profile = json["networkProfiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|profile| profile["id"] == "workspace-egress")
            .unwrap();
        assert_eq!(profile["name"], "Workspace egress");
        assert_eq!(profile["default"], "deny");
        assert_eq!(profile["allowHosts"].as_array().unwrap().len(), 2);

        let resp = app
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
        assert_eq!(backend["networkProfile"], "workspace-egress");
        assert!(json["networkProfiles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|profile| profile["id"] == "workspace-egress"));
    }
}
