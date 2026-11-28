use crate::api::DaemonState;
use std::collections::HashSet;
use std::path::PathBuf;
use tracing::warn;

use super::execution::{fire_schedule, sanitize_id, schedule_run_event};
use super::model::{ScheduleEntry, ScheduleSpec};
use super::persistence::{
    append_run_event, claim_due_schedules, load_runtime_metadata, load_schedules,
    reconcile_orphaned_runs, replay_causal_receipts,
};
use super::time::{current_epoch_minute, next_due_epoch_minute_in_time_zone};

/// Idempotently materialize one legacy schedule in workflow storage.
/// Legacy JSON remains CRUD authority; workflow rows own execution after cutover.
pub(crate) async fn ensure_workflow_schedule_mapping(
    state: &DaemonState,
    root: &std::path::Path,
    entry: &ScheduleEntry,
) -> Result<(), String> {
    if !state.workflows_feature.is_enabled() {
        return Ok(());
    }
    let storage = state
        .storage
        .as_ref()
        .ok_or_else(|| "workflow storage unavailable".to_string())?;
    let key = format!("legacy-schedule:{}:{}", root.to_string_lossy(), entry.id);
    let store = storage.lock().await;
    let repo = rightkit_agent_engine_storage::WorkflowRepository::new(store.conn());
    let existing_trigger = repo
        .find_trigger_by_dedupe_key(&key)
        .map_err(|e| e.to_string())?;
    let legacy = rightkit_workflow::control_plane::migration::LegacySchedule {
        id: entry.id.clone(),
        prompt: entry.prompt.clone(),
        cron: Some(entry.cron_expr.clone()),
        timezone: Some(entry.time_zone.clone()),
        skills: entry.skills.clone(),
        workdir: entry
            .workdir
            .clone()
            .or_else(|| Some(root.to_string_lossy().into_owned())),
        toolset: entry.toolset.clone(),
        backend: entry.execution_backend.clone(),
        delivery: serde_json::to_string(&entry.delivery).ok(),
        script_allowlist: entry.script_allowlist.clone(),
        mode: Some(entry.mode.clone()),
        paused: entry.paused,
    };
    let source = rightkit_workflow::control_plane::migration::migrate_to_source(&legacy, "pending")
        .map_err(|e| e.to_string())?
        .source;
    rightkit_workflow::control_plane::compiler::validate_source_authority(&source)
        .map_err(|e| e.to_string())?;
    let now = crate::api::now_iso();
    let workflow_id = source.definition.id.stable().as_str().to_string();
    let compiled = match rightkit_workflow::control_plane::compiler::compile(
        &source.definition,
        &source.nodes,
        &now,
    ) {
        rightkit_workflow::control_plane::compiler::CompilerOutcome::Compiled {
            version_id,
            content_hash,
            plan,
        } => (
            version_id.stable().as_str().to_string(),
            content_hash.as_str().to_string(),
            serde_json::to_string(&plan).map_err(|e| e.to_string())?,
        ),
        rightkit_workflow::control_plane::compiler::CompilerOutcome::Failed { diagnostics } => {
            return Err(serde_json::to_string(&diagnostics)
                .unwrap_or_else(|_| "workflow compile failed".into()))
        }
    };
    let definition_json = serde_json::to_string(&source).map_err(|e| e.to_string())?;
    let next_fire_at_unix_ms = next_due_epoch_minute_in_time_zone(
        &entry.cron_expr,
        &entry.time_zone,
        current_epoch_minute(),
        527_040,
    )?
    .map(|minute| minute.saturating_mul(60_000));
    let writer = rightkit_agent_engine_storage::WorkflowDefinitionWriter::new(store.conn());
    writer
        .upsert_definition(&rightkit_agent_engine_storage::WorkflowDefinitionRow {
            id: workflow_id.clone(),
            name: source.definition.name.clone(),
            source_kind: "imported".into(),
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .map_err(|e| e.to_string())?;
    let version_exists = repo
        .find_version(&compiled.0)
        .map_err(|e| e.to_string())?
        .is_some();
    if !version_exists {
        writer
            .insert_version(&rightkit_agent_engine_storage::WorkflowVersionRow {
                id: compiled.0.clone(),
                workflow_id: workflow_id.clone(),
                version_number: repo
                    .list_versions(&workflow_id)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .map(|version| version.version_number)
                    .max()
                    .unwrap_or(0)
                    + 1,
                content_hash: compiled.1,
                definition_json,
                compiled_plan_json: compiled.2,
                publication_state: "published".into(),
                created_at: now.clone(),
                published_at: Some(now.clone()),
                created_by: "legacy-schedule-migration".into(),
                notes: "WP-06 legacy schedule mapping".into(),
            })
            .map_err(|e| e.to_string())?;
    }
    let trigger = rightkit_agent_engine_storage::WorkflowTriggerRow {
        id: existing_trigger
            .as_ref()
            .map(|trigger| trigger.id.clone())
            .unwrap_or_else(|| format!("wft_legacy_{}", entry.id)),
        workflow_id,
        kind: "cron".into(),
        enabled: !entry.paused && workflow_schedule_cutover_enabled(),
        cron_expression: Some(entry.cron_expr.clone()),
        timezone: Some(entry.time_zone.clone()),
        version_pin: Some(compiled.0),
        dedupe_key: Some(key),
        overlap_policy: "skip".into(),
        misfire_policy: "coalesce".into(),
        concurrency_cap: 1,
        next_fire_at_unix_ms,
        revision: 1,
        created_at: now.clone(),
        updated_at: now,
    };
    if existing_trigger.is_some() {
        repo.update_legacy_trigger(&trigger)
    } else {
        repo.insert_trigger(&trigger)
    }
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn spawn_schedule_runner(state: DaemonState) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            run_once(&state).await;
        }
    });
}

