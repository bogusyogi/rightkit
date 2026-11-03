// Test-only: env guard (`env_lock`) intentionally held across `.await` to
// serialize tests that mutate process env. Not a production deadlock path.
use super::*;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use std::time::{SystemTime, UNIX_EPOCH};
use tower::ServiceExt;

fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::test_support::process_env_lock()
}

fn temp_path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("coderight-{label}-{nanos}"))
}

#[test]
fn minimax_is_a_supported_persisted_provider() {
    assert_eq!(provider_env_var("minimax"), Some("MINIMAX_API_KEY"));
    assert!(validate_provider("minimax").is_ok());
}

#[test]
fn tinyfish_is_a_supported_persisted_provider() {
    assert_eq!(provider_env_var("tinyfish"), Some("TINYFISH_API_KEY"));
    assert!(validate_provider("tinyfish").is_ok());
}

#[test]
fn inference_catalog_cache_keys_revisions_and_uses_bounded_ttls() {
    assert_ne!(
        direct_catalog_cache_key("local", 1, 2),
        direct_catalog_cache_key("local", 2, 2)
    );
    assert_ne!(
        direct_catalog_cache_key("local", 1, 2),
        direct_catalog_cache_key("local", 1, 3)
    );
    assert_eq!(catalog_cache_ttl(true), std::time::Duration::from_secs(30));
    assert_eq!(catalog_cache_ttl(false), std::time::Duration::from_secs(2));
}

fn query_encode(raw: &str) -> String {
    raw.bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                vec![byte as char]
            }
            b' ' => vec!['%', '2', '0'],
            other => format!("%{other:02X}").chars().collect(),
        })
        .collect()
}

#[test]
fn settings_openapi_components_cover_orchestration_and_config_routes() {
    let components = settings_openapi_components();
    for name in [
        "OrchestrationSettingsResponse",
        "SaveSettings",
        "SaveSettingsResponse",
        "ConfigStatus",
        "ConfigFileStatus",
        "ReloadSettingsResponse",
        "ProviderKeyStatus",
        "SaveProviderKeyRequest",
        "ProviderKeyTestResponse",
        "SettingsRouting",
    ] {
        assert!(components.contains_key(name), "{name} schema missing");
    }
    assert_eq!(
        components["OrchestrationSettingsResponse"]["properties"]["routing"]["$ref"],
        "#/components/schemas/SettingsRouting"
    );
    assert_eq!(
        components["OrchestrationSettingsResponse"]["properties"]["roleModelSplitAvailable"]
            ["type"],
        "boolean"
    );
    assert_eq!(
        components["SaveSettings"]["properties"]["routing"]["$ref"],
        "#/components/schemas/SettingsRouting"
    );

    assert_eq!(
        orchestration_settings_response_schema()["$ref"],
        "#/components/schemas/OrchestrationSettingsResponse"
    );
    assert_eq!(
        save_settings_request_schema()["$ref"],
        "#/components/schemas/SaveSettings"
    );
    assert_eq!(
        config_status_response_schema()["$ref"],
        "#/components/schemas/ConfigStatus"
    );
    assert_eq!(
        reload_settings_request_schema()["$ref"],
        "#/components/schemas/ReloadSettingsRequest"
    );
    assert_eq!(
        reload_settings_response_schema()["$ref"],
        "#/components/schemas/ReloadSettingsResponse"
    );
    assert_eq!(
        save_provider_key_request_schema()["$ref"],
        "#/components/schemas/SaveProviderKeyRequest"
    );
    assert_eq!(
        provider_key_status_response_schema()["$ref"],
        "#/components/schemas/ProviderKeyStatus"
    );
    assert_eq!(
        components["OrchestrationSettingsResponse"]["properties"]["models"]["items"]["$ref"],
        "#/components/schemas/SettingsModelCatalogEntry"
    );
    assert_eq!(
        components["SaveSettings"]["properties"]["remote"]["$ref"],
        "#/components/schemas/SaveRemoteSettings"
    );
    assert_eq!(
        components["ReloadSettingsResponse"]["properties"]["settings"]["$ref"],
        "#/components/schemas/OrchestrationSettingsResponse"
    );
    assert_eq!(
        components["ProviderKeyStatus"]["properties"]["source"]["enum"][0],
        "env"
    );
}

#[tokio::test]
async fn settings_report_single_model_policy_when_role_split_is_not_entitled() {
    let app = crate::api::build_router(DaemonState::new(""));
    let response = app
        .oneshot(
            Request::builder()
                .uri("/settings/orchestration")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["roleModelMode"], "single");
    assert_eq!(json["roleModelSplitAvailable"], false);
}

