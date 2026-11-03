pub(crate) fn command_status(
    state: &DaemonState,
    active_session_id: Option<&str>,
) -> CommandResult {
    let mut live = state.list_sessions();
    live.sort_by(|a, b| a.id.cmp(&b.id));
    let diagnostics = crate::doctor::gather_runtime();
    let provider = state
        .engine_cfg
        .as_ref()
        .map(|cfg| format!("{} via {}", cfg.model_id, cfg.base_url))
        .unwrap_or_else(|| "not configured".to_string());
    let plugin_manifests = plugin_manifests_with_roots(&state.plugin_dir);
    let enabled_plugins = plugin_manifests.iter().filter(|(m, _)| m.enabled).count();
    let mcp_count = configured_mcp_server_count(state);
    let settings = lock_recover(&state.settings, "state.settings").clone();
    let context_status = crate::context_status::from_metrics(&serde_json::json!({
        "membraneBinding": crate::context_status::cli_binding_json(),
    }));
    let mut output = format!(
        "**RightKit status**\n\
Daemon: running\n\
Port: {}\n\
Database: {}\n\
Diagnostics: {}\n\
Provider route: {}\n\
Primary orchestrator: {}\n\
Live sessions: {}\n\
Plugins: {} installed, {} enabled\n\
MCP servers: {}\n\
Plugin dir: {}\n\
MCP config: {}",
        state.bound_port,
        if state.database_path.is_empty() {
            "(not persisted)"
        } else {
            &state.database_path
        },
        doctor_status_label(diagnostics.verdict),
        provider,
        settings.primary_orchestrator_model_id,
        live.len(),
        plugin_manifests.len(),
        enabled_plugins,
        mcp_count,
        state.plugin_dir.display(),
        state.mcp_user_config_path.display()
    );
    output.push_str("\n\n");
    // #347: the active auth method of the current vendor session, with any env override warning.
    if let Some(live_session) = active_session_id.and_then(|id| state.get_session(id)) {
        let driver_kind = lock_recover(&live_session.driver_kind, "session.driver_kind").clone();
        let provider = match driver_kind.as_str() {
            "claude" => Some("claude"),
            "codex" => Some("codex"),
            _ => None,
        };
        if let Some(provider) = provider {
            output.push_str(
                &crate::main_runtime::session_auth_report(
                    provider,
                    &live_session.id,
                    &live_session.workspace_root,
                )
                .render_line(),
            );
            output.push_str("\n\n");
        }
    }
    output.push_str(&crate::context_status::render_text(&context_status));
    if live.is_empty() {
        output.push_str("\nNo live sessions.");
    } else {
        output.push_str("\n\nLive session rows:");
        for session in live.iter().take(12) {
            let model = lock_recover(&session.model, "session.model").clone();
            let title = lock_recover(&session.title, "session.title").clone();
            let role = session.role.as_deref().unwrap_or("primary");
            output.push_str(&format!(
                "\n- {} [{}] {} - {} ({})",
                session.id, role, title, model, session.workspace_root
            ));
        }
        if live.len() > 12 {
            output.push_str(&format!("\n- ... {} more", live.len() - 12));
        }
    }
    command_ok(output)
}

fn doctor_status_label(status: crate::doctor::Status) -> &'static str {
    match status {
        crate::doctor::Status::Pass => "ready",
        crate::doctor::Status::Warn => "ready with warnings",
        crate::doctor::Status::Block => "blocked",
    }
}

pub(crate) fn command_caps() -> CommandResult {
    let capabilities = rightkit_agent_tools::coding_tools::default_tool_capabilities();
    command_ok(rightkit_agent_tools::coding_tools::render_tool_capabilities_markdown(&capabilities))
}

pub(crate) fn command_doctor() -> CommandResult {
    let report = crate::doctor::gather_runtime();
    command_ok(format!(
        "```text\n{}```",
        report.render_text("RightKit").trim_end()
    ))
}