static RECONCILED_ROOTS: std::sync::LazyLock<std::sync::Mutex<HashSet<PathBuf>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

async fn run_once(state: &DaemonState) {
    let epoch_minute = current_epoch_minute();
    for root in scheduled_workspace_roots(state, true) {
        // R7: the first time this daemon sees a workspace, close the runs a previous daemon left open.
        let first_sight = RECONCILED_ROOTS
            .lock()
            .map(|mut seen| seen.insert(root.clone()))
            .unwrap_or(false);
        if first_sight {
            if let Err(error) = reconcile_orphaned_runs(&root) {
                warn!(workspace_root = %root.display(), %error, "could not reconcile orphaned schedule runs");
            }
        }
        // An untrusted workspace is scanned only so its due occurrence fails visibly with
        // `untrusted_workspace` (R41): no workflow mapping is made for it and no workflow trigger
        // may take its occurrence over.
        let trusted = rightkit_agent_config::is_workspace_trusted(&root);
        if trusted {
            if let Ok(entries) = load_schedules(&root) {
                for entry in entries.values() {
                    map_workflow_schedule(state, &root, entry).await;
                }
            }
        }
        match claim_due_schedules(&root, epoch_minute) {
            Ok(due) => {
                for entry in due {
                    if trusted {
                        map_workflow_schedule(state, &root, &entry).await;
                    }
                    if trusted && workflow_schedule_cutover(state, &root, &entry) {
                        if let Err(error) = append_run_event(
                            &root,
                            schedule_run_event(
                                &entry,
                                epoch_minute,
                                "workflow_cutover",
                                &format!("schedule-{}", entry.id),
                                "workflow trigger owns scheduled execution",
                            ),
                        ) {
                            warn!(schedule_id = %entry.id, %error, "could not record the workflow cutover occurrence");
                        }
                        continue;
                    }
                    if entry.mode.eq_ignore_ascii_case("scriptOnly") {
                        // A scriptOnly fire awaits its whole child process, so it runs
                        // detached: one slow or stuck command must not stall every other
                        // schedule the daemon serves. The in-flight guard keeps one run
                        // per schedule, as the serial loop used to.
                        let Some(guard) = ScriptRunGuard::try_begin(&root, &entry.id) else {
                            record_script_still_running(&root, &entry, epoch_minute);
                            continue;
                        };
                        let state = state.clone();
                        let root = root.clone();
                        tokio::spawn(async move {
                            let _guard = guard;
                            fire_schedule(&state, &root, &entry).await;
                            validate_causal_receipts(&root, &entry.id);
                        });
                        continue;
                    }
                    fire_schedule(state, &root, &entry).await;
                    validate_causal_receipts(&root, &entry.id);
                }
            }
            Err(e) => {
                warn!(workspace_root = %root.display(), error = %e, "schedule workspace scan failed")
            }
        }
    }
}

