pub(crate) fn command_skills(state: &DaemonState) -> CommandResult {
    command_ok(render_skills_command(&state.skill_dirs))
}

pub(crate) fn command_plugins(state: &crate::api::DaemonState, args: &[String]) -> CommandResult {
    if args
        .first()
        .is_some_and(|arg| arg.eq_ignore_ascii_case("marketplace"))
    {
        return command_plugin_marketplace(state, &args[1..]);
    }

    let manifests = plugin_manifests_with_roots(&state.plugin_dir);
    let total = manifests.len();
    let enabled = manifests.iter().filter(|(m, _)| m.enabled).count();
    let mut rows: Vec<_> = manifests
        .into_iter()
        .map(|(m, _)| {
            format!(
                "- {} [{}] tools:{} commands:{}",
                m.id,
                if m.enabled { "enabled" } else { "disabled" },
                m.provides_tools.len(),
                m.provides_commands.len()
            )
        })
        .collect();
    rows.sort();
    let mut output = format!(
        "**Plugins:** {enabled} enabled / {total} installed\nDirectory: {}",
        state.plugin_dir.display()
    );
    if rows.is_empty() {
        output.push_str("\nNo installed plugin manifests found.");
    } else {
        output.push('\n');
        output.push_str(&rows.join("\n"));
    }
    command_ok(output)
}

pub(crate) async fn command_benchmarks(state: &crate::api::DaemonState) -> CommandResult {
    let value = benchmark_history_json(state).await;
    let proof = benchmark_proof_json(state).await;
    let rows = value.as_array().cloned().unwrap_or_default();
    let total_repeats = proof["totalRepeats"].as_u64().unwrap_or(0);
    let total_successes = proof["totalSuccesses"].as_u64().unwrap_or(0);
    let success_rate = proof["successRate"].as_f64().unwrap_or(0.0);
    let mut output = format!(
        "**Benchmarks:** {} recorded\nProof: {total_successes}/{total_repeats} successful repeats ({success_rate:.2})",
        rows.len()
    );
    if rows.is_empty() {
        output.push_str("\nNo benchmark reports recorded.");
    } else {
        output.push('\n');
        output.push_str(
            &rows
                .into_iter()
                .take(10)
                .map(|row| {
                    let role = row["role"].as_str().unwrap_or("unknown");
                    let model = row["model"].as_str().unwrap_or("unknown");
                    let suite = row["suite"].as_str().unwrap_or("unknown");
                    let successes = row["successes"].as_u64().unwrap_or(0);
                    let repeats = row["repeats"].as_u64().unwrap_or(0);
                    let rate = row["successRate"].as_f64().unwrap_or(0.0);
                    format!("- {role} on {model}: {successes}/{repeats} ({rate:.2}) [{suite}]")
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    command_ok(output)
}

pub(crate) async fn command_terminal_bench(
    state: &crate::api::DaemonState,
    args: &[String],
) -> CommandResult {
    let subcommand = args.first().map(String::as_str).unwrap_or("tasks");
    match subcommand {
        "tasks" | "list" => command_terminal_bench_tasks(),
        "run" => {
            if args.len() < 4 {
                return command_err("Usage: /terminal-bench run <task-id> <agent> <session-id>");
            }
            let body = TerminalBenchRunRequest {
                task_id: args[1].clone(),
                agent: args[2].clone(),
                session_id: args[3].clone(),
            };
            match run_terminal_bench_task(state, body).await {
                Ok(value) => {
                    let passed = value["passed"].as_bool().unwrap_or(false);
                    let task = value["taskId"].as_str().unwrap_or("unknown");
                    let role = value["role"].as_str().unwrap_or("unknown");
                    let rate = value["successRate"].as_f64().unwrap_or(0.0);
                    let status = if passed { "passed" } else { "failed" };
                    command_ok(format!(
                        "**RightKit task pack {task}:** {status}\nRole: {role}\nSuccess rate: {rate:.2}\nRecorded in `/benchmarks`."
                    ))
                }
                Err(axum::http::StatusCode::NOT_FOUND) => {
                    command_err("Unknown RightKit task-pack id.")
                }
                Err(axum::http::StatusCode::BAD_REQUEST) => command_err("Unknown agent role."),
                Err(_) => command_err("RightKit task-pack run failed."),
            }
        }
        _ => command_err(
            "Usage: /terminal-bench [tasks] or /terminal-bench run <task-id> <agent> <session-id>",
        ),
    }
}

fn command_terminal_bench_tasks() -> CommandResult {
    let tasks = rightkit_agent_orchestration::TbenchCatalog::all_tasks();
    let mut output = format!(
        "**RightKit Internal Tasks:** {} available\nSuite: {}",
        tasks.len(),
        rightkit_agent_orchestration::TbenchCatalog::suite_id()
    );
    if !tasks.is_empty() {
        output.push('\n');
        output.push_str(
            &tasks
                .into_iter()
                .take(12)
                .map(|task| {
                    format!(
                        "- {} [{:?}/{:?}] {}",
                        task.id, task.category, task.difficulty, task.prompt
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    command_ok(output)
}

fn command_plugin_marketplace(state: &crate::api::DaemonState, args: &[String]) -> CommandResult {
    let catalog = match load_plugin_marketplace(&state.plugin_marketplace_path) {
        Ok(catalog) => catalog,
        Err(e) => return command_err(format!("Failed to read plugin marketplace: {e}")),
    };
    let query = args.join(" ").trim().to_lowercase();
    let mut entries = catalog
        .plugins
        .iter()
        .filter(|entry| {
            if query.is_empty() {
                return true;
            }
            entry.manifest.id.to_lowercase().contains(&query)
                || entry.manifest.name.to_lowercase().contains(&query)
                || entry.manifest.description.to_lowercase().contains(&query)
        })
        .collect::<Vec<_>>();
    entries.sort_by(|a, b| {
        b.rating
            .partial_cmp(&a.rating)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| b.downloads.cmp(&a.downloads))
            .then_with(|| a.manifest.id.cmp(&b.manifest.id))
    });
    let mut output = format!(
        "**Plugin marketplace:** {} match(es)\nCatalog: {}",
        entries.len(),
        state.plugin_marketplace_path.display()
    );
    if entries.is_empty() {
        output.push_str("\nNo marketplace plugins found.");
    } else {
        for entry in entries {
            output.push_str(&format!(
                "\n- {} v{} rating:{:.1} downloads:{} sha256:{} signer:{}",
                entry.manifest.id,
                entry.manifest.version,
                entry.rating,
                entry.downloads,
                entry.checksum_sha256,
                if entry.signed_by.is_empty() {
                    "unknown"
                } else {
                    entry.signed_by.as_str()
                }
            ));
        }
        output.push_str(
            "\nSync via POST /plugins/marketplace/sync with trustAccepted and expectedSha256.\nInstall via POST /plugins/marketplace/{id}/install with trustAccepted and expectedSha256.",
        );
    }
    command_ok(output)
}
