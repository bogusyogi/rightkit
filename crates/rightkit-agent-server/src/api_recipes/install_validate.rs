async fn list_recipes_handler(State(state): State<DaemonState>) -> Json<Vec<InstalledRecipe>> {
    Json(lock_recover(&state.recipes, "state.recipes").list())
}

async fn install_recipe_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<InstallRecipeRequest>,
) -> Result<(StatusCode, Json<InstalledRecipe>), RecipeApiError> {
    enforce_recipe_trusted_source(&headers)?;
    install_recipe_document(
        &state,
        body.recipe,
        body.source.unwrap_or(RecipeSource::Inline),
        body.source_uri,
        body.trusted,
    )
}

async fn install_recipe_file_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<InstallRecipeFileRequest>,
) -> Result<(StatusCode, Json<InstalledRecipe>), RecipeApiError> {
    enforce_recipe_trusted_source(&headers)?;
    let path = validated_recipe_file_path(&body.path)?;
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        RecipeApiError::bad_request(serde_json::json!({
            "error": format!("failed to read recipe file: {e}")
        }))
    })?;
    if raw.len() > 512 * 1024 {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": "recipe file exceeds 512 KiB"
        })));
    }
    let recipe = parse_recipe_document(&raw, &path)?;
    install_recipe_document(
        &state,
        recipe,
        RecipeSource::Local,
        Some(path.display().to_string()),
        body.trusted,
    )
}

async fn install_recipe_url_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<InstallRecipeUrlRequest>,
) -> Result<(StatusCode, Json<InstalledRecipe>), RecipeApiError> {
    enforce_recipe_trusted_source(&headers)?;
    let url = validated_recipe_github_url(&body.url)?;
    let raw_url = normalized_recipe_github_raw_url(&url)?;
    let client = rightkit_agent_daemon::http_client::endpoint_client(
        raw_url.as_str(),
        std::time::Duration::from_secs(15),
    )
    .map_err(|e| {
        RecipeApiError::server_error(serde_json::json!({
            "error": format!("failed to create recipe HTTP client: {e}")
        }))
    })?;
    let request = rightkit_http::Request::get(raw_url.as_str())
        .header("User-Agent", "RightKit recipe installer")
        .max_response_bytes(512 * 1024);
    let response = client.send_any(&request).await.map_err(|e| match e {
        rightkit_http::HttpError::BodyTooLarge { .. } => RecipeApiError::bad_request(
            serde_json::json!({ "error": "recipe URL exceeds 512 KiB" }),
        ),
        other => RecipeApiError::bad_request(serde_json::json!({
            "error": format!("failed to fetch recipe URL: {other}")
        })),
    })?;
    if !rightkit_agent_daemon::http_client::is_success(response.status) {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": format!("recipe URL returned HTTP {}", response.status)
        })));
    }
    let bytes = response.body;
    let raw = std::str::from_utf8(&bytes).map_err(|e| {
        RecipeApiError::bad_request(serde_json::json!({
            "error": format!("recipe URL is not UTF-8: {e}")
        }))
    })?;
    let recipe = parse_recipe_document_from_name(raw, raw_url.path())?;
    install_recipe_document(
        &state,
        recipe,
        RecipeSource::Github,
        Some(url.to_string()),
        body.trusted,
    )
}

async fn validate_recipe_handler(
    State(state): State<DaemonState>,
    Json(body): Json<ValidateRecipeRequest>,
) -> Json<rightkit_agent_recipes::RecipeValidationRun> {
    let catalog = recipe_validation_catalog(&state, &body.recipe);
    Json(validate_recipe_with_catalog(
        &body.recipe,
        &catalog,
        format!("recipe-validation-{}", now_millis()),
        now_iso(),
    ))
}

fn install_recipe_document(
    state: &DaemonState,
    recipe: Recipe,
    source: RecipeSource,
    source_uri: Option<String>,
    trusted: bool,
) -> Result<(StatusCode, Json<InstalledRecipe>), RecipeApiError> {
    let catalog = recipe_validation_catalog(state, &recipe);
    let validation = validate_recipe_with_catalog(
        &recipe,
        &catalog,
        format!("recipe-validation-{}", now_millis()),
        now_iso(),
    );
    if validation.verdict == rightkit_agent_recipes::RecipeVerdict::Block {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": "recipe validation failed",
            "validation": validation,
        })));
    }

    let id = stable_recipe_id(&recipe);
    let installed = InstalledRecipe {
        id: id.clone(),
        source,
        source_uri,
        title: recipe.title.clone(),
        version: recipe.version.clone(),
        checksum: recipe_checksum(&recipe),
        trusted,
        recipe: Recipe {
            id: Some(id),
            ..recipe
        },
        installed_at: now_iso(),
        latest_validation: Some(validation),
    };
    let snapshot = {
        let mut store = lock_recover(&state.recipes, "state.recipes");
        store.install(installed.clone());
        store.snapshot()
    };
    persist_recipe_store_snapshot(state, &snapshot).map_err(|e| {
        RecipeApiError::server_error(serde_json::json!({
            "error": "failed to persist recipe store",
            "detail": e,
        }))
    })?;
    Ok((StatusCode::CREATED, Json(installed)))
}

