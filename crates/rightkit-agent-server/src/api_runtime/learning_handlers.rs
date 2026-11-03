async fn approve_learning_proposal(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "evolution")?;
    if state.learning.approve_proposal(&id) {
        let instinct_written = match state.learning.proposal(&id) {
            Some(proposal) => write_approved_proposal_instinct(&state, &proposal)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
            None => false,
        };
        Ok(Json(serde_json::json!({
            "ok": true,
            "id": id,
            "status": "Promoted",
            "instinctWritten": instinct_written
        })))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

fn write_approved_proposal_instinct(
    state: &DaemonState,
    proposal: &rightkit_adapt::Proposal,
) -> Result<bool, String> {
    let Some(instinct) = proposal_to_instinct(proposal) else {
        return Ok(false);
    };
    let mut store = state.instincts.lock().map_err(|e| e.to_string())?;
    if store
        .all()
        .iter()
        .any(|existing| existing.id == instinct.id)
    {
        return Ok(false);
    }
    store.add(instinct);
    persist_instinct_store(&store, &default_instincts_store_path())?;
    Ok(true)
}

fn proposal_to_instinct(proposal: &rightkit_adapt::Proposal) -> Option<rightkit_adapt::Instinct> {
    if !matches!(
        proposal.kind,
        rightkit_adapt::ProposalKind::HarnessRule | rightkit_adapt::ProposalKind::Memory
    ) {
        return None;
    }
    let rule = proposal.summary.trim();
    if rule.is_empty() {
        return None;
    }
    Some(rightkit_adapt::Instinct {
        id: format!("learning-{}", sanitize_instinct_id(&proposal.id)),
        rule: rule.to_string(),
        observed_at_sessions: vec![proposal.id.clone()],
        created_at: now_iso(),
        deprecated: false,
    })
}

fn sanitize_instinct_id(value: &str) -> String {
    let mut out = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches('-');
    if out.is_empty() {
        "proposal".to_string()
    } else {
        out.to_string()
    }
}

fn persist_instinct_store(
    store: &rightkit_adapt::InstinctStore,
    path: &FsPath,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let payload = store.to_json().map_err(|e| e.to_string())?;
    crate::store_write::write_store_file(path, payload.as_bytes()).map_err(|e| e.to_string())
}

async fn reject_learning_proposal(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "evolution")?;
    if state.learning.reject_proposal(&id) {
        Ok(Json(
            serde_json::json!({ "ok": true, "id": id, "status": "Rejected" }),
        ))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

async fn get_metrics(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    Json(serde_json::json!(state.learning.metrics_json()))
}

async fn get_taste(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    Json(serde_json::json!(state.observations.taste_json()))
}

async fn get_proposals(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    Json(serde_json::json!(state.observations.proposals_json()))
}

async fn get_rollouts(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    Json(serde_json::json!(state.rollouts.json()))
}

async fn get_finetuning(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    Json(serde_json::json!(state.observations.finetuning_json()))
}
