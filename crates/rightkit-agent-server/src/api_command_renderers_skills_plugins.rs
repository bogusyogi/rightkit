pub(crate) fn command_skills(state: &DaemonState) -> CommandResult {
    command_ok(render_skills_command(&state.skill_dirs))
}

pub(crate) async fn command_plugins(
    state: &crate::api::DaemonState,
    args: &[String],
    session_id: Option<&str>,
) -> CommandResult {
    if args.first().map(String::as_str) == Some("ui") {
        return command_plugin_ui(state, args.get(1).map(String::as_str), session_id).await;
    }
    let manifests = plugin_manifests_with_roots(&state.plugin_dir);
    let total = manifests.len();
    let enabled = manifests.iter().filter(|(m, _)| m.enabled).count();
    let mut rows: Vec<_> = manifests
        .into_iter()
        .map(|(m, _)| {
            let mut row = format!(
                "- {} [{}] tools:{} commands:{}",
                m.id,
                if m.enabled { "enabled" } else { "disabled" },
                m.provides_tools.len(),
                m.provides_commands.len()
            );
            // Slots an enabled plugin declares; `/plugins ui <id>` shows one.
            for slot in m.active_ui_contributions() {
                row.push_str(&format!(
                    "\n    ui {} {}{}",
                    slot.slot.as_str(),
                    slot.id,
                    if slot.title.trim().is_empty() {
                        String::new()
                    } else {
                        format!(": {}", slot.title.trim())
                    }
                ));
            }
            row
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

/// `/plugins ui [slot-id]`: the TUI surface of the `uiSlots` a plugin declares. Bare lists the
/// slots of every enabled plugin; an id shows that slot's title and, when it names a command,
/// the text the command produces (run like any plugin command, in the session's workspace).
async fn command_plugin_ui(
    state: &crate::api::DaemonState,
    slot_id: Option<&str>,
    session_id: Option<&str>,
) -> CommandResult {
    let manifests = plugin_manifests_with_roots(&state.plugin_dir);
    let mut slots: Vec<(String, rightkit_agent_plugins::PluginUiContribution)> = Vec::new();
    for (manifest, _) in &manifests {
        for slot in manifest.active_ui_contributions() {
            slots.push((manifest.id.clone(), slot.clone()));
        }
    }
    let Some(slot_id) = slot_id else {
        if slots.is_empty() {
            return command_ok(
                "No plugin declares a UI slot (an enabled plugin lists them in `uiSlots`)."
                    .to_string(),
            );
        }
        let lines: Vec<String> = slots
            .iter()
            .map(|(plugin, slot)| {
                format!(
                    "- {} {} ({plugin}): {}",
                    slot.slot.as_str(),
                    slot.id,
                    slot.title
                )
            })
            .collect();
        return command_ok(format!("**Plugin UI slots:**\n{}", lines.join("\n")));
    };
    let Some((plugin, slot)) = slots.iter().find(|(_, slot)| slot.id == slot_id) else {
        return CommandResult {
            success: false,
            output: format!("No UI slot `{slot_id}` from an enabled plugin."),
            action: CommandAction::None,
        };
    };
    let Some(command) = slot.command.as_deref() else {
        return command_ok(format!("{plugin} {}: {}", slot.id, slot.title));
    };
    // A slot refresh may only read: a plugin manifest must not run a built-in command that
    // changes state (`/browser-consent grant`, `/permissions <mode>`, ...) by naming it here.
    if let Some(entry) = rightkit_agent_skills::commands::resolve_query(
        command.split_whitespace().next().unwrap_or(""),
    ) {
        if entry.effect != rightkit_agent_skills::commands::CommandEffect::Read {
            return CommandResult {
                success: false,
                output: format!(
                    "Plugin UI slot `{}` names `/{command}`, which changes state; slot commands may only read.",
                    slot.id
                ),
                action: CommandAction::None,
            };
        }
    }
    // The refresh command is an ordinary plugin command, so the shared executor runs it.
    let result = Box::pin(crate::api_commands::execute_command_on_state(
        state,
        &format!("/{command}"),
        session_id,
    ))
    .await;
    CommandResult {
        output: format!("{} ({plugin}):\n{}", slot.title, result.output),
        ..result
    }
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
                        "**{app} task pack {task}:** {status}\nRole: {role}\nSuccess rate: {rate:.2}\nRecorded in `/benchmarks`.",
                        app = rightkit_agent_config::app_name()
                    ))
                }
                Err(axum::http::StatusCode::NOT_FOUND) => command_err(format!(
                    "Unknown {} task-pack id.",
                    rightkit_agent_config::app_name()
                )),
                Err(axum::http::StatusCode::BAD_REQUEST) => command_err("Unknown agent role."),
                Err(_) => command_err(format!(
                    "{} task-pack run failed.",
                    rightkit_agent_config::app_name()
                )),
            }
        }
        _ => command_err(
            "Usage: /terminal-bench [tasks] or /terminal-bench run <task-id> <agent> <session-id>",
        ),
    }
}

fn command_terminal_bench_tasks() -> CommandResult {
    let tasks = rightkit_agent_orchestration::coding::TbenchCatalog::all_tasks();
    let mut output = format!(
        "**{} Internal Tasks:** {} available\nSuite: {}",
        rightkit_agent_config::app_name(),
        tasks.len(),
        rightkit_agent_orchestration::coding::TbenchCatalog::suite_id()
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
