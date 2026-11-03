async fn checklist_spec_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ChecklistSpecRequest>,
) -> Result<Json<SpecChecklistResponse>, SpecError> {
    enforce_spec_trusted_source_authority(&headers)?;
    let mut store = lock_recover(&state.specs, "state.specs");
    let spec = store
        .spec(&id)
        .ok_or_else(|| SpecError::not_found("spec not found"))?;
    let tasks = store.tasks(&id);
    let decisions = store.constitution_decisions(&id);
    let analysis = analyze_spec_with_constitution_decisions(
        &spec,
        &tasks,
        &decisions,
        format!("sar-{}", now_millis()),
        now_iso(),
    );
    let items = checklist_items(&spec, &tasks, &analysis, body.focus.as_deref());
    let blocker_count = analysis
        .findings
        .iter()
        .filter(|finding| {
            matches!(
                finding.severity,
                rightkit_evidence::FindingSeverity::Critical
            )
        })
        .count();
    let warning_count = analysis
        .findings
        .iter()
        .filter(|finding| {
            matches!(
                finding.severity,
                rightkit_evidence::FindingSeverity::Warning
            )
        })
        .count();
    store.record_analysis(analysis.clone());
    persist_spec_store(&state, &store)?;
    Ok(Json(SpecChecklistResponse {
        spec_id: id,
        focus: body.focus,
        ready: blocker_count == 0,
        blocker_count,
        warning_count,
        items,
        analysis,
    }))
}

async fn analyze_spec_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<rightkit_evidence::SpecAnalysisRun>, SpecError> {
    enforce_spec_trusted_source_authority(&headers)?;
    let mut store = lock_recover(&state.specs, "state.specs");
    let spec = store
        .spec(&id)
        .ok_or_else(|| SpecError::not_found("spec not found"))?;
    let tasks = store.tasks(&id);
    let decisions = store.constitution_decisions(&id);
    let run = analyze_spec_with_constitution_decisions(
        &spec,
        &tasks,
        &decisions,
        format!("sar-{}", now_millis()),
        now_iso(),
    );
    store.record_analysis(run.clone());
    persist_spec_store(&state, &store)?;
    Ok(Json(run))
}

async fn implement_spec_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ImplementSpecRequest>,
) -> Result<Json<SpecImplementResponse>, SpecError> {
    enforce_spec_trusted_source_authority(&headers)?;
    let (spec, analysis, prompt, blocked) = {
        let mut store = lock_recover(&state.specs, "state.specs");
        let spec = store
            .spec(&id)
            .ok_or_else(|| SpecError::not_found("spec not found"))?;
        let tasks = store.tasks(&id);
        let decisions = store.constitution_decisions(&id);
        let analysis = analyze_spec_with_constitution_decisions(
            &spec,
            &tasks,
            &decisions,
            format!("sar-{}", now_millis()),
            now_iso(),
        );
        let prompt = implementation_prompt(&spec, &tasks, &analysis);
        let blocked = analysis.status == AnalysisStatus::Block;
        store.record_analysis(analysis.clone());
        if !blocked {
            if let Some(updated) = store.spec_mut(&id) {
                updated.status = SpecStatus::Implementing;
                updated.updated_at = now_iso();
            }
        }
        persist_spec_store(&state, &store)?;
        (spec, analysis, prompt, blocked)
    };

    if blocked {
        return Err(SpecError::conflict(
            "spec cannot enter implementation while critical checklist blockers remain",
        ));
    }

    if body.dry_run {
        return Ok(Json(SpecImplementResponse {
            spec_id: id,
            session_id: None,
            accepted: false,
            dry_run: true,
            prompt,
            analysis,
        }));
    }

    let session_id = format!("impl-{}", now_millis());
    let title = body
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("Implement {}", spec.title));
    let live = state.create_session(
        &session_id,
        Some("spec-implementer"),
        &spec.workspace_root,
        &title,
        None,
    );
    *lock_recover(&live.spec_id, "live.spec_id") = Some(id.clone());
    if let Some(storage) = &state.storage {
        let storage2 = storage.clone();
        let id2 = session_id.clone();
        let spec_id2 = id.clone();
        let now = now_iso();
        tokio::task::spawn_blocking(move || {
            let store = storage2.blocking_lock();
            let _ = store.update_session_config_string(&id2, "specId", Some(&spec_id2), &now);
        })
        .await
        .map_err(|err| {
            SpecError::internal_error(format!("failed to bind implementation session: {err}"))
        })?;
    }
    let accepted = state
        .submit_text_with_trust(
            &session_id,
            &prompt,
            rightkit_agent_engine_protocol::ContentTrust::WorkspaceTrusted,
            "spec-implement",
        )
        .await
        .is_ok();
    Ok(Json(SpecImplementResponse {
        spec_id: id,
        session_id: Some(session_id),
        accepted,
        dry_run: false,
        prompt,
        analysis,
    }))
}

async fn get_spec_trace_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<rightkit_evidence::SpecTrace>, SpecError> {
    let (spec, tasks, latest, decisions) = {
        let store = lock_recover(&state.specs, "state.specs");
        let spec = store
            .spec(&id)
            .ok_or_else(|| SpecError::not_found("spec not found"))?;
        let tasks = store.tasks(&id);
        let latest = store.latest_analysis(&id);
        let decisions = store.constitution_decisions(&id);
        (spec, tasks, latest, decisions)
    };
    let mut evidence = live_spec_evidence(&state, &spec, &tasks);
    evidence.extend(constitution_decision_spec_evidence(
        &spec, &tasks, &decisions,
    ));
    evidence.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Json(build_trace_with_evidence(
        &spec,
        &tasks,
        latest,
        now_iso(),
        evidence,
    )))
}
