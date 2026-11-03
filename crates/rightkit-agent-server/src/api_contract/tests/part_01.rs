use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

#[tokio::test]
async fn api_contract_route_lists_stable_daemon_endpoints() {
    let app = crate::api::build_router(DaemonState::new(""));
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/api/contract")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["name"], "coderight-daemon");
    let endpoints = json["endpoints"].as_array().unwrap();
    assert!(endpoints
        .iter()
        .any(|endpoint| { endpoint["method"] == "GET" && endpoint["path"] == "/tool-schemas" }));
    assert!(endpoints.iter().any(|endpoint| {
        endpoint["method"] == "GET" && endpoint["path"] == "/tool-invocations/{id}"
    }));
    assert!(endpoints
        .iter()
        .any(|endpoint| { endpoint["method"] == "GET" && endpoint["path"] == "/hooks/status" }));
}
