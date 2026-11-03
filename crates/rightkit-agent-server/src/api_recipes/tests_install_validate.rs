#[tokio::test]
async fn recipe_authority_routes_reject_lower_trust_sources() {
    let store_path = test_store_path("trust-gate");
    if let Some(parent) = store_path.parent() {
        let _ = std::fs::remove_dir_all(parent);
        std::fs::create_dir_all(parent).unwrap();
    }
    let recipe_path = store_path.parent().unwrap().join("blocked-recipe.json");
    std::fs::write(&recipe_path, sample_recipe_json().to_string()).unwrap();
    let app =
        crate::api::build_router(DaemonState::new("").with_recipe_store_path(store_path.clone()));
    let cases = [
        (
            Method::POST,
            "/recipes/install",
            serde_json::json!({
                "recipe": sample_recipe_json(),
                "trusted": true
            }),
        ),
        (
            Method::POST,
            "/recipes/install-file",
            serde_json::json!({
                "path": recipe_path,
                "trusted": true
            }),
        ),
        (
            Method::POST,
            "/recipes/install-url",
            serde_json::json!({
                "url": "https://github.com/acme/repo/blob/main/recipe.json",
                "trusted": true
            }),
        ),
        (
            Method::POST,
            "/recipes/recipe-test/run",
            serde_json::json!({"params": {"feature": "blocked"}}),
        ),
        (
            Method::POST,
            "/recipes/recipe-test/schedule",
            serde_json::json!({
                "params": {"feature": "blocked"},
                "workspaceRoot": store_path.parent().unwrap(),
                "cronExpr": "0 9 * * 1",
                "timeZone": "UTC"
            }),
        ),
    ];

    for (method, uri, body) in cases {
        let status = lower_trust_recipe_request_status(app.clone(), method, uri, body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri}");
    }
    assert!(
        !store_path.exists(),
        "lower-trust recipe authority routes must fail before persisting recipe state"
    );
    assert!(
        !store_path
            .parent()
            .unwrap()
            .join(".coderight")
            .join("schedules.json")
            .exists(),
        "lower-trust recipe schedule must fail before writing schedule state"
    );

    if let Some(parent) = store_path.parent() {
        let _ = std::fs::remove_dir_all(parent);
    }
}

fn test_recipe_file_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir()
        .join(format!("coderight-recipe-file-{name}-{}", now_millis()))
        .join("recipe.json")
}

#[tokio::test]
async fn recipes_install_and_list_route() {
    let app = crate::api::build_router(DaemonState::new(""));
    let (status, installed) = json_post(
        app.clone(),
        "/recipes/install",
        serde_json::json!({ "recipe": sample_recipe_json(), "trusted": true }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(installed["id"], "recipe-test");

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/recipes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let recipes: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(recipes.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn recipes_install_from_local_file_route() {
    let file_path = test_recipe_file_path("install");
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(
        &file_path,
        serde_json::to_string_pretty(&sample_recipe_json()).unwrap(),
    )
    .unwrap();
    let app = crate::api::build_router(DaemonState::new(""));

    let (status, installed) = json_post(
        app,
        "/recipes/install-file",
        serde_json::json!({
            "path": file_path.display().to_string(),
            "trusted": true
        }),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(installed["source"], "local");
    assert_eq!(installed["sourceUri"], file_path.display().to_string());
    assert_eq!(installed["id"], "recipe-test");

    let _ = std::fs::remove_dir_all(file_path.parent().unwrap());
}

#[tokio::test]
async fn recipes_install_from_local_yaml_file_route() {
    let file_path = test_recipe_file_path("yaml").with_extension("yaml");
    std::fs::create_dir_all(file_path.parent().unwrap()).unwrap();
    std::fs::write(
        &file_path,
        r#"id: recipe-yaml
version: 1.0.0
title: YAML recipe
description: Install a YAML recipe.
instructions: Follow the workspace rules.
prompt: "Implement {{feature}}."
parameters:
  - key: feature
    inputType: string
    requirement: required
    description: Feature to implement
requiredToolsets:
  - dev-safe
settings:
  toolset: dev-safe
"#,
    )
    .unwrap();
    let app = crate::api::build_router(DaemonState::new(""));

    let (status, installed) = json_post(
        app,
        "/recipes/install-file",
        serde_json::json!({
            "path": file_path.display().to_string(),
            "trusted": true
        }),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(installed["source"], "local");
    assert_eq!(installed["sourceUri"], file_path.display().to_string());
    assert_eq!(installed["id"], "recipe-yaml");
    assert_eq!(installed["title"], "YAML recipe");

    let _ = std::fs::remove_dir_all(file_path.parent().unwrap());
}

#[tokio::test]
async fn recipes_install_from_local_file_rejects_relative_path() {
    let app = crate::api::build_router(DaemonState::new(""));

    let (status, body) = json_post(
        app,
        "/recipes/install-file",
        serde_json::json!({ "path": "recipe.json" }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("absolute"));
}

#[tokio::test]
async fn recipes_install_from_local_file_rejects_unknown_extension() {
    let file_path = test_recipe_file_path("toml").with_extension("toml");
    let app = crate::api::build_router(DaemonState::new(""));

    let (status, body) = json_post(
        app,
        "/recipes/install-file",
        serde_json::json!({ "path": file_path.display().to_string() }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains(".yaml"));
}

#[tokio::test]
async fn recipes_install_from_url_rejects_non_github_url() {
    let app = crate::api::build_router(DaemonState::new(""));

    let (status, body) = json_post(
        app,
        "/recipes/install-url",
        serde_json::json!({ "url": "https://example.com/recipe.yaml" }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("github.com"));
}

#[test]
fn recipe_github_blob_url_normalizes_to_raw_url() {
    let url =
        url::Url::parse("https://github.com/acme/cookbook/blob/main/recipes/review.yaml").unwrap();

    let raw = normalized_recipe_github_raw_url(&url).unwrap();

    assert_eq!(
        raw.as_str(),
        "https://raw.githubusercontent.com/acme/cookbook/main/recipes/review.yaml"
    );
}

#[test]
fn recipe_url_validation_rejects_non_recipe_extensions() {
    let err = validated_recipe_github_url(
        "https://github.com/acme/cookbook/blob/main/recipes/review.txt",
    )
    .unwrap_err();

    assert!(err.body["error"].as_str().unwrap().contains(".yaml"));
}

#[tokio::test]
async fn recipes_validate_blocks_bad_recipe() {
    let app = crate::api::build_router(DaemonState::new(""));
    let (status, validation) = json_post(
        app,
        "/recipes/validate",
        serde_json::json!({
            "recipe": {
                "version": "1.0.0",
                "title": "",
                "description": "",
                "prompt": "Hello {{missing}}"
            }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(validation["verdict"], "block");
}

#[tokio::test]
async fn recipes_install_rejects_unknown_sub_recipe_reference() {
    let mut recipe = sample_recipe_json();
    recipe["subRecipes"] = serde_json::json!([{
        "id": "missing-child",
        "params": { "feature": "delegation" }
    }]);
    let app = crate::api::build_router(DaemonState::new(""));

    let (status, body) = json_post(
        app,
        "/recipes/install",
        serde_json::json!({ "recipe": recipe, "trusted": true }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "recipe validation failed");
    assert!(body["validation"]["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|finding| finding["kind"] == "unknown_sub_recipe"));
}
