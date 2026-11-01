pub async fn fire_schedule_by_id(
    state: &DaemonState,
    root: &Path,
    schedule_id: &str,
) -> Result<(), String> {
    if let Some((entry, profile)) = crate::api_schedules::take_transient_bot_run(schedule_id) {
        fire_bot_once(state, root, &entry, profile).await;
        return Ok(());
    }
    let schedules = load_schedules(root)?;
    let entry = schedules
        .get(schedule_id)
        .ok_or_else(|| format!("schedule '{schedule_id}' not found"))?;
    if entry.paused {
        return Err(format!("schedule '{schedule_id}' is paused"));
    }
    fire_schedule(state, root, entry).await;
    Ok(())
}

/// Execute an API-materialized bot occurrence through the same scheduler
/// consumer as persisted schedules. The entry is intentionally ephemeral;
/// profile authority remains in workflow version storage.
pub(crate) async fn fire_schedule_entry(state: &DaemonState, root: &Path, entry: &ScheduleEntry) {
    fire_schedule(state, root, entry).await;
}

fn scheduled_prompt(entry: &ScheduleEntry, rendered_recipe_prompt: Option<&str>) -> String {
    let mut lines = vec![
        format!("[Scheduled prompt: {}]", entry.id),
        format!("Cron: {}", entry.cron_expr),
        format!("Time zone: {}", entry.time_zone),
        format!("Mode: {}", entry.mode),
    ];
    if let Some(recipe_id) = entry
        .source_recipe_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!("Recipe: {recipe_id}"));
        if !entry.recipe_params.is_empty() {
            lines.push(format!(
                "Recipe params: {}",
                serde_json::to_string(&entry.recipe_params).unwrap_or_else(|_| "{}".to_string())
            ));
        }
    }
    if !entry.skills.is_empty() {
        lines.push(format!("Skills: {}", entry.skills.join(", ")));
    }
    if let Some(toolset) = entry
        .toolset
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!("Toolset: {toolset}"));
    }
    if let Some(backend) = entry
        .execution_backend
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!("Execution backend: {backend}"));
    }
    if let Some(workdir) = entry
        .workdir
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        lines.push(format!("Workdir: {workdir}"));
    }
    if !entry.delivery.is_empty() {
        let delivery = entry
            .delivery
            .iter()
            .map(|target| {
                target
                    .target
                    .as_deref()
                    .map(|value| format!("{}:{value}", target.kind))
                    .unwrap_or_else(|| target.kind.clone())
            })
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("Delivery: {delivery}"));
    }
    lines.push(String::new());
    lines.push(rendered_recipe_prompt.unwrap_or(&entry.prompt).to_string());
    lines.join("\n")
}

fn clean_skill_ids(skills: &[String]) -> Vec<String> {
    let mut cleaned = Vec::new();
    for skill in skills {
        let skill = skill.trim();
        if !skill.is_empty() && !cleaned.iter().any(|existing| existing == skill) {
            cleaned.push(skill.to_string());
        }
    }
    cleaned
}

