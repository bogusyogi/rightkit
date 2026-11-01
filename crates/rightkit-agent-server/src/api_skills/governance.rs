async fn list_skill_proposals_handler(
    State(state): State<DaemonState>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let path = skill_governance_store_path(&state.skill_dirs)?;
    let store = load_skill_governance_store(&path);
    Ok(Json(serde_json::json!({
        "storePath": path.display().to_string(),
        "proposals": store.proposals,
        "versions": store.versions,
        "benchmarkVerdicts": store.benchmark_verdicts,
        "auditLog": store.audit_log,
    })))
}

async fn create_skill_proposal_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<CreateSkillProposalRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let slug = normalize_skill_slug(&body.slug).ok_or(StatusCode::BAD_REQUEST)?;
    if body.summary.trim().is_empty() || body.content.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let id = body
        .id
        .as_deref()
        .and_then(normalize_skill_slug)
        .unwrap_or_else(|| format!("proposal-{}-{}", slug, unix_timestamp_secs()));
    let path = skill_governance_store_path(&state.skill_dirs)?;
    let mut store = load_skill_governance_store(&path);
    if store.proposals.iter().any(|proposal| proposal.id == id) {
        return Err(StatusCode::CONFLICT);
    }
    let now = unix_timestamp_secs();
    let content_hash = format!(
        "sha256:{}",
        rightkit_fs::digest::sha256_bytes(body.content.as_bytes())
    );
    let proposal = SkillProposalRecord {
        id: id.clone(),
        slug: slug.clone(),
        title: body
            .title
            .as_deref()
            .map(str::trim)
            .filter(|title| !title.is_empty())
            .unwrap_or(&slug)
            .chars()
            .take(120)
            .collect(),
        summary: body.summary.trim().chars().take(1000).collect(),
        proposed_content: body.content,
        content_hash,
        status: SkillProposalStatus::Proposed,
        source_session_id: body.source_session_id,
        source_task_id: body.source_task_id,
        created_at_unix_secs: now,
        updated_at_unix_secs: now,
    };
    store.audit_log.push(governance_audit_entry(
        "proposal",
        &proposal.id,
        Some(&proposal.slug),
        "create",
        Some("stored skill proposal"),
        false,
    ));
    store.proposals.push(proposal.clone());
    write_skill_governance_store(&path, &store)?;
    Ok(Json(serde_json::json!({
        "created": true,
        "storePath": path.display().to_string(),
        "proposal": proposal,
    })))
}

async fn quarantine_skill_proposal_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SkillProposalQuarantineRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    let id = normalize_skill_slug(&id).ok_or(StatusCode::BAD_REQUEST)?;
    let dir = managed_skill_dir(&state.skill_dirs)?;
    let path = skill_governance_store_path(&state.skill_dirs)?;
    let mut store = load_skill_governance_store(&path);
    let Some(index) = store
        .proposals
        .iter()
        .position(|proposal| proposal.id == id)
    else {
        return Err(StatusCode::NOT_FOUND);
    };
    let proposal = store.proposals[index].clone();
    std::fs::create_dir_all(&dir).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let skill_path = dir.join(format!("{}.md", proposal.slug));
    ensure_skill_path_is_managed(&state.skill_dirs, &skill_path)?;
    let parent_hash = std::fs::read_to_string(&skill_path).ok().map(|content| {
        format!(
            "sha256:{}",
            rightkit_fs::digest::sha256_bytes(content.as_bytes())
        )
    });
    let content = proposal_content_for_quarantine(&proposal);
    std::fs::write(&skill_path, &content).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let now = unix_timestamp_secs();
    let next_version = next_skill_version(&store, &proposal.slug);
    let version = SkillVersionRecord {
        id: format!("{}-v{}", proposal.slug, next_version),
        slug: proposal.slug.clone(),
        version: next_version,
        content_hash: format!(
            "sha256:{}",
            rightkit_fs::digest::sha256_bytes(content.as_bytes())
        ),
        parent_hash,
        proposal_id: Some(proposal.id.clone()),
        change_reason: body
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .unwrap_or("proposal quarantined")
            .chars()
            .take(500)
            .collect(),
        created_at_unix_secs: now,
    };
    store.proposals[index].status = SkillProposalStatus::Quarantined;
    store.proposals[index].updated_at_unix_secs = now;
    store.versions.push(version.clone());
    store.audit_log.push(governance_audit_entry(
        "proposal",
        &proposal.id,
        Some(&proposal.slug),
        "quarantine",
        body.reason.as_deref(),
        false,
    ));
    write_skill_governance_store(&path, &store)?;
    let (item, content) =
        load_skill_item(&proposal.slug, &skill_path, &dir, "file").ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({
        "quarantined": true,
        "proposal": store.proposals[index],
        "version": version,
        "skill": skill_item_json(&item),
        "content": content,
    })))
}

