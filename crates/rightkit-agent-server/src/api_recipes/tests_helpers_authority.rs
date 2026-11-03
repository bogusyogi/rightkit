use super::*;
use axum::body::Body;
use axum::http::{Method, Request};
use http_body_util::BodyExt;
use tower::ServiceExt;

async fn json_post(
    app: Router,
    uri: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn json_get(app: Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

async fn lower_trust_recipe_request_status(
    app: Router,
    method: Method,
    uri: &str,
    body: serde_json::Value,
) -> StatusCode {
    app.oneshot(
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .header("x-coderight-source-trust", "remoteAgentUntrusted")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

fn sample_recipe_json() -> serde_json::Value {
    serde_json::json!({
        "id": "recipe-test",
        "version": "1.0.0",
        "title": "Test recipe",
        "description": "Render and run a test recipe.",
        "instructions": "Follow the workspace rules.",
        "prompt": "Implement {{feature}}.",
        "parameters": [{
            "key": "feature",
            "inputType": "string",
            "requirement": "required",
            "description": "Feature to implement"
        }],
        "requiredToolsets": ["dev-safe"],
        "settings": { "toolset": "dev-safe" }
    })
}

#[test]
fn a_damaged_recipe_store_is_set_aside_and_loads_empty() {
    let path = test_store_path("damaged");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, br#"{"recipes": [{"id": "r1""#).unwrap();
    let _store = load_recipe_store(&path);
    assert!(
        !path.exists(),
        "the partial file is out of the way, so a save cannot replace the only copy"
    );
    let kept: Vec<String> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        kept.iter().any(|n| n.starts_with("recipes.json.damaged-")),
        "{kept:?}"
    );
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

fn test_store_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir()
        .join(format!("coderight-recipes-{name}-{}", now_millis()))
        .join("recipes.json")
}

fn test_workspace_root(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "coderight-recipe-workspace-{name}-{}",
        now_millis()
    ))
}
