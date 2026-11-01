async fn list_specs_handler(
    State(state): State<DaemonState>,
    Query(query): Query<ListSpecsQuery>,
) -> Json<Vec<FeatureSpec>> {
    let store = lock_recover(&state.specs, "state.specs");
    Json(store.list_specs(query.workspace_root.as_deref()))
}

async fn create_spec_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<CreateSpecRequest>,
) -> Result<(StatusCode, Json<FeatureSpec>), SpecError> {
    enforce_spec_trusted_source_authority(&headers)?;
    let spec = if body.user_stories.is_empty()
        && body.functional_requirements.is_empty()
        && body.success_criteria.is_empty()
    {
        let mut spec = build_basic_spec(
            &body.workspace_root,
            body.title.as_deref(),
            &body.description,
            body.branch.as_deref(),
        )
        .map_err(SpecError::bad_request)?;
        spec.assumptions = body.assumptions.clone();
        spec.edge_cases = body.edge_cases.clone();
        spec
    } else {
        let now = now_iso();
        let id = format!("spec-{}", now_millis());
        let title = body
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| first_sentence(&body.description));
        FeatureSpec {
            id,
            workspace_root: body.workspace_root.clone(),
            branch: body.branch.clone(),
            title,
            status: SpecStatus::Draft,
            source_prompt_hash: rightkit_fs::digest::sha256_bytes(body.description.as_bytes()),
            user_stories: materialize_user_stories(&body),
            functional_requirements: materialize_requirements(&body),
            success_criteria: materialize_success_criteria(&body),
            assumptions: body.assumptions.clone(),
            edge_cases: body.edge_cases.clone(),
            constitution_sources: load_constitution_sources(&body.workspace_root),
            created_at: now.clone(),
            updated_at: now,
        }
    };

    if spec_has_placeholder_heavy_content(&spec) {
        return Err(SpecError::bad_request(
            "spec contains too much placeholder content to govern implementation",
        ));
    }

    {
        let mut store = lock_recover(&state.specs, "state.specs");
        store.upsert_spec(spec.clone());
        persist_spec_store(&state, &store)?;
    }
    Ok((StatusCode::CREATED, Json(spec)))
}

async fn get_spec_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<SpecEnvelope>, SpecError> {
    let store = lock_recover(&state.specs, "state.specs");
    let spec = store
        .spec(&id)
        .ok_or_else(|| SpecError::not_found("spec not found"))?;
    let tasks = store.tasks(&id);
    Ok(Json(SpecEnvelope { spec, tasks }))
}

async fn patch_spec_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<PatchSpecRequest>,
) -> Result<Json<FeatureSpec>, SpecError> {
    enforce_spec_trusted_source_authority(&headers)?;
    if body
        .title
        .as_ref()
        .is_some_and(|title| title.trim().is_empty())
    {
        return Err(SpecError::bad_request("title cannot be empty"));
    }

    let mut store = lock_recover(&state.specs, "state.specs");
    if body
        .status
        .as_ref()
        .is_some_and(|status| status == &SpecStatus::Done)
    {
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
        let status = run.status.clone();
        let blocking_findings = run
            .findings
            .iter()
            .filter(|finding| {
                matches!(finding.severity, rightkit_evidence::specs::FindingSeverity::Critical)
            })
            .map(|finding| finding.kind.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        store.record_analysis(run);
        persist_spec_store(&state, &store)?;
        if status == AnalysisStatus::Block {
            return Err(SpecError::conflict(format!(
                "spec cannot be marked done until blocking analysis findings are resolved: {blocking_findings}"
            )));
        }
    }

    let updated_spec = {
        let spec = store
            .spec_mut(&id)
            .ok_or_else(|| SpecError::not_found("spec not found"))?;
        if let Some(title) = body.title {
            spec.title = title;
        }
        if let Some(status) = body.status {
            spec.status = status;
        }
        if let Some(assumptions) = body.assumptions {
            spec.assumptions = assumptions;
        }
        if let Some(edge_cases) = body.edge_cases {
            spec.edge_cases = edge_cases;
        }
        spec.updated_at = now_iso();
        spec.clone()
    };
    persist_spec_store(&state, &store)?;
    Ok(Json(updated_spec))
}

async fn clarify_spec_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<SpecClarifyResponse>, SpecError> {
    let store = lock_recover(&state.specs, "state.specs");
    let spec = store
        .spec(&id)
        .ok_or_else(|| SpecError::not_found("spec not found"))?;
    let mut questions = Vec::new();
    for req in &spec.functional_requirements {
        if req.needs_clarification || looks_placeholder(&req.text) {
            questions.push(SpecClarification {
                id: format!("clarify-{}", req.id),
                source_kind: rightkit_evidence::specs::TraceKind::Requirement,
                source_id: req.id.clone(),
                question: format!(
                    "What exact behavior, edge cases, and acceptance evidence are required for {}?",
                    req.id
                ),
                required: true,
            });
        }
    }
    for story in &spec.user_stories {
        if looks_placeholder(&story.independent_test) {
            questions.push(SpecClarification {
                id: format!("clarify-{}", story.id),
                source_kind: rightkit_evidence::specs::TraceKind::Story,
                source_id: story.id.clone(),
                question: format!(
                    "What independent user-observable test proves story {} is complete?",
                    story.id
                ),
                required: false,
            });
        }
    }
    Ok(Json(SpecClarifyResponse {
        spec_id: id,
        blocking: questions.iter().any(|question| question.required),
        next_action: if questions.is_empty() {
            "plan".to_string()
        } else {
            "answer_clarifications".to_string()
        },
        questions,
    }))
}

async fn plan_spec_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<PlanSpecRequest>,
) -> Result<Json<SpecPlanResponse>, SpecError> {
    enforce_spec_trusted_source_authority(&headers)?;
    let mut store = lock_recover(&state.specs, "state.specs");
    let spec = store
        .spec(&id)
        .ok_or_else(|| SpecError::not_found("spec not found"))?;
    let existing_tasks = store.tasks(&id);
    let existing_count = existing_tasks.len();
    let now = now_iso();
    let generated = generated_plan_tasks(&spec, &existing_tasks, &body, now.clone());
    let skipped_existing_count = existing_tasks.len()
        + spec
            .functional_requirements
            .iter()
            .filter(|req| {
                existing_tasks
                    .iter()
                    .any(|task| task.requirement_ids.iter().any(|id| id == &req.id))
            })
            .count()
        - existing_count;
    let created_task_count = generated.len();
    if !generated.is_empty() {
        store.add_tasks(&id, generated);
    }
    let tasks = store.tasks(&id);
    let decisions = store.constitution_decisions(&id);
    let analysis = analyze_spec_with_constitution_decisions(
        &spec,
        &tasks,
        &decisions,
        format!("sar-{}", now_millis()),
        now_iso(),
    );
    store.record_analysis(analysis.clone());
    let updated_spec = {
        let spec = store
            .spec_mut(&id)
            .ok_or_else(|| SpecError::not_found("spec not found"))?;
        spec.status = SpecStatus::Planned;
        spec.updated_at = now;
        spec.clone()
    };
    persist_spec_store(&state, &store)?;
    Ok(Json(SpecPlanResponse {
        spec: updated_spec,
        tasks,
        created_task_count,
        skipped_existing_count,
        analysis,
    }))
}

