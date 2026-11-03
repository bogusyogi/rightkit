use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, patch, post, put},
    Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Mutex, OnceLock};

use rightkit_agent_engine_protocol::{
    default_job_mode, ScheduleRunState as StoredScheduleRunState,
};

use crate::api::{require_license_feature, DaemonState};
use crate::api_permissions::enforce_trusted_source_authority;

pub use super::{
    BotProfileV1, BotTriggerBinding, BrowserPolicy, MisfirePolicy, ScheduleKind,
    ScheduleLifecycleState,
};

static TRANSIENT_BOT_RUNS: OnceLock<
    Mutex<HashMap<String, (crate::scheduler::ScheduleEntry, BotProfileV1)>>,
> = OnceLock::new();

pub(crate) fn register_transient_bot_run(
    entry: crate::scheduler::ScheduleEntry,
    profile: BotProfileV1,
) {
    TRANSIENT_BOT_RUNS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("transient bot registry poisoned")
        .insert(entry.id.clone(), (entry, profile));
}

pub(crate) fn take_transient_bot_run(
    id: &str,
) -> Option<(crate::scheduler::ScheduleEntry, BotProfileV1)> {
    TRANSIENT_BOT_RUNS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("transient bot registry poisoned")
        .remove(id)
}

pub fn schedule_routes() -> Router<DaemonState> {
    Router::new()
        .route(
            "/schedules",
            get(list_schedules_handler).post(create_schedule_handler),
        )
        .route("/schedules/audit", get(schedule_admin_audit_handler))
        .route(
            "/schedules/{id}",
            patch(patch_schedule_handler).delete(delete_schedule_handler),
        )
        .route("/schedules/{id}/runs", get(schedule_runs_handler))
        .route("/schedules/{id}/run", post(run_schedule_now_handler))
        .route("/schedules/{id}/event", post(admit_schedule_event_handler))
        .route("/schedules/{id}/pause", post(pause_schedule_handler))
        .route("/schedules/{id}/resume", post(resume_schedule_handler))
        .route(
            "/schedules/{id}/time-zone",
            put(update_schedule_timezone_handler),
        )
        .route("/bots", get(list_bots_handler).post(create_bot_handler))
        .route("/bots/import", post(import_bot_handler))
        .route(
            "/bots/{id}",
            get(show_bot_handler)
                .patch(update_bot_handler)
                .delete(delete_bot_handler),
        )
        .route("/bots/{id}/run", post(run_bot_handler))
        .route("/bots/{id}/export", get(export_bot_handler))
}

