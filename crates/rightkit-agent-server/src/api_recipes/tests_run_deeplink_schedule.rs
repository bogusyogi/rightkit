#[tokio::test]
async fn recipes_render_and_run_record_prompt_hash() {
    let state = DaemonState::new("");
    let app = crate::api::build_router(state.clone());
    let (install_status, _) = json_post(
        app.clone(),
        "/recipes/install",
        serde_json::json!({ "recipe": sample_recipe_json(), "trusted": true }),
    )
    .await;
    assert_eq!(install_status, StatusCode::CREATED);

    let (render_status, rendered) = json_post(
        app.clone(),
        "/recipes/render",
        serde_json::json!({
            "recipeId": "recipe-test",
            "params": { "feature": "recipes" }
        }),
    )
    .await;
    assert_eq!(render_status, StatusCode::OK);
    assert!(rendered["renderedPrompt"]
        .as_str()
        .unwrap()
        .contains("Implement recipes."));

    let (run_status, run) = json_post(
        app.clone(),
        "/recipes/recipe-test/run",
        serde_json::json!({ "params": { "feature": "recipes" } }),
    )
    .await;
    assert_eq!(run_status, StatusCode::OK);
    assert_eq!(run["recipeId"], "recipe-test");
    assert_eq!(run["renderedPromptHash"], rendered["renderedPromptHash"]);
    assert!(run["resultArtifactId"]
        .as_str()
        .unwrap()
        .starts_with("recipe-artifact-rr-"));
    let live = state
        .get_session(run["sessionId"].as_str().unwrap())
        .expect("recipe session");
    assert_eq!(live.toolset.as_deref(), Some("dev-safe"));
    assert_eq!(live.mcp_channel, "recipe");
    assert!(live.active_skills.is_empty());

    let artifacts_uri = format!(
        "/sessions/{}/runtime/artifacts",
        run["sessionId"].as_str().unwrap()
    );
    let (artifacts_status, artifacts) = json_get(app, &artifacts_uri).await;
    assert_eq!(artifacts_status, StatusCode::OK);
    let artifact = artifacts
        .as_array()
        .unwrap()
        .iter()
        .find(|artifact| artifact["id"] == run["resultArtifactId"])
        .unwrap();
    assert_eq!(artifact["kind"], "recipe_prompt");
    assert_eq!(artifact["contentHash"], rendered["renderedPromptHash"]);
    assert!(artifact["meta"]
        .as_str()
        .unwrap()
        .contains("\"recipeId\":\"recipe-test\""));
}

#[tokio::test]
async fn recipes_run_rejects_unknown_required_toolset() {
    let mut recipe = sample_recipe_json();
    recipe["requiredToolsets"] = serde_json::json!(["missing-recipe-toolset"]);
    recipe["settings"] = serde_json::json!({});
    let app = crate::api::build_router(DaemonState::new(""));
    let (install_status, _) = json_post(
        app.clone(),
        "/recipes/install",
        serde_json::json!({ "recipe": recipe, "trusted": true }),
    )
    .await;
    assert_eq!(install_status, StatusCode::CREATED);

    let (run_status, body) = json_post(
        app,
        "/recipes/recipe-test/run",
        serde_json::json!({ "params": { "feature": "recipes" } }),
    )
    .await;

    assert_eq!(run_status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("unknown toolset"));
    assert_eq!(body["requiredToolsets"][0], "missing-recipe-toolset");
}

#[tokio::test]
async fn recipes_schedule_rejects_toolset_override_outside_required_set() {
    let root = test_workspace_root("schedule-toolset");
    std::fs::create_dir_all(&root).unwrap();
    let app = crate::api::build_router(DaemonState::new(""));
    let (install_status, _) = json_post(
        app.clone(),
        "/recipes/install",
        serde_json::json!({ "recipe": sample_recipe_json(), "trusted": true }),
    )
    .await;
    assert_eq!(install_status, StatusCode::CREATED);

    let (schedule_status, body) = json_post(
        app,
        "/recipes/recipe-test/schedule",
        serde_json::json!({
            "id": "recipe-nightly",
            "cronExpr": "* * * * *",
            "workspaceRoot": root.display().to_string(),
            "params": { "feature": "scheduled recipes" },
            "toolset": "read-only"
        }),
    )
    .await;

    assert_eq!(schedule_status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("does not satisfy requiredToolsets"));
    assert_eq!(body["toolset"], "read-only");
    assert_eq!(body["requiredToolsets"][0], "dev-safe");

    let _ = std::fs::remove_dir_all(&root);
}

