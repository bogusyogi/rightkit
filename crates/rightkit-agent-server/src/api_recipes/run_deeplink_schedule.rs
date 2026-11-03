async fn render_recipe_handler(
    State(state): State<DaemonState>,
    Json(body): Json<RenderRecipeRequest>,
) -> Result<Json<RenderedRecipe>, RecipeApiError> {
    let recipe = resolve_recipe(&state, body.recipe_id.as_deref(), body.recipe)?;
    let rendered = render_recipe(&recipe, &body.params)
        .map_err(|e| RecipeApiError::bad_request(serde_json::json!({ "error": e.message })))?;
    Ok(Json(rendered))
}

async fn run_recipe_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RunRecipeRequest>,
) -> Result<Json<RecipeRun>, RecipeApiError> {
    enforce_recipe_trusted_source(&headers)?;
    let installed = state
        .recipes
        .lock()
        .unwrap()
        .get(&id)
        .ok_or_else(|| RecipeApiError::not_found("recipe not found"))?;
    let rendered = render_recipe(&installed.recipe, &body.params)
        .map_err(|e| RecipeApiError::bad_request(serde_json::json!({ "error": e.message })))?;
    let workspace_root = body
        .workspace_root
        .or_else(|| rendered.settings.workspace.clone())
        .unwrap_or_default();
    let workspace_config_root = recipe_workspace_config_root(&workspace_root)?;
    let effective_toolset =
        recipe_effective_toolset(&installed.recipe, &rendered, None, &workspace_config_root)?;
    let execution_backend =
        clean_optional_recipe_schedule_string(rendered.settings.execution_backend.as_deref());
    let session_id = format!("recipe-run-{}", now_millis());
    let live = state
        .create_recipe_session_with_toolset_policy(
            &session_id,
            &workspace_root,
            &format!("Recipe: {}", rendered.title),
            effective_toolset.as_deref(),
            clean_recipe_skills(&installed.recipe.skills),
            execution_backend,
        )
        .map_err(|_| {
            RecipeApiError::bad_request(serde_json::json!({
                "error": "failed to create recipe session with requested toolset or execution backend",
                "toolset": effective_toolset,
            }))
        })?;
    let created_at = now_iso();
    let trust = trust_envelope(
        ContentTrust::WorkspaceTrusted,
        "recipe",
        &rendered.rendered_prompt,
    );
    live.transcript
        .lock()
        .unwrap()
        .push(TranscriptMessage::User {
            id: format!("recipe-user-{}", now_millis()),
            created_at: created_at.clone(),
            content: rendered.rendered_prompt.clone(),
            trust: Some(trust.clone()),
            pinned: false,
            attachments: Vec::new(),
        });
    let seq = live.conduit.next_seq();
    live.conduit.publish(ProtocolEvent::UserMessageSubmitted {
        session_id: session_id.clone(),
        seq,
        created_at: created_at.clone(),
        text: rendered.rendered_prompt.clone(),
        nonce: Some(format!("recipe:{}", installed.id)),
        trust: Some(trust),
        attachments: Vec::new(),
    });

    let run_id = format!("rr-{}", now_millis());
    let evidence_artifact = recipe_run_artifact(&installed, &rendered, &run_id, &created_at);
    let result_artifact_id = Some(evidence_artifact.id.clone());
    push_session_artifact(&live, evidence_artifact);

    let (status, completed_at) = if live.msg_tx.is_some() {
        // HCTX-002 (sibling): if the prompt never reaches the engine the run is
        // not Running - reporting Running for a dropped prompt would claim work
        // that will never happen.
        match live
            .try_admit_route_bound_text(rendered.rendered_prompt.clone())
            .await
        {
            Ok(()) => (RecipeRunStatus::Running, None),
            Err(error) => {
                tracing::warn!(
                    run_id = %run_id,
                    error = %error,
                    "recipe prompt was not admitted to the session turn queue"
                );
                (RecipeRunStatus::Failed, Some(now_iso()))
            }
        }
    } else {
        (RecipeRunStatus::Completed, Some(now_iso()))
    };

    let run = RecipeRun {
        id: run_id,
        recipe_id: installed.id.clone(),
        session_id,
        status,
        params: rendered.params,
        rendered_prompt_hash: rendered.rendered_prompt_hash,
        result_artifact_id,
        created_at,
        completed_at,
    };
    let snapshot = {
        let mut store = lock_recover(&state.recipes, "state.recipes");
        store.add_run(run.clone());
        store.snapshot()
    };
    persist_recipe_store_snapshot(&state, &snapshot).map_err(|e| {
        RecipeApiError::server_error(serde_json::json!({
            "error": "failed to persist recipe run",
            "detail": e,
        }))
    })?;
    Ok(Json(run))
}

fn recipe_run_artifact(
    installed: &InstalledRecipe,
    rendered: &RenderedRecipe,
    run_id: &str,
    created_at: &str,
) -> ArtifactRow {
    ArtifactRow {
        id: format!("recipe-artifact-{run_id}"),
        path: format!("recipes/{}/runs/{run_id}/rendered-prompt.md", installed.id),
        kind: "recipe_prompt".to_string(),
        content_hash: rendered.rendered_prompt_hash.clone(),
        shippable: false,
        trust_label: "unreviewed".to_string(),
        producer: "recipe-run".to_string(),
        provenance: format!(
            "Rendered prompt for recipe `{}` run `{run_id}`.",
            installed.id
        ),
        verdict: None,
        approvals: Vec::new(),
        meta: serde_json::json!({
            "recipeId": installed.id.clone(),
            "recipeTitle": rendered.title.clone(),
            "runId": run_id,
            "renderedPromptHash": rendered.rendered_prompt_hash.clone(),
            "paramKeys": rendered.params.keys().cloned().collect::<Vec<_>>(),
            "source": "recipe-run"
        })
        .to_string(),
        created_at: created_at.to_string(),
    }
}