async fn map_workflow_schedule(state: &DaemonState, root: &std::path::Path, entry: &ScheduleEntry) {
    if let Err(error) = ensure_workflow_schedule_mapping(state, root, entry).await {
        warn!(schedule_id = %entry.id, %error, "could not map the schedule into workflow storage");
    }
}

/// Replay validator runs against durable run history so malformed
/// trigger/run/task/result links never stay invisible after a daemon restart.
fn validate_causal_receipts(root: &std::path::Path, schedule_id: &str) {
    if let Err(e) = replay_causal_receipts(root, schedule_id) {
        warn!(
            schedule_id = %schedule_id,
            error = %e,
            "causal receipt replay validation failed"
        );
    }
}

static SCRIPT_RUNS_IN_FLIGHT: std::sync::LazyLock<std::sync::Mutex<HashSet<(PathBuf, String)>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

/// Marks one scriptOnly schedule as running in this daemon until dropped.
struct ScriptRunGuard {
    key: (PathBuf, String),
}

impl ScriptRunGuard {
    fn try_begin(root: &std::path::Path, schedule_id: &str) -> Option<Self> {
        let key = (root.to_path_buf(), schedule_id.to_string());
        let mut running = SCRIPT_RUNS_IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Build the guard only on success: a dropped guard releases the slot.
        if running.insert(key.clone()) {
            Some(Self { key })
        } else {
            None
        }
    }
}

impl Drop for ScriptRunGuard {
    fn drop(&mut self) {
        SCRIPT_RUNS_IN_FLIGHT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.key);
    }
}

fn record_script_still_running(root: &std::path::Path, entry: &ScheduleEntry, epoch_minute: i64) {
    let event = schedule_run_event(
        entry,
        epoch_minute,
        "skipped",
        &format!("schedule-{}", sanitize_id(&entry.id)),
        "previous scriptOnly run is still running",
    );
    if let Err(error) = append_run_event(root, event) {
        warn!(schedule_id = %entry.id, %error, "could not record skipped scriptOnly occurrence");
    }
}

/// Legacy schedules remain source-of-truth for CRUD/history. A workflow cron
/// becomes execution authority only when its explicit mapping key matches
/// and its pinned, published shape agrees with legacy schedule state.
pub(crate) fn sync_workflow_schedule_compat(
    state: &DaemonState,
    root: &std::path::Path,
    entry: &ScheduleEntry,
) {
    if !state.workflows_feature.is_enabled() {
        return;
    }
    let key = format!("legacy-schedule:{}:{}", root.to_string_lossy(), entry.id);
    let Some(storage) = &state.storage else {
        return;
    };
    let Ok(store) = storage.try_lock() else {
        return;
    };
    let repo = rightkit_agent_engine_storage::WorkflowRepository::new(store.conn());
    let mapped = match repo.find_trigger_by_dedupe_key(&key) {
        Ok(value) => value,
        Err(error) => {
            state.audit.append(
                "workflow_schedule_shadow_error",
                &entry.id,
                &error.to_string(),
            );
            return;
        }
    };
    let Some(trigger) = mapped else {
        return;
    };
    let version_ok = trigger
        .version_pin
        .as_deref()
        .and_then(|id| repo.find_version(id).ok().flatten())
        .is_some_and(|v| {
            v.workflow_id == trigger.workflow_id && v.publication_state == "published"
        });
    let compatible = trigger.kind == "cron"
        && trigger.cron_expression.as_deref() == Some(entry.cron_expr.as_str())
        && trigger.timezone.as_deref() == Some(entry.time_zone.as_str())
        && version_ok;
    let desired_enabled = compatible && !entry.paused && workflow_schedule_cutover_enabled();
    if trigger.enabled != desired_enabled {
        let _ = repo.set_trigger_enabled(&trigger.id, desired_enabled, &crate::api::now_iso());
    }
    let mode = if compatible { "match" } else { "mismatch" };
    state.audit.append(
        "workflow_schedule_shadow",
        &entry.id,
        &format!("{mode}:{}", trigger.id),
    );
}

