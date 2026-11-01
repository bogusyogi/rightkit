fn generated_plan_tasks(
    spec: &FeatureSpec,
    existing_tasks: &[FeatureTask],
    request: &PlanSpecRequest,
    now: String,
) -> Vec<FeatureTask> {
    let mut out = Vec::new();
    let mut next_id = existing_tasks.len() + 1;
    let constraints = request
        .constraints
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    for req in &spec.functional_requirements {
        if existing_tasks
            .iter()
            .chain(out.iter())
            .any(|task| task.requirement_ids.iter().any(|id| id == &req.id))
        {
            continue;
        }
        let mut description = format!("Implement requirement {}: {}", req.id, req.text);
        if let Some(constraints) = constraints {
            description.push_str(&format!(" Constraints: {constraints}"));
        }
        out.push(FeatureTask {
            id: format!("T{next_id:03}"),
            spec_id: spec.id.clone(),
            story_id: req.source_story_id.clone(),
            requirement_ids: vec![req.id.clone()],
            success_criterion_ids: Vec::new(),
            foundation_phase: None,
            description,
            paths: Vec::new(),
            parallelizable: true,
            status: TaskStatus::Todo,
            evidence_ids: Vec::new(),
            created_at: now.clone(),
            updated_at: now.clone(),
        });
        next_id += 1;
    }

    if request.include_verification {
        for criterion in spec
            .success_criteria
            .iter()
            .filter(|criterion| criterion.buildable)
        {
            if existing_tasks.iter().chain(out.iter()).any(|task| {
                task.success_criterion_ids
                    .iter()
                    .any(|id| id == &criterion.id)
                    && !task.evidence_ids.is_empty()
            }) {
                continue;
            }
            out.push(FeatureTask {
                id: format!("T{next_id:03}"),
                spec_id: spec.id.clone(),
                story_id: None,
                requirement_ids: Vec::new(),
                success_criterion_ids: vec![criterion.id.clone()],
                foundation_phase: Some("verification".to_string()),
                description: format!(
                    "Produce verification evidence for success criterion {}: {}",
                    criterion.id, criterion.text
                ),
                paths: Vec::new(),
                parallelizable: false,
                status: TaskStatus::Todo,
                evidence_ids: vec![format!("EV-{}", criterion.id)],
                created_at: now.clone(),
                updated_at: now.clone(),
            });
            next_id += 1;
        }
    }

    out
}

