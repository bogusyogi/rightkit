fn command_spec_new(
    state: &DaemonState,
    args: &[String],
    active_session_id: Option<&str>,
) -> CommandResult {
    let (workspace_arg, rest) = command_spec_workspace_and_rest(args);
    let description = rest.join(" ");
    if description.trim().is_empty() {
        return command_err("Usage: /spec new [--workspace <path>] <description>");
    }
    let workspace = workspace_arg.unwrap_or_else(|| {
        active_session_id
            .and_then(|id| state.get_session(id))
            .map(|session| session.workspace_root.clone())
            .filter(|root| !root.trim().is_empty())
            .unwrap_or_else(|| {
                std::env::current_dir()
                    .unwrap_or_else(|_| PathBuf::from("."))
                    .display()
                    .to_string()
            })
    });
    let spec = match crate::api_specs::build_basic_spec(&workspace, None, &description, None) {
        Ok(spec) => spec,
        Err(error) => return command_err(format!("Spec creation failed: {error}")),
    };
    let persist_result = {
        let mut store = lock_recover(&state.specs, "state.specs");
        store.upsert_spec(spec.clone());
        persist_slash_spec_store(state, &store)
    };
    if let Err(error) = persist_result {
        return command_err(format!(
            "Spec created in memory but could not be persisted: {error}"
        ));
    }
    command_ok(format!(
        "**Spec created:** `{}`\nTitle: {}\nWorkspace: {}\nStatus: draft",
        spec.id, spec.title, spec.workspace_root
    ))
}

fn command_spec_status(state: &DaemonState, spec_id: &str) -> CommandResult {
    let store = lock_recover(&state.specs, "state.specs");
    let Some(spec) = store.spec(spec_id) else {
        return command_err(format!("Spec not found: `{spec_id}`"));
    };
    let tasks = store.tasks(spec_id);
    let latest = store.latest_analysis(spec_id);
    let mut output = format!(
        "**Spec:** `{}` [{}]\nTitle: {}\nWorkspace: {}\nTasks: {}\nRequirements: {}\nSuccess criteria: {}",
        spec.id,
        slash_spec_status(&spec.status),
        spec.title,
        spec.workspace_root,
        tasks.len(),
        spec.functional_requirements.len(),
        spec.success_criteria.len()
    );
    if let Some(run) = latest {
        output.push_str(&format!(
            "\nLatest analysis: `{}` [{}], findings: {}",
            run.id,
            slash_analysis_status(&run.status),
            run.findings.len()
        ));
    } else {
        output.push_str("\nLatest analysis: none");
    }
    command_ok(output)
}

fn command_spec_analyze(state: &DaemonState, spec_id: &str) -> CommandResult {
    let (run, persist_result) = {
        let mut store = lock_recover(&state.specs, "state.specs");
        let Some(spec) = store.spec(spec_id) else {
            return command_err(format!("Spec not found: `{spec_id}`"));
        };
        let tasks = store.tasks(spec_id);
        let run = rightkit_evidence::analyze_spec(
            &spec,
            &tasks,
            format!("spec-slash-analysis-{}", crate::api::now_millis()),
            crate::api::now_iso(),
        );
        store.record_analysis(run.clone());
        let persist_result = persist_slash_spec_store(state, &store);
        (run, persist_result)
    };
    if let Err(error) = persist_result {
        return command_err(format!(
            "Spec analysis ran but could not be persisted: {error}"
        ));
    }
    let mut output = format!(
        "**Spec analysis:** `{}` [{}]\nRequirements mapped: {}/{}\nBuildable criteria proven: {}/{}",
        run.id,
        slash_analysis_status(&run.status),
        run.coverage.requirements_with_tasks,
        run.coverage.requirement_count,
        run.coverage.buildable_criteria_with_evidence,
        run.coverage.success_criteria_count
    );
    for finding in run.findings.iter().take(12) {
        output.push_str(&format!(
            "\n- {:?} `{}` {}: {}",
            finding.severity, finding.source_id, finding.kind, finding.message
        ));
    }
    if run.findings.len() > 12 {
        output.push_str(&format!(
            "\n- ... {} more findings",
            run.findings.len() - 12
        ));
    }
    command_ok(output)
}

fn command_spec_trace(state: &DaemonState, spec_id: &str) -> CommandResult {
    let store = lock_recover(&state.specs, "state.specs");
    let Some(spec) = store.spec(spec_id) else {
        return command_err(format!("Spec not found: `{spec_id}`"));
    };
    let tasks = store.tasks(spec_id);
    let trace = rightkit_evidence::build_trace(
        &spec,
        &tasks,
        store.latest_analysis(spec_id),
        crate::api::now_iso(),
    );
    let mut output = format!(
        "**Spec trace:** `{}`\nLinks: {}\nEvidence: {}",
        trace.spec_id,
        trace.links.len(),
        trace.evidence.len()
    );
    for link in trace.links.iter().take(20) {
        output.push_str(&format!(
            "\n- {:?}:`{}` -> {:?}:`{}` ({:.2})",
            link.from_kind, link.from_id, link.to_kind, link.to_id, link.confidence
        ));
    }
    if trace.links.len() > 20 {
        output.push_str(&format!("\n- ... {} more links", trace.links.len() - 20));
    }
    command_ok(output)
}

fn command_spec_clarify(state: &DaemonState, spec_id: &str) -> CommandResult {
    let store = lock_recover(&state.specs, "state.specs");
    let Some(spec) = store.spec(spec_id) else {
        return command_err(format!("Spec not found: `{spec_id}`"));
    };
    let mut questions = Vec::new();
    for req in &spec.functional_requirements {
        if req.needs_clarification || slash_looks_placeholder(&req.text) {
            questions.push(format!(
                "- [required] `{}` What exact behavior, edge cases, and acceptance evidence are required?",
                req.id
            ));
        }
    }
    for story in &spec.user_stories {
        if slash_looks_placeholder(&story.independent_test) {
            questions.push(format!(
                "- [optional] `{}` What independent user-observable test proves this story is complete?",
                story.id
            ));
        }
    }
    let blocking = questions
        .iter()
        .any(|question| question.contains("[required]"));
    let mut output = format!(
        "**Spec clarify:** `{}`\nBlocking: {}\nNext action: {}",
        spec.id,
        blocking,
        if questions.is_empty() {
            "plan"
        } else {
            "answer clarifications"
        }
    );
    if questions.is_empty() {
        output.push_str("\nNo clarification questions found.");
    } else {
        output.push('\n');
        output.push_str(&questions.join("\n"));
    }
    command_ok(output)
}
