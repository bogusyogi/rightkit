// Sub-recipe run-plan execution + response-schema pass-through for
// `coderight recipe run` (spliced into main_runtime via include!).

#[allow(clippy::too_many_arguments)]
async fn run_recipe_sub_steps(
    recipe: &Recipe,
    params: &std::collections::BTreeMap<String, String>,
    store: &RecipeStore,
    cli_model: Option<String>,
    provider: String,
    workspace: Option<String>,
    auto_approve: bool,
    stream_json: bool,
    max_turns: Option<u32>,
    toolset: Option<String>,
    max_budget_usd: Option<f64>,
) -> Result<(), i32> {
    let plan = match coderight_recipes::resolve_run_plan(
        recipe,
        params,
        &cli_recipe_catalog(store, Some(recipe)),
    ) {
        Ok(plan) => plan,
        Err(error) => {
            eprintln!("recipe run-plan resolution failed: {error}");
            return Err(1);
        }
    };
    if plan.steps.len() <= 1 {
        return Ok(());
    }
    let child_count = plan.steps.len() - 1;
    for (index, step) in plan.steps.iter().take(child_count).enumerate() {
        let run_id = format!("recipe-cli-subrun-{}-{}", unix_millis(), index);
        eprintln!(
            "sub-recipe {}/{}: {} (path {:?})",
            index + 1,
            child_count,
            step.recipe_id,
            step.path
        );
        let permission_auto_approve = step
            .rendered
            .settings
            .permission_mode
            .as_deref()
            .map(|mode| matches!(mode, "acceptEdits" | "accept-edits" | "yes" | "auto"))
            .unwrap_or(false);
        let effective_model = cli_model
            .clone()
            .or_else(|| step.rendered.settings.model.clone())
            .unwrap_or_else(|| "moonshotai/kimi-k2.6".to_string());
        let effective_workspace = workspace
            .clone()
            .or_else(|| step.rendered.settings.workspace.clone());
        let effective_toolset = toolset
            .clone()
            .or_else(|| step.rendered.settings.toolset.clone())
            .or_else(|| step.rendered.required_toolsets.first().cloned());
        let code = run_session(RunArgs {
            context_mode: ContextMode::effective(),
            output_schema: None,
            prompt: step.rendered.rendered_prompt.clone(),
            print: false,
            output_format: OutputFormat::Text,
            json_event: false,
            model: Some(effective_model),
            provider: Some(provider.clone()),
            effort: None,
            workspace: effective_workspace,
            auto_approve: auto_approve || permission_auto_approve,
            bypass_approvals: false,
            permission_mode: None,
            stream_json,
            exit_on_error: false,
            max_turns,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            toolset: effective_toolset,
            max_budget_usd,
            bare: false,
            safe_mode: false,
            system_prompt: None,
            system_prompt_file: None,
            append_system_prompt: None,
            append_system_prompt_file: None,
            mcp_config: None,
            session_id: Some(run_id),
            add_dir: Vec::new(),
            input_format: crate::main_runtime::InputFormat::Text,
            image: Vec::new(),
            ephemeral: false,
            output_last_message: None,
            color: crate::cli::CliColor::Auto,
            tools: Vec::new(),
            exclude_tools: Vec::new(),
            no_mcp: false,
        })
        .await;
        if code != 0 {
            eprintln!(
                "sub-recipe {} failed with exit {code}; aborting parent run",
                step.recipe_id
            );
            return Err(code);
        }
    }
    Ok(())
}

// Materializes the recipe's declared response schema (if any) to a temp file
// in the exact shape `RunArgs.output_schema` consumes, so the existing
// steering + final-response validation machinery applies unchanged.
fn recipe_response_schema_file(recipe: &Recipe) -> Result<Option<String>, String> {
    let Some(schema) = recipe.response_schema.as_ref() else {
        return Ok(None);
    };
    let dir = std::env::temp_dir().join("coderight-recipe-schemas");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let path = dir.join(format!(
        "recipe-schema-{}-{}.json",
        std::process::id(),
        unix_millis()
    ));
    let bytes = serde_json::to_vec(schema).map_err(|error| error.to_string())?;
    std::fs::write(&path, bytes).map_err(|error| error.to_string())?;
    Ok(Some(path.to_string_lossy().into_owned()))
}
