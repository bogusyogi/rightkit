#[cfg(test)]
mod tests_routes_schema {
    #![allow(clippy::await_holding_lock)]
    use super::*;

    use axum::body::Body;
    use axum::http::Request;

    use http_body_util::BodyExt;
    use tower::ServiceExt;
    #[tokio::test]
    async fn execution_backends_route_lists_default_registry() {
        let app = crate::api::build_router(DaemonState::new(""));
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/execution/backends")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["defaultBackend"], "local");
        assert!(json["backends"]
            .as_array()
            .unwrap()
            .iter()
            .any(|backend| { backend["id"] == "local" && backend["kind"] == "local" }));
    }

    #[test]
    fn execution_openapi_components_cover_execution_routes() {
        let components = execution_openapi_components();
        assert!(components.contains_key("ExecutionBackendRegistry"));
        assert!(components.contains_key("SetExecutionBackendDefaultRequest"));
        assert!(components.contains_key("UpdateExecutionBackendRequest"));
        assert!(components.contains_key("SaveExecutionNetworkProfileRequest"));
        assert!(components.contains_key("ExecutionWorkspacesResponse"));
        assert!(components.contains_key("ExecutionProcessRow"));
        assert!(components.contains_key("AcknowledgeExecutionProcessRequest"));
        assert_eq!(
            components["ExecutionProcessRow"]["properties"]["executionChannel"]["type"],
            "string"
        );
        assert_eq!(
            components["ExecutionProcessRow"]["properties"]["persistence"]["type"],
            "string"
        );
        assert_eq!(
            components["ExecutionProcessRow"]["properties"]["leaseEpoch"]["type"],
            "integer"
        );
        assert_eq!(
            components["WriteExecutionProcessStdinRequest"]["properties"]["inputSequence"]["type"],
            "integer"
        );
        assert_eq!(
            execution_backend_registry_response_schema()["$ref"],
            "#/components/schemas/ExecutionBackendRegistry"
        );
        assert_eq!(
            set_execution_backend_default_request_schema()["$ref"],
            "#/components/schemas/SetExecutionBackendDefaultRequest"
        );
        assert_eq!(
            execution_processes_response_schema()["$ref"],
            "#/components/schemas/ExecutionProcessesResponse"
        );
        assert_eq!(
            write_execution_process_stdin_request_schema()["$ref"],
            "#/components/schemas/WriteExecutionProcessStdinRequest"
        );
        assert_eq!(
            execution_action_request_schema()["$ref"],
            "#/components/schemas/ExecutionActionRequest"
        );
        assert_eq!(
            components["ExecutionProcessRow"]["properties"]["live"]["type"],
            "boolean"
        );
        assert_eq!(
            components["ExecutionProcessRow"]["properties"]["executionWorkspaceId"]["type"][0],
            "string"
        );
        assert_eq!(
            components["ExecutionProcessRow"]["properties"]["networkProfile"]["type"][0],
            "string"
        );
        assert_eq!(
            components["ExecutionProcessRow"]["properties"]["controlMode"]["type"],
            "string"
        );
        assert_eq!(
            components["ExecutionProcessRow"]["properties"]["controlUnavailableReason"]["type"][0],
            "string"
        );
        assert_eq!(
            components["CreateExecutionWorkspaceRequest"]["required"][0],
            "backendId"
        );
    }
}
