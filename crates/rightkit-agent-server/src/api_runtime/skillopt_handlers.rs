#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessRadarScheduleBody {
    #[serde(default)]
    pub workspace_root: String,
    #[serde(default)]
    pub cron_expr: Option<String>,
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(default)]
    pub paused: bool,
}

async fn get_learning(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    Json(serde_json::json!(state.learning.proposals_json()))
}

async fn list_skillopt_runs(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    let store = lock_recover(&state.skillopt_runs, "state.skillopt_runs");
    Json(serde_json::json!({
        "storePath": state.skillopt_store_path.as_ref().map(|path| path.display().to_string()),
        "runs": store.runs(),
    }))
}

async fn create_skillopt_run(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<CreateSkillOptRunBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "evolution")?;
    if body.name.trim().is_empty()
        || body.target.skill_id.trim().is_empty()
        || body.target.objective.trim().is_empty()
        || body.target.optimizer.trim().is_empty()
    {
        return Err(StatusCode::BAD_REQUEST);
    }

    let run = {
        let mut store = lock_recover(&state.skillopt_runs, "state.skillopt_runs");
        let run = store.add_run(
            format!("skillopt-{}", now_millis()),
            body.name.trim().to_string(),
            body.target,
            body.rejected_edits,
            body.gates,
            now_iso(),
        );
        persist_skillopt_store(&state, &store)?;
        run
    };
    Ok(Json(serde_json::to_value(run).unwrap()))
}

async fn get_skillopt_run(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let store = lock_recover(&state.skillopt_runs, "state.skillopt_runs");
    let run = store.run(&id).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({
        "storePath": state.skillopt_store_path.as_ref().map(|path| path.display().to_string()),
        "run": run,
    })))
}

async fn resume_skillopt_run(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ResumeSkillOptRunBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "evolution")?;
    let message = body
        .message
        .as_deref()
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .unwrap_or("operator resumed training run")
        .chars()
        .take(500)
        .collect::<String>();
    let run = {
        let mut store = lock_recover(&state.skillopt_runs, "state.skillopt_runs");
        let run = store
            .resume_run(&id, body.checkpoint, message, now_iso())
            .ok_or(StatusCode::NOT_FOUND)?;
        persist_skillopt_store(&state, &store)?;
        run
    };
    Ok(Json(serde_json::json!({
        "resumed": true,
        "run": run,
    })))
}

async fn execute_skillopt_epoch(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<rightkit_adapt::SkillOptimizationEpochInput>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "evolution")?;
    if body.proposed_body.trim().is_empty() || !body.validation_score.is_finite() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let result = {
        let mut store = lock_recover(&state.skillopt_runs, "state.skillopt_runs");
        let result = store
            .execute_epoch(&id, body, now_iso())
            .ok_or(StatusCode::NOT_FOUND)?;
        persist_skillopt_store(&state, &store)?;
        result
    };
    Ok(Json(serde_json::json!({
        "executed": true,
        "accepted": result.accepted,
        "candidate": result.candidate,
        "run": result.run,
    })))
}

async fn propose_skillopt_candidate(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<rightkit_adapt::SkillOptimizationProposalInput>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "evolution")?;
    let result = {
        let mut store = lock_recover(&state.skillopt_runs, "state.skillopt_runs");
        let result = store
            .propose_candidate(&id, body, now_iso())
            .ok_or(StatusCode::NOT_FOUND)?;
        persist_skillopt_store(&state, &store)?;
        result
    };
    Ok(Json(serde_json::json!(result)))
}

async fn cancel_skillopt_run(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<CancelSkillOptRunBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "evolution")?;
    let reason = body
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .unwrap_or("operator cancelled training run")
        .chars()
        .take(500)
        .collect::<String>();
    let run = {
        let mut store = lock_recover(&state.skillopt_runs, "state.skillopt_runs");
        let run = store
            .cancel_run(&id, reason, now_iso())
            .ok_or(StatusCode::NOT_FOUND)?;
        persist_skillopt_store(&state, &store)?;
        run
    };
    Ok(Json(serde_json::json!({
        "cancelled": true,
        "run": run,
    })))
}

async fn list_skillopt_run_history(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let store = lock_recover(&state.skillopt_runs, "state.skillopt_runs");
    let run = store.run(&id).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({
        "runId": id,
        "history": &run.history,
    })))
}

async fn list_skillopt_rejected_edits(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let store = lock_recover(&state.skillopt_runs, "state.skillopt_runs");
    let run = store.run(&id).ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({
        "runId": id,
        "rejectedEdits": &run.rejected_edits,
    })))
}

/// A missing file is an empty store; a file that does not parse is set aside (`<name>.damaged-<time>`) before the empty
/// store is returned, so the next save cannot overwrite the only copy.
pub(crate) fn load_skillopt_store(path: &FsPath) -> rightkit_adapt::SkillOptimizationStore {
    crate::store_write::load_or_set_aside::<rightkit_adapt::SkillOptimizationStore>(path)
        .unwrap_or_else(rightkit_adapt::SkillOptimizationStore::new)
}

fn persist_skillopt_store(
    state: &DaemonState,
    store: &rightkit_adapt::SkillOptimizationStore,
) -> Result<(), StatusCode> {
    let Some(path) = state.skillopt_store_path.as_deref() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    let payload =
        serde_json::to_string_pretty(store).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    crate::store_write::write_store_file(path, payload.as_bytes())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
