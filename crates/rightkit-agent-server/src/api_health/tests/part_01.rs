use super::*;
use axum::{body::Body, http::Request};
use http_body_util::BodyExt;
use std::sync::Arc;
use tower::ServiceExt;

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::test_support::process_env_lock()
}

fn seeded_state() -> DaemonState {
    let mut store = rightkit_agent_engine_storage::Storage::open_in_memory().unwrap();
    store
        .create_session(&rightkit_agent_engine_storage::SessionRow {
            id: "support-session".to_string(),
            title: "Support".to_string(),
            workspace_root: "D:\\Claude\\secret-project".to_string(),
            created_at: "2026-06-08T00:00:00Z".to_string(),
            updated_at: "2026-06-08T00:00:00Z".to_string(),
            parent_session_id: None,
            role: None,
            status: "dormant".to_string(),
            config_json: serde_json::json!({
                "apiKey": "sk-test-secret",
                "model": "gpt-5"
            })
            .to_string(),
            tags: String::new(),
            recovery_marker: None,
        })
        .unwrap();
    store
        .append_event(
            "support-session",
            "userMessageSubmitted",
            &serde_json::json!({
                "$kind": "userMessageSubmitted",
                "sessionId": "support-session",
                "seq": 1,
                "createdAt": "2026-06-08T00:00:01Z",
                "text": "private user request from person@example.com",
                "nonce": null
            })
            .to_string(),
        )
        .unwrap();
    let storage = Arc::new(tokio::sync::Mutex::new(store));
    DaemonState::new("").with_storage(storage)
}

fn test_workspace(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir()
        .join("coderight-test-tmp")
        .join(format!("sandbox-settings-{label}-{nanos}"));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn encode_query_value(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            other => format!("%{other:02X}").chars().collect(),
        })
        .collect()
}