fn clean_optional_string(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[allow(clippy::too_many_arguments)]
fn record_scheduled_job_span(
    state: &DaemonState,
    phase: MiddlewarePhase,
    status: &str,
    session_id: &str,
    parent_id: Option<&str>,
    root: &Path,
    entry: &ScheduleEntry,
    message: Option<&str>,
    extra: serde_json::Value,
) -> Option<String> {
    let definitions = state
        .runtime_middleware
        .lock()
        .unwrap()
        .enabled_for_phase(phase);
    if definitions.is_empty() {
        return None;
    }
    let middleware_ids = definitions
        .iter()
        .map(|definition| definition.id.clone())
        .collect::<Vec<_>>();
    let now = crate::api::now_iso();
    let id = format!(
        "runtime-{}-{}-{}",
        session_id,
        schedule_phase_key(phase),
        current_epoch_millis()
    );
    let attributes = merge_json_objects(
        serde_json::json!({
            "middlewareIds": middleware_ids,
            "middlewareCount": definitions.len(),
            "scheduleId": entry.id,
            "mode": entry.mode,
            "workspaceRoot": root,
            "cronExpr": entry.cron_expr,
            "timeZone": entry.time_zone,
            "toolset": entry.toolset,
            "executionBackend": entry.execution_backend,
            "skillCount": entry.skills.len(),
            "deliveryCount": entry.delivery.len(),
            "sourceRecipeId": entry.source_recipe_id,
            "message": message,
        }),
        extra,
    );
    state
        .runtime_middleware
        .lock()
        .unwrap()
        .record_span(RuntimeSpan {
            id: id.clone(),
            parent_id: parent_id.map(str::to_string),
            session_id: session_id.to_string(),
            turn_id: None,
            phase,
            status: status.to_string(),
            started_at: now.clone(),
            ended_at: Some(now),
            attributes,
        });
    Some(id)
}

fn merge_json_objects(mut base: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
    if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    base
}

fn schedule_phase_key(phase: MiddlewarePhase) -> &'static str {
    match phase {
        MiddlewarePhase::ScheduledJobStart => "scheduledJobStart",
        MiddlewarePhase::ScheduledJobEnd => "scheduledJobEnd",
        _ => "scheduledJob",
    }
}

pub(super) fn record_schedule_failure(
    root: &Path,
    entry: &ScheduleEntry,
    session_id: &str,
    message: &str,
) -> ScheduleFireOutcome {
    // S:N3a/S:N4 — even pre-submit failures carry the run's isolation
    // namespace and the captured trigger receipt, so a failure that never
    // reached a session still has honest causal evidence.
    let isolation = TaskIsolationV1::fresh(session_id).ok();
    let chain = [trigger_receipt(
        &entry.id,
        session_id,
        current_epoch_millis(),
    )];
    let structured = causal_message_with_isolation(message, &chain, isolation.as_ref());
    let event = schedule_run_event(
        entry,
        current_epoch_minute(),
        "failed",
        session_id,
        &structured,
    );
    if let Err(record_err) = append_run_event(root, event) {
        warn!(schedule_id = %entry.id, error = %record_err, "failed to record schedule failure");
    }
    ScheduleFireOutcome {
        status: "failed".to_string(),
        message: message.to_string(),
    }
}

pub(super) fn schedule_run_event(
    entry: &ScheduleEntry,
    epoch_minute: i64,
    status: &str,
    session_id: &str,
    message: &str,
) -> ScheduleRunEvent {
    ScheduleRunEvent {
        schema_version: 1,
        schedule_id: entry.id.clone(),
        epoch_minute,
        status: status.to_string(),
        session_id: session_id.to_string(),
        message: message.to_string(),
        started_at: crate::api::now_iso(),
        finished_at: None,
        mode: entry.mode.clone(),
        toolset: entry.toolset.clone(),
        execution_backend: entry.execution_backend.clone(),
        // Requested/default backend is intent, not resolution evidence. The
        // concrete execution path fills this only after session submission or
        // process spawn succeeds.
        resolved_execution_backend: None,
        execution_workspace_id: None,
        execution_workspace_path: None,
        resource_limits: None,
        workdir: entry.workdir.clone(),
        skills: entry.skills.clone(),
        delivery_kinds: entry
            .delivery
            .iter()
            .map(|target| target.kind.clone())
            .collect(),
        source_recipe_id: entry.source_recipe_id.clone(),
        exit_code: None,
        artifact_path: None,
        delivery_status: None,
        delivery_errors: Vec::new(),
    }
}

pub(super) fn mark_local_execution_resolved(event: &mut ScheduleRunEvent) {
    event.resolved_execution_backend = Some("local".to_string());
}

fn apply_script_only_execution_audit(
    event: &mut ScheduleRunEvent,
    audit: ScriptOnlyExecutionAudit,
) {
    if audit.resolved_execution_backend.is_some() {
        event.resolved_execution_backend = audit.resolved_execution_backend;
    }
    event.execution_workspace_id = audit.execution_workspace_id;
    event.execution_workspace_path = audit.execution_workspace_path;
    event.resource_limits = audit.resource_limits;
}

pub(super) fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

#[cfg(test)]
mod manual_and_events_tests {
    use super::*;
    use crate::scheduler::DeliveryTarget;

