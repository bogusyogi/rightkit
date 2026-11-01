async fn quarantine_skill_handler(
    State(state): State<DaemonState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SkillLifecycleRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    transition_skill_lifecycle(
        &state.skill_dirs,
        &slug,
        SkillLifecycle::Quarantined,
        false,
        false,
        "quarantine",
        body,
    )
}

async fn activate_skill_handler(
    State(state): State<DaemonState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SkillLifecycleRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    transition_skill_lifecycle(
        &state.skill_dirs,
        &slug,
        SkillLifecycle::Active,
        true,
        true,
        "activate",
        body,
    )
}

async fn retire_skill_handler(
    State(state): State<DaemonState>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SkillLifecycleRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    transition_skill_lifecycle(
        &state.skill_dirs,
        &slug,
        SkillLifecycle::Retired,
        false,
        false,
        "retire",
        body,
    )
}

fn required_trimmed(value: Option<&str>) -> Result<String, StatusCode> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(1000).collect())
        .ok_or(StatusCode::BAD_REQUEST)
}

fn default_skill_benchmark_threshold(mode: coderight_skills::SkillGateMode) -> f64 {
    match mode {
        coderight_skills::SkillGateMode::Hard => 1.0,
        coderight_skills::SkillGateMode::Soft | coderight_skills::SkillGateMode::Mixed => 0.75,
    }
}

fn skill_benchmark_gate_passed(
    mode: coderight_skills::SkillGateMode,
    score: f64,
    runner_passed: bool,
    threshold: f64,
) -> bool {
    match mode {
        coderight_skills::SkillGateMode::Hard => runner_passed && score >= threshold,
        coderight_skills::SkillGateMode::Soft => score >= threshold,
        coderight_skills::SkillGateMode::Mixed => runner_passed && score >= threshold,
    }
}

fn proposal_gate_mode(dirs: &[PathBuf], slug: &str) -> Option<coderight_skills::SkillGateMode> {
    find_skill_item(dirs, slug).and_then(|(item, _)| item.metadata.coderight.gate_mode)
}

fn discover_skill_items(dirs: &[PathBuf]) -> Vec<SkillItem> {
    let mut rows = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut paths = entries
            .flatten()
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        for path in paths {
            let (slug, skill_path, kind) =
                if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("md") {
                    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    (stem.to_string(), path.clone(), "file")
                } else if path.is_dir() {
                    let skill_md = path.join("SKILL.md");
                    if !skill_md.is_file() {
                        continue;
                    }
                    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    (name.to_string(), skill_md, "directory")
                } else {
                    continue;
                };
            let Some(slug) = normalize_skill_slug(&slug) else {
                continue;
            };
            if rows.iter().any(|row: &SkillItem| row.slug == slug) {
                continue;
            }
            if let Some((item, _)) = load_skill_item(&slug, &skill_path, dir, kind) {
                rows.push(item);
            }
        }
    }
    rows.sort_by(|a, b| a.slug.cmp(&b.slug));
    rows
}

fn transition_skill_lifecycle(
    dirs: &[PathBuf],
    slug: &str,
    lifecycle: SkillLifecycle,
    enabled: bool,
    require_activation_gate: bool,
    action: &str,
    body: SkillLifecycleRequest,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let slug = normalize_skill_slug(slug).ok_or(StatusCode::BAD_REQUEST)?;
    let (item, content) = find_skill_item(dirs, &slug).ok_or(StatusCode::NOT_FOUND)?;
    ensure_skill_path_is_managed(dirs, &item.path)?;
    let operator_override = body.operator_override.unwrap_or(false);
    if require_activation_gate && !skill_has_passing_benchmark(dirs, &slug) && !operator_override {
        return Err(StatusCode::PRECONDITION_FAILED);
    }
    let reason = body
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(|reason| reason.chars().take(500).collect::<String>());
    let content = set_skill_enabled_in_content(&content, enabled);
    let content = set_skill_lifecycle_in_content(&content, lifecycle);
    std::fs::write(&item.path, content).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    append_skill_audit_log(
        &item.directory,
        SkillAuditEntry {
            slug: slug.clone(),
            action: action.to_string(),
            lifecycle,
            actor: "operator".to_string(),
            reason: reason.clone(),
            operator_override,
            timestamp_unix_secs: unix_timestamp_secs(),
        },
    )?;
    if let Ok(path) = skill_governance_store_path(dirs) {
        let mut store = load_skill_governance_store(&path);
        if let Some(proposal) = store
            .proposals
            .iter_mut()
            .find(|proposal| proposal.slug == slug)
        {
            proposal.status = match lifecycle {
                SkillLifecycle::Quarantined => SkillProposalStatus::Quarantined,
                SkillLifecycle::Active => SkillProposalStatus::Active,
                SkillLifecycle::Retired => SkillProposalStatus::Retired,
                _ => proposal.status,
            };
            proposal.updated_at_unix_secs = unix_timestamp_secs();
        }
        store.audit_log.push(governance_audit_entry(
            "skill",
            &slug,
            Some(&slug),
            action,
            reason.as_deref(),
            operator_override,
        ));
        write_skill_governance_store(&path, &store)?;
    }
    let (item, content) = load_skill_item(&slug, &item.path, &item.directory, &item.kind)
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({
        "updated": true,
        "action": action,
        "skill": skill_item_json(&item),
        "content": content,
    })))
}