pub(crate) fn disable_workflow_schedule_compat(
    state: &DaemonState,
    root: &std::path::Path,
    schedule_id: &str,
) {
    if !state.workflows_feature.is_enabled() {
        return;
    }
    let Some(storage) = &state.storage else {
        return;
    };
    let Ok(store) = storage.try_lock() else {
        return;
    };
    let repo = rightkit_agent_engine_storage::WorkflowRepository::new(store.conn());
    let key = format!("legacy-schedule:{}:{schedule_id}", root.to_string_lossy());
    if let Ok(Some(trigger)) = repo.find_trigger_by_dedupe_key(&key) {
        let _ = repo.set_trigger_enabled(&trigger.id, false, &crate::api::now_iso());
        state.audit.append(
            "workflow_schedule_shadow",
            schedule_id,
            &format!("deleted:{}", trigger.id),
        );
    }
}

fn workflow_schedule_cutover(
    state: &DaemonState,
    root: &std::path::Path,
    entry: &ScheduleEntry,
) -> bool {
    if !state.workflows_feature.is_enabled() {
        return false;
    }
    sync_workflow_schedule_compat(state, root, entry);
    let key = format!("legacy-schedule:{}:{}", root.to_string_lossy(), entry.id);
    let Some(storage) = &state.storage else {
        return false;
    };
    let Ok(store) = storage.try_lock() else {
        return false;
    };
    let repo = rightkit_agent_engine_storage::WorkflowRepository::new(store.conn());
    let Ok(Some(trigger)) = repo.find_trigger_by_dedupe_key(&key) else {
        return false;
    };
    let matches = trigger.kind == "cron"
        && trigger.enabled
        && trigger.cron_expression.as_deref() == Some(entry.cron_expr.as_str())
        && trigger.timezone.as_deref() == Some(entry.time_zone.as_str());
    matches && workflow_schedule_cutover_enabled()
}

pub(crate) fn workflow_schedule_cutover_enabled() -> bool {
    matches!(
        std::env::var(rightkit_agent_config::env_name("WORKFLOW_SCHEDULE_CUTOVER")).as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

/// Workspaces whose schedules the runner scans. With `with_untrusted_schedules` a workspace that
/// is not trusted but carries a `{state dir}/schedules.json` is included, so a due occurrence there
/// fails at once and visibly with `untrusted_workspace` (scheduler contract R41) instead of being
/// skipped without a trace. Residency checks pass `false`: an untrusted folder never keeps the
/// daemon alive.
pub(super) fn scheduled_workspace_roots(
    state: &DaemonState,
    with_untrusted_schedules: bool,
) -> Vec<PathBuf> {
    if !state.has_license_feature("schedules") {
        return Vec::new();
    }
    let mut roots = HashSet::new();
    for session in state.list_sessions() {
        insert_workspace_root(&mut roots, &session.workspace_root);
    }
    if let Some(storage) = &state.storage {
        match storage.try_lock() {
            Ok(store) => match store.list_sessions() {
                Ok(rows) => {
                    for row in rows {
                        insert_workspace_root(&mut roots, &row.workspace_root);
                    }
                }
                Err(e) => warn!(error = %e, "failed to list persisted sessions"),
            },
            Err(_) => warn!("storage busy; skipping dormant roots this tick"),
        }
    }
    roots
        .into_iter()
        .filter(|root| workspace_may_fire_schedules(root, with_untrusted_schedules))
        .collect()
}

static UNTRUSTED_NOTED: std::sync::LazyLock<std::sync::Mutex<HashSet<PathBuf>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

/// A checked-in `{state dir}/schedules.json` must not run prompts or scripts until the user has
/// trusted the workspace; the skip is logged once per workspace per daemon run.
fn workspace_may_fire_schedules(root: &std::path::Path, with_untrusted_schedules: bool) -> bool {
    if rightkit_agent_config::is_workspace_trusted(root) {
        return true;
    }
    if root
        .join(crate::host_identity::home_dir())
        .join("schedules.json")
        .is_file()
    {
        if with_untrusted_schedules {
            // `fire_schedule_inner` records the failed run; nothing executes.
            return true;
        }
        let first = UNTRUSTED_NOTED
            .lock()
            .map(|mut seen| seen.insert(root.to_path_buf()))
            .unwrap_or(false);
        if first {
            warn!(workspace_root = %root.display(), "schedules in an untrusted workspace will not fire until it is trusted");
        }
    }
    false
}

fn insert_workspace_root(roots: &mut HashSet<PathBuf>, workspace_root: &str) {
    let root = if workspace_root.trim().is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(workspace_root.trim())
    };
    roots.insert(root);
}

/// True when any persisted schedule in any reachable workspace is enabled,
/// unpaused, and has a future cron occurrence — the presence reaper reads
/// this to skip its self-exit so headless schedules keep firing when the
/// desktop app is closed.
///
/// Returns false on any per-workspace I/O error so a transient parse fault
/// cannot pin the daemon resident forever (over-approximating "busy" is
/// fine, but pinning the daemon open is the wrong direction for errors).
pub fn has_enabled_schedules_with_future_occurrence(state: &DaemonState) -> bool {
    if !state.has_license_feature("schedules") {
        return false;
    }
    let epoch_minute = current_epoch_minute();
    for root in scheduled_workspace_roots(state, false) {
        let entries = match load_schedules(&root) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(workspace = %root.display(), error = %e, "has_enabled_schedules: load failed");
                continue;
            }
        };
        if entries.is_empty() {
            tracing::debug!(workspace = %root.display(), "has_enabled_schedules: no entries");
            continue;
        }
        for entry in entries.values() {
            if entry_keeps_daemon_resident(&root, entry, epoch_minute) {
                return true;
            }
        }
    }
    false
}