    fn base_entry() -> ScheduleEntry {
        ScheduleEntry {
            id: "sched-1".to_string(),
            cron_expr: "0 9 * * *".to_string(),
            prompt: "do the thing".to_string(),
            time_zone: "UTC".to_string(),
            skills: Vec::new(),
            workdir: None,
            toolset: None,
            execution_backend: None,
            delivery: Vec::new(),
            script_allowlist: Vec::new(),
            mode: "agent".to_string(),
            paused: false,
            source_recipe_id: None,
            recipe_params: Default::default(),
            max_runtime_seconds: None,
        }
    }

    #[test]
    fn sanitize_id_replaces_non_alphanumeric_chars() {
        assert_eq!(sanitize_id("a b/c:d-1"), "a-b-c-d-1");
    }

    #[test]
    fn sanitize_id_preserves_alphanumeric() {
        assert_eq!(sanitize_id("abc123XYZ"), "abc123XYZ");
    }

    #[test]
    fn clean_skill_ids_trims_and_dedupes() {
        let skills = vec![
            " skill-a ".to_string(),
            "skill-a".to_string(),
            "".to_string(),
            "  ".to_string(),
            "skill-b".to_string(),
        ];
        assert_eq!(clean_skill_ids(&skills), vec!["skill-a", "skill-b"]);
    }

    #[test]
    fn clean_skill_ids_empty_input_yields_empty_output() {
        assert!(clean_skill_ids(&[]).is_empty());
    }

    #[test]
    fn clean_optional_string_trims_and_drops_blank() {
        assert_eq!(
            clean_optional_string(Some("  hi  ")),
            Some("hi".to_string())
        );
        assert_eq!(clean_optional_string(Some("   ")), None);
        assert_eq!(clean_optional_string(None), None);
    }

    #[test]
    fn scheduled_prompt_includes_core_fields_and_prompt() {
        let entry = base_entry();
        let rendered = scheduled_prompt(&entry, None);
        assert!(rendered.contains("[Scheduled prompt: sched-1]"));
        assert!(rendered.contains("Cron: 0 9 * * *"));
        assert!(rendered.contains("Time zone: UTC"));
        assert!(rendered.contains("Mode: agent"));
        assert!(rendered.ends_with("do the thing"));
        assert!(!rendered.contains("Recipe:"));
        assert!(!rendered.contains("Skills:"));
        assert!(!rendered.contains("Delivery:"));
    }

    #[test]
    fn scheduled_prompt_prefers_rendered_recipe_prompt_over_entry_prompt() {
        let entry = base_entry();
        let rendered = scheduled_prompt(&entry, Some("rendered override"));
        assert!(rendered.ends_with("rendered override"));
        assert!(!rendered.contains("do the thing"));
    }

    #[test]
    fn scheduled_prompt_omits_blank_source_recipe_id() {
        let mut entry = base_entry();
        entry.source_recipe_id = Some("   ".to_string());
        let rendered = scheduled_prompt(&entry, None);
        assert!(!rendered.contains("Recipe:"));
    }

    #[test]
    fn scheduled_prompt_includes_recipe_and_params_when_present() {
        let mut entry = base_entry();
        entry.source_recipe_id = Some("recipe-1".to_string());
        entry.recipe_params.insert("k".to_string(), "v".to_string());
        let rendered = scheduled_prompt(&entry, None);
        assert!(rendered.contains("Recipe: recipe-1"));
        assert!(rendered.contains("Recipe params:"));
        assert!(rendered.contains("\"k\":\"v\""));
    }

    #[test]
    fn scheduled_prompt_lists_skills_joined_by_comma() {
        let mut entry = base_entry();
        entry.skills = vec!["a".to_string(), "b".to_string()];
        let rendered = scheduled_prompt(&entry, None);
        assert!(rendered.contains("Skills: a, b"));
    }

