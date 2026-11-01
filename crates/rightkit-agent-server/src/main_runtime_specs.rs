async fn cli_spec(command: SpecCommand) -> i32 {
    match command {
        SpecCommand::List {
            workspace,
            store,
            json,
        } => {
            let store_path = cli_spec_store_path(store.as_deref());
            let store = api_specs::load_spec_store(&store_path);
            let rows = store.list_specs(workspace.as_deref());
            if json {
                print_json_value(serde_json::json!({
                    "storePath": store_path,
                    "workspaceRoot": workspace,
                    "count": rows.len(),
                    "specs": rows,
                }));
            } else if rows.is_empty() {
                println!("No specs in {}", store_path.display());
            } else {
                println!("Feature specs ({})", store_path.display());
                for spec in rows {
                    println!(
                        "- {} [{}] {} ({})",
                        spec.id,
                        format_spec_status(&spec.status),
                        spec.title,
                        spec.workspace_root
                    );
                }
            }
            0
        }
        SpecCommand::New {
            workspace,
            description,
            title,
            store,
            json,
        } => {
            let workspace_root = workspace.unwrap_or_else(default_cli_workspace);
            let store_path = cli_spec_store_path(store.as_deref());
            let mut store = api_specs::load_spec_store(&store_path);
            let spec = match api_specs::build_basic_spec(
                &workspace_root,
                title.as_deref(),
                &description,
                None,
            ) {
                Ok(spec) => spec,
                Err(error) => {
                    eprintln!("spec creation failed: {error}");
                    return 1;
                }
            };
            store.upsert_spec(spec.clone());
            if let Err(error) = api_specs::write_spec_store(&store_path, &store) {
                eprintln!("failed to persist spec store: {error}");
                return 1;
            }
            if json {
                print_json_value(serde_json::to_value(&spec).unwrap_or_default());
            } else {
                println!(
                    "Spec created: {} [{}]\nWorkspace: {}\nStore: {}",
                    spec.id,
                    spec.title,
                    spec.workspace_root,
                    store_path.display()
                );
            }
            0
        }
        SpecCommand::Status {
            spec_id,
            store,
            json,
        } => {
            let store_path = cli_spec_store_path(store.as_deref());
            let store = api_specs::load_spec_store(&store_path);
            let Some(spec) = store.spec(&spec_id) else {
                eprintln!("spec not found: {spec_id}");
                return 1;
            };
            let tasks = store.tasks(&spec_id);
            let latest = store.latest_analysis(&spec_id);
            if json {
                print_json_value(serde_json::json!({
                    "storePath": store_path,
                    "spec": spec,
                    "tasks": tasks,
                    "latestAnalysis": latest,
                }));
            } else {
                print_spec_status(&spec, tasks.len(), latest.as_ref(), &store_path);
            }
            0
        }
        SpecCommand::Analyze {
            spec_id,
            store,
            json,
        } => {
            let store_path = cli_spec_store_path(store.as_deref());
            let mut store = api_specs::load_spec_store(&store_path);
            let Some(spec) = store.spec(&spec_id) else {
                eprintln!("spec not found: {spec_id}");
                return 1;
            };
            let tasks = store.tasks(&spec_id);
            let run = rightkit_evidence::analyze_spec(
                &spec,
                &tasks,
                format!("spec-cli-analysis-{}", unix_millis()),
                now_iso(),
            );
            store.record_analysis(run.clone());
            if let Err(error) = api_specs::write_spec_store(&store_path, &store) {
                eprintln!("failed to persist spec analysis: {error}");
                return 1;
            }
            if json {
                print_json_value(serde_json::to_value(&run).unwrap_or_default());
            } else {
                println!(
                    "Spec analysis: {} [{}]\nRequirements mapped: {}/{}\nBuildable criteria proven: {}/{}",
                    run.id,
                    format_analysis_status(&run.status),
                    run.coverage.requirements_with_tasks,
                    run.coverage.requirement_count,
                    run.coverage.buildable_criteria_with_evidence,
                    run.coverage.success_criteria_count
                );
                for finding in &run.findings {
                    println!(
                        "- {:?} {} {}: {}",
                        finding.severity, finding.source_id, finding.kind, finding.message
                    );
                }
            }
            0
        }
        SpecCommand::Trace {
            spec_id,
            store,
            json,
        } => {
            let store_path = cli_spec_store_path(store.as_deref());
            let store = api_specs::load_spec_store(&store_path);
            let Some(spec) = store.spec(&spec_id) else {
                eprintln!("spec not found: {spec_id}");
                return 1;
            };
            let tasks = store.tasks(&spec_id);
            let trace = rightkit_evidence::build_trace(
                &spec,
                &tasks,
                store.latest_analysis(&spec_id),
                now_iso(),
            );
            if json {
                print_json_value(serde_json::to_value(&trace).unwrap_or_default());
            } else {
                println!(
                    "Spec trace: {}\nLinks: {}\nEvidence: {}\nStore: {}",
                    spec.id,
                    trace.links.len(),
                    trace.evidence.len(),
                    store_path.display()
                );
                for link in trace.links.iter().take(30) {
                    println!(
                        "- {:?}:{} -> {:?}:{} ({:.2})",
                        link.from_kind, link.from_id, link.to_kind, link.to_id, link.confidence
                    );
                }
                if trace.links.len() > 30 {
                    println!("- ... {} more links", trace.links.len() - 30);
                }
            }
            0
        }
        SpecCommand::Clarify { spec_id } => {
            daemon_post_pretty_json(
                &format!("/specs/{}/clarify", url_encode_component(&spec_id)),
                serde_json::json!({}),
                "spec clarify",
            )
            .await
        }
        SpecCommand::Plan {
            spec_id,
            constraints,
            no_verification,
        } => {
            daemon_post_pretty_json(
                &format!("/specs/{}/plan", url_encode_component(&spec_id)),
                serde_json::json!({
                    "constraints": constraints,
                    "includeVerification": !no_verification,
                }),
                "spec plan",
            )
            .await
        }
        SpecCommand::Checklist { spec_id, focus } => {
            daemon_post_pretty_json(
                &format!("/specs/{}/checklist", url_encode_component(&spec_id)),
                serde_json::json!({ "focus": focus }),
                "spec checklist",
            )
            .await
        }
        SpecCommand::Implement {
            spec_id,
            title,
            dry_run,
        } => {
            daemon_post_pretty_json(
                &format!("/specs/{}/implement", url_encode_component(&spec_id)),
                serde_json::json!({
                    "title": title,
                    "dryRun": dry_run,
                }),
                "spec implement",
            )
            .await
        }
    }
}

