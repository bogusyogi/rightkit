pub(crate) async fn command_spec(
    state: &DaemonState,
    args: &[String],
    active_session_id: Option<&str>,
) -> CommandResult {
    let subcommand = args.first().map(String::as_str).unwrap_or("list");
    match subcommand {
        "list" => command_spec_list(state, &args[1..]),
        "new" => command_spec_new(state, &args[1..], active_session_id),
        "status" => {
            let Some(spec_id) = args.get(1).map(String::as_str) else {
                return command_err("Usage: /spec status <spec-id>");
            };
            command_spec_status(state, spec_id)
        }
        "analyze" => {
            let Some(spec_id) = args.get(1).map(String::as_str) else {
                return command_err("Usage: /spec analyze <spec-id>");
            };
            command_spec_analyze(state, spec_id)
        }
        "trace" => {
            let Some(spec_id) = args.get(1).map(String::as_str) else {
                return command_err("Usage: /spec trace <spec-id>");
            };
            command_spec_trace(state, spec_id)
        }
        "clarify" => {
            let Some(spec_id) = args.get(1).map(String::as_str) else {
                return command_err("Usage: /spec clarify <spec-id>");
            };
            command_spec_clarify(state, spec_id)
        }
        "plan" => {
            let Some(spec_id) = args.get(1).map(String::as_str) else {
                return command_err(
                    "Usage: /spec plan <spec-id> [--constraints <text>] [--no-verification]",
                );
            };
            command_spec_plan(state, spec_id, &args[2..])
        }
        "checklist" => {
            let Some(spec_id) = args.get(1).map(String::as_str) else {
                return command_err("Usage: /spec checklist <spec-id> [--focus <text>]");
            };
            command_spec_checklist(state, spec_id, &args[2..])
        }
        "implement" => {
            let Some(spec_id) = args.get(1).map(String::as_str) else {
                return command_err("Usage: /spec implement <spec-id> [--title <text>] [--dry-run]");
            };
            command_spec_implement(state, spec_id, &args[2..]).await
        }
        _ => command_err(
            "Usage: /spec [list] [--workspace <path>] | /spec new [--workspace <path>] <description> | /spec status <spec-id> | /spec analyze <spec-id> | /spec trace <spec-id> | /spec clarify <spec-id> | /spec plan <spec-id> | /spec checklist <spec-id> | /spec implement <spec-id>",
        ),
    }
}

fn command_spec_list(state: &DaemonState, args: &[String]) -> CommandResult {
    let (workspace, _) = command_spec_workspace_and_rest(args);
    let mut specs = lock_recover(&state.specs, "state.specs").list_specs(workspace.as_deref());
    specs.sort_by(|a, b| a.id.cmp(&b.id));
    let mut output = format!(
        "**Specs:** {}{}",
        specs.len(),
        workspace
            .as_deref()
            .map(|root| format!(" in `{root}`"))
            .unwrap_or_default()
    );
    if specs.is_empty() {
        output.push_str("\nNo feature specs.");
    } else {
        for spec in specs.iter().take(20) {
            output.push_str(&format!(
                "\n- `{}` [{}] {} — {} req, {} criteria",
                spec.id,
                slash_spec_status(&spec.status),
                spec.title,
                spec.functional_requirements.len(),
                spec.success_criteria.len()
            ));
        }
        if specs.len() > 20 {
            output.push_str(&format!("\n- ... {} more", specs.len() - 20));
        }
    }
    command_ok(output)
}
