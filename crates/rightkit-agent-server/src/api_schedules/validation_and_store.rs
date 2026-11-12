fn validated_schedule_id(value: &str) -> Result<String, StatusCode> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || !trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(trimmed.to_string())
}

fn normalize_schedule_time_zone(value: &str) -> Result<String, StatusCode> {
    crate::scheduler::normalize_time_zone(value).map_err(|_| StatusCode::BAD_REQUEST)
}

fn validated_cron_expr(value: &str, time_zone: &str) -> Result<String, StatusCode> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    crate::scheduler::next_due_epoch_minute_in_time_zone(
        trimmed,
        time_zone,
        crate::scheduler::current_epoch_minute(),
        366 * 24 * 60,
    )
    .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(trimmed.to_string())
}

fn validated_prompt(value: &str) -> Result<String, StatusCode> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        Err(StatusCode::BAD_REQUEST)
    } else {
        Ok(trimmed.to_string())
    }
}

fn clean_string_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn clean_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn validated_optional_workdir(
    value: Option<&str>,
    workspace_root: &FsPath,
) -> Result<Option<String>, StatusCode> {
    let Some(value) = clean_optional_string(value) else {
        return Ok(None);
    };
    let path = PathBuf::from(&value);
    if !path.is_absolute() {
        return Err(StatusCode::BAD_REQUEST);
    }
    if !path_stays_inside(&path, workspace_root) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(Some(value))
}

fn path_stays_inside(path: &FsPath, root: &FsPath) -> bool {
    normalize_lexical(path).starts_with(normalize_lexical(root))
}

fn normalize_lexical(path: &FsPath) -> PathBuf {
    let mut out = PathBuf::new();
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

fn validated_delivery(
    delivery: Vec<StoredDeliveryTarget>,
) -> Result<Vec<StoredDeliveryTarget>, StatusCode> {
    if delivery.iter().any(|target| target.kind.trim().is_empty()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(delivery
        .into_iter()
        .map(|mut target| {
            target.kind = target.kind.trim().to_string();
            target.target = clean_optional_string(target.target.as_deref());
            target
        })
        .collect())
}

fn validated_mode(value: &str) -> Result<String, StatusCode> {
    if value.eq_ignore_ascii_case("agent") {
        Ok("agent".to_string())
    } else if value.eq_ignore_ascii_case("scriptOnly") {
        Ok("scriptOnly".to_string())
    } else {
        Err(StatusCode::BAD_REQUEST)
    }
}

async fn delete_schedule_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ScheduleWorkspaceQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "schedules")?;
    let root = schedule_workspace_root(&q);
    let mut schedules = load_api_schedules(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let removed = schedules.remove(&id).ok_or(StatusCode::NOT_FOUND)?;
    save_api_schedules(&root, &schedules).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    crate::api_clients::supervisor_seed_startup_residency(
        schedules.values().filter(|entry| !entry.paused).count(),
    );
    crate::scheduler::disable_workflow_schedule_compat(&state, &root, &id);
    record_schedule_admin_audit(
        &root,
        &id,
        "delete",
        "persisted",
        schedule_admin_audit_details(&removed, None),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "deleted": true,
        "id": id,
        "scheduleStorePath": schedules_store_path(&root),
    })))
}

fn schedule_workspace_root(q: &ScheduleWorkspaceQuery) -> PathBuf {
    if q.workspace_root.trim().is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(q.workspace_root.trim())
    }
}

pub fn schedules_store_path(root: &FsPath) -> PathBuf {
    root.join(crate::host_identity::home_dir()).join("schedules.json")
}

fn schedule_runs_store_path(root: &FsPath) -> PathBuf {
    root.join(crate::host_identity::home_dir()).join("schedule-runs.json")
}

fn schedule_admin_audit_store_path(root: &FsPath) -> PathBuf {
    root.join(crate::host_identity::home_dir()).join("schedule-admin-audit.json")
}

pub fn load_api_schedules(root: &FsPath) -> Result<HashMap<String, StoredScheduleEntry>, String> {
    let path = schedules_store_path(root);
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read schedules '{}': {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("invalid schedules '{}': {e}", path.display()))
}

pub fn save_api_schedules(
    root: &FsPath,
    schedules: &HashMap<String, StoredScheduleEntry>,
) -> Result<(), String> {
    let path = schedules_store_path(root);
    let text = serde_json::to_string_pretty(schedules)
        .map_err(|e| format!("failed to serialize schedules: {e}"))?;
    rightkit_fs::atomic_write_with(
        &path,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..rightkit_fs::AtomicOptions::default()
        },
        |f| std::io::Write::write_all(f, text.as_bytes()),
    )
    .map_err(|e| format!("failed to write '{}': {e}", path.display()))
}

pub fn load_api_schedule_runs(root: &FsPath) -> Result<StoredScheduleRunState, String> {
    let path = schedule_runs_store_path(root);
    if !path.exists() {
        return Ok(StoredScheduleRunState::default());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("failed to read schedule runs '{}': {e}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|e| format!("invalid schedule runs '{}': {e}", path.display()))
}

pub fn save_api_schedule_runs(root: &FsPath, state: &StoredScheduleRunState) -> Result<(), String> {
    let path = schedule_runs_store_path(root);
    let text = serde_json::to_string_pretty(state)
        .map_err(|e| format!("failed to serialize schedule runs: {e}"))?;
    rightkit_fs::atomic_write_with(
        &path,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..rightkit_fs::AtomicOptions::default()
        },
        |f| std::io::Write::write_all(f, text.as_bytes()),
    )
    .map_err(|e| format!("failed to write '{}': {e}", path.display()))
}

pub fn load_schedule_admin_audit(root: &FsPath) -> Result<StoredScheduleAdminAuditState, String> {
    let path = schedule_admin_audit_store_path(root);
    if !path.exists() {
        return Ok(StoredScheduleAdminAuditState::default());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "failed to read schedule admin audit '{}': {e}",
            path.display()
        )
    })?;
    serde_json::from_str(&text)
        .map_err(|e| format!("invalid schedule admin audit '{}': {e}", path.display()))
}

pub fn save_schedule_admin_audit(
    root: &FsPath,
    state: &StoredScheduleAdminAuditState,
) -> Result<(), String> {
    let path = schedule_admin_audit_store_path(root);
    let text = serde_json::to_string_pretty(state)
        .map_err(|e| format!("failed to serialize schedule admin audit: {e}"))?;
    rightkit_fs::atomic_write_with(
        &path,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..rightkit_fs::AtomicOptions::default()
        },
        |f| std::io::Write::write_all(f, text.as_bytes()),
    )
    .map_err(|e| format!("failed to write '{}': {e}", path.display()))
}