async fn list_recipe_runs_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Json<Vec<RecipeRun>> {
    Json(lock_recover(&state.recipes, "state.recipes").runs(&id))
}

async fn create_recipe_deeplink_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    Json(body): Json<CreateRecipeDeeplinkRequest>,
) -> Result<Json<RecipeDeeplink>, RecipeApiError> {
    let installed = state
        .recipes
        .lock()
        .unwrap()
        .get(&id)
        .ok_or_else(|| RecipeApiError::not_found("recipe not found"))?;
    render_recipe(&installed.recipe, &body.params)
        .map_err(|e| RecipeApiError::bad_request(serde_json::json!({ "error": e.message })))?;
    let link = build_recipe_deeplink(
        &installed.id,
        body.action.as_deref().unwrap_or("run"),
        &body.params,
    )
    .map_err(|e| RecipeApiError::bad_request(serde_json::json!({ "error": e.message })))?;
    Ok(Json(link))
}

async fn parse_recipe_deeplink_handler(
    State(state): State<DaemonState>,
    Json(body): Json<ParseRecipeDeeplinkRequest>,
) -> Result<Json<RecipeDeeplink>, RecipeApiError> {
    let link = parse_recipe_deeplink(&body.url)
        .map_err(|e| RecipeApiError::bad_request(serde_json::json!({ "error": e.message })))?;
    let installed = state
        .recipes
        .lock()
        .unwrap()
        .get(&link.recipe_id)
        .ok_or_else(|| RecipeApiError::not_found("recipe not found"))?;
    render_recipe(&installed.recipe, &link.params)
        .map_err(|e| RecipeApiError::bad_request(serde_json::json!({ "error": e.message })))?;
    Ok(Json(link))
}

async fn schedule_recipe_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ScheduleRecipeRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), RecipeApiError> {
    enforce_recipe_trusted_source(&headers)?;
    let installed = state
        .recipes
        .lock()
        .unwrap()
        .get(&id)
        .ok_or_else(|| RecipeApiError::not_found("recipe not found"))?;
    let rendered = render_recipe(&installed.recipe, &body.params)
        .map_err(|e| RecipeApiError::bad_request(serde_json::json!({ "error": e.message })))?;
    let root = schedule_recipe_workspace_root(&body, &rendered)?;
    let time_zone =
        crate::scheduler::normalize_time_zone(body.time_zone.as_deref().unwrap_or("UTC"))
            .map_err(|e| RecipeApiError::bad_request(serde_json::json!({ "error": e })))?;
    let cron_expr = validated_recipe_schedule_cron(&body.cron_expr, &time_zone)?;
    let schedule_id = body
        .id
        .as_deref()
        .map(validated_recipe_schedule_id)
        .transpose()?
        .unwrap_or_else(|| {
            format!(
                "recipe-{}-{}",
                sanitize_recipe_schedule_id(&id),
                now_millis()
            )
        });
    let mut schedules = crate::api_schedules::load_api_schedules(&root)
        .map_err(|e| RecipeApiError::server_error(serde_json::json!({ "error": e })))?;
    if schedules.contains_key(&schedule_id) {
        return Err(RecipeApiError {
            status: StatusCode::CONFLICT,
            body: serde_json::json!({ "error": "schedule already exists" }),
        });
    }
    let effective_toolset =
        recipe_effective_toolset(&installed.recipe, &rendered, body.toolset.as_deref(), &root)?;
    let entry = crate::api_schedules::StoredScheduleEntry {
        id: schedule_id.clone(),
        cron_expr,
        prompt: rendered.rendered_prompt.clone(),
        time_zone,
        skills: recipe_schedule_skills(&body, &installed.recipe),
        workdir: validated_recipe_schedule_workdir(body.workdir.as_deref(), &root)?,
        toolset: effective_toolset,
        execution_backend: clean_optional_recipe_schedule_string(body.execution_backend.as_deref())
            .or_else(|| rendered.settings.execution_backend.clone()),
        delivery: validated_recipe_schedule_delivery(body.delivery)?,
        script_allowlist: Vec::new(),
        mode: "agent".to_string(),
        paused: body.paused,
        source_recipe_id: Some(installed.id.clone()),
        recipe_params: rendered.params.clone(),
        max_runtime_seconds: None,
    };
    schedules.insert(schedule_id.clone(), entry.clone());
    crate::api_schedules::save_api_schedules(&root, &schedules)
        .map_err(|e| RecipeApiError::server_error(serde_json::json!({ "error": e })))?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "created": true,
            "id": schedule_id,
            "recipeId": installed.id,
            "workspaceRoot": root,
            "scheduleStorePath": crate::api_schedules::schedules_store_path(&root),
            "schedule": crate::api_schedules::schedule_entry_json(&entry),
        })),
    ))
}