fn append_skill_audit_log(dir: &FsPath, entry: SkillAuditEntry) -> Result<(), StatusCode> {
    std::fs::create_dir_all(dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let path = dir.join(".coderight-skill-audit-log.json");
    let mut rows = if path.is_file() {
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Vec<SkillAuditEntry>>(&raw).ok())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    rows.push(entry);
    let json =
        serde_json::to_string_pretty(&rows).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    std::fs::write(path, json).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn managed_skill_dir(dirs: &[PathBuf]) -> Result<PathBuf, StatusCode> {
    dirs.first()
        .cloned()
        .or_else(|| default_skills_dirs().into_iter().next())
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)
}

fn skill_governance_store_path(dirs: &[PathBuf]) -> Result<PathBuf, StatusCode> {
    Ok(managed_skill_dir(dirs)?.join(".coderight-skill-governance.json"))
}

fn load_skill_governance_store(path: &FsPath) -> SkillGovernanceStore {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return SkillGovernanceStore::default();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn write_skill_governance_store(
    path: &FsPath,
    store: &SkillGovernanceStore,
) -> Result<(), StatusCode> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    let raw = serde_json::to_string_pretty(store).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    std::fs::write(path, raw).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn next_skill_version(store: &SkillGovernanceStore, slug: &str) -> u32 {
    store
        .versions
        .iter()
        .filter(|version| version.slug == slug)
        .map(|version| version.version)
        .max()
        .unwrap_or(0)
        + 1
}

fn proposal_content_for_quarantine(proposal: &SkillProposalRecord) -> String {
    let content = proposal.proposed_content.trim();
    if has_frontmatter_marker(content) {
        let content = set_skill_enabled_in_content(content, false);
        return set_skill_lifecycle_in_content(&content, SkillLifecycle::Quarantined);
    }
    format!(
        "+++\nname = \"{}\"\ndescription = \"{}\"\nenabled = false\nlifecycle = \"quarantined\"\n+++\n\n{}",
        toml_escape(&proposal.slug),
        toml_escape(&proposal.summary),
        content
    )
}

fn governance_audit_entry(
    target_type: &str,
    target_id: &str,
    slug: Option<&str>,
    action: &str,
    reason: Option<&str>,
    operator_override: bool,
) -> SkillGovernanceAuditEntry {
    let now = unix_timestamp_secs();
    SkillGovernanceAuditEntry {
        id: format!("{target_type}-{target_id}-{action}-{now}"),
        target_type: target_type.to_string(),
        target_id: target_id.to_string(),
        slug: slug.map(str::to_string),
        action: action.to_string(),
        reason: reason
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map(|reason| reason.chars().take(500).collect()),
        actor: "operator".to_string(),
        operator_override,
        timestamp_unix_secs: now,
    }
}

fn skill_has_passing_benchmark(dirs: &[PathBuf], slug: &str) -> bool {
    let Ok(path) = skill_governance_store_path(dirs) else {
        return false;
    };
    let store = load_skill_governance_store(&path);
    store
        .benchmark_verdicts
        .iter()
        .any(|verdict| verdict.slug == slug && verdict.passed)
}

fn unix_timestamp_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
