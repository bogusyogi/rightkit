/// Export captured high-quality run trajectories as OpenAI chat fine-tuning
/// JSONL (one record per line). `?min_gds=` filters by quality (default 0.90).
async fn export_finetuning(
    State(state): State<DaemonState>,
    Query(q): Query<FinetuneExportQuery>,
) -> impl axum::response::IntoResponse {
    let min_gds = q.min_gds.unwrap_or(0.90);
    let body = state.trajectories.export_jsonl(min_gds);
    (
        [(axum::http::header::CONTENT_TYPE, "application/x-ndjson")],
        body,
    )
}

async fn get_audit(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    let entries = state.audit.entries();
    let verified = crate::audit::AuditLog::verify(&entries);
    Json(serde_json::json!({
        "verified": verified,
        "entries": entries,
    }))
}

async fn get_runtime_middleware(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    crate::api_plugins::sync_runtime_middleware_from_plugins(&state);
    let registry = lock_recover(&state.runtime_middleware, "state.runtime_middleware");
    Json(serde_json::to_value(registry.snapshot()).unwrap_or(serde_json::Value::Null))
}

async fn set_runtime_middleware_enabled(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SetMiddlewareEnabledBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    require_license_feature(&state, "evolution")?;
    let mut registry = lock_recover(&state.runtime_middleware, "state.runtime_middleware");
    let Some(middleware) = registry.set_enabled(&id, body.enabled) else {
        return Err(StatusCode::NOT_FOUND);
    };
    Ok(Json(serde_json::json!({
        "updated": true,
        "middleware": middleware,
    })))
}

async fn get_runtime_spans(
    State(state): State<DaemonState>,
    Query(q): Query<RuntimeSessionQuery>,
) -> Json<serde_json::Value> {
    let registry = lock_recover(&state.runtime_middleware, "state.runtime_middleware");
    Json(serde_json::json!({
        "spans": registry.spans(q.session_id.as_deref()),
    }))
}

async fn get_runtime_span(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let registry = lock_recover(&state.runtime_middleware, "state.runtime_middleware");
    let Some(span) = registry.span(&id) else {
        return Err(StatusCode::NOT_FOUND);
    };
    Ok(Json(
        serde_json::to_value(span).unwrap_or(serde_json::Value::Null),
    ))
}

async fn get_runtime_observations(
    State(state): State<DaemonState>,
    Query(q): Query<RuntimeSessionQuery>,
) -> Json<serde_json::Value> {
    let registry = lock_recover(&state.runtime_middleware, "state.runtime_middleware");
    Json(serde_json::json!({
        "observations": registry.observations(q.session_id.as_deref()),
    }))
}

/// Run the CoEvoSkills synthesis loop with real LLM callbacks.
///
/// Takes a task description and optional template, runs synthesize -> jury-verify
/// -> refine cycles (up to 4), and returns the resulting `SkillBundle` as JSON.
/// Returns 503 if no provider is configured.
async fn synthesize_skill_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<SynthesizeSkillBody>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    use rightkit_adapt::types::{Proposal, ProposalKind, ProposalStatus};

    let cfg = state
        .engine_cfg
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let loop_ = crate::skill_loop::build_skill_synthesis_loop(cfg);

    let proposal = Proposal {
        id: format!("synth-{}", now_millis()),
        kind: ProposalKind::Skill,
        summary: body.task_description.clone(),
        evidence: if body.template.is_empty() {
            "from synthesis endpoint".to_string()
        } else {
            format!("template: {}", body.template)
        },
        score: 0.0,
        status: ProposalStatus::Candidate,
        created_at: now_iso(),
        age_sessions: 0,
        rejection_reason: None,
    };

    let bundle = loop_.synthesize(&[&proposal]).await;
    Ok(Json(
        serde_json::to_value(&bundle).unwrap_or(serde_json::Value::Null),
    ))
}
