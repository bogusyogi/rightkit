fn command_spec_plan(state: &DaemonState, spec_id: &str, args: &[String]) -> CommandResult {
    let (constraints, include_verification) = command_spec_plan_args(args);
    let (created_count, skipped_count, tasks_len, analysis, persist_result) = {
        let mut store = lock_recover(&state.specs, "state.specs");
        let Some(spec) = store.spec(spec_id) else {
            return command_err(format!("Spec not found: `{spec_id}`"));
        };
        let existing = store.tasks(spec_id);
        let generated = slash_generated_plan_tasks(
            &spec,
            &existing,
            constraints.as_deref(),
            include_verification,
            crate::api::now_iso(),
        );
        let created_count = generated.len();
        let skipped_count = spec
            .functional_requirements
            .iter()
            .filter(|req| {
                existing
                    .iter()
                    .any(|task| task.requirement_ids.iter().any(|id| id == &req.id))
            })
            .count();
        if !generated.is_empty() {
            store.add_tasks(spec_id, generated);
        }
        let tasks = store.tasks(spec_id);
        let decisions = store.constitution_decisions(spec_id);
        let analysis = rightkit_evidence::analyze_spec_with_constitution_decisions(
            &spec,
            &tasks,
            &decisions,
            format!("spec-slash-plan-{}", crate::api::now_millis()),
            crate::api::now_iso(),
        );
        store.record_analysis(analysis.clone());
        if let Some(updated) = store.spec_mut(spec_id) {
            updated.status = rightkit_evidence::SpecStatus::Planned;
            updated.updated_at = crate::api::now_iso();
        }
        let persist_result = persist_slash_spec_store(state, &store);
        (
            created_count,
            skipped_count,
            tasks.len(),
            analysis,
            persist_result,
        )
    };
    if let Err(error) = persist_result {
        return command_err(format!("Spec plan ran but could not be persisted: {error}"));
    }
    command_ok(format!(
        "**Spec plan:** `{spec_id}`\nCreated tasks: {created_count}\nSkipped existing requirements: {skipped_count}\nTotal tasks: {tasks_len}\nAnalysis: {} with {} findings",
        slash_analysis_status(&analysis.status),
        analysis.findings.len()
    ))
}

fn command_spec_checklist(state: &DaemonState, spec_id: &str, args: &[String]) -> CommandResult {
    let focus = command_option_value(args, "--focus");
    let (ready, blocker_count, warning_count, findings, persist_result) = {
        let mut store = lock_recover(&state.specs, "state.specs");
        let Some(spec) = store.spec(spec_id) else {
            return command_err(format!("Spec not found: `{spec_id}`"));
        };
        let tasks = store.tasks(spec_id);
        let decisions = store.constitution_decisions(spec_id);
        let analysis = rightkit_evidence::analyze_spec_with_constitution_decisions(
            &spec,
            &tasks,
            &decisions,
            format!("spec-slash-checklist-{}", crate::api::now_millis()),
            crate::api::now_iso(),
        );
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
        let findings = analysis.findings.clone();
        store.record_analysis(analysis);
        let persist_result = persist_slash_spec_store(state, &store);
        (
            blocker_count == 0,
            blocker_count,
            warning_count,
            findings,
            persist_result,
        )
    };
    if let Err(error) = persist_result {
        return command_err(format!(
            "Spec checklist ran but could not be persisted: {error}"
        ));
    }
    let mut output = format!(
        "**Spec checklist:** `{spec_id}`{}\nReady: {ready}\nBlockers: {blocker_count}\nWarnings: {warning_count}",
        focus
            .as_deref()
            .map(|value| format!(" ({value})"))
            .unwrap_or_default()
    );
    for finding in findings.iter().take(10) {
        output.push_str(&format!(
            "\n- {:?} `{}` {}: {}",
            finding.severity, finding.source_id, finding.kind, finding.message
        ));
    }
    if findings.len() > 10 {
        output.push_str(&format!("\n- ... {} more findings", findings.len() - 10));
    }
    command_ok(output)
}

async fn command_spec_implement(
    state: &DaemonState,
    spec_id: &str,
    args: &[String],
) -> CommandResult {
    let dry_run = args.iter().any(|arg| arg == "--dry-run");
    let title = command_option_value(args, "--title");
    let (spec, analysis, prompt, persist_result) = {
        let mut store = lock_recover(&state.specs, "state.specs");
        let Some(spec) = store.spec(spec_id) else {
            return command_err(format!("Spec not found: `{spec_id}`"));
        };
        let tasks = store.tasks(spec_id);
        let decisions = store.constitution_decisions(spec_id);
        let analysis = rightkit_evidence::analyze_spec_with_constitution_decisions(
            &spec,
            &tasks,
            &decisions,
            format!("spec-slash-implement-{}", crate::api::now_millis()),
            crate::api::now_iso(),
        );
        let prompt = slash_implementation_prompt(&spec, &tasks, &analysis);
        store.record_analysis(analysis.clone());
        if analysis.status != rightkit_evidence::AnalysisStatus::Block && !dry_run {
            if let Some(updated) = store.spec_mut(spec_id) {
                updated.status = rightkit_evidence::SpecStatus::Implementing;
                updated.updated_at = crate::api::now_iso();
            }
        }
        let persist_result = persist_slash_spec_store(state, &store);
        (spec, analysis, prompt, persist_result)
    };
    if let Err(error) = persist_result {
        return command_err(format!(
            "Spec implementation check ran but could not be persisted: {error}"
        ));
    }
    if analysis.status == rightkit_evidence::AnalysisStatus::Block {
        return command_err(format!(
            "Spec cannot enter implementation while critical checklist blockers remain. Findings: {}",
            analysis.findings.len()
        ));
    }
    if dry_run {
        return command_ok(format!(
            "**Spec implement dry-run:** `{spec_id}`\nPrompt bytes: {}\nAnalysis: {}",
            prompt.len(),
            slash_analysis_status(&analysis.status)
        ));
    }
    let session_id = format!("impl-{}", crate::api::now_millis());
    let live = state.create_session(
        &session_id,
        Some("spec-implementer"),
        &spec.workspace_root,
        title
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("Spec implementation"),
        None,
    );
    *lock_recover(&live.spec_id, "live.spec_id") = Some(spec_id.to_string());
    let accepted = state
        .submit_text_with_trust(
            &session_id,
            &prompt,
            rightkit_agent_engine_protocol::ContentTrust::WorkspaceTrusted,
            "spec-implement",
        )
        .await
        .is_ok();
    command_ok(format!(
        "**Spec implementation session:** `{session_id}`\nSpec: `{spec_id}`\nPrompt accepted: {accepted}"
    ))
}