pub(crate) fn command_instructions(args: &[String]) -> CommandResult {
    let workspace = command_workspace_arg(args);
    let workspace_text = workspace.to_string_lossy().to_string();
    let (loader, files) = discover_instruction_files(&workspace_text);
    let composed_bytes = if files.is_empty() {
        0
    } else {
        loader.compose(&files).trim().len()
    };
    let mut output = format!(
        "**Instruction sources:** {}\nFiles: {}\nComposed bytes: {}",
        workspace.display(),
        files.len(),
        composed_bytes
    );
    if files.is_empty() {
        output.push_str("\nNo instruction files discovered.");
        return command_ok(output);
    }

    for file in files {
        let mut filters = Vec::new();
        filters.extend(
            file.frontmatter
                .globs
                .iter()
                .map(|glob| format!("glob:{glob}")),
        );
        filters.extend(
            file.frontmatter
                .paths
                .iter()
                .map(|path| format!("path:{path}")),
        );
        filters.extend(
            file.frontmatter
                .includes
                .iter()
                .map(|include| format!("include:{include}")),
        );
        filters.extend(
            file.frontmatter
                .hook_refs
                .iter()
                .map(|hook| format!("hook:{hook}")),
        );
        let filter_text = if filters.is_empty() {
            String::new()
        } else {
            format!(" ({})", filters.join(", "))
        };
        output.push_str(&format!(
            "\n- [{}] {} - {} bytes, {} lines{}",
            file.scope,
            file.path,
            file.body.len(),
            file.body.lines().count(),
            filter_text
        ));
    }
    output.push_str("\n\nBodies are withheld; this command reports provenance only.");
    command_ok(output)
}

pub(crate) fn command_context(state: &DaemonState, args: &[String]) -> CommandResult {
    let root = command_workspace_arg(args);
    let workspace_text = root.to_string_lossy().to_string();
    let (loader, instruction_files) = discover_instruction_files(&workspace_text);
    let instruction_bytes = if instruction_files.is_empty() {
        0
    } else {
        loader.compose(&instruction_files).trim().len()
    };
    let task_count = load_workspace_task_records(&root)
        .map(|records| records.len())
        .unwrap_or(0);
    let schedule_count = load_api_schedules(&root)
        .map(|records| records.len())
        .unwrap_or(0);
    let memory_summary = "Membrane (Pulse)".to_string();
    let plugin_manifests = plugin_manifests_with_roots(&state.plugin_dir);
    let enabled_plugins = plugin_manifests.iter().filter(|(m, _)| m.enabled).count();
    let (total_skills, enabled_skills) =
        crate::api_skills::skill_inventory_counts(&state.skill_dirs);
    let capabilities = rightkit_agent_tools::coding_tools::default_tool_capabilities();
    let read_only_tools = capabilities.iter().filter(|tool| tool.read_only).count();
    let mut live = state.list_sessions();
    live.sort_by(|a, b| a.id.cmp(&b.id));
    let settings = lock_recover(&state.settings, "state.settings").clone();

    let mut output = format!(
        "**Context:** {}\n\
Live sessions: {}\n\
Primary orchestrator: {}\n\
Instruction sources: {} ({} composed bytes)\n\
Durable memories: {}\n\
Workspace tasks: {}\n\
Workspace schedules: {}\n\
Tools: {} total, {} read-only\n\
Skills: {} enabled / {} discovered\n\
Plugins: {} enabled / {} installed\n\
MCP servers: {}",
        root.display(),
        live.len(),
        settings.primary_orchestrator_model_id,
        instruction_files.len(),
        instruction_bytes,
        memory_summary,
        task_count,
        schedule_count,
        capabilities.len(),
        read_only_tools,
        enabled_skills,
        total_skills,
        enabled_plugins,
        plugin_manifests.len(),
        configured_mcp_server_count(state)
    );

    if !live.is_empty() {
        output.push_str("\n\nLive sessions:");
        for session in live.iter().take(8) {
            let role = session.role.as_deref().unwrap_or("primary");
            let model = lock_recover(&session.model, "session.model").clone();
            let title = lock_recover(&session.title, "session.title").clone();
            output.push_str(&format!(
                "\n- {} [{}] {} ({})",
                session.id, role, title, model
            ));
        }
        if live.len() > 8 {
            output.push_str(&format!("\n- ... {} more", live.len() - 8));
        }
    }

    if !instruction_files.is_empty() {
        output.push_str("\n\nInstruction sources:");
        for file in instruction_files.iter().take(8) {
            output.push_str(&format!(
                "\n- [{}] {} ({} bytes)",
                file.scope,
                file.path,
                file.body.len()
            ));
        }
        if instruction_files.len() > 8 {
            output.push_str(&format!("\n- ... {} more", instruction_files.len() - 8));
        }
    }

    let live_ids: std::collections::HashSet<&str> =
        live.iter().map(|session| session.id.as_str()).collect();
    output.push_str(&host_delivery_inventory_section(
        &rightkit_logs::agent_telemetry::evidence::provider_turn_evidence_ring(),
        &rightkit_logs::agent_telemetry::remaining_context_ceilings(),
        &live_ids,
    ));

    output.push_str(
        "\n\nUse `/instructions`, `/memory`, `/tasks`, `/schedules`, `/caps`, `/skills`, `/plugin`, or `/mcp` for details.",
    );
    command_ok(output)
}