    #[test]
    fn scheduled_prompt_formats_delivery_targets_with_and_without_target() {
        let mut entry = base_entry();
        entry.delivery = vec![
            DeliveryTarget {
                kind: "webhook".to_string(),
                target: Some("https://example.test".to_string()),
                allow_private_network: false,
            },
            DeliveryTarget {
                kind: "desktop_notification".to_string(),
                target: None,
                allow_private_network: false,
            },
        ];
        let rendered = scheduled_prompt(&entry, None);
        assert!(rendered.contains("Delivery: webhook:https://example.test, desktop_notification"));
    }

    #[test]
    fn merge_json_objects_extra_overrides_base_keys() {
        let base = serde_json::json!({"a": 1, "b": 2});
        let extra = serde_json::json!({"b": 3, "c": 4});
        let merged = merge_json_objects(base, extra);
        assert_eq!(merged, serde_json::json!({"a": 1, "b": 3, "c": 4}));
    }

    #[test]
    fn merge_json_objects_non_object_base_is_returned_unchanged() {
        let base = serde_json::json!("not-an-object");
        let extra = serde_json::json!({"c": 4});
        let merged = merge_json_objects(base.clone(), extra);
        assert_eq!(merged, base);
    }

    #[test]
    fn schedule_phase_key_maps_start_and_end() {
        assert_eq!(
            schedule_phase_key(MiddlewarePhase::ScheduledJobStart),
            "scheduledJobStart"
        );
        assert_eq!(
            schedule_phase_key(MiddlewarePhase::ScheduledJobEnd),
            "scheduledJobEnd"
        );
    }

    #[test]
    fn schedule_run_event_carries_entry_fields_and_no_terminal_state() {
        let mut entry = base_entry();
        entry.delivery = vec![DeliveryTarget {
            kind: "webhook".to_string(),
            target: None,
            allow_private_network: false,
        }];
        let event = schedule_run_event(&entry, 42, "started", "session-1", "starting up");
        assert_eq!(event.schedule_id, "sched-1");
        assert_eq!(event.epoch_minute, 42);
        assert_eq!(event.status, "started");
        assert_eq!(event.session_id, "session-1");
        assert_eq!(event.message, "starting up");
        assert_eq!(event.delivery_kinds, vec!["webhook".to_string()]);
        assert!(event.resolved_execution_backend.is_none());
        assert!(event.finished_at.is_none());
        assert!(event.exit_code.is_none());
        assert!(event.delivery_status.is_none());
        assert!(event.delivery_errors.is_empty());
    }

    #[test]
    fn mark_local_execution_resolved_sets_local_backend() {
        let entry = base_entry();
        let mut event = schedule_run_event(&entry, 0, "started", "s", "m");
        assert!(event.resolved_execution_backend.is_none());
        mark_local_execution_resolved(&mut event);
        assert_eq!(event.resolved_execution_backend, Some("local".to_string()));
    }

    #[test]
    fn apply_script_only_execution_audit_overwrites_only_present_backend() {
        let entry = base_entry();
        let mut event = schedule_run_event(&entry, 0, "started", "s", "m");
        mark_local_execution_resolved(&mut event);
        apply_script_only_execution_audit(
            &mut event,
            ScriptOnlyExecutionAudit {
                resolved_execution_backend: None,
                execution_workspace_id: Some("ws-1".to_string()),
                execution_workspace_path: Some("/tmp/ws".to_string()),
                resource_limits: None,
            },
        );
        // backend was None in the audit, so the previously resolved value
        // must survive untouched.
        assert_eq!(event.resolved_execution_backend, Some("local".to_string()));
        assert_eq!(event.execution_workspace_id, Some("ws-1".to_string()));
        assert_eq!(event.execution_workspace_path, Some("/tmp/ws".to_string()));
    }

    #[test]
    fn apply_script_only_execution_audit_overwrites_backend_when_present() {
        let entry = base_entry();
        let mut event = schedule_run_event(&entry, 0, "started", "s", "m");
        apply_script_only_execution_audit(
            &mut event,
            ScriptOnlyExecutionAudit {
                resolved_execution_backend: Some("remote".to_string()),
                execution_workspace_id: None,
                execution_workspace_path: None,
                resource_limits: None,
            },
        );
        assert_eq!(event.resolved_execution_backend, Some("remote".to_string()));
    }
}