async fn admit_schedule_event_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ScheduleWorkspaceQuery>,
    Json(event): Json<Value>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "schedules")?;
    let root = schedule_workspace_root(&q);
    let schedules = load_api_schedules(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let entry = schedules.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    if !entry.cron_expr.starts_with("monitor:") {
        return Err(StatusCode::CONFLICT);
    }
    if entry.paused {
        return Err(StatusCode::CONFLICT);
    }
    let raw_monitor = entry.cron_expr.strip_prefix("monitor:").unwrap_or_default();
    let mut monitor_parts = raw_monitor.splitn(2, ':');
    let topic = monitor_parts.next().unwrap_or_default();
    let filter = monitor_parts.next().unwrap_or_default();
    if event.get("topic").and_then(Value::as_str) != Some(topic) {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    if !filter.is_empty()
        && !event
            .get("payload")
            .map(|payload| payload.to_string().contains(filter))
            .unwrap_or(false)
    {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let event_id = event
        .get("eventId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let raw_record = std::fs::read_to_string(schedules_store_path(&root))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.get(&id).cloned())
        .unwrap_or(Value::Null);
    if raw_record
        .get("lifecycle")
        .and_then(|value| value.get("paused"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || raw_record
            .get("botProfile")
            .and_then(|value| value.get("enabled"))
            .and_then(Value::as_bool)
            .is_some_and(|enabled| !enabled)
    {
        return Err(StatusCode::CONFLICT);
    }
    let state_runs =
        load_api_schedule_runs(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if state_runs.history.iter().any(|run| {
        run.schedule_id == id
            && serde_json::from_str::<Value>(&run.message)
                .ok()
                .and_then(|value| {
                    value
                        .get("monitorEventId")
                        .and_then(Value::as_str)
                        .map(|key| key == event_id)
                })
                .unwrap_or(false)
    }) {
        return Ok(Json(
            json!({"admitted": false, "duplicate": true, "eventId": event_id}),
        ));
    }
    let mut state_runs = state_runs;
    state_runs
        .history
        .push(rightkit_agent_engine_protocol::ScheduleRunEvent {
            schema_version: 1,
            schedule_id: id.clone(),
            epoch_minute: crate::scheduler::current_epoch_minute(),
            status: "event_admitted".into(),
            session_id: format!("monitor-event-{event_id}"),
            message: serde_json::to_string(
                &json!({"monitorEventId": event_id, "topic": topic, "payload": event}),
            )
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            started_at: crate::api::now_iso(),
            finished_at: None,
            mode: "agent".into(),
            toolset: None,
            execution_backend: None,
            resolved_execution_backend: None,
            execution_workspace_id: None,
            execution_workspace_path: None,
            resource_limits: None,
            workdir: None,
            skills: Vec::new(),
            delivery_kinds: Vec::new(),
            source_recipe_id: None,
            exit_code: None,
            artifact_path: None,
            delivery_status: None,
            delivery_errors: Vec::new(),
        });
    save_api_schedule_runs(&root, &state_runs).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    crate::scheduler::fire_schedule_by_id(&state, &root, &id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        json!({"admitted": true, "eventId": event_id, "topic": topic}),
    ))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleWorkspaceQuery {
    #[serde(default)]
    pub workspace_root: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredScheduleEntry {
    pub id: String,
    pub cron_expr: String,
    pub prompt: String,
    #[serde(default = "default_schedule_time_zone")]
    pub time_zone: String,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub toolset: Option<String>,
    #[serde(default)]
    pub execution_backend: Option<String>,
    #[serde(default)]
    pub delivery: Vec<StoredDeliveryTarget>,
    #[serde(default)]
    pub script_allowlist: Vec<String>,
    #[serde(default = "default_job_mode")]
    pub mode: String,
    #[serde(default)]
    pub paused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_recipe_id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub recipe_params: BTreeMap<String, String>,
    /// Maximum runtime in seconds for scriptOnly schedules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runtime_seconds: Option<u64>,
}

impl StoredScheduleEntry {
    pub(super) fn scheduler_entry(&self) -> crate::scheduler::ScheduleEntry {
        crate::scheduler::ScheduleEntry {
            id: self.id.clone(),
            cron_expr: self.cron_expr.clone(),
            prompt: self.prompt.clone(),
            time_zone: self.time_zone.clone(),
            skills: self.skills.clone(),
            workdir: self.workdir.clone(),
            toolset: self.toolset.clone(),
            execution_backend: self.execution_backend.clone(),
            delivery: self
                .delivery
                .iter()
                .map(|target| crate::scheduler::DeliveryTarget {
                    kind: target.kind.clone(),
                    target: target.target.clone(),
                    allow_private_network: target.allow_private_network,
                })
                .collect(),
            script_allowlist: self.script_allowlist.clone(),
            mode: self.mode.clone(),
            paused: self.paused,
            source_recipe_id: self.source_recipe_id.clone(),
            recipe_params: self.recipe_params.clone(),
            max_runtime_seconds: self.max_runtime_seconds,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredDeliveryTarget {
    pub kind: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub allow_private_network: bool,
}

fn default_schedule_time_zone() -> String {
    "UTC".to_string()
}

fn preview_schedule(expr: &str, time_zone: &str, now_minute: i64) -> Result<Option<i64>, String> {
    if let Some(raw) = expr.strip_prefix("once:") {
        return raw
            .parse::<i64>()
            .map(|at| (at > now_minute * 60_000).then_some(at))
            .map_err(|_| "invalid once timestamp".into());
    }
    if let Some(raw) = expr.strip_prefix("interval:") {
        let mut parts = raw.split(':');
        let every = parts
            .next()
            .and_then(|v| v.parse::<i64>().ok())
            .ok_or("invalid interval duration")?;
        if every <= 0 {
            return Err("interval must be positive".into());
        }
        let start = parts
            .next()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(now_minute * 60_000 + every * 1000);
        let now = now_minute * 60_000;
        let next = if start > now {
            start
        } else {
            start + ((now - start) / (every * 1000) + 1) * every * 1000
        };
        return Ok(Some(next));
    }
    if expr.starts_with("monitor:") {
        return Ok(None);
    }
    crate::scheduler::next_due_epoch_minute_in_time_zone(expr, time_zone, now_minute, 366 * 24 * 60)
        .map(|value| value.map(|minute| minute * 60_000))
}

fn bot_definition_id(id: &str) -> String {
    format!("bot:{id}")
}

fn bot_content_hash(profile: &BotProfileV1) -> Result<String, StatusCode> {
    let bytes = serde_json::to_vec(profile).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(format!(
        "sha256:{}",
        rightkit_fs::digest::sha256_bytes(AsRef::<[u8]>::as_ref(&bytes))
    ))
}

async fn read_bot_profile(state: &DaemonState, id: &str) -> Result<BotProfileV1, StatusCode> {
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let store = storage.lock().await;
    let repo = rightkit_agent_engine_storage::WorkflowRepository::new(store.conn());
    let definition = repo
        .find_definition(&bot_definition_id(id))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .filter(|row| row.source_kind == "bot-profile")
        .ok_or(StatusCode::NOT_FOUND)?;
    let version = repo
        .list_versions(&definition.id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .next()
        .ok_or(StatusCode::NOT_FOUND)?;
    serde_json::from_str(&version.definition_json).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn write_bot_profile(
    state: &DaemonState,
    profile: &BotProfileV1,
) -> Result<BotProfileV1, StatusCode> {
    profile.validate().map_err(|_| StatusCode::BAD_REQUEST)?;
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let store = storage.lock().await;
    let repo = rightkit_agent_engine_storage::WorkflowRepository::new(store.conn());
    let writer = rightkit_agent_engine_storage::WorkflowDefinitionWriter::new(store.conn());
    let now = crate::api::now_iso();
    let id = bot_definition_id(&profile.id);
    writer
        .upsert_definition(&rightkit_agent_engine_storage::WorkflowDefinitionRow {
            id: id.clone(),
            name: profile.name.clone(),
            source_kind: "bot-profile".into(),
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let next = repo
        .list_versions(&id)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .map(|version| version.version_number)
        .max()
        .unwrap_or(0)
        + 1;
    // Storage history, API payload, & persisted daemon binding must agree on
    // one revision. Never accept a client-supplied revision on update.
    let mut persisted = profile.clone();
    persisted.version = u32::try_from(next).map_err(|_| StatusCode::CONFLICT)?;
    let definition_json =
        serde_json::to_string(&persisted).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    writer
        .insert_version(&rightkit_agent_engine_storage::WorkflowVersionRow {
            id: format!("{id}:v{next}"),
            workflow_id: id,
            version_number: next,
            content_hash: bot_content_hash(&persisted)?,
            definition_json,
            compiled_plan_json: "{}".into(),
            publication_state: "draft".into(),
            created_at: now,
            published_at: None,
            created_by: "schedule-api".into(),
            notes: "versioned bot profile".into(),
        })
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(persisted)
}

async fn list_bots_handler(State(state): State<DaemonState>) -> Result<Json<Value>, StatusCode> {
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let store = storage.lock().await;
    let rows = rightkit_agent_engine_storage::WorkflowRepository::new(store.conn())
        .list_definitions(500)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .into_iter()
        .filter(|row| row.source_kind == "bot-profile")
        .map(|row| json!({"id": row.id.strip_prefix("bot:").unwrap_or(&row.id), "name": row.name, "updatedAt": row.updated_at}))
        .collect::<Vec<_>>();
    Ok(Json(json!({"bots": rows})))
}

async fn show_bot_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<BotProfileV1>, StatusCode> {
    Ok(Json(read_bot_profile(&state, &id).await?))
}

async fn update_bot_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ScheduleWorkspaceQuery>,
    Json(profile): Json<BotProfileV1>,
) -> Result<Json<BotProfileV1>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "schedules")?;
    if profile.id != id {
        return Err(StatusCode::BAD_REQUEST);
    }
    let previous = read_bot_profile(&state, &id).await?;
    validate_bot_trigger_schedules(&profile)?;
    let persisted = write_bot_profile(&state, &profile).await?;
    sync_bot_trigger_schedules(&state, &bot_profile_root(&persisted, &q), &persisted).await?;
    if bot_profile_root(&previous, &q) != bot_profile_root(&persisted, &q) {
        remove_bot_trigger_schedules(&state, &bot_profile_root(&previous, &q), &id).await?;
    }
    Ok(Json(persisted))
}

fn bot_profile_root(profile: &BotProfileV1, query: &ScheduleWorkspaceQuery) -> PathBuf {
    if query.workspace_root.trim().is_empty() {
        profile
            .default_project
            .as_deref()
            .or(profile.project.as_deref())
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| schedule_workspace_root(query))
    } else {
        schedule_workspace_root(query)
    }
}

fn bot_trigger_schedule_id(profile: &BotProfileV1, trigger: &BotTriggerBinding) -> String {
    let digest = rightkit_crypto::sha256(format!("{}:{}", profile.id, trigger.id).as_bytes());
    format!("bot-{}", &rightkit_crypto::hex_encode(&digest)[..24])
}

fn validate_bot_trigger_schedules(profile: &BotProfileV1) -> Result<(), StatusCode> {
    for trigger in &profile.triggers {
        bot_trigger_spec(trigger)?;
    }
    Ok(())
}

fn bot_trigger_spec(
    trigger: &BotTriggerBinding,
) -> Result<crate::scheduler::ScheduleSpec, StatusCode> {
    let expression = trigger
        .expression
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(StatusCode::BAD_REQUEST)?;
    let expression = match trigger.schedule {
        ScheduleKind::Cron => expression.to_string(),
        ScheduleKind::Once => format!("once:{expression}"),
        ScheduleKind::Interval => format!("interval:{expression}"),
        ScheduleKind::Monitor => format!("monitor:{expression}"),
    };
    let spec = crate::scheduler::ScheduleSpec::from_legacy(&expression, "UTC")
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    spec.validate().map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(spec)
}

async fn remove_bot_trigger_schedules(
    state: &DaemonState,
    root: &FsPath,
    bot_id: &str,
) -> Result<(), StatusCode> {
    let path = schedules_store_path(root);
    let mut raw = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Map<String, Value>>(&text).ok())
        .unwrap_or_default();
    let removed = raw
        .iter()
        .filter(|&(_id, record)| {
            record.pointer("/botProfile/id").and_then(Value::as_str) == Some(bot_id)
        })
        .map(|(id, _record)| id.clone())
        .collect::<Vec<_>>();
    for id in &removed {
        raw.remove(id);
        crate::scheduler::disable_workflow_schedule_compat(state, root, id);
    }
    if !removed.is_empty() {
        let text =
            serde_json::to_string_pretty(&raw).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        rightkit_fs::atomic_write_with(
            &path,
            rightkit_fs::AtomicOptions {
                create_parents: true,
                ..rightkit_fs::AtomicOptions::default()
            },
            |f| std::io::Write::write_all(f, text.as_bytes()),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(())
}

async fn sync_bot_trigger_schedules(
    state: &DaemonState,
    root: &FsPath,
    profile: &BotProfileV1,
) -> Result<(), StatusCode> {
    remove_bot_trigger_schedules(state, root, &profile.id).await?;
    if profile.triggers.is_empty() {
        return Ok(());
    }
    let path = schedules_store_path(root);
    let mut raw = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Map<String, Value>>(&text).ok())
        .unwrap_or_default();
    for trigger in &profile.triggers {
        let spec = bot_trigger_spec(trigger)?;
        let id = bot_trigger_schedule_id(profile, trigger);
        let entry = StoredScheduleEntry {
            id: id.clone(),
            cron_expr: match &spec {
                crate::scheduler::ScheduleSpec::Once { at_unix_ms } => format!("once:{at_unix_ms}"),
                crate::scheduler::ScheduleSpec::Cron { expression, .. } => expression.clone(),
                crate::scheduler::ScheduleSpec::Interval {
                    every_seconds,
                    start_unix_ms,
                } => start_unix_ms
                    .map(|start| format!("interval:{every_seconds}:{start}"))
                    .unwrap_or_else(|| format!("interval:{every_seconds}")),
                crate::scheduler::ScheduleSpec::Monitor { topic, filter } => filter
                    .as_ref()
                    .map(|filter| format!("monitor:{topic}:{filter}"))
                    .unwrap_or_else(|| format!("monitor:{topic}")),
            },
            prompt: if profile.instructions.is_empty() {
                profile.name.clone()
            } else {
                profile.instructions.clone()
            },
            time_zone: "UTC".into(),
            skills: Vec::new(),
            workdir: profile.default_project.clone().or(profile.project.clone()),
            toolset: None,
            execution_backend: Some("local".into()),
            delivery: Vec::new(),
            script_allowlist: Vec::new(),
            mode: "agent".into(),
            paused: !profile.enabled || trigger.paused,
            source_recipe_id: None,
            recipe_params: BTreeMap::new(),
            max_runtime_seconds: profile.budget.max_seconds,
        };
        let lifecycle = ScheduleLifecycleState {
            paused: entry.paused,
            overlap: profile.concurrency.clone(),
            misfire: match trigger.misfire {
                MisfirePolicy::Skip => "skip",
                MisfirePolicy::Coalesce => "coalesce",
                MisfirePolicy::CatchUp => "catch_up",
            }
            .into(),
            idempotency_key: Some(format!("bot:{}:trigger:{}", profile.id, trigger.id)),
            retry_limit: trigger.retry_limit,
        };
        let mut record =
            serde_json::to_value(&entry).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let object = record
            .as_object_mut()
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
        object.insert(
            "botProfile".into(),
            serde_json::to_value(profile).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        );
        object.insert(
            "trigger".into(),
            serde_json::to_value(spec).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        );
        object.insert(
            "lifecycle".into(),
            serde_json::to_value(lifecycle).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        );
        raw.insert(id, record);
    }
    let text = serde_json::to_string_pretty(&raw).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    rightkit_fs::atomic_write_with(
        &path,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..rightkit_fs::AtomicOptions::default()
        },
        |f| std::io::Write::write_all(f, text.as_bytes()),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(())
}

async fn create_bot_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Query(q): Query<ScheduleWorkspaceQuery>,
    Json(profile): Json<BotProfileV1>,
) -> Result<(StatusCode, Json<BotProfileV1>), StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "schedules")?;
    if read_bot_profile(&state, &profile.id).await.is_ok() {
        return Err(StatusCode::CONFLICT);
    }
    validate_bot_trigger_schedules(&profile)?;
    let persisted = write_bot_profile(&state, &profile).await?;
    sync_bot_trigger_schedules(&state, &bot_profile_root(&persisted, &q), &persisted).await?;
    Ok((StatusCode::CREATED, Json(persisted)))
}

async fn delete_bot_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ScheduleWorkspaceQuery>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "schedules")?;
    let profile = read_bot_profile(&state, &id).await?;
    remove_bot_trigger_schedules(&state, &bot_profile_root(&profile, &q), &id).await?;
    let storage = state
        .storage
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let store = storage.lock().await;
    let definition_id = bot_definition_id(&id);
    store
        .conn()
        .execute(
            "DELETE FROM workflow_versions WHERE workflow_id = ?1",
            [&definition_id],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let deleted = store
        .conn()
        .execute(
            "DELETE FROM workflow_definitions WHERE id = ?1 AND source_kind = 'bot-profile'",
            [&definition_id],
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if deleted != 1 {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(json!({"deleted": true, "id": id})))
}

async fn import_bot_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<(StatusCode, Json<BotProfileV1>), StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "schedules")?;
    let package =
        rightkit_workflow::control_plane::import_export::import_authority_free_bot_package(&body)
            .map_err(|_| StatusCode::BAD_REQUEST)?;
    let profile = BotProfileV1 {
        schema_version: 1,
        version: 1,
        id: package.id,
        name: package.display_name,
        role_pack: "imported-disabled".into(),
        model: "catalog/default".into(),
        instructions: package.instructions,
        task_templates: package.task_templates,
        tools: package.capability_labels,
        browser: BrowserPolicy {
            mode: "disabled".into(),
            ..BrowserPolicy::default()
        },
        concurrency: "queue-one".into(),
        concurrency_cap: 1,
        enabled: false,
        ..BotProfileV1::default()
    };
    let persisted = write_bot_profile(&state, &profile).await?;
    Ok((StatusCode::CREATED, Json(persisted)))
}

async fn export_bot_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let profile = read_bot_profile(&state, &id).await?;
    let package = rightkit_workflow::control_plane::import_export::AuthorityFreeBotPackage {
        schema_version: 1,
        id: profile.id,
        display_name: profile.name,
        instructions: profile.instructions,
        task_templates: profile.task_templates,
        capability_labels: profile.tools,
        schedules: Vec::new(),
        provenance: "coderight:authority-free-export".into(),
        content_hash: String::new(),
    };
    let bytes = rightkit_workflow::control_plane::import_export::export_authority_free_bot_package(
        &package,
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    serde_json::from_slice(&bytes)
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn run_bot_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<ScheduleWorkspaceQuery>,
) -> Result<Json<Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "schedules")?;
    let profile = read_bot_profile(&state, &id).await?;
    if !profile.enabled {
        return Err(StatusCode::CONFLICT);
    }
    let schedule_id = format!("bot-run-{id}-{}", rightkit_agent_engine_protocol::uuid_v7());
    let stored = StoredScheduleEntry {
        id: schedule_id.clone(),
        cron_expr: profile
            .triggers
            .first()
            .and_then(|binding| binding.expression.clone())
            .unwrap_or_else(|| "* * * * *".into()),
        prompt: if profile.instructions.is_empty() {
            profile.name.clone()
        } else {
            profile.instructions.clone()
        },
        time_zone: "UTC".into(),
        skills: Vec::new(),
        workdir: None,
        toolset: None,
        execution_backend: Some("local".into()),
        delivery: Vec::new(),
        script_allowlist: Vec::new(),
        mode: "agent".into(),
        paused: false,
        source_recipe_id: None,
        recipe_params: BTreeMap::new(),
        max_runtime_seconds: profile.budget.max_seconds,
    };
    let root = schedule_workspace_root(&q);
    crate::api_schedules::register_transient_bot_run(stored.scheduler_entry(), profile.clone());
    crate::scheduler::fire_schedule_by_id(&state, &root, &schedule_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        json!({"submitted": true, "botId": id, "scheduleId": schedule_id}),
    ))
}

const MAX_SCHEDULE_ADMIN_AUDIT_EVENTS: usize = 500;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredScheduleAdminAuditState {
    #[serde(default)]
    pub history: Vec<StoredScheduleAdminAuditEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredScheduleAdminAuditEvent {
    pub id: String,
    pub schedule_id: String,
    pub action: String,
    pub actor: String,
    pub source: String,
    pub created_at: String,
    pub status: String,
    #[serde(default)]
    pub details: serde_json::Value,
}

async fn list_schedules_handler(
    Query(q): Query<ScheduleWorkspaceQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let root = schedule_workspace_root(&q);
    let schedules = load_api_schedules(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let run_state = load_api_schedule_runs(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let now = crate::scheduler::current_epoch_minute();
    let mut rows: Vec<_> = schedules
        .values()
        .map(|entry| {
            let runs = schedule_runs_for(&run_state, &entry.id);
            let latest_run = runs.first().cloned().unwrap_or(serde_json::Value::Null);
            let (next_due, next_due_error) =
                match preview_schedule(&entry.cron_expr, &entry.time_zone, now) {
                    Ok(value) => (value, serde_json::Value::Null),
                    Err(e) => (None, serde_json::json!(e)),
                };
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
                "nextDueEpochMinute": next_due,
                "nextDueError": next_due_error,
                "lastClaimedMinute": run_state.last_claimed_minute.get(&entry.id).copied(),
                "latestRun": latest_run,
                "runs": runs,
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        a["id"]
            .as_str()
            .unwrap_or_default()
            .cmp(b["id"].as_str().unwrap_or_default())
    });
    Ok(Json(serde_json::json!({
        "workspaceRoot": root,
        "scheduleStorePath": schedules_store_path(&root),
        "runStorePath": schedule_runs_store_path(&root),
        "adminAuditStorePath": schedule_admin_audit_store_path(&root),
        "schedules": rows,
    })))
}

pub fn schedule_runs_for(
    state: &StoredScheduleRunState,
    schedule_id: &str,
) -> Vec<serde_json::Value> {
    let mut runs: Vec<_> = state
        .history
        .iter()
        .rev()
        .filter(|event| event.schedule_id == schedule_id)
        .cloned()
        .collect();
    runs.sort_by_key(|run| std::cmp::Reverse(run.epoch_minute));
    runs.into_iter()
        .take(20)
        .map(|event| serde_json::to_value(event).unwrap_or(serde_json::Value::Null))
        .collect()
}

async fn schedule_runs_handler(
    Path(id): Path<String>,
    Query(q): Query<ScheduleWorkspaceQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let root = schedule_workspace_root(&q);
    let schedules = load_api_schedules(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let run_state = load_api_schedule_runs(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let runs = schedule_runs_for(&run_state, &id);
    // Manual bot runs have durable run receipts but no schedule definition.
    if !schedules.contains_key(&id) && runs.is_empty() {
        return Err(StatusCode::NOT_FOUND);
    }
    Ok(Json(serde_json::json!({
        "id": id,
        "workspaceRoot": root,
        "runStorePath": schedule_runs_store_path(&root),
        "latestRun": runs.first().cloned().unwrap_or(serde_json::Value::Null),
        "runs": runs,
    })))
}

async fn schedule_admin_audit_handler(
    Query(q): Query<ScheduleWorkspaceQuery>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let root = schedule_workspace_root(&q);
    let audit = load_schedule_admin_audit(&root).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut history = audit.history;
    history.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.id.cmp(&a.id))
    });
    Ok(Json(serde_json::json!({
        "workspaceRoot": root,
        "auditStorePath": schedule_admin_audit_store_path(&root),
        "history": history,
    })))
}