/// A048/A049: render the host request budget (H8) and the retained delivery
/// inventory (H9) from the bounded host sinks. Missing observations render as
/// `unavailable`, never as zero, and only live sessions are listed. Pure so it
/// is unit-testable without a daemon.
pub(crate) fn host_delivery_inventory_section(
    ring: &rightkit_logs::agent_telemetry::evidence::ProviderTurnEvidenceRingV1,
    ceilings: &[rightkit_logs::agent_telemetry::RemainingContextCeilingV1],
    live_ids: &std::collections::HashSet<&str>,
) -> String {
    use rightkit_logs::agent_telemetry::evidence::ObservationCoverage as Coverage;

    let mut out =
        String::from("\n\nHost request budget (H8) and retained delivery inventory (H9):");
    let mut sessions: Vec<&str> = live_ids.iter().copied().collect();
    sessions.sort_unstable();
    for sid in sessions.iter().take(8) {
        let budget = ceilings
            .iter()
            .rev()
            .find(|row| row.session_id == *sid)
            .and_then(|row| row.remaining_tokens.estimate.value);
        let loaded = ring.rows.iter().rev().find(|row| row.session_id == *sid);
        let budget_text = match budget {
            Some(tokens) => format!("{tokens} tokens remaining"),
            None => "budget unavailable".to_string(),
        };
        let inventory_text = match loaded {
            Some(row) if row.loaded_context_identities.coverage == Coverage::Complete => {
                match row.loaded_context_identities.value.as_ref() {
                    Some(ids) => format!("{} retained context identities", ids.len()),
                    None => "inventory unavailable".to_string(),
                }
            }
            _ => "inventory unavailable".to_string(),
        };
        out.push_str(&format!("\n- {sid}: {budget_text}; {inventory_text}"));
    }
    if sessions.is_empty() {
        out.push_str("\n- no live sessions");
    }
    if sessions.len() > 8 {
        out.push_str(&format!("\n- ... {} more", sessions.len() - 8));
    }
    if ring.dropped_rows > 0 {
        out.push_str(&format!(
            "\nEvidence ring dropped {} oldest row(s).",
            ring.dropped_rows
        ));
    }
    out
}

pub(crate) async fn command_diff(args: &[String]) -> CommandResult {
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use rightkit_agent_tools::coding_tools::Tool;

    let staged = args.iter().any(|arg| arg == "--staged");
    let workspace_arg = args
        .iter()
        .filter(|arg| arg.as_str() != "--staged")
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let workspace = if workspace_arg.trim().is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        PathBuf::from(workspace_arg.trim())
    };
    let ctx = rightkit_agent_tools::coding_tools::ToolContext::new(
        "slash-diff",
        &workspace,
        PermissionMode::Bypass,
        WorkspaceTrust::Trusted,
    );

    let status = rightkit_agent_tools::coding_tools::git_read::GitReadTool
        .execute(serde_json::json!({ "repo_path": workspace }), &ctx)
        .await;
    if status.is_error {
        return command_err(format!(
            "**Diff:** {}\n\n{}",
            workspace.display(),
            status.content
        ));
    }

    let diff = rightkit_agent_tools::coding_tools::git_diff::GitDiffTool
        .execute(
            serde_json::json!({
                "repo_path": workspace,
                "staged": staged
            }),
            &ctx,
        )
        .await;
    if diff.is_error {
        return command_err(format!(
            "**Diff:** {}\n\n{}",
            workspace.display(),
            diff.content
        ));
    }

    let scope = if staged { "staged" } else { "unstaged" };
    let mut output = format!(
        "**Diff:** {}\nScope: {scope}\n\n```text\n{}```\n\n",
        workspace.display(),
        status.content.trim_end()
    );
    if diff.content.trim() == "(no changes)" {
        output.push_str("No changes in selected scope.");
    } else {
        output.push_str("```diff\n");
        output.push_str(diff.content.trim_end());
        output.push_str("\n```");
    }
    command_ok(output)
}