#[tokio::test]
async fn config_status_reports_workspace_config_fingerprint() {
    let _guard = env_lock();
    let prior_home = std::env::var_os("RIGHTKIT_SUITE_ROOT");
    let home = temp_path("home");
    let workspace = temp_path("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    unsafe {
        rightkit_app_settings::agent_config::test_isolation::set_process_suite_root(&home);
    }
    let config_dir = workspace.join(".coderight");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("config.toml"),
        "[orchestration]\n[orchestration.budget]\nmax-iterations = 123\n",
    )
    .unwrap();

    let app = crate::api::build_router(DaemonState::new(""));
    let uri = format!(
        "/settings/config-status?workspaceRoot={}",
        query_encode(&workspace.display().to_string())
    );
    let resp = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();

    unsafe {
        match prior_home {
            Some(value) => std::env::set_var("RIGHTKIT_SUITE_ROOT", value),
            None => std::env::remove_var("RIGHTKIT_SUITE_ROOT"),
        }
    }

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["workspaceRoot"], workspace.display().to_string());
    assert!(json["fingerprint"].as_str().unwrap().len() >= 16);
    let files = json["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[1]["scope"], "workspace");
    assert_eq!(files[1]["exists"], true);
    assert!(
        files[1]["path"]
            .as_str()
            .unwrap()
            .ends_with(".coderight\\config.toml")
            || files[1]["path"]
                .as_str()
                .unwrap()
                .ends_with(".coderight/config.toml")
    );
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn settings_reload_reapplies_workspace_budget_to_running_store() {
    let _guard = env_lock();
    let prior_home = std::env::var_os("RIGHTKIT_SUITE_ROOT");
    let home = temp_path("home");
    let workspace = temp_path("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    unsafe {
        rightkit_app_settings::agent_config::test_isolation::set_process_suite_root(&home);
    }
    let config_dir = workspace.join(".coderight");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
            config_dir.join("config.toml"),
            "[orchestration]\nboss-model = \"planner\"\n[orchestration.budget]\nmax-iterations = 77\nmax-workers = 9\n",
        )
        .unwrap();

    let state = DaemonState::new("");
    let app = crate::api::build_router(state.clone());
    let uri = format!(
        "/settings/reload?workspaceRoot={}",
        query_encode(&workspace.display().to_string())
    );
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    unsafe {
        match prior_home {
            Some(value) => std::env::set_var("RIGHTKIT_SUITE_ROOT", value),
            None => std::env::remove_var("RIGHTKIT_SUITE_ROOT"),
        }
    }

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["reloaded"], true);
    assert_eq!(json["settings"]["budget"]["maxIterations"], 77);
    assert_eq!(json["settings"]["budget"]["maxWorkers"], 9);
    assert_eq!(json["settings"]["primaryOrchestratorModelId"], "planner");
    let store = state.settings.lock().unwrap();
    assert_eq!(store.max_iterations, 77);
    assert_eq!(store.max_workers, 9);
    assert_eq!(store.primary_orchestrator_model_id, "planner");
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn settings_put_persists_and_reflects_per_agent_model_override() {
    let _guard = env_lock();
    let prior_home = std::env::var_os("RIGHTKIT_SUITE_ROOT");
    let home = temp_path("home-model-override");
    let workspace = temp_path("workspace-model-override");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    unsafe {
        rightkit_app_settings::agent_config::test_isolation::set_process_suite_root(&home);
    }
    let workspace_query = query_encode(&workspace.display().to_string());
    let state = DaemonState::new("");
    let app = crate::api::build_router(state.clone());

    // PUT a per-agent override (trusted local source â€” no lower-trust header).
    let put = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings/orchestration")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "modelOverrides": { "coder": "test/override-model" } })
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);

    // GET reflects the effective model for the "coder" agent (override wins).
    let get = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/settings/orchestration?workspaceRoot={workspace_query}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    let body = get.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let coder = json["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "coder")
        .expect("coder model entry present");
    assert_eq!(coder["modelSlug"], "test/override-model");

    unsafe {
        match prior_home {
            Some(value) => std::env::set_var("RIGHTKIT_SUITE_ROOT", value),
            None => std::env::remove_var("RIGHTKIT_SUITE_ROOT"),
        }
    }
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn settings_put_persists_and_reflects_routing_toggle() {
    // R1a-L4: the Settings > Models "Capability router" toggle round-trips
    // through PUT /settings/orchestration -> GET, same pattern as the
    // per-agent model override test above.
    let _guard = env_lock();
    let prior_home = std::env::var_os("RIGHTKIT_SUITE_ROOT");
    let home = temp_path("home-routing-toggle");
    let workspace = temp_path("workspace-routing-toggle");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    unsafe {
        rightkit_app_settings::agent_config::test_isolation::set_process_suite_root(&home);
    }
    let workspace_query = query_encode(&workspace.display().to_string());
    let state = DaemonState::new("");
    let app = crate::api::build_router(state.clone());

    let put = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/settings/orchestration")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "routing": {
                            "capabilityRouter": true,
                            "aggressiveness": "aggressive"
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);

    let get = app
        .oneshot(
            Request::builder()
                .uri(format!(
                    "/settings/orchestration?workspaceRoot={workspace_query}"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get.status(), StatusCode::OK);
    let body = get.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["routing"]["capabilityRouter"], true);
    assert_eq!(json["routing"]["aggressiveness"], "aggressive");

    unsafe {
        match prior_home {
            Some(value) => std::env::set_var("RIGHTKIT_SUITE_ROOT", value),
            None => std::env::remove_var("RIGHTKIT_SUITE_ROOT"),
        }
    }
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&workspace);
}

