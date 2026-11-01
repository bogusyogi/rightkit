async fn update_schedule_timezone_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ScheduleWorkspaceQuery>,
    Json(req): Json<UpdateScheduleTimeZoneRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "schedules")?;
    let root = schedule_workspace_root(&q);
    let time_zone = crate::scheduler::normalize_time_zone(&req.time_zone)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    let mut schedules = load_api_schedules(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let schedule = schedules.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;
    schedule.time_zone = time_zone;
    let updated_entry = schedule.clone();
    let schedule_json = schedule_entry_json(&updated_entry);
    save_api_schedules(&root, &schedules).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    crate::scheduler::sync_workflow_schedule_compat(
        &state,
        &root,
        &updated_entry.scheduler_entry(),
    );
    record_schedule_admin_audit(
        &root,
        &id,
        "updateTimeZone",
        "persisted",
        schedule_admin_audit_details(
            &updated_entry,
            Some(json!({"timeZone": schedule_json["timeZone"]})),
        ),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "updated": true,
        "scheduleStorePath": schedules_store_path(&root),
        "schedule": schedule_json,
    })))
}

async fn run_schedule_now_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ScheduleWorkspaceQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "schedules")?;
    let root = schedule_workspace_root(&q);
    if state.workflows_feature.is_enabled() && crate::scheduler::workflow_schedule_cutover_enabled()
    {
        let schedules = load_api_schedules(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let entry = schedules.get(&id).ok_or(StatusCode::NOT_FOUND)?;
        let runtime_entry = entry.scheduler_entry();
        crate::scheduler::ensure_workflow_schedule_mapping(&state, &root, &runtime_entry)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let trigger_id = {
            let storage = state
                .storage
                .as_ref()
                .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
            let storage = storage.lock().await;
            let key = format!("legacy-schedule:{}:{}", root.to_string_lossy(), id);
            rightkit_agent_engine_storage::WorkflowRepository::new(storage.conn())
                .find_trigger_by_dedupe_key(&key)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                .map(|trigger| trigger.id)
                .ok_or(StatusCode::CONFLICT)?
        };
        let occurrence =
            crate::api_workflows::routes::admit_manual_occurrence(state.clone(), trigger_id)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        record_schedule_admin_audit(
            &root,
            &id,
            "runNow",
            "workflowSubmitted",
            json!({"manual":true,"workflowOccurrence":occurrence}),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        return Ok(Json(json!({
            "submitted": true,
            "id": id,
            "workflowOccurrence": occurrence,
        })));
    }
    crate::scheduler::fire_schedule_by_id(&state, &root, &id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let runs = load_api_schedule_runs(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    record_schedule_admin_audit(&root, &id, "runNow", "submitted", json!({"manual": true}))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "submitted": true,
        "id": id,
        "runStorePath": schedule_runs_store_path(&root),
        "latestRun": schedule_runs_for(&runs, &id).first().cloned().unwrap_or(serde_json::Value::Null),
    })))
}

async fn pause_schedule_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ScheduleWorkspaceQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "schedules")?;
    update_schedule_paused(&state, id, q, true)
}

async fn resume_schedule_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ScheduleWorkspaceQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "schedules")?;
    update_schedule_paused(&state, id, q, false)
}

fn update_schedule_paused(
    state: &DaemonState,
    id: String,
    q: ScheduleWorkspaceQuery,
    paused: bool,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let root = schedule_workspace_root(&q);
    let mut schedules = load_api_schedules(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let schedule = schedules.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;
    schedule.paused = paused;
    let updated_entry = schedule.clone();
    let schedule_json = serde_json::to_value(&updated_entry).unwrap_or(serde_json::Value::Null);
    save_api_schedules(&root, &schedules).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    crate::scheduler::sync_workflow_schedule_compat(state, &root, &updated_entry.scheduler_entry());
    record_schedule_admin_audit(
        &root,
        &id,
        if paused { "pause" } else { "resume" },
        "persisted",
        schedule_admin_audit_details(&updated_entry, Some(json!({"paused": paused}))),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "updated": true,
        "id": id,
        "paused": paused,
        "scheduleStorePath": schedules_store_path(&root),
        "schedule": schedule_json,
    })))
}

