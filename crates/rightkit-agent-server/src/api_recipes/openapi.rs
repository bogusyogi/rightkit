pub(crate) fn recipes_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert("Recipe".to_string(), json!({"type": "object"}));
    schemas.insert("InstalledRecipe".to_string(), json!({"type": "object"}));
    schemas.insert("RecipeRun".to_string(), json!({"type": "object"}));
    schemas.insert("RecipeValidationRun".to_string(), json!({"type": "object"}));
    schemas.insert("RenderedRecipe".to_string(), json!({"type": "object"}));
    schemas.insert("RecipeDeeplink".to_string(), json!({"type": "object"}));
    schemas.insert(
        "InstallRecipeRequest".to_string(),
        json!({
            "type": "object",
            "required": ["recipe"],
            "properties": {
                "recipe": {"$ref": "#/components/schemas/Recipe"},
                "source": {"type": ["string", "null"]},
                "sourceUri": {"type": ["string", "null"]},
                "trusted": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "InstallRecipeFileRequest".to_string(),
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": {"type": "string"},
                "trusted": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "InstallRecipeUrlRequest".to_string(),
        json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": {"type": "string"},
                "trusted": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ValidateRecipeRequest".to_string(),
        json!({
            "type": "object",
            "required": ["recipe"],
            "properties": {"recipe": {"$ref": "#/components/schemas/Recipe"}},
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RenderRecipeRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "recipeId": {"type": ["string", "null"]},
                "recipe": {"anyOf": [{"$ref": "#/components/schemas/Recipe"}, {"type": "null"}]},
                "params": {"type": "object", "additionalProperties": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ParseRecipeDeeplinkRequest".to_string(),
        json!({
            "type": "object",
            "required": ["url"],
            "properties": {"url": {"type": "string"}},
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "RunRecipeRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "params": {"type": "object", "additionalProperties": {"type": "string"}},
                "workspaceRoot": {"type": ["string", "null"]}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ScheduleRecipeRequest".to_string(),
        json!({
            "type": "object",
            "required": ["cronExpr"],
            "properties": {
                "id": {"type": ["string", "null"]},
                "cronExpr": {"type": "string"},
                "params": {"type": "object", "additionalProperties": {"type": "string"}},
                "workspaceRoot": {"type": ["string", "null"]},
                "timeZone": {"type": ["string", "null"]},
                "skills": {"type": "array", "items": {"type": "string"}},
                "workdir": {"type": ["string", "null"]},
                "toolset": {"type": ["string", "null"]},
                "executionBackend": {"type": ["string", "null"]},
                "delivery": {"type": "array", "items": {"type": "object"}},
                "paused": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "CreateRecipeDeeplinkRequest".to_string(),
        json!({
            "type": "object",
            "properties": {
                "action": {"type": ["string", "null"]},
                "params": {"type": "object", "additionalProperties": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ScheduleRecipeResponse".to_string(),
        json!({
            "type": "object",
            "required": ["schedule"],
            "properties": {"schedule": {"type": "object"}}
        }),
    );
    schemas
}

pub(crate) fn recipes_list_response_schema() -> Value {
    json!({"type": "array", "items": {"$ref": "#/components/schemas/InstalledRecipe"}})
}

pub(crate) fn install_recipe_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/InstallRecipeRequest"})
}

pub(crate) fn install_recipe_file_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/InstallRecipeFileRequest"})
}

pub(crate) fn install_recipe_url_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/InstallRecipeUrlRequest"})
}

pub(crate) fn installed_recipe_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/InstalledRecipe"})
}

pub(crate) fn validate_recipe_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/ValidateRecipeRequest"})
}

pub(crate) fn validate_recipe_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/RecipeValidationRun"})
}

pub(crate) fn render_recipe_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/RenderRecipeRequest"})
}

pub(crate) fn render_recipe_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/RenderedRecipe"})
}

pub(crate) fn parse_recipe_deeplink_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/ParseRecipeDeeplinkRequest"})
}

pub(crate) fn recipe_deeplink_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/RecipeDeeplink"})
}

pub(crate) fn run_recipe_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/RunRecipeRequest"})
}

pub(crate) fn recipe_run_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/RecipeRun"})
}

pub(crate) fn schedule_recipe_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/ScheduleRecipeRequest"})
}

pub(crate) fn schedule_recipe_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ScheduleRecipeResponse"})
}

pub(crate) fn create_recipe_deeplink_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/CreateRecipeDeeplinkRequest"})
}

pub(crate) fn recipe_runs_response_schema() -> Value {
    json!({"type": "array", "items": {"$ref": "#/components/schemas/RecipeRun"}})
}
