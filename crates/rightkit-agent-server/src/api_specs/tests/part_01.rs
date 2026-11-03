use super::*;
use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use std::collections::BTreeSet;
use tower::ServiceExt;

#[test]
fn spec_openapi_components_include_helper_ref_targets() {
    let components = specs_openapi_components();
    let mut refs = BTreeSet::new();
    for endpoint in spec_manifest_endpoints() {
        if matches!(endpoint.method, "POST" | "PATCH") {
            collect_schema_refs(
                &specs_openapi_request_schema(endpoint.method, endpoint.path).unwrap(),
                &mut refs,
            );
        }
        collect_schema_refs(
            &specs_openapi_response_schema(endpoint.method, endpoint.path).unwrap(),
            &mut refs,
        );
    }

    assert!(!refs.is_empty());
    for schema_ref in refs {
        let name = schema_ref
            .strip_prefix("#/components/schemas/")
            .expect("spec helper refs should target OpenAPI component schemas");
        assert!(
            components.contains_key(name),
            "missing spec OpenAPI component for {schema_ref}"
        );
    }
}

#[test]
fn spec_openapi_request_helpers_cover_mutating_spec_routes() {
    let routes = spec_manifest_endpoints();
    let mut covered = BTreeSet::new();
    for endpoint in routes {
        if !matches!(endpoint.method, "POST" | "PATCH") {
            assert!(
                specs_openapi_request_schema(endpoint.method, endpoint.path).is_none(),
                "{} {} should not declare a request body schema",
                endpoint.method,
                endpoint.path
            );
            continue;
        }
        let schema = specs_openapi_request_schema(endpoint.method, endpoint.path)
            .unwrap_or_else(|| panic!("missing request schema for {}", endpoint.path));
        assert!(
            schema_ref_name(&schema).is_some(),
            "request schema for {} {} should be a component ref",
            endpoint.method,
            endpoint.path
        );
        covered.insert((endpoint.method, endpoint.path));
    }

    assert_eq!(
        covered,
        BTreeSet::from([
            ("POST", "/specs"),
            ("PATCH", "/specs/{id}"),
            ("POST", "/specs/{id}/clarify"),
            ("POST", "/specs/{id}/plan"),
            ("POST", "/specs/{id}/tasks"),
            ("POST", "/specs/{id}/constitution/decisions"),
            ("POST", "/specs/{id}/checklist"),
            ("POST", "/specs/{id}/analyze"),
            ("POST", "/specs/{id}/implement"),
        ])
    );
}

#[test]
fn spec_openapi_response_helpers_cover_all_spec_routes() {
    let mut covered = BTreeSet::new();
    for endpoint in spec_manifest_endpoints() {
        let schema = specs_openapi_response_schema(endpoint.method, endpoint.path)
            .unwrap_or_else(|| panic!("missing response schema for {}", endpoint.path));
        let mut refs = BTreeSet::new();
        collect_schema_refs(&schema, &mut refs);
        assert!(
            !refs.is_empty(),
            "response schema for {} {} should reference a component",
            endpoint.method,
            endpoint.path
        );
        covered.insert((endpoint.method, endpoint.path));
    }

    assert_eq!(
        covered,
        BTreeSet::from([
            ("GET", "/specs"),
            ("POST", "/specs"),
            ("GET", "/specs/{id}"),
            ("PATCH", "/specs/{id}"),
            ("POST", "/specs/{id}/clarify"),
            ("POST", "/specs/{id}/plan"),
            ("POST", "/specs/{id}/tasks"),
            ("GET", "/specs/{id}/constitution/decisions"),
            ("POST", "/specs/{id}/constitution/decisions"),
            ("POST", "/specs/{id}/checklist"),
            ("POST", "/specs/{id}/analyze"),
            ("POST", "/specs/{id}/implement"),
            ("GET", "/specs/{id}/trace"),
        ])
    );
}

fn spec_manifest_endpoints() -> Vec<&'static crate::api_contract::ApiEndpoint> {
    crate::api_contract::DAEMON_API_ENDPOINTS
        .iter()
        .filter(|endpoint| endpoint.path.starts_with("/specs"))
        .collect()
}

fn schema_ref_name(schema: &Value) -> Option<&str> {
    schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|value| value.strip_prefix("#/components/schemas/"))
}

fn collect_schema_refs(schema: &Value, refs: &mut BTreeSet<String>) {
    match schema {
        Value::Object(map) => {
            if let Some(schema_ref) = map.get("$ref").and_then(Value::as_str) {
                refs.insert(schema_ref.to_string());
            }
            for value in map.values() {
                collect_schema_refs(value, refs);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_schema_refs(value, refs);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn specs_route_creates_and_lists_spec() {
    let app = crate::api::build_router(DaemonState::new(""));
    let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/specs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"workspaceRoot":"D:/work","title":"Auth","description":"Add login support","functionalRequirements":[{"text":"Users can log in"}],"successCriteria":[{"text":"Login test passes"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(created["workspaceRoot"], "D:/work");
    assert_eq!(created["functionalRequirements"][0]["id"], "FR-001");
}

#[tokio::test]
async fn specs_route_rejects_placeholder_heavy_specs() {
    let app = crate::api::build_router(DaemonState::new(""));
    let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/specs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"workspaceRoot":"D:/work","title":"TBD","description":"TODO","functionalRequirements":[{"text":"TODO"}],"successCriteria":[{"text":"placeholder"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn spec_workflow_routes_plan_checklist_and_start_implementation() {
    let app = crate::api::build_router(DaemonState::new(""));
    let create_resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/specs")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"workspaceRoot":"D:/work","title":"Auth","description":"Add login support"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let body = create_resp.into_body().collect().await.unwrap().to_bytes();
    let created: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let spec_id = created["id"].as_str().unwrap();

    let clarify_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/specs/{spec_id}/clarify"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(clarify_resp.status(), StatusCode::OK);
    let body = clarify_resp.into_body().collect().await.unwrap().to_bytes();
    let clarify: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(clarify["questions"].as_array().unwrap().len(), 0);

    let blocked_impl = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/specs/{spec_id}/implement"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(blocked_impl.status(), StatusCode::CONFLICT);

    let plan_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/specs/{spec_id}/plan"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"includeVerification":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(plan_resp.status(), StatusCode::OK);
    let body = plan_resp.into_body().collect().await.unwrap().to_bytes();
    let plan: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(plan["createdTaskCount"], 2);
    assert_eq!(plan["analysis"]["status"], "pass");

    let checklist_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/specs/{spec_id}/checklist"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(checklist_resp.status(), StatusCode::OK);
    let body = checklist_resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let checklist: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(checklist["ready"], true);

    let implement_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/specs/{spec_id}/implement"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(implement_resp.status(), StatusCode::OK);
    let body = implement_resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let implement: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(implement["specId"], spec_id);
    assert!(implement["sessionId"]
        .as_str()
        .unwrap()
        .starts_with("impl-"));

    let sessions_resp = app
        .oneshot(
            Request::builder()
                .uri("/sessions")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = sessions_resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let sessions: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
    assert!(sessions.iter().any(|session| session["specId"] == spec_id));
}
