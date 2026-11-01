async fn cli_recipe(command: RecipeCommand) -> i32 {
    match command {
        RecipeCommand::List { store, json } => {
            let store_path = cli_recipe_store_path(store.as_deref());
            let store = api_recipes::load_recipe_store(&store_path);
            let recipes = store.list();
            if json {
                print_json_value(serde_json::json!({
                    "storePath": store_path,
                    "count": recipes.len(),
                    "recipes": recipes,
                }));
            } else if recipes.is_empty() {
                println!("No installed recipes in {}", store_path.display());
            } else {
                println!("Installed recipes ({})", store_path.display());
                for recipe in recipes {
                    println!(
                        "- {}  {}  {}  trusted={}  source={:?}",
                        recipe.id, recipe.title, recipe.version, recipe.trusted, recipe.source
                    );
                }
            }
            0
        }
        RecipeCommand::Validate {
            target,
            store,
            json,
        } => {
            let store_path = cli_recipe_store_path(store.as_deref());
            let store = api_recipes::load_recipe_store(&store_path);
            let recipe = match resolve_cli_recipe_target(&target, &store) {
                Ok(recipe) => recipe,
                Err(error) => {
                    eprintln!("{error}");
                    return 1;
                }
            };
            let validation = validate_recipe_with_catalog(
                &recipe,
                &cli_recipe_catalog(&store, Some(&recipe)),
                format!("recipe-cli-validation-{}", unix_millis()),
                now_iso(),
            );
            if json {
                print_json_value(serde_json::to_value(&validation).unwrap_or_default());
            } else {
                println!("Recipe validation: {:?}", validation.verdict);
                for finding in &validation.findings {
                    println!(
                        "- {:?} {}: {}",
                        finding.severity, finding.kind, finding.message
                    );
                }
            }
            if validation.verdict == RecipeVerdict::Block {
                1
            } else {
                0
            }
        }
        RecipeCommand::Render {
            target,
            params,
            store,
            json,
        } => {
            let store_path = cli_recipe_store_path(store.as_deref());
            let store = api_recipes::load_recipe_store(&store_path);
            let recipe = match resolve_cli_recipe_target(&target, &store) {
                Ok(recipe) => recipe,
                Err(error) => {
                    eprintln!("{error}");
                    return 1;
                }
            };
            let params = match parse_recipe_cli_params(&params) {
                Ok(params) => params,
                Err(error) => {
                    eprintln!("{error}");
                    return 1;
                }
            };
            let validation = validate_recipe_with_catalog(
                &recipe,
                &cli_recipe_catalog(&store, Some(&recipe)),
                format!("recipe-cli-validation-{}", unix_millis()),
                now_iso(),
            );
            if validation.verdict == RecipeVerdict::Block {
                eprintln!("recipe validation blocked render");
                for finding in validation.findings {
                    eprintln!(
                        "- {:?} {}: {}",
                        finding.severity, finding.kind, finding.message
                    );
                }
                return 1;
            }
            let rendered = match render_recipe(&recipe, &params) {
                Ok(rendered) => rendered,
                Err(error) => {
                    eprintln!("recipe render failed: {error}");
                    return 1;
                }
            };
            if json {
                print_json_value(serde_json::to_value(&rendered).unwrap_or_default());
            } else {
                println!("{}", rendered.rendered_prompt);
            }
            0
        }
        RecipeCommand::Run {
            target,
            params,
            store,
            model,
            provider,
            workspace,
            auto_approve,
            stream_json,
            max_turns,
            toolset,
            max_budget_usd,
        } => {
            let store_path = cli_recipe_store_path(store.as_deref());
            let mut store = api_recipes::load_recipe_store(&store_path);
            let recipe = match resolve_cli_recipe_target(&target, &store) {
                Ok(recipe) => recipe,
                Err(error) => {
                    eprintln!("{error}");
                    return 1;
                }
            };
            let params = match parse_recipe_cli_params(&params) {
                Ok(params) => params,
                Err(error) => {
                    eprintln!("{error}");
                    return 1;
                }
            };
            let validation = validate_recipe_with_catalog(
                &recipe,
                &cli_recipe_catalog(&store, Some(&recipe)),
                format!("recipe-cli-validation-{}", unix_millis()),
                now_iso(),
            );
            if validation.verdict == RecipeVerdict::Block {
                eprintln!("recipe validation blocked run");
                for finding in validation.findings {
                    eprintln!(
                        "- {:?} {}: {}",
                        finding.severity, finding.kind, finding.message
                    );
                }
                return 1;
            }
            let rendered = match render_recipe(&recipe, &params) {
                Ok(rendered) => rendered,
                Err(error) => {
                    eprintln!("recipe render failed: {error}");
                    return 1;
                }
            };
            let output_schema = match recipe_response_schema_file(&recipe) {
                Ok(path) => path,
                Err(error) => {
                    eprintln!("recipe response-schema materialization failed: {error}");
                    return 1;
                }
            };
            if let Err(code) = run_recipe_sub_steps(
                &recipe,
                &params,
                &store,
                model.clone(),
                provider.clone(),
                workspace.clone(),
                auto_approve,
                stream_json,
                max_turns,
                toolset.clone(),
                max_budget_usd,
            )
            .await
            {
                return code;
            }
            let recipe_id = rendered
                .recipe_id
                .clone()
                .unwrap_or_else(|| stable_recipe_id(&recipe));
            if store.get(&recipe_id).is_none() {
                let source_uri = std::path::PathBuf::from(&target)
                    .exists()
                    .then(|| target.clone());
                store.install(InstalledRecipe {
                    id: recipe_id.clone(),
                    source: RecipeSource::Local,
                    source_uri,
                    title: recipe.title.clone(),
                    version: recipe.version.clone(),
                    checksum: recipe_checksum(&recipe),
                    trusted: true,
                    recipe: Recipe {
                        id: Some(recipe_id.clone()),
                        ..recipe.clone()
                    },
                    installed_at: now_iso(),
                    latest_validation: Some(validation.clone()),
                });
            }
            let permission_auto_approve = rendered
                .settings
                .permission_mode
                .as_deref()
                .map(|mode| matches!(mode, "acceptEdits" | "accept-edits" | "yes" | "auto"))
                .unwrap_or(false);
            let effective_model = model
                .or_else(|| rendered.settings.model.clone())
                .unwrap_or_else(|| "moonshotai/kimi-k2.6".to_string());
            let effective_workspace = workspace.or_else(|| rendered.settings.workspace.clone());
            let effective_toolset = toolset
                .or_else(|| rendered.settings.toolset.clone())
                .or_else(|| rendered.required_toolsets.first().cloned());
            let run_id = format!("recipe-cli-run-{}", unix_millis());
            let code = run_session(RunArgs {
                context_mode: ContextMode::effective(),
                output_schema,
                prompt: rendered.rendered_prompt.clone(),
                print: false,
                output_format: OutputFormat::Text,
                json_event: false,
                model: Some(effective_model),
                provider: Some(provider),
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
                // Recipe runs honor the strict policy / permission tier only
                // when the recipe itself opts-in. Until recipes surface a
                // safe_mode field we default to false to avoid regressing
                // existing recipe executions.
                safe_mode: false,
                system_prompt: None,
                system_prompt_file: None,
                append_system_prompt: None,
                append_system_prompt_file: None,
                mcp_config: None,
                session_id: Some(run_id.clone()),
                add_dir: Vec::new(),
                // Recipe runs always feed the engine from the materialized
                // rendered prompt — never stdin JSONL — so force Text.
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
            store.add_run(RecipeRun {
                id: run_id.clone(),
                recipe_id,
                session_id: run_id,
                status: if code == 0 {
                    RecipeRunStatus::Completed
                } else {
                    RecipeRunStatus::Failed
                },
                params: rendered.params,
                rendered_prompt_hash: rendered.rendered_prompt_hash,
                result_artifact_id: None,
                created_at: now_iso(),
                completed_at: Some(now_iso()),
            });
            if let Err(error) = api_recipes::write_recipe_store(&store_path, &store.snapshot()) {
                eprintln!("failed to persist recipe run history: {error}");
            }
            code
        }
        RecipeCommand::Deeplink {
            target,
            params,
            action,
            store,
            json,
        } => {
            let store_path = cli_recipe_store_path(store.as_deref());
            let store = api_recipes::load_recipe_store(&store_path);
            let recipe = match resolve_cli_recipe_target(&target, &store) {
                Ok(recipe) => recipe,
                Err(error) => {
                    eprintln!("{error}");
                    return 1;
                }
            };
            let params = match parse_recipe_cli_params(&params) {
                Ok(params) => params,
                Err(error) => {
                    eprintln!("{error}");
                    return 1;
                }
            };
            let recipe_id = recipe
                .id
                .clone()
                .unwrap_or_else(|| stable_recipe_id(&recipe));
            let deeplink = match build_recipe_deeplink(&recipe_id, &action, &params) {
                Ok(deeplink) => deeplink,
                Err(error) => {
                    eprintln!("recipe deeplink failed: {error}");
                    return 1;
                }
            };
            if json {
                print_json_value(serde_json::to_value(&deeplink).unwrap_or_default());
            } else {
                println!("{}", deeplink.url);
            }
            0
        }
    }
}

fn cli_recipe_store_path(store: Option<&str>) -> std::path::PathBuf {
    store
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| recipe_store_path(&default_db_path()))
}

fn resolve_cli_recipe_target(target: &str, store: &RecipeStore) -> Result<Recipe, String> {
    let path = std::path::PathBuf::from(target);
    if path.exists() {
        return read_recipe_document(&path);
    }
    store
        .get(target)
        .map(|installed| installed.recipe)
        .ok_or_else(|| {
            format!(
                "recipe target `{target}` is neither an existing JSON/YAML file nor an installed recipe id"
            )
        })
}

fn read_recipe_document(path: &std::path::Path) -> Result<Recipe, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("failed to read recipe file {}: {e}", path.display()))?;
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("json") => serde_json::from_str(&raw)
            .map_err(|e| format!("invalid recipe JSON {}: {e}", path.display())),
        Some("yaml") | Some("yml") => serde_yaml::from_str(&raw)
            .map_err(|e| format!("invalid recipe YAML {}: {e}", path.display())),
        _ => Err("recipe file must be .json, .yaml, or .yml".to_string()),
    }
}

fn parse_recipe_cli_params(
    params: &[String],
) -> Result<std::collections::BTreeMap<String, String>, String> {
    let mut out = std::collections::BTreeMap::new();
    for param in params {
        let Some((key, value)) = param.split_once('=') else {
            return Err(format!("recipe parameter `{param}` must be key=value"));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(format!("recipe parameter `{param}` has an empty key"));
        }
        out.insert(key.to_string(), value.to_string());
    }
    Ok(out)
}

fn cli_recipe_catalog(
    store: &RecipeStore,
    current: Option<&Recipe>,
) -> std::collections::BTreeMap<String, Recipe> {
    let mut catalog = store
        .list()
        .into_iter()
        .map(|installed: InstalledRecipe| (installed.id, installed.recipe))
        .collect::<std::collections::BTreeMap<_, _>>();
    if let Some(recipe) = current {
        let id = stable_recipe_id(recipe);
        catalog.insert(
            id.clone(),
            Recipe {
                id: Some(id),
                ..recipe.clone()
            },
        );
    }
    catalog
}

fn unix_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