pub(crate) fn schedule_entry_json(entry: &StoredScheduleEntry) -> serde_json::Value {
    serde_json::json!({
        "id": entry.id,
        "cronExpr": entry.cron_expr,
        "prompt": entry.prompt,
        "timeZone": entry.time_zone,
        "skills": entry.skills,
        "workdir": entry.workdir,
        "toolset": entry.toolset,
        "executionBackend": entry.execution_backend,
        "delivery": entry.delivery,
        "scriptAllowlist": entry.script_allowlist,
        "mode": entry.mode,
        "paused": entry.paused,
        "sourceRecipeId": entry.source_recipe_id,
        "recipeParams": entry.recipe_params,
        "maxRuntimeSeconds": entry.max_runtime_seconds,
    })
}

fn schedule_patch_changed_fields(req: &PatchScheduleRequest) -> Vec<&'static str> {
    let mut fields = Vec::new();
    if req.cron_expr.is_some() {
        fields.push("cronExpr");
    }
    if req.prompt.is_some() {
        fields.push("prompt");
    }
    if req.time_zone.is_some() {
        fields.push("timeZone");
    }
    if req.skills.is_some() {
        fields.push("skills");
    }
    if req.workdir.is_some() {
        fields.push("workdir");
    }
    if req.toolset.is_some() {
        fields.push("toolset");
    }
    if req.execution_backend.is_some() {
        fields.push("executionBackend");
    }
    if req.delivery.is_some() {
        fields.push("delivery");
    }
    if req.script_allowlist.is_some() {
        fields.push("scriptAllowlist");
    }
    if req.mode.is_some() {
        fields.push("mode");
    }
    if req.paused.is_some() {
        fields.push("paused");
    }
    if req.max_runtime_seconds.is_some() {
        fields.push("maxRuntimeSeconds");
    }
    fields
}

fn schedule_admin_audit_details(
    entry: &StoredScheduleEntry,
    extra: Option<serde_json::Value>,
) -> serde_json::Value {
    let delivery_kinds: Vec<_> = entry
        .delivery
        .iter()
        .map(|target| target.kind.clone())
        .collect();
    let mut details = json!({
        "cronExpr": entry.cron_expr,
        "timeZone": entry.time_zone,
        "mode": entry.mode,
        "paused": entry.paused,
        "toolset": entry.toolset,
        "executionBackend": entry.execution_backend,
        "hasWorkdir": entry.workdir.is_some(),
        "skillCount": entry.skills.len(),
        "deliveryKinds": delivery_kinds,
        "scriptAllowlistCount": entry.script_allowlist.len(),
        "sourceRecipeId": entry.source_recipe_id,
        "maxRuntimeSeconds": entry.max_runtime_seconds,
    });
    if let (Some(base), Some(extra)) = (details.as_object_mut(), extra) {
        if let Some(extra) = extra.as_object() {
            for (key, value) in extra {
                base.insert(key.clone(), value.clone());
            }
        }
    }
    details
}

fn record_schedule_admin_audit(
    root: &FsPath,
    schedule_id: &str,
    action: &str,
    status: &str,
    details: serde_json::Value,
) -> Result<StoredScheduleAdminAuditEvent, String> {
    let mut state = load_schedule_admin_audit(root)?;
    let event = StoredScheduleAdminAuditEvent {
        id: format!(
            "schedule-admin-{}-{}",
            crate::api::now_millis(),
            state.history.len() + 1
        ),
        schedule_id: schedule_id.to_string(),
        action: action.to_string(),
        actor: "local-daemon-api".to_string(),
        source: "daemon-http".to_string(),
        created_at: crate::api::now_iso(),
        status: status.to_string(),
        details,
    };
    state.history.push(event.clone());
    if state.history.len() > MAX_SCHEDULE_ADMIN_AUDIT_EVENTS {
        let excess = state.history.len() - MAX_SCHEDULE_ADMIN_AUDIT_EVENTS;
        state.history.drain(0..excess);
    }
    save_schedule_admin_audit(root, &state)?;
    Ok(event)
}
