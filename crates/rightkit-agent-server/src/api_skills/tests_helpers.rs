use super::*;
use axum::body::Body;
use axum::http::{Method, Request};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn ref_target_name(schema: &Value) -> &str {
    schema["$ref"]
        .as_str()
        .and_then(|reference| reference.strip_prefix("#/components/schemas/"))
        .expect("component schema ref")
}

fn temp_skill_dir(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "coderight-skills-trust-gate-{label}-{}",
        crate::api::now_millis()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

async fn lower_trust_skill_request_status(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: serde_json::Value,
) -> axum::http::StatusCode {
    app.oneshot(
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .header("x-coderight-source-trust", "untrusted")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

async fn trusted_skill_request(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: serde_json::Value,
) -> (axum::http::StatusCode, serde_json::Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header("x-coderight-source-trust", "trusted")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, body)
}