fn checklist_items(
    spec: &FeatureSpec,
    tasks: &[FeatureTask],
    analysis: &rightkit_evidence::SpecAnalysisRun,
    focus: Option<&str>,
) -> Vec<SpecChecklistItem> {
    let mut items = Vec::new();
    for req in &spec.functional_requirements {
        let mapped = tasks
            .iter()
            .any(|task| task.requirement_ids.iter().any(|id| id == &req.id));
        items.push(SpecChecklistItem {
            id: format!("check-{}", req.id),
            label: format!("Requirement {} has implementation task coverage", req.id),
            status: if mapped { "done" } else { "blocked" }.to_string(),
            source_kind: rightkit_evidence::TraceKind::Requirement,
            source_id: req.id.clone(),
            detail: if mapped {
                "At least one task maps to this requirement.".to_string()
            } else {
                "No task maps to this requirement yet.".to_string()
            },
        });
    }

    for criterion in spec
        .success_criteria
        .iter()
        .filter(|criterion| criterion.buildable)
    {
        let evidenced = tasks.iter().any(|task| {
            task.success_criterion_ids
                .iter()
                .any(|id| id == &criterion.id)
                && !task.evidence_ids.is_empty()
        });
        items.push(SpecChecklistItem {
            id: format!("check-{}", criterion.id),
            label: format!(
                "Success criterion {} has evidence task coverage",
                criterion.id
            ),
            status: if evidenced { "done" } else { "warn" }.to_string(),
            source_kind: rightkit_evidence::TraceKind::SuccessCriterion,
            source_id: criterion.id.clone(),
            detail: if evidenced {
                "At least one mapped task declares evidence for this criterion.".to_string()
            } else {
                "No evidence-bearing task maps to this criterion yet.".to_string()
            },
        });
    }

    for task in tasks {
        let mapped = task.story_id.is_some()
            || !task.requirement_ids.is_empty()
            || !task.success_criterion_ids.is_empty()
            || task.foundation_phase.is_some();
        items.push(SpecChecklistItem {
            id: format!("check-{}", task.id),
            label: format!("Task {} is trace-mapped", task.id),
            status: if mapped { "done" } else { "blocked" }.to_string(),
            source_kind: rightkit_evidence::TraceKind::Task,
            source_id: task.id.clone(),
            detail: if mapped {
                "Task has a story, requirement, criterion, or foundation-phase mapping.".to_string()
            } else {
                "Task has no story, requirement, criterion, or foundation-phase mapping."
                    .to_string()
            },
        });
    }

    for finding in &analysis.findings {
        if matches!(finding.severity, rightkit_evidence::FindingSeverity::Info) {
            continue;
        }
        items.push(SpecChecklistItem {
            id: format!("finding-{}", finding.id),
            label: finding.kind.clone(),
            status: match finding.severity {
                rightkit_evidence::FindingSeverity::Critical => "blocked",
                rightkit_evidence::FindingSeverity::Warning => "warn",
                rightkit_evidence::FindingSeverity::Info => "open",
            }
            .to_string(),
            source_kind: finding.source_kind.clone(),
            source_id: finding.source_id.clone(),
            detail: finding.message.clone(),
        });
    }

    if let Some(focus) = focus.map(str::trim).filter(|value| !value.is_empty()) {
        let needle = focus.to_ascii_lowercase();
        items.retain(|item| {
            item.id.to_ascii_lowercase().contains(&needle)
                || item.label.to_ascii_lowercase().contains(&needle)
                || item.source_id.to_ascii_lowercase().contains(&needle)
                || item.detail.to_ascii_lowercase().contains(&needle)
        });
    }
    items
}

fn implementation_prompt(
    spec: &FeatureSpec,
    tasks: &[FeatureTask],
    analysis: &rightkit_evidence::SpecAnalysisRun,
) -> String {
    let mut prompt = format!(
        "Implement the bound RightKit feature spec.\n\nSpec: {} ({})\nWorkspace: {}\nStatus: {:?}\n\n",
        spec.title, spec.id, spec.workspace_root, spec.status
    );
    prompt.push_str("Requirements:\n");
    for req in &spec.functional_requirements {
        prompt.push_str(&format!("- {}: {}\n", req.id, req.text));
    }
    prompt.push_str("\nTasks:\n");
    for task in tasks {
        prompt.push_str(&format!(
            "- {}: {} [req: {}; criteria: {}; evidence: {}]\n",
            task.id,
            task.description,
            task.requirement_ids.join(", "),
            task.success_criterion_ids.join(", "),
            task.evidence_ids.join(", ")
        ));
    }
    prompt.push_str("\nCompletion contract:\n");
    prompt.push_str("- Keep this session bound to the spec id in all implementation evidence.\n");
    prompt.push_str(
        "- Produce verification evidence for mapped success criteria before marking done.\n",
    );
    prompt.push_str(&format!(
        "- Current analysis status: {:?}; blockers: {}.\n",
        analysis.status,
        analysis
            .findings
            .iter()
            .filter(|finding| {
                matches!(finding.severity, rightkit_evidence::FindingSeverity::Critical)
            })
            .count()
    ));
    prompt
}

fn looks_placeholder(text: &str) -> bool {
    let t = text.trim().to_ascii_lowercase();
    t.is_empty()
        || matches!(
            t.as_str(),
            "todo" | "tbd" | "placeholder" | "lorem ipsum" | "n/a" | "na"
        )
        || t.contains("todo")
        || t.contains("placeholder")
        || t.contains("to be decided")
}
