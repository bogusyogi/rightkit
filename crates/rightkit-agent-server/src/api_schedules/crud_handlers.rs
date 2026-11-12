#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateScheduleTimeZoneRequest {
    time_zone: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateScheduleRequest {
    #[serde(default)]
    id: Option<String>,
    cron_expr: String,
    prompt: String,
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
    delivery: Vec<StoredDeliveryTarget>,
    #[serde(default)]
    script_allowlist: Vec<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    max_runtime_seconds: Option<u64>,
    #[serde(default)]
    trigger: Option<serde_json::Value>,
    #[serde(default)]
    lifecycle: Option<ScheduleLifecycleState>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PatchScheduleRequest {
    #[serde(default)]
    cron_expr: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    time_zone: Option<String>,
    #[serde(default)]
    skills: Option<Vec<String>>,
    #[serde(default)]
    workdir: Option<Option<String>>,
    #[serde(default)]
    toolset: Option<Option<String>>,
    #[serde(default)]
    execution_backend: Option<Option<String>>,
    #[serde(default)]
    delivery: Option<Vec<StoredDeliveryTarget>>,
    #[serde(default)]
    script_allowlist: Option<Vec<String>>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    paused: Option<bool>,
    #[serde(default)]
    max_runtime_seconds: Option<Option<u64>>,
}

async fn create_schedule_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(q): Query<ScheduleWorkspaceQuery>,
    Json(req): Json<CreateScheduleRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "schedules")?;
    let root = schedule_workspace_root(&q);
    let mut schedules = load_api_schedules(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let id = req
        .id
        .as_deref()
        .map(validated_schedule_id)
        .transpose()?
        .unwrap_or_else(|| format!("schedule-{}", crate::api::now_millis()));
    if schedules.contains_key(&id) {
        return Err(StatusCode::CONFLICT);
    }
    let time_zone = normalize_schedule_time_zone(req.time_zone.as_deref().unwrap_or("UTC"))?;
    let cron_expr = if let Some(trigger) = &req.trigger {
        let kind = trigger
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        match kind {
            "once" => format!(
                "once:{}",
                trigger
                    .get("atUnixMs")
                    .and_then(serde_json::Value::as_i64)
                    .ok_or(StatusCode::BAD_REQUEST)?
            ),
            "interval" => format!(
                "interval:{}:{}",
                trigger
                    .get("everySeconds")
                    .and_then(serde_json::Value::as_u64)
                    .filter(|v| *v > 0)
                    .ok_or(StatusCode::BAD_REQUEST)?,
                trigger
                    .get("startUnixMs")
                    .and_then(serde_json::Value::as_i64)
                    .map(|v| v.to_string())
                    .unwrap_or_default()
            ),
            "monitor" => format!(
                "monitor:{}:{}",
                trigger
                    .get("topic")
                    .and_then(serde_json::Value::as_str)
                    .filter(|v| !v.trim().is_empty())
                    .ok_or(StatusCode::BAD_REQUEST)?,
                trigger
                    .get("filter")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
            ),
            "cron" => validated_cron_expr(
                trigger
                    .get("expression")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&req.cron_expr),
                trigger
                    .get("timeZone")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&time_zone),
            )?,
            _ => return Err(StatusCode::BAD_REQUEST),
        }
    } else {
        validated_cron_expr(&req.cron_expr, &time_zone)?
    };
    let entry = StoredScheduleEntry {
        id: id.clone(),
        cron_expr,
        prompt: validated_prompt(&req.prompt)?,
        time_zone,
        skills: clean_string_list(req.skills),
        workdir: validated_optional_workdir(req.workdir.as_deref(), &root)?,
        toolset: clean_optional_string(req.toolset.as_deref()),
        execution_backend: clean_optional_string(req.execution_backend.as_deref()),
        delivery: validated_delivery(req.delivery)?,
        script_allowlist: clean_string_list(req.script_allowlist),
        mode: validated_mode(req.mode.as_deref().unwrap_or("agent"))?,
        paused: req.paused,
        source_recipe_id: None,
        recipe_params: BTreeMap::new(),
        max_runtime_seconds: req.max_runtime_seconds.filter(|v| *v > 0),
    };
    schedules.insert(id.clone(), entry.clone());
    save_api_schedules(&root, &schedules).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // DL-009: publish durable scheduled-work count into the singleton
    // supervisor immediately, so idle refresh protects newly-created work.
    crate::api_clients::supervisor_seed_startup_residency(
        schedules.values().filter(|entry| !entry.paused).count(),
    );
    if req.trigger.is_some() || req.lifecycle.is_some() {
        let mut raw =
            serde_json::to_value(&schedules).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if let Some(record) = raw.get_mut(&id).and_then(serde_json::Value::as_object_mut) {
            if let Some(trigger) = req.trigger {
                record.insert("trigger".into(), trigger);
            }
            if let Some(lifecycle) = req.lifecycle {
                record.insert(
                    "lifecycle".into(),
                    serde_json::to_value(lifecycle)
                        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
                );
            }
        }
        let text =
            serde_json::to_string_pretty(&raw).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        rightkit_fs::atomic_write_with(
            &root
                .join(crate::host_identity::home_dir())
                .join("schedules.json"),
            rightkit_fs::AtomicOptions {
                create_parents: true,
                ..rightkit_fs::AtomicOptions::default()
            },
            |f| std::io::Write::write_all(f, text.as_bytes()),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    let runtime_entry = entry.scheduler_entry();
    crate::scheduler::ensure_workflow_schedule_mapping(&state, &root, &runtime_entry)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    crate::scheduler::sync_workflow_schedule_compat(&state, &root, &runtime_entry);
    record_schedule_admin_audit(
        &root,
        &id,
        "create",
        "persisted",
        schedule_admin_audit_details(&entry, None),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({
            "created": true,
            "id": id,
            "scheduleStorePath": schedules_store_path(&root),
            "schedule": schedule_entry_json(&entry),
        })),
    ))
}

