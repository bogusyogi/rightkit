fn command_spec_plan_args(args: &[String]) -> (Option<String>, bool) {
    let mut constraints = None;
    let mut include_verification = true;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--constraints" => {
                if let Some(value) = args.get(index + 1) {
                    constraints = Some(value.clone());
                    index += 2;
                    continue;
                }
            }
            "--no-verification" => {
                include_verification = false;
                index += 1;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    (constraints, include_verification)
}

fn command_option_value(args: &[String], flag: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair.first().is_some_and(|arg| arg == flag))
        .and_then(|pair| pair.get(1))
        .cloned()
}

fn slash_generated_plan_tasks(
    spec: &rightkit_evidence::FeatureSpec,
    existing_tasks: &[rightkit_evidence::FeatureTask],
    constraints: Option<&str>,
    include_verification: bool,
    now: String,
) -> Vec<rightkit_evidence::FeatureTask> {
    let mut out = Vec::new();
    let mut next_id = existing_tasks.len() + 1;
    let constraints = constraints.map(str::trim).filter(|value| !value.is_empty());

    for req in &spec.functional_requirements {
        if existing_tasks
            .iter()
            .chain(out.iter())
            .any(|task: &rightkit_evidence::FeatureTask| {
                task.requirement_ids.iter().any(|id| id == &req.id)
            })
        {
            continue;
        }
        let mut description = format!("Implement requirement {}: {}", req.id, req.text);
        if let Some(constraints) = constraints {
            description.push_str(&format!(" Constraints: {constraints}"));
        }
        out.push(rightkit_evidence::FeatureTask {
            id: format!("T{next_id:03}"),
            spec_id: spec.id.clone(),
            story_id: req.source_story_id.clone(),
            requirement_ids: vec![req.id.clone()],
            success_criterion_ids: Vec::new(),
            foundation_phase: None,
            description,
            paths: Vec::new(),
            parallelizable: true,
            status: rightkit_evidence::TaskStatus::Todo,
            evidence_ids: Vec::new(),
            created_at: now.clone(),
            updated_at: now.clone(),
        });
        next_id += 1;
    }

    if include_verification {
        for criterion in spec
            .success_criteria
            .iter()
            .filter(|criterion| criterion.buildable)
        {
            if existing_tasks
                .iter()
                .chain(out.iter())
                .any(|task: &rightkit_evidence::FeatureTask| {
                    task.success_criterion_ids
                        .iter()
                        .any(|id| id == &criterion.id)
                })
            {
                continue;
            }
            out.push(rightkit_evidence::FeatureTask {
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
                status: rightkit_evidence::TaskStatus::Todo,
                evidence_ids: vec![format!("EV-{}", criterion.id)],
                created_at: now.clone(),
                updated_at: now.clone(),
            });
            next_id += 1;
        }
    }
    out
}

fn slash_looks_placeholder(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("todo")
        || lower.contains("tbd")
        || lower.contains("placeholder")
        || lower.trim().len() < 8
}

fn slash_implementation_prompt(
    spec: &rightkit_evidence::FeatureSpec,
    tasks: &[rightkit_evidence::FeatureTask],
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
        prompt.push_str(&format!("- {}: {}\n", task.id, task.description));
    }
    prompt.push_str(&format!(
        "\nLatest analysis: {:?}, findings: {}\n",
        analysis.status,
        analysis.findings.len()
    ));
    prompt
}

fn command_spec_workspace_and_rest(args: &[String]) -> (Option<String>, Vec<String>) {
    let mut workspace = None;
    let mut rest = Vec::new();
    let mut index = 0;
    while index < args.len() {
        if args[index] == "--workspace" {
            if let Some(value) = args.get(index + 1) {
                workspace = Some(value.clone());
                index += 2;
                continue;
            }
        }
        rest.push(args[index].clone());
        index += 1;
    }
    (workspace, rest)
}

fn persist_slash_spec_store(
    state: &DaemonState,
    store: &rightkit_evidence::SpecStore,
) -> Result<(), String> {
    let Some(path) = state.spec_store_path.as_deref() else {
        return Ok(());
    };
    crate::api_specs::write_spec_store(path, store)
}

fn slash_spec_status(status: &rightkit_evidence::SpecStatus) -> &'static str {
    match status {
        rightkit_evidence::SpecStatus::Draft => "draft",
        rightkit_evidence::SpecStatus::Planned => "planned",
        rightkit_evidence::SpecStatus::Implementing => "implementing",
        rightkit_evidence::SpecStatus::Done => "done",
        rightkit_evidence::SpecStatus::Blocked => "blocked",
    }
}

fn slash_analysis_status(status: &rightkit_evidence::AnalysisStatus) -> &'static str {
    match status {
        rightkit_evidence::AnalysisStatus::Pass => "pass",
        rightkit_evidence::AnalysisStatus::Warn => "warn",
        rightkit_evidence::AnalysisStatus::Block => "block",
    }
}