fn parse_recipe_document(raw: &str, path: &FsPath) -> Result<Recipe, RecipeApiError> {
    match recipe_file_format(path.extension().and_then(|value| value.to_str()))? {
        RecipeFileFormat::Json => parse_recipe_json(raw),
        RecipeFileFormat::Yaml => parse_recipe_yaml(raw),
    }
}

fn parse_recipe_document_from_name(raw: &str, name: &str) -> Result<Recipe, RecipeApiError> {
    let extension = name.rsplit_once('.').map(|(_, extension)| extension);
    match recipe_file_format(extension)? {
        RecipeFileFormat::Json => parse_recipe_json(raw),
        RecipeFileFormat::Yaml => parse_recipe_yaml(raw),
    }
}

fn parse_recipe_json(raw: &str) -> Result<Recipe, RecipeApiError> {
    serde_json::from_str::<Recipe>(raw).map_err(|e| {
        RecipeApiError::bad_request(serde_json::json!({
            "error": format!("invalid recipe JSON: {e}")
        }))
    })
}

fn parse_recipe_yaml(raw: &str) -> Result<Recipe, RecipeApiError> {
    serde_yaml::from_str::<Recipe>(raw).map_err(|e| {
        RecipeApiError::bad_request(serde_json::json!({
            "error": format!("invalid recipe YAML: {e}")
        }))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecipeFileFormat {
    Json,
    Yaml,
}

fn recipe_file_format(extension: Option<&str>) -> Result<RecipeFileFormat, RecipeApiError> {
    let Some(ext) = extension else {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": "recipe document must be a .json, .yaml, or .yml file"
        })));
    };
    match ext.to_ascii_lowercase().as_str() {
        "json" => Ok(RecipeFileFormat::Json),
        "yaml" | "yml" => Ok(RecipeFileFormat::Yaml),
        _ => Err(RecipeApiError::bad_request(serde_json::json!({
            "error": "recipe document must be a .json, .yaml, or .yml file"
        }))),
    }
}

fn validated_recipe_github_url(raw: &str) -> Result<url::Url, RecipeApiError> {
    let url = url::Url::parse(raw.trim()).map_err(|e| {
        RecipeApiError::bad_request(serde_json::json!({
            "error": format!("invalid recipe URL: {e}")
        }))
    })?;
    if url.scheme() != "https" {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": "recipe URL must use https"
        })));
    }
    let host = url.host_str().unwrap_or_default();
    if host != "github.com" && host != "raw.githubusercontent.com" {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": "recipe URL must be hosted on github.com or raw.githubusercontent.com"
        })));
    }
    let raw_url = normalized_recipe_github_raw_url(&url)?;
    recipe_file_format(
        raw_url
            .path()
            .rsplit_once('.')
            .map(|(_, extension)| extension),
    )?;
    Ok(url)
}

fn normalized_recipe_github_raw_url(url: &url::Url) -> Result<url::Url, RecipeApiError> {
    if url.host_str() == Some("raw.githubusercontent.com") {
        return Ok(url.clone());
    }
    let parts = url
        .path_segments()
        .map(|segments| segments.collect::<Vec<_>>())
        .unwrap_or_default();
    if url.host_str() != Some("github.com") || parts.len() < 5 || parts[2] != "blob" {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": "github.com recipe URL must point at a repository blob"
        })));
    }
    let owner = parts[0];
    let repo = parts[1];
    let branch = parts[3];
    let path = parts[4..].join("/");
    url::Url::parse(&format!(
        "https://raw.githubusercontent.com/{owner}/{repo}/{branch}/{path}"
    ))
    .map_err(|e| {
        RecipeApiError::bad_request(serde_json::json!({
            "error": format!("failed to normalize GitHub recipe URL: {e}")
        }))
    })
}

fn recipe_validation_catalog(state: &DaemonState, recipe: &Recipe) -> BTreeMap<String, Recipe> {
    let mut catalog = state
        .recipes
        .lock()
        .unwrap()
        .list()
        .into_iter()
        .map(|installed| (installed.id, installed.recipe))
        .collect::<BTreeMap<_, _>>();
    let id = stable_recipe_id(recipe);
    catalog.insert(
        id.clone(),
        Recipe {
            id: Some(id),
            ..recipe.clone()
        },
    );
    catalog
}

fn validated_recipe_file_path(raw: &str) -> Result<std::path::PathBuf, RecipeApiError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": "path is required"
        })));
    }
    let path = std::path::PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": "recipe file path must be absolute"
        })));
    }
    recipe_file_format(path.extension().and_then(|value| value.to_str()))?;
    Ok(path)
}