async fn patch_schedule_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ScheduleWorkspaceQuery>,
    Json(req): Json<PatchScheduleRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "schedules")?;
    let root = schedule_workspace_root(&q);
    let changed_fields = schedule_patch_changed_fields(&req);
    let mut schedules = load_api_schedules(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let schedule = schedules.get_mut(&id).ok_or(StatusCode::NOT_FOUND)?;
    let next_time_zone = req
        .time_zone
        .as_deref()
        .map(normalize_schedule_time_zone)
        .transpose()?
        .unwrap_or_else(|| schedule.time_zone.clone());
    let next_cron = req
        .cron_expr
        .as_deref()
        .map(|cron| validated_cron_expr(cron, &next_time_zone))
        .transpose()?
        .unwrap_or_else(|| schedule.cron_expr.clone());
    schedule.cron_expr = next_cron;
    schedule.time_zone = next_time_zone;
    if let Some(prompt) = req.prompt {
        schedule.prompt = validated_prompt(&prompt)?;
    }
    if let Some(skills) = req.skills {
        schedule.skills = clean_string_list(skills);
    }
    if let Some(workdir) = req.workdir {
        schedule.workdir = validated_optional_workdir(workdir.as_deref(), &root)?;
    }
    if let Some(toolset) = req.toolset {
        schedule.toolset = clean_optional_string(toolset.as_deref());
    }
    if let Some(execution_backend) = req.execution_backend {
        schedule.execution_backend = clean_optional_string(execution_backend.as_deref());
    }
    if let Some(delivery) = req.delivery {
        schedule.delivery = validated_delivery(delivery)?;
    }
    if let Some(script_allowlist) = req.script_allowlist {
        schedule.script_allowlist = clean_string_list(script_allowlist);
    }
    if let Some(mode) = req.mode {
        schedule.mode = validated_mode(&mode)?;
    }
    if let Some(paused) = req.paused {
        schedule.paused = paused;
    }
    if let Some(max_runtime) = req.max_runtime_seconds {
        schedule.max_runtime_seconds = max_runtime.filter(|v| *v > 0);
    }
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
        "patch",
        "persisted",
        schedule_admin_audit_details(
            &updated_entry,
            Some(json!({
                "changedFields": changed_fields,
            })),
        ),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "updated": true,
        "id": id,
        "scheduleStorePath": schedules_store_path(&root),
        "schedule": schedule_json,
    })))
}