#[tokio::test]
async fn settings_mutations_reject_lower_trust_sources() {
    let _guard = env_lock();
    let prior_home = std::env::var_os("RIGHTKIT_SUITE_ROOT");
    let home = temp_path("home-untrusted-settings");
    let workspace = temp_path("workspace-untrusted-settings");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    unsafe {
        rightkit_app_settings::agent_config::test_isolation::set_process_suite_root(&home);
    }
    let workspace_query = query_encode(&workspace.display().to_string());
    let state = DaemonState::new("");
    let app = crate::api::build_router(state.clone());
    let cases = [
        (
            "PUT",
            "/settings/orchestration".to_string(),
            Some(serde_json::json!({
                "primaryOrchestratorModelId": "lower-trust-model",
                "remote": {"webEnabled": true}
            })),
        ),
        (
            "POST",
            format!("/settings/reload?workspaceRoot={workspace_query}"),
            None,
        ),
        (
            "PUT",
            format!("/settings/toolsets/lower-trust?workspaceRoot={workspace_query}"),
            Some(serde_json::json!({
                "name": "Lower trust",
                "allowedTools": ["shell"],
                "disallowedTools": []
            })),
        ),
        (
            "DELETE",
            format!("/settings/toolsets/lower-trust?workspaceRoot={workspace_query}"),
            None,
        ),
        (
            "PUT",
            "/provider-keys/openrouter".to_string(),
            Some(serde_json::json!({"apiKey": "sk-lower-trust"})),
        ),
    ];

    for (method, uri, body) in cases {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("x-coderight-source-trust", "remoteAgentUntrusted");
        let body = if let Some(body) = body {
            builder = builder.header("content-type", "application/json");
            Body::from(body.to_string())
        } else {
            Body::empty()
        };
        let resp = app
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{method}");
    }

    assert_ne!(
        state.settings.lock().unwrap().primary_orchestrator_model_id,
        "lower-trust-model"
    );
    assert!(!workspace.join(".coderight").join("config.toml").exists());

    unsafe {
        match prior_home {
            Some(value) => std::env::set_var("RIGHTKIT_SUITE_ROOT", value),
            None => std::env::remove_var("RIGHTKIT_SUITE_ROOT"),
        }
    }
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&workspace);
}