async fn add_spec_tasks_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<AddTasksRequest>,
) -> Result<Json<Vec<FeatureTask>>, SpecError> {
    enforce_spec_trusted_source_authority(&headers)?;
    if body.tasks.is_empty() {
        return Err(SpecError::bad_request(
            "tasks must contain at least one task",
        ));
    }

    let mut store = lock_recover(&state.specs, "state.specs");
    if store.spec(&id).is_none() {
        return Err(SpecError::not_found("spec not found"));
    }
    let existing_count = store.tasks(&id).len();
    let now = now_iso();
    let tasks = body
        .tasks
        .into_iter()
        .enumerate()
        .map(|(index, input)| FeatureTask {
            id: input
                .id
                .unwrap_or_else(|| format!("T{:03}", existing_count + index + 1)),
            spec_id: id.clone(),
            story_id: input.story_id,
            requirement_ids: input.requirement_ids,
            success_criterion_ids: input.success_criterion_ids,
            foundation_phase: input.foundation_phase,
            description: input.description,
            paths: input.paths,
            parallelizable: input.parallelizable,
            status: input.status.unwrap_or(TaskStatus::Todo),
            evidence_ids: input.evidence_ids,
            created_at: now.clone(),
            updated_at: now.clone(),
        })
        .collect::<Vec<_>>();
    store.add_tasks(&id, tasks.clone());
    if let Some(updated) = store.spec_mut(&id) {
        updated.status = SpecStatus::Planned;
        updated.updated_at = now;
    }
    persist_spec_store(&state, &store)?;
    Ok(Json(tasks))
}

async fn list_constitution_decisions_handler(
    State(state): State<DaemonState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<ConstitutionDecision>>, SpecError> {
    let store = lock_recover(&state.specs, "state.specs");
    if store.spec(&id).is_none() {
        return Err(SpecError::not_found("spec not found"));
    }
    Ok(Json(store.constitution_decisions(&id)))
}

async fn record_constitution_decision_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<ConstitutionDecisionRequest>,
) -> Result<(StatusCode, Json<ConstitutionDecision>), SpecError> {
    enforce_spec_trusted_source_authority(&headers)?;
    if body.rule.trim().is_empty() {
        return Err(SpecError::bad_request("rule cannot be empty"));
    }
    if body.rationale.trim().is_empty() {
        return Err(SpecError::bad_request("rationale cannot be empty"));
    }
    if matches!(body.verdict, ConstitutionDecisionVerdict::Overridden)
        && body.evidence_ids.is_empty()
    {
        return Err(SpecError::bad_request(
            "constitution overrides must include at least one evidence id",
        ));
    }

    let mut store = lock_recover(&state.specs, "state.specs");
    let spec = store
        .spec(&id)
        .ok_or_else(|| SpecError::not_found("spec not found"))?;
    let source = resolve_constitution_source(
        &spec,
        body.source_path.as_deref(),
        body.source_sha256.as_deref(),
    )?;
    let now = now_iso();
    let decision = ConstitutionDecision {
        id: body
            .id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("constitution-decision-{}", now_millis())),
        spec_id: id.clone(),
        source_path: source.path,
        source_sha256: source.body_sha256,
        rule: body.rule.trim().to_string(),
        verdict: body.verdict,
        rationale: body.rationale.trim().to_string(),
        evidence_ids: body
            .evidence_ids
            .into_iter()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .collect(),
        created_at: now.clone(),
        updated_at: now,
    };
    store.add_constitution_decision(decision.clone());
    persist_spec_store(&state, &store)?;
    Ok((StatusCode::CREATED, Json(decision)))
}
