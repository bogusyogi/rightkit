async fn get_harness_radar() -> Json<serde_json::Value> {
    let radar = current_harness_radar_snapshot();
    Json(serde_json::json!({
        "sources": radar.sources,
        "entries": radar.entries,
        "capabilities": rightkit_adapt::HARNESS_RADAR_CAPABILITIES,
        "capabilityMap": radar.capability_map,
        "proposals": radar.proposals,
        "changes": radar.changes,
        "validations": radar.validations,
        "notes": [
            "Network fetching is intentionally out of band; source validation checks the bundled snapshot before promotion.",
            "Historical diffing compares the current bundled snapshot against the previous baseline before generating proposals.",
            "Imported entries should normalize into proposals before promotion into the implementation backlog."
        ],
    }))
}

async fn promote_harness_radar_proposal(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "evolution")?;
    let radar = current_harness_radar_snapshot();
    let Some(proposal) = radar.proposals.iter().find(|proposal| proposal.id == id) else {
        return Err(StatusCode::NOT_FOUND);
    };
    let learning = rightkit_adapt::Proposal {
        id: proposal.id.clone(),
        kind: rightkit_adapt::ProposalKind::HarnessRule,
        summary: proposal.title.clone(),
        evidence: format!(
            "Harness Radar promoted proposal. source={} capability={} projects={} rationale={}",
            proposal.source_id,
            proposal.capability,
            proposal.project_names.join(", "),
            proposal.rationale
        ),
        score: 0.0,
        status: rightkit_adapt::ProposalStatus::Candidate,
        created_at: now_iso(),
        age_sessions: 0,
        rejection_reason: None,
    };
    let stored = state.learning.add_proposal_if_absent(learning);
    Ok(Json(serde_json::to_value(stored).unwrap()))
}

async fn schedule_harness_radar_refresh(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<HarnessRadarScheduleBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "evolution")?;
    require_license_feature(&state, "schedules")?;
    let root = if body.workspace_root.trim().is_empty() {
        std::env::current_dir().map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    } else {
        std::path::PathBuf::from(body.workspace_root.trim())
    };
    let time_zone =
        crate::scheduler::normalize_time_zone(body.time_zone.as_deref().unwrap_or("UTC").trim())
            .map_err(|_| StatusCode::BAD_REQUEST)?;
    let cron_expr = body
        .cron_expr
        .as_deref()
        .unwrap_or("0 9 * * 1")
        .trim()
        .to_string();
    crate::scheduler::next_due_epoch_minute_in_time_zone(
        &cron_expr,
        &time_zone,
        crate::scheduler::current_epoch_minute(),
        366 * 24 * 60,
    )
    .map_err(|_| StatusCode::BAD_REQUEST)?;

    let mut schedules = crate::api_schedules::load_api_schedules(&root)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let id = "harness-radar-refresh".to_string();
    let entry = crate::api_schedules::StoredScheduleEntry {
        id: id.clone(),
        cron_expr: cron_expr.clone(),
        prompt: harness_radar_refresh_prompt(),
        time_zone: time_zone.clone(),
        skills: Vec::new(),
        workdir: Some(root.display().to_string()),
        toolset: Some("safe-research".to_string()),
        execution_backend: Some("local".to_string()),
        delivery: vec![crate::api_schedules::StoredDeliveryTarget {
            kind: "desktop_notification".to_string(),
            target: None,
            allow_private_network: false,
        }],
        script_allowlist: Vec::new(),
        mode: "agent".to_string(),
        paused: body.paused,
        source_recipe_id: None,
        recipe_params: std::collections::BTreeMap::new(),
        max_runtime_seconds: None,
    };
    let existed = schedules.insert(id.clone(), entry.clone()).is_some();
    crate::api_schedules::save_api_schedules(&root, &schedules)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(serde_json::json!({
        "scheduled": true,
        "created": !existed,
        "updated": existed,
        "id": id,
        "workspaceRoot": root,
        "scheduleStorePath": crate::api_schedules::schedules_store_path(&root),
        "schedule": {
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
        }
    })))
}

fn harness_radar_refresh_prompt() -> String {
    "Harness Radar refresh: read assets/harness-radar/sources.toml, fetch the configured public watchlist sources when network is available, compare them with assets/harness-radar/projects.snapshot.yaml, validate required capabilities/categories, summarize new or changed agent-harness projects, and propose only RightKit-relevant backlog updates with source evidence. Do not auto-promote proposals without operator approval.".to_string()
}

struct RuntimeHarnessRadarSnapshot {
    sources: Vec<rightkit_adapt::HarnessRadarSource>,
    entries: Vec<rightkit_adapt::HarnessRadarEntry>,
    proposals: Vec<rightkit_adapt::HarnessRadarProposal>,
    changes: Vec<rightkit_adapt::HarnessRadarChange>,
    validations: Vec<rightkit_adapt::HarnessRadarValidation>,
    capability_map: std::collections::BTreeMap<String, usize>,
}

fn current_harness_radar_snapshot() -> RuntimeHarnessRadarSnapshot {
    let sources = rightkit_adapt::parse_harness_radar_sources(include_str!(
        "../../../../../assets/harness-radar/sources.toml"
    ));
    let source_id = sources
        .first()
        .map(|source| source.id.as_str())
        .unwrap_or("awesome-agent-harness");
    let entries = rightkit_adapt::import_harness_radar_yaml(
        source_id,
        include_str!("../../../../../assets/harness-radar/projects.snapshot.yaml"),
    );
    let previous_entries = rightkit_adapt::import_harness_radar_yaml(
        source_id,
        include_str!("../../../../../assets/harness-radar/projects.previous.yaml"),
    );
    let changes =
        rightkit_adapt::diff_harness_radar_entries(source_id, &previous_entries, &entries);
    let actionable_entries = entries
        .iter()
        .filter(|entry| {
            let key = if entry.repo_url.trim().is_empty() {
                entry.project_name.to_ascii_lowercase()
            } else {
                entry.repo_url.to_ascii_lowercase()
            };
            changes.iter().any(|change| {
                let change_key = if change.repo_url.trim().is_empty() {
                    change.project_name.to_ascii_lowercase()
                } else {
                    change.repo_url.to_ascii_lowercase()
                };
                change_key == key && change.status != "unchanged"
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    let proposals = rightkit_adapt::generate_harness_radar_proposals(&actionable_entries);
    let validations = sources
        .iter()
        .map(|source| rightkit_adapt::validate_harness_radar_snapshot(source, &entries))
        .collect::<Vec<_>>();
    let mut capability_map = std::collections::BTreeMap::<String, usize>::new();
    for capability in rightkit_adapt::HARNESS_RADAR_CAPABILITIES {
        capability_map.insert((*capability).to_string(), 0);
    }
    for entry in &entries {
        let count = capability_map
            .entry(rightkit_adapt::harness_radar_capability_for_entry(
                entry,
            ))
            .or_insert(0);
        *count += 1;
    }
    RuntimeHarnessRadarSnapshot {
        sources,
        entries,
        proposals,
        changes,
        validations,
        capability_map,
    }
}
