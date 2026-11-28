async fn approve_learning_proposal(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "evolution")?;
    // The approver may give the instruction text; an empty or missing body keeps the proposal's own.
    let edited_rule = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("rule")
                .and_then(serde_json::Value::as_str)
                .map(|rule| rule.trim().to_string())
        })
        .filter(|rule| !rule.is_empty());
    if state.learning.approve_proposal(&id) {
        let mut note = None;
        let instinct_written = match state.learning.proposal(&id) {
            Some(proposal) => {
                if edited_rule.is_none() && is_generated_failure_text(&proposal) {
                    // "[daily-analyzer] <session>: Session failed" says what went wrong, not
                    // what to do; it never becomes a system-prompt instruction unedited.
                    note = Some(format!(
                        "this proposal's text is generated from a failed session and was not stored as an instruction; approve again with a rule ({}) to store one",
                        crate::host_identity::command("learning approve <id> --rule \"...\"")
                    ));
                    false
                } else {
                    write_approved_proposal_instinct(&state, &proposal, edited_rule.as_deref())
                        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                }
            }
            None => false,
        };
        Ok(Json(serde_json::json!({
            "ok": true,
            "id": id,
            "status": "Promoted",
            "instinctWritten": instinct_written,
            "note": note,
        })))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

/// A proposal minted by the daily analyzer: its text only names the failed session and reason.
fn is_generated_failure_text(proposal: &rightkit_agent_learning::Proposal) -> bool {
    proposal.id.starts_with("daily-analyzer-") && proposal.summary.starts_with("[daily-analyzer]")
}

/// The approved instructions injected into session prompts, and how many the size cap cuts off.
async fn get_instincts(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    let rules = state.active_instinct_rules();
    let rule_refs = rules.iter().map(String::as_str).collect::<Vec<_>>();
    let included = crate::api::instinct_rules_within_cap(&rule_refs);
    let stored = state.instincts.lock().map(|store| {
        store
            .all()
            .iter()
            .map(|instinct| {
                serde_json::json!({
                    "id": instinct.id,
                    "rule": instinct.rule,
                    "deprecated": instinct.deprecated,
                    "createdAt": instinct.created_at,
                    "observedAtSessions": instinct.observed_at_sessions,
                })
            })
            .collect::<Vec<_>>()
    });
    let dropped = rules.len().saturating_sub(included);
    Json(serde_json::json!({
        "instincts": stored.unwrap_or_default(),
        "activeCount": rules.len(),
        "includedInPrompt": included,
        "truncated": dropped > 0,
        "capBytes": crate::api::MAX_INSTINCT_BYTES,
        "notice": (dropped > 0).then(|| format!(
            "{dropped} active instinct(s) do not fit the {} byte prompt cap and are not sent to the model",
            crate::api::MAX_INSTINCT_BYTES
        )),
    }))
}

fn write_approved_proposal_instinct(
    state: &DaemonState,
    proposal: &rightkit_agent_learning::Proposal,
    edited_rule: Option<&str>,
) -> Result<bool, String> {
    let Some(mut instinct) = proposal_to_instinct(proposal) else {
        return Ok(false);
    };
    if let Some(rule) = edited_rule {
        instinct.rule = rule.to_string();
    }
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

fn proposal_to_instinct(
    proposal: &rightkit_agent_learning::Proposal,
) -> Option<rightkit_agent_learning::Instinct> {
    if !matches!(
        proposal.kind,
        rightkit_agent_learning::ProposalKind::HarnessRule
            | rightkit_agent_learning::ProposalKind::Memory
    ) {
        return None;
    }
    let rule = proposal.summary.trim();
    if rule.is_empty() {
        return None;
    }
    Some(rightkit_agent_learning::Instinct {
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
    store: &rightkit_agent_learning::InstinctStore,
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