/// R2 / R42 / D2 / D35: whether one schedule keeps the daemon resident. The probe and the claim agree
/// on "paused" (the entry, its lifecycle or a disabled bot profile), and every trigger kind counts:
/// cron and interval while they have a future occurrence, once while it is in the future, monitor
/// always. Webhook triggers are not schedules and never count (R50). A record that cannot be
/// interpreted is not an occurrence, so a bad record cannot pin the daemon open.
pub(super) fn entry_keeps_daemon_resident(
    root: &std::path::Path,
    entry: &ScheduleEntry,
    epoch_minute: i64,
) -> bool {
    if entry.paused {
        return false;
    }
    let metadata = match load_runtime_metadata(root, entry) {
        Ok(metadata) => metadata,
        Err(error) => {
            tracing::warn!(schedule_id = %entry.id, %error, "has_enabled_schedules: unreadable schedule record");
            return false;
        }
    };
    if metadata.lifecycle.paused || metadata.profile.as_ref().is_some_and(|p| !p.enabled) {
        return false;
    }
    let now_ms = epoch_minute.saturating_mul(60_000);
    match metadata.trigger {
        None => false,
        Some(ScheduleSpec::Monitor { .. }) => true,
        // Through the occurrence's own minute: the runner claims it on its next tick inside that minute, so a daemon
        // that stopped being resident as the minute began could exit before the run it was kept for.
        Some(ScheduleSpec::Once { at_unix_ms }) => at_unix_ms.div_euclid(60_000) >= epoch_minute,
        Some(spec @ ScheduleSpec::Interval { .. }) => spec.next_at(now_ms).is_some(),
        Some(ScheduleSpec::Cron {
            expression,
            time_zone,
        }) => matches!(
            next_due_epoch_minute_in_time_zone(
                &expression,
                &time_zone,
                epoch_minute,
                60 * 24 * 365 * 2,
            ),
            Ok(Some(next)) if next > epoch_minute
        ),
    }
}

#[cfg(test)]
mod script_run_guard_tests {
    use super::ScriptRunGuard;

    #[test]
    fn one_run_per_schedule_until_the_guard_drops() {
        let root = std::env::temp_dir().join(format!(
            "{}-{}",
            crate::host_identity::prefixed("guard"),
            std::process::id()
        ));
        let first = ScriptRunGuard::try_begin(&root, "nightly").expect("first run starts");
        assert!(ScriptRunGuard::try_begin(&root, "nightly").is_none());
        let other = ScriptRunGuard::try_begin(&root, "hourly").expect("other schedule runs");
        drop(first);
        assert!(ScriptRunGuard::try_begin(&root, "nightly").is_some());
        drop(other);
    }
}