#[tokio::test]
async fn recipes_create_and_parse_validated_deeplink() {
    let app = crate::api::build_router(DaemonState::new(""));
    let (install_status, _) = json_post(
        app.clone(),
        "/recipes/install",
        serde_json::json!({ "recipe": sample_recipe_json(), "trusted": true }),
    )
    .await;
    assert_eq!(install_status, StatusCode::CREATED);

    let (link_status, link) = json_post(
        app.clone(),
        "/recipes/recipe-test/deeplink",
        serde_json::json!({
            "action": "run",
            "params": { "feature": "deeplinks" }
        }),
    )
    .await;
    assert_eq!(link_status, StatusCode::OK);
    assert_eq!(link["recipeId"], "recipe-test");
    assert_eq!(link["action"], "run");
    assert!(link["url"]
        .as_str()
        .unwrap()
        .starts_with("coderight://recipes/recipe-test?"));

    let (parse_status, parsed) = json_post(
        app,
        "/recipes/deeplink/parse",
        serde_json::json!({ "url": link["url"] }),
    )
    .await;
    assert_eq!(parse_status, StatusCode::OK);
    assert_eq!(parsed["recipeId"], "recipe-test");
    assert_eq!(parsed["params"]["feature"], "deeplinks");
}

#[tokio::test]
async fn recipe_deeplink_creation_requires_renderable_params() {
    let app = crate::api::build_router(DaemonState::new(""));
    let (install_status, _) = json_post(
        app.clone(),
        "/recipes/install",
        serde_json::json!({ "recipe": sample_recipe_json(), "trusted": true }),
    )
    .await;
    assert_eq!(install_status, StatusCode::CREATED);

    let (link_status, body) = json_post(
        app,
        "/recipes/recipe-test/deeplink",
        serde_json::json!({ "action": "run", "params": {} }),
    )
    .await;

    assert_eq!(link_status, StatusCode::BAD_REQUEST);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("missing required recipe parameter"));
}

#[tokio::test]
async fn recipes_reload_from_persisted_store() {
    let store_path = test_store_path("reload");
    let state = DaemonState::new("").with_recipe_store_path(store_path.clone());
    let app = crate::api::build_router(state);
    let (install_status, _) = json_post(
        app.clone(),
        "/recipes/install",
        serde_json::json!({ "recipe": sample_recipe_json(), "trusted": true }),
    )
    .await;
    assert_eq!(install_status, StatusCode::CREATED);

    let (run_status, _) = json_post(
        app,
        "/recipes/recipe-test/run",
        serde_json::json!({ "params": { "feature": "recipes" } }),
    )
    .await;
    assert_eq!(run_status, StatusCode::OK);
    assert!(store_path.exists());

    let reloaded =
        crate::api::build_router(DaemonState::new("").with_recipe_store_path(store_path.clone()));
    let recipes_resp = reloaded
        .clone()
        .oneshot(
            Request::builder()
                .uri("/recipes")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let recipes_body = recipes_resp.into_body().collect().await.unwrap().to_bytes();
    let recipes: serde_json::Value = serde_json::from_slice(&recipes_body).unwrap();
    assert_eq!(recipes.as_array().unwrap().len(), 1);

    let runs_resp = reloaded
        .oneshot(
            Request::builder()
                .uri("/recipes/recipe-test/runs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let runs_body = runs_resp.into_body().collect().await.unwrap().to_bytes();
    let runs: serde_json::Value = serde_json::from_slice(&runs_body).unwrap();
    assert_eq!(runs.as_array().unwrap().len(), 1);

    let _ = std::fs::remove_dir_all(store_path.parent().unwrap());
}

#[tokio::test]
async fn recipes_can_create_schedulable_runbooks_with_run_history() {
    let root = test_workspace_root("schedule");
    std::fs::create_dir_all(&root).unwrap();
    let store_path = root.join("recipes.json");
    let state = DaemonState::new("").with_recipe_store_path(store_path);
    let app = crate::api::build_router(state.clone());
    let (install_status, _) = json_post(
        app.clone(),
        "/recipes/install",
        serde_json::json!({ "recipe": sample_recipe_json(), "trusted": true }),
    )
    .await;
    assert_eq!(install_status, StatusCode::CREATED);

    let (schedule_status, schedule) = json_post(
        app.clone(),
        "/recipes/recipe-test/schedule",
        serde_json::json!({
            "id": "recipe-nightly",
            "cronExpr": "* * * * *",
            "workspaceRoot": root.display().to_string(),
            "params": { "feature": "scheduled recipes" },
            "delivery": [{ "kind": "desktop_notification" }]
        }),
    )
    .await;
    assert_eq!(schedule_status, StatusCode::CREATED);
    assert_eq!(schedule["recipeId"], "recipe-test");
    assert_eq!(schedule["schedule"]["sourceRecipeId"], "recipe-test");
    assert_eq!(
        schedule["schedule"]["recipeParams"]["feature"],
        "scheduled recipes"
    );

    crate::scheduler::fire_schedule_by_id(&state, &root, "recipe-nightly")
        .await
        .unwrap();

    let runs_resp = app
        .oneshot(
            Request::builder()
                .uri("/recipes/recipe-test/runs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let runs_body = runs_resp.into_body().collect().await.unwrap().to_bytes();
    let runs: serde_json::Value = serde_json::from_slice(&runs_body).unwrap();
    assert_eq!(runs.as_array().unwrap().len(), 1);
    assert_eq!(runs[0]["status"], "submitted");
    assert_eq!(runs[0]["params"]["feature"], "scheduled recipes");

    let _ = std::fs::remove_dir_all(&root);
}
