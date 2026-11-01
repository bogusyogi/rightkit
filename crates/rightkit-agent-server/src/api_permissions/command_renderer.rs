pub fn render_permissions_command(
    state: &DaemonState,
    args: &[String],
    active_session_id: Option<&str>,
) -> CommandResult {
    if let Some(session_id) = args
        .first()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .or_else(|| active_session_id.map(str::trim).filter(|s| !s.is_empty()))
    {
        let Some(live) = state.get_session(session_id) else {
            return command_err(format!("No live session named `{session_id}`."));
        };
        let mode = live_session_permission_mode(&live);
        let rules = lock_recover(&live.permission_rules, "live.permission_rules");
        let mut output = format!(
            "**Permissions:** {session_id}\nMode: {mode}\nRules: {}",
            rules.all_rules().len()
        );
        if rules.all_rules().is_empty() {
            output.push_str("\nNo session permission rules.");
        } else {
            output.push('\n');
            output.push_str(
                &rules
                    .all_rules()
                    .iter()
                    .enumerate()
                    .map(|(index, rule)| {
                        format!(
                            "- #{index} [{}] {} => {}",
                            permission_rule_source_label(rule.source),
                            rule.pattern,
                            permission_decision_label(rule.effect)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
        output.push_str(permission_rule_examples());
        return command_ok(output);
    }

    let sessions: Vec<_> = lock_recover(&state.sessions, "state.sessions")
        .values()
        .cloned()
        .collect();
    let mut output = format!("**Permissions:** {} live session(s)", sessions.len());
    if sessions.is_empty() {
        output.push_str("\nNo live sessions.");
    } else {
        output.push('\n');
        for live in sessions {
            let mode = live_session_permission_mode(&live);
            let rule_count = lock_recover(&live.permission_rules, "live.permission_rules")
                .all_rules()
                .len();
            output.push_str(&format!("- {} mode:{mode} rules:{rule_count}\n", live.id));
        }
        output.push_str("\nUse `/permissions <session-id>` to inspect another session's rules.");
    }
    output.push_str(permission_rule_examples());
    command_ok(output)
}

fn permission_rule_examples() -> &'static str {
    "\n\nRule examples:\n- `Bash(git push*)` ask/deny/allow shell commands by command text\n- `FileEdit(src/**)` constrain edit paths\n- `read_file(**/.env*)` protect secret-like reads"
}

fn command_ok(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: true,
        output: output.into(),
        action: CommandAction::None,
    }
}

fn command_err(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: false,
        output: output.into(),
        action: CommandAction::None,
    }
}