/// Saving a provider key writes to the OS keychain and fails closed when
/// the keychain cannot verify the write. It never creates a plaintext file under HOME.
#[tokio::test]
async fn save_provider_key_writes_keychain_and_fails_closed() {
    let _guard = env_lock();
    let prior_home = std::env::var_os("HOME");
    let prior_userprofile = std::env::var_os("USERPROFILE");
    let prior_coderight_home = std::env::var_os("RIGHTKIT_SUITE_ROOT");
    // Sibling home and suite root: on macOS the real data folder derives from HOME.
    let root = temp_path("home-provider-key-keychain");
    let (home, suite) = (root.join("home"), root.join("suite"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&suite).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        rightkit_app_settings::agent_config::test_isolation::set_process_suite_root(&suite);
    }

    // Clean slate: no stray key from a prior aborted run.
    let _ = rightkit_llm::agent_providers::KeyStore::delete_api_key("openrouter");

    let state = DaemonState::new("");
    let app = crate::api::build_router(state.clone());
    let put = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/provider-keys/openrouter")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "apiKey": "sk-keychain-test-value" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let put_status = put.status();
    let put_body = put.into_body().collect().await.unwrap().to_bytes();

    if put_status == StatusCode::OK {
        let put_json: serde_json::Value = serde_json::from_slice(&put_body).unwrap();
        assert_eq!(put_json["source"], "keychain");
        // Keychain accepted the write: a second read (status GET) must still report keychain.
        let get = app
            .oneshot(
                Request::builder()
                    .uri("/provider-keys/openrouter")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
        let get_body = get.into_body().collect().await.unwrap().to_bytes();
        let get_json: serde_json::Value = serde_json::from_slice(&get_body).unwrap();
        assert_eq!(get_json["source"], "keychain");
        assert_eq!(get_json["configured"], true);
        let _ = rightkit_llm::agent_providers::KeyStore::delete_api_key("openrouter");
    } else {
        assert_eq!(put_status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    unsafe {
        match prior_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match prior_userprofile {
            Some(value) => std::env::set_var("USERPROFILE", value),
            None => std::env::remove_var("USERPROFILE"),
        }
        match prior_coderight_home {
            Some(value) => std::env::set_var("RIGHTKIT_SUITE_ROOT", value),
            None => std::env::remove_var("RIGHTKIT_SUITE_ROOT"),
        }
    }
    let _ = std::fs::remove_dir_all(&root);
}

// ── S:H13: provider credential resolution & probe classification ─────

/// Isolated home + controlled OPENROUTER_API_KEY for resolver tests.
/// Returns `(home_path, prior_key_env)`; callers restore the env and
/// remove the directory at the end, matching this file's cleanup pattern.
fn resolver_env() -> (std::path::PathBuf, Option<std::ffi::OsString>) {
    // Home and suite root are siblings: on macOS the real data folder derives from HOME,
    // so a suite root equal to HOME would overlap it and trip the isolation guard.
    let root = temp_path("home-provider-resolution");
    let home = root.join("home");
    let suite = root.join("suite");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&suite).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("USERPROFILE", &home);
        rightkit_app_settings::agent_config::test_isolation::set_process_suite_root(&suite);
    }
    let prior_key = std::env::var_os("OPENROUTER_API_KEY");
    (root, prior_key)
}

fn restore_key_env(prior_key: Option<std::ffi::OsString>) {
    unsafe {
        match prior_key {
            Some(value) => std::env::set_var("OPENROUTER_API_KEY", value),
            None => std::env::remove_var("OPENROUTER_API_KEY"),
        }
    }
}

#[test]
fn save_provider_key_accepts_snake_case_alias_for_cli_payload() {
    // The CLI historically posted {"api_key": ...}; without the serde
    // alias that payload died on a bodyless 422 before reaching the
    // handler. Both spellings must deserialize.
    let snake: Result<SaveProviderKey, _> = serde_json::from_value(json!({ "api_key": "sk-x" }));
    let camel: Result<SaveProviderKey, _> = serde_json::from_value(json!({ "apiKey": "sk-x" }));
    assert!(
        snake.is_ok(),
        "CLI snake_case api_key payload must deserialize"
    );
    assert!(
        camel.is_ok(),
        "desktop camelCase apiKey payload must deserialize"
    );
}

#[test]
fn provider_credential_resolution_prefers_stored_key_over_env() {
    let _guard = env_lock();
    let (home, prior_key) = resolver_env();
    unsafe { std::env::set_var("OPENROUTER_API_KEY", "sk-env-value") };

    let backend = rightkit_llm::agent_providers::InMemoryKeyStore::new();
    backend
        .set("coderight", "openrouter", "sk-stored-value")
        .unwrap();
    let resolved = resolve_provider_credential_with(&backend, "openrouter").unwrap();
    assert_eq!(
        resolved,
        ResolvedProviderCredential::Present {
            secret: "sk-stored-value".to_string(),
            source: "keychain",
        },
        "a stored key must win over the environment, matching the loader"
    );

    restore_key_env(prior_key);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn provider_credential_resolution_falls_back_to_env_without_stored_key() {
    let _guard = env_lock();
    let (home, prior_key) = resolver_env();
    unsafe { std::env::set_var("OPENROUTER_API_KEY", "sk-env-value") };

    let backend = rightkit_llm::agent_providers::InMemoryKeyStore::new();
    let resolved = resolve_provider_credential_with(&backend, "openrouter").unwrap();
    assert_eq!(
        resolved,
        ResolvedProviderCredential::Present {
            secret: "sk-env-value".to_string(),
            source: "env",
        },
        "env must serve only when nothing is stored"
    );

    restore_key_env(prior_key);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn provider_credential_resolution_reports_unreadable_keychain_item() {
    // The macOS ACL story: the item exists but the OS refuses this build.
    // Distinct from missing, with actionable remedy attached.
    let _guard = env_lock();
    let (home, prior_key) = resolver_env();
    unsafe { std::env::remove_var("OPENROUTER_API_KEY") };

    let backend = rightkit_llm::agent_providers::InMemoryKeyStore::unreadable("acl refused");
    let resolved = resolve_provider_credential_with(&backend, "openrouter").unwrap();
    match resolved {
        ResolvedProviderCredential::Unreadable { remedy } => {
            assert!(
                remedy.is_some(),
                "unreadable must carry the remedy guidance"
            );
        }
        other => panic!("expected unreadable, got {other:?}"),
    }

    restore_key_env(prior_key);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn provider_credential_resolution_is_missing_without_any_source() {
    let _guard = env_lock();
    let (home, prior_key) = resolver_env();
    unsafe { std::env::remove_var("OPENROUTER_API_KEY") };

    let backend = rightkit_llm::agent_providers::InMemoryKeyStore::new();
    let resolved = resolve_provider_credential_with(&backend, "openrouter").unwrap();
    assert_eq!(resolved, ResolvedProviderCredential::Missing);

    restore_key_env(prior_key);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn provider_probe_classification_never_marks_missing_route_as_rejection() {
    // 200-range means the credential was accepted...
    assert_eq!(
        classify_provider_probe_status(200),
        ProviderCredentialProbe::Accepted
    );
    // ...401/403 mean it was explicitly rejected...
    assert_eq!(
        classify_provider_probe_status(401),
        ProviderCredentialProbe::Rejected
    );
    assert_eq!(
        classify_provider_probe_status(403),
        ProviderCredentialProbe::Rejected
    );
    // ...but a missing probe route must NEVER read as a bad key.
    assert_eq!(
        classify_provider_probe_status(404),
        ProviderCredentialProbe::ProbeUnavailable
    );
    assert_eq!(
        classify_provider_probe_status(405),
        ProviderCredentialProbe::ProbeUnavailable
    );
    assert_eq!(
        classify_provider_probe_status(500),
        ProviderCredentialProbe::ProviderError(500)
    );
}

#[test]
fn provider_probe_outcome_wire_marks_only_accepted_as_valid() {
    let (state, valid, _detail, status) = probe_outcome_wire(&ProviderCredentialProbe::Accepted);
    assert_eq!((state, valid, status), ("accepted", true, None));

    for outcome in [
        ProviderCredentialProbe::Rejected,
        ProviderCredentialProbe::ProbeUnavailable,
        ProviderCredentialProbe::Unsupported,
        ProviderCredentialProbe::ProviderError(503),
        ProviderCredentialProbe::Unreachable("down".to_string()),
    ] {
        let (state, valid, _detail, _status) = probe_outcome_wire(&outcome);
        assert!(!valid, "{state} must not claim a valid credential");
    }
}

#[tokio::test]
async fn provider_test_payload_reports_missing_without_a_probe() {
    // Full payload path against an injectable backend: no credential
    // anywhere must yield the explicit missing state — not an error and
    // not a network call.
    let _guard = env_lock();
    let (home, prior_key) = resolver_env();
    unsafe { std::env::remove_var("OPENROUTER_API_KEY") };

    let backend = rightkit_llm::agent_providers::InMemoryKeyStore::new();
    let payload = test_provider_key_payload_with(&backend, "openrouter")
        .await
        .expect("missing credential must produce a payload, not an error");
    assert_eq!(payload["state"], "missing");
    assert_eq!(payload["valid"], false);
    assert_eq!(payload["provider"], "openrouter");

    restore_key_env(prior_key);
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn provider_test_payload_reports_unreadable_with_remedy() {
    // The unreadable branch must reach the payload unchanged (also no
    // network): the user gets remedy guidance instead of a probe verdict.
    let _guard = env_lock();
    let (home, prior_key) = resolver_env();
    unsafe { std::env::remove_var("OPENROUTER_API_KEY") };

    let backend = rightkit_llm::agent_providers::InMemoryKeyStore::unreadable("acl refused");
    let payload = test_provider_key_payload_with(&backend, "openrouter")
        .await
        .expect("unreadable credential must produce a payload, not an error");
    assert_eq!(payload["state"], "unreadable");
    assert_eq!(payload["valid"], false);
    assert!(
        payload["remedy"].is_string(),
        "remedy guidance must be present"
    );

    restore_key_env(prior_key);
    let _ = std::fs::remove_dir_all(&home);
}