#[tokio::test]
async fn sandbox_settings_persist_per_workspace() {
    let _guard = env_lock();
    let previous = std::env::var_os("RIGHTKIT_SANDBOX_TIER");
    std::env::remove_var("RIGHTKIT_SANDBOX_TIER");
    let root = test_workspace("persist");
    let root_query = encode_query_value(root.to_str().unwrap());
    let app = crate::api::build_router(DaemonState::new(""));

    let save = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/sandbox/settings?workspaceRoot={root_query}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"tier":"landlock"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(save.status(), StatusCode::OK);
    let save_body = save.into_body().collect().await.unwrap().to_bytes();
    let saved: serde_json::Value = serde_json::from_slice(&save_body).unwrap();
    assert_eq!(saved["tier"], "landlock-seccomp");
    assert_eq!(saved["workspaceTier"], "landlock");
    assert_eq!(saved["source"], "workspace");
    assert!(root
        .join(".coderight")
        .join("sandbox-settings.json")
        .exists());

    let fresh_app = crate::api::build_router(DaemonState::new(""));
    let read = fresh_app
        .oneshot(
            Request::builder()
                .uri(format!("/sandbox/settings?workspaceRoot={root_query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK);
    let read_body = read.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&read_body).unwrap();
    assert_eq!(json["tier"], "landlock-seccomp");
    assert_eq!(json["workspaceTier"], "landlock");
    assert_eq!(json["source"], "workspace");
    assert!(json["allowedTiers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tier| tier == "none"));

    match previous {
        Some(value) => std::env::set_var("RIGHTKIT_SANDBOX_TIER", value),
        None => std::env::remove_var("RIGHTKIT_SANDBOX_TIER"),
    }
}

#[tokio::test]
async fn sandbox_settings_reject_invalid_tier() {
    let root = test_workspace("invalid");
    let root_query = encode_query_value(root.to_str().unwrap());
    let app = crate::api::build_router(DaemonState::new(""));

    let save = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/sandbox/settings?workspaceRoot={root_query}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"tier":"rootless-dream"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(save.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn sandbox_operator_floor_cannot_be_weakened_by_workspace_api() {
    let _guard = env_lock();
    let previous = std::env::var_os("RIGHTKIT_SANDBOX_TIER");
    std::env::set_var("RIGHTKIT_SANDBOX_TIER", "strict");
    let root = test_workspace("operator-floor");
    let root_query = encode_query_value(root.to_str().unwrap());
    let app = crate::api::build_router(DaemonState::new(""));

    let save = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/sandbox/settings?workspaceRoot={root_query}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"tier":"none"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(save.status(), StatusCode::OK);
    let body = save.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["workspaceTier"], "none");
    assert_eq!(json["operatorTier"], "landlock-seccomp");
    assert_eq!(json["tier"], "landlock-seccomp");
    assert_eq!(json["source"], "environment+workspace");

    match previous {
        Some(value) => std::env::set_var("RIGHTKIT_SANDBOX_TIER", value),
        None => std::env::remove_var("RIGHTKIT_SANDBOX_TIER"),
    }
}

#[tokio::test]
async fn malformed_workspace_sandbox_tier_fails_closed_in_api() {
    let _guard = env_lock();
    let previous = std::env::var_os("RIGHTKIT_SANDBOX_TIER");
    std::env::remove_var("RIGHTKIT_SANDBOX_TIER");
    let root = test_workspace("invalid-file-fail-closed");
    let settings_dir = root.join(".coderight");
    std::fs::create_dir_all(&settings_dir).unwrap();
    std::fs::write(
        settings_dir.join("sandbox-settings.json"),
        r#"{"tier":"weaker-than-none","updatedAt":"2026-07-17T00:00:00Z"}"#,
    )
    .unwrap();
    let root_query = encode_query_value(root.to_str().unwrap());
    let app = crate::api::build_router(DaemonState::new(""));

    let read = app
        .oneshot(
            Request::builder()
                .uri(format!("/sandbox/settings?workspaceRoot={root_query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK);
    let body = read.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["tier"], "landlock-seccomp");
    assert_eq!(json["workspaceValid"], false);

    match previous {
        Some(value) => std::env::set_var("RIGHTKIT_SANDBOX_TIER", value),
        None => std::env::remove_var("RIGHTKIT_SANDBOX_TIER"),
    }
}

#[tokio::test]
async fn sandbox_and_diagnostics_mutations_reject_lower_trust_sources() {
    let root = test_workspace("trust-gate");
    let root_query = encode_query_value(root.to_str().unwrap());
    let app = crate::api::build_router(seeded_state());

    let sandbox = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/sandbox/settings?workspaceRoot={root_query}"))
                .header("content-type", "application/json")
                .header("x-coderight-source-trust", "remoteAgentUntrusted")
                .body(Body::from(r#"{"tier":"landlock"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(sandbox.status(), StatusCode::FORBIDDEN);
    assert!(!root
        .join(".coderight")
        .join("sandbox-settings.json")
        .exists());

    let diagnostics = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/diagnostics/bundle")
                .header("content-type", "application/json")
                .header("x-coderight-source-trust", "toolUntrusted")
                .body(Body::from(
                    serde_json::json!({
                        "sessionId": "support-session",
                        "includeTranscript": false
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(diagnostics.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn diagnostics_report_workspace_sandbox_tier() {
    let _guard = env_lock();
    let previous = std::env::var_os("RIGHTKIT_SANDBOX_TIER");
    std::env::remove_var("RIGHTKIT_SANDBOX_TIER");
    let root = test_workspace("diagnostics");
    let root_query = encode_query_value(root.to_str().unwrap());
    let app = crate::api::build_router(DaemonState::new(""));

    let save = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/sandbox/settings?workspaceRoot={root_query}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"tier":"none"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(save.status(), StatusCode::OK);

    let diagnostics = app
        .oneshot(
            Request::builder()
                .uri(format!("/health/diagnostics?workspaceRoot={root_query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(diagnostics.status(), StatusCode::OK);
    let body = diagnostics.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let check = json["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "core_sandbox")
        .unwrap();
    assert!(check["detail"]
        .as_str()
        .unwrap()
        .contains(".coderight/sandbox-settings.json=landlock-seccomp"));

    match previous {
        Some(value) => std::env::set_var("RIGHTKIT_SANDBOX_TIER", value),
        None => std::env::remove_var("RIGHTKIT_SANDBOX_TIER"),
    }
}

#[test]
fn health_openapi_components_have_expected_shape() {
    let schemas = health_openapi_components();
    let health = schemas.get("HealthResponse").unwrap();
    assert_eq!(health["type"], "object");
    let required = health["required"].as_array().unwrap();
    assert_eq!(required.len(), 7);
    assert!(required.contains(&serde_json::json!("service")));
    assert!(required.contains(&serde_json::json!("storageReadPoolIdle")));
    assert!(required.contains(&serde_json::json!("providerKeyStorage")));
    assert_eq!(
        health["properties"]["providerKeyStorage"]["$ref"],
        "#/components/schemas/ProviderKeyStorageStatus"
    );
    assert!(schemas.contains_key("ProviderKeyStorageStatus"));

    let diagnostics = schemas.get("HealthDiagnosticsResponse").unwrap();
    assert_eq!(diagnostics["type"], "object");
    assert_eq!(diagnostics["properties"]["checks"]["type"], "array");
    assert_eq!(
        health_response_schema()["$ref"],
        "#/components/schemas/HealthResponse"
    );
    assert_eq!(
        health_diagnostics_response_schema()["$ref"],
        "#/components/schemas/HealthDiagnosticsResponse"
    );
}

#[test]
fn health_openapi_components_cover_sandbox_and_diagnostics_bundle_routes() {
    let schemas = health_openapi_components();
    for key in [
        "SandboxTier",
        "SandboxSettingsSource",
        "SaveSandboxSettingsRequest",
        "SandboxSettingsResponse",
        "DiagnosticsBundleSummary",
        "DiagnosticsBundleDownload",
    ] {
        assert!(schemas.contains_key(key), "missing schema component {key}");
    }

    assert_eq!(
        schemas["SaveSandboxSettingsRequest"]["properties"]["tier"]["$ref"],
        "#/components/schemas/SandboxTier"
    );
    assert_eq!(
        schemas["SandboxSettingsResponse"]["properties"]["source"]["$ref"],
        "#/components/schemas/SandboxSettingsSource"
    );
    assert_eq!(
        schemas["SandboxTier"]["enum"],
        serde_json::json!(allowed_sandbox_tiers())
    );
}

#[test]
fn health_openapi_helpers_cover_sandbox_and_diagnostics_bundle_refs() {
    let schemas = health_openapi_components();
    let helper_refs = [
        save_sandbox_settings_request_schema(),
        sandbox_settings_response_schema(),
        diagnostics_bundle_response_schema(),
        diagnostics_bundle_download_response_schema(),
    ];

    for schema in helper_refs {
        let reference = schema["$ref"].as_str().unwrap();
        let key = reference
            .strip_prefix("#/components/schemas/")
            .expect("helper must return a component schema ref");
        assert!(schemas.contains_key(key), "missing helper ref target {key}");
    }

    assert_eq!(
        save_sandbox_settings_request_schema()["$ref"],
        "#/components/schemas/SaveSandboxSettingsRequest"
    );
    assert_eq!(
        sandbox_settings_response_schema()["$ref"],
        "#/components/schemas/SandboxSettingsResponse"
    );
    assert_eq!(
        diagnostics_bundle_response_schema()["$ref"],
        "#/components/schemas/DiagnosticsBundleSummary"
    );
    assert_eq!(
        diagnostics_bundle_download_response_schema()["$ref"],
        "#/components/schemas/DiagnosticsBundleDownload"
    );
}
