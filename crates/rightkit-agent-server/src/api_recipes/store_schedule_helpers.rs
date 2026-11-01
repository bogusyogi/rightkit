fn resolve_recipe(
    state: &DaemonState,
    recipe_id: Option<&str>,
    inline: Option<Recipe>,
) -> Result<Recipe, RecipeApiError> {
    if let Some(recipe) = inline {
        return Ok(recipe);
    }
    let Some(recipe_id) = recipe_id else {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": "recipeId or recipe is required"
        })));
    };
    state
        .recipes
        .lock()
        .unwrap()
        .get(recipe_id)
        .map(|installed| installed.recipe)
        .ok_or_else(|| RecipeApiError::not_found("recipe not found"))
}

/// A missing file is an empty store. A file that exists but does not parse (partial or empty after an interrupted
/// write) is set aside as `<name>.damaged-<time>` first, so the empty store the caller starts with is never saved over
/// the only copy of the recipes.
pub(crate) fn load_recipe_store(path: &FsPath) -> RecipeStore {
    crate::store_write::load_or_set_aside::<RecipeStoreSnapshot>(path)
        .map(RecipeStore::from_snapshot)
        .unwrap_or_default()
}

pub(crate) fn write_recipe_store(
    path: &FsPath,
    snapshot: &RecipeStoreSnapshot,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(snapshot).map_err(|e| e.to_string())?;
    crate::store_write::write_store_file(path, raw.as_bytes()).map_err(|e| e.to_string())
}

pub(crate) fn persist_recipe_store_snapshot(
    state: &DaemonState,
    snapshot: &RecipeStoreSnapshot,
) -> Result<(), String> {
    let Some(path) = state.recipe_store_path.as_deref() else {
        return Ok(());
    };
    write_recipe_store(path, snapshot)
}

fn schedule_recipe_workspace_root(
    body: &ScheduleRecipeRequest,
    rendered: &RenderedRecipe,
) -> Result<std::path::PathBuf, RecipeApiError> {
    let raw = body
        .workspace_root
        .as_deref()
        .or(rendered.settings.workspace.as_deref())
        .unwrap_or_default()
        .trim();
    if raw.is_empty() {
        std::env::current_dir().map_err(|e| {
            RecipeApiError::server_error(serde_json::json!({
                "error": format!("failed to resolve current directory: {e}")
            }))
        })
    } else {
        Ok(std::path::PathBuf::from(raw))
    }
}

fn recipe_workspace_config_root(raw: &str) -> Result<std::path::PathBuf, RecipeApiError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        std::env::current_dir().map_err(|e| {
            RecipeApiError::server_error(serde_json::json!({
                "error": format!("failed to resolve current directory: {e}")
            }))
        })
    } else {
        Ok(std::path::PathBuf::from(trimmed))
    }
}

fn recipe_effective_toolset(
    recipe: &Recipe,
    rendered: &RenderedRecipe,
    requested_toolset: Option<&str>,
    workspace_root: &FsPath,
) -> Result<Option<String>, RecipeApiError> {
    let required = clean_unique_recipe_values(&recipe.required_toolsets);
    let selected = clean_optional_recipe_schedule_string(requested_toolset)
        .or_else(|| clean_optional_recipe_schedule_string(rendered.settings.toolset.as_deref()))
        .or_else(|| required.first().cloned());
    let config = rightkit_app_settings::agent_config::ConfigLoader::new().load_files_for_workspace(workspace_root);

    for required_toolset in &required {
        if resolve_toolset_profile_with_config(required_toolset, &config).is_err() {
            return Err(RecipeApiError::bad_request(serde_json::json!({
                "error": format!("recipe requires unknown toolset '{required_toolset}'"),
                "requiredToolsets": required,
            })));
        }
    }

    let Some(selected) = selected else {
        return Ok(None);
    };
    if resolve_toolset_profile_with_config(&selected, &config).is_err() {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": format!("unknown recipe toolset '{selected}'"),
            "toolset": selected,
        })));
    }
    if !required.is_empty() && !required.iter().any(|toolset| toolset == &selected) {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": format!("recipe toolset '{selected}' does not satisfy requiredToolsets"),
            "toolset": selected,
            "requiredToolsets": required,
        })));
    }
    Ok(Some(selected))
}

fn validated_recipe_schedule_cron(value: &str, time_zone: &str) -> Result<String, RecipeApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": "cronExpr is required"
        })));
    }
    crate::scheduler::next_due_epoch_minute_in_time_zone(
        trimmed,
        time_zone,
        crate::scheduler::current_epoch_minute(),
        366 * 24 * 60,
    )
    .map_err(|e| RecipeApiError::bad_request(serde_json::json!({ "error": e })))?;
    Ok(trimmed.to_string())
}

fn validated_recipe_schedule_id(value: &str) -> Result<String, RecipeApiError> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || !trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": "schedule id must contain only ASCII letters, numbers, '-', '_', or '.'"
        })));
    }
    Ok(trimmed.to_string())
}

fn sanitize_recipe_schedule_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect()
}

fn recipe_schedule_skills(body: &ScheduleRecipeRequest, recipe: &Recipe) -> Vec<String> {
    let source = if body.skills.is_empty() {
        &recipe.skills
    } else {
        &body.skills
    };
    clean_unique_recipe_values(source)
}

fn clean_recipe_skills(skills: &[String]) -> Vec<String> {
    clean_unique_recipe_values(skills)
}

fn clean_unique_recipe_values(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .fold(Vec::new(), |mut acc, value| {
            if !acc.iter().any(|existing| existing == &value) {
                acc.push(value);
            }
            acc
        })
}

fn clean_optional_recipe_schedule_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn validated_recipe_schedule_workdir(
    value: Option<&str>,
    root: &FsPath,
) -> Result<Option<String>, RecipeApiError> {
    let Some(value) = clean_optional_recipe_schedule_string(value) else {
        return Ok(None);
    };
    let path = std::path::PathBuf::from(&value);
    if !path.is_absolute() {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": "workdir must be absolute"
        })));
    }
    if !normalize_recipe_schedule_path(&path).starts_with(normalize_recipe_schedule_path(root)) {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": "workdir must stay inside workspaceRoot"
        })));
    }
    Ok(Some(value))
}

fn normalize_recipe_schedule_path(path: &FsPath) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn validated_recipe_schedule_delivery(
    delivery: Vec<crate::api_schedules::StoredDeliveryTarget>,
) -> Result<Vec<crate::api_schedules::StoredDeliveryTarget>, RecipeApiError> {
    if delivery.iter().any(|target| target.kind.trim().is_empty()) {
        return Err(RecipeApiError::bad_request(serde_json::json!({
            "error": "delivery target kind is required"
        })));
    }
    Ok(delivery
        .into_iter()
        .map(|mut target| {
            target.kind = target.kind.trim().to_string();
            target.target = clean_optional_recipe_schedule_string(target.target.as_deref());
            target
        })
        .collect())
}