async fn record_skill_proposal_benchmark_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SkillBenchmarkRequest>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    record_skill_proposal_benchmark(&state, &id, body).map(Json)
}

fn record_skill_proposal_benchmark(
    state: &DaemonState,
    id: &str,
    body: SkillBenchmarkRequest,
) -> Result<serde_json::Value, StatusCode> {
    let id = normalize_skill_slug(id).ok_or(StatusCode::BAD_REQUEST)?;
    if body.benchmark_id.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let path = skill_governance_store_path(&state.skill_dirs)?;
    let mut store = load_skill_governance_store(&path);
    let Some(index) = store
        .proposals
        .iter()
        .position(|proposal| proposal.id == id)
    else {
        return Err(StatusCode::NOT_FOUND);
    };
    if !matches!(
        store.proposals[index].status,
        SkillProposalStatus::Quarantined | SkillProposalStatus::Benchmarked
    ) {
        return Err(StatusCode::PRECONDITION_FAILED);
    }
    let slug = body
        .slug
        .as_deref()
        .and_then(normalize_skill_slug)
        .unwrap_or_else(|| store.proposals[index].slug.clone());
    if slug != store.proposals[index].slug {
        return Err(StatusCode::BAD_REQUEST);
    }
    let gate_mode = body
        .gate_mode
        .unwrap_or(rightkit_agent_host_traits::skill_host::SkillGateMode::Hard);
    let score = body
        .score_continuous
        .unwrap_or(if body.passed { 1.0 } else { 0.0 });
    let benchmark_id = body
        .benchmark_id
        .trim()
        .chars()
        .take(160)
        .collect::<String>();
    let verdict_payload = body.verdict.unwrap_or_else(|| {
        serde_json::json!({
            "proposalId": id,
            "slug": slug.clone(),
            "benchmarkId": benchmark_id.clone(),
            "gateMode": gate_mode,
            "scoreContinuous": score,
            "passed": body.passed,
        })
    });
    let now = unix_timestamp_secs();
    let verdict = SkillBenchmarkVerdict {
        id: format!(
            "{}-{}-{}",
            slug,
            benchmark_id.clone(),
            store.benchmark_verdicts.len() + 1
        ),
        slug: slug.clone(),
        proposal_id: Some(id.clone()),
        benchmark_id,
        gate_mode,
        score_continuous: score,
        passed: body.passed,
        verdict_hash: format!(
            "sha256:{}",
            rightkit_fs::digest::sha256_bytes(verdict_payload.to_string().as_bytes())
        ),
        created_at_unix_secs: now,
    };
    if body.passed {
        store.proposals[index].status = SkillProposalStatus::Benchmarked;
    }
    store.proposals[index].updated_at_unix_secs = now;
    store.benchmark_verdicts.push(verdict.clone());
    store.audit_log.push(governance_audit_entry(
        "proposal",
        &id,
        Some(&slug),
        "benchmark",
        body.reason.as_deref(),
        false,
    ));
    write_skill_governance_store(&path, &store)?;
    Ok(serde_json::json!({
        "recorded": true,
        "proposal": store.proposals[index],
        "benchmarkVerdict": verdict,
    }))
}
