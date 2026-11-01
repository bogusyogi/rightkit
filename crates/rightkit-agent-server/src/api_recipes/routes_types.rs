use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use rightkit_agent_engine_protocol::{ArtifactRow, ContentTrust, ProtocolEvent, TranscriptMessage};
use coderight_recipes::{
    build_recipe_deeplink, parse_recipe_deeplink, recipe_checksum, render_recipe, stable_recipe_id,
    validate_recipe_with_catalog, InstalledRecipe, Recipe, RecipeDeeplink, RecipeRun,
    RecipeRunStatus, RecipeSource, RecipeStore, RecipeStoreSnapshot, RenderedRecipe,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::Path as FsPath;

use crate::api::{
    lock_recover, now_iso, now_millis, push_session_artifact, trust_envelope, DaemonState,
};
use crate::api_permissions::enforce_trusted_source_authority;
use crate::api_toolsets::resolve_toolset_profile_with_config;

pub(crate) fn recipe_routes() -> Router<DaemonState> {
    Router::new()
        .route("/recipes", get(list_recipes_handler))
        .route("/recipes/install", post(install_recipe_handler))
        .route("/recipes/install-file", post(install_recipe_file_handler))
        .route("/recipes/install-url", post(install_recipe_url_handler))
        .route("/recipes/validate", post(validate_recipe_handler))
        .route("/recipes/render", post(render_recipe_handler))
        .route(
            "/recipes/deeplink/parse",
            post(parse_recipe_deeplink_handler),
        )
        .route("/recipes/{id}/run", post(run_recipe_handler))
        .route("/recipes/{id}/schedule", post(schedule_recipe_handler))
        .route(
            "/recipes/{id}/deeplink",
            post(create_recipe_deeplink_handler),
        )
        .route("/recipes/{id}/runs", get(list_recipe_runs_handler))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallRecipeRequest {
    recipe: Recipe,
    source: Option<RecipeSource>,
    source_uri: Option<String>,
    #[serde(default)]
    trusted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallRecipeFileRequest {
    path: String,
    #[serde(default)]
    trusted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallRecipeUrlRequest {
    url: String,
    #[serde(default)]
    trusted: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ValidateRecipeRequest {
    recipe: Recipe,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenderRecipeRequest {
    recipe_id: Option<String>,
    recipe: Option<Recipe>,
    #[serde(default)]
    params: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunRecipeRequest {
    #[serde(default)]
    params: BTreeMap<String, String>,
    workspace_root: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateRecipeDeeplinkRequest {
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    params: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParseRecipeDeeplinkRequest {
    url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScheduleRecipeRequest {
    #[serde(default)]
    id: Option<String>,
    cron_expr: String,
    #[serde(default)]
    params: BTreeMap<String, String>,
    #[serde(default)]
    workspace_root: Option<String>,
    #[serde(default)]
    time_zone: Option<String>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    workdir: Option<String>,
    #[serde(default)]
    toolset: Option<String>,
    #[serde(default)]
    execution_backend: Option<String>,
    #[serde(default)]
    delivery: Vec<crate::api_schedules::StoredDeliveryTarget>,
    #[serde(default)]
    paused: bool,
}