fn cli_spec_store_path(store: Option<&str>) -> std::path::PathBuf {
    store
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| spec_store_path(&default_db_path()))
}

fn default_cli_workspace() -> String {
    std::env::current_dir()
        .unwrap_or_else(|_| std::path::PathBuf::from("."))
        .display()
        .to_string()
}

fn print_spec_status(
    spec: &rightkit_evidence::FeatureSpec,
    task_count: usize,
    latest: Option<&rightkit_evidence::SpecAnalysisRun>,
    store_path: &std::path::Path,
) {
    println!(
        "Spec: {} [{}]\nTitle: {}\nWorkspace: {}\nTasks: {}\nRequirements: {}\nSuccess criteria: {}\nStore: {}",
        spec.id,
        format_spec_status(&spec.status),
        spec.title,
        spec.workspace_root,
        task_count,
        spec.functional_requirements.len(),
        spec.success_criteria.len(),
        store_path.display()
    );
    if let Some(run) = latest {
        println!(
            "Latest analysis: {} [{}], findings: {}",
            run.id,
            format_analysis_status(&run.status),
            run.findings.len()
        );
    } else {
        println!("Latest analysis: none");
    }
}

fn format_spec_status(status: &rightkit_evidence::SpecStatus) -> &'static str {
    match status {
        rightkit_evidence::SpecStatus::Draft => "draft",
        rightkit_evidence::SpecStatus::Planned => "planned",
        rightkit_evidence::SpecStatus::Implementing => "implementing",
        rightkit_evidence::SpecStatus::Done => "done",
        rightkit_evidence::SpecStatus::Blocked => "blocked",
    }
}

fn format_analysis_status(status: &rightkit_evidence::AnalysisStatus) -> &'static str {
    match status {
        rightkit_evidence::AnalysisStatus::Pass => "pass",
        rightkit_evidence::AnalysisStatus::Warn => "warn",
        rightkit_evidence::AnalysisStatus::Block => "block",
    }
}
