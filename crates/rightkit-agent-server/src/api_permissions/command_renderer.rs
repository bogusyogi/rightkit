pub fn render_permissions_command(
    state: &DaemonState,
    args: &[String],
    active_session_id: Option<&str>,
) -> CommandResult {
    // `/permissions <plan|ask|accept-edits|auto-safe> [session-id]` sets the live session's mode;
    // `/permissions [session-id]` inspects. A mode word is never a session id.
    if let Some(word) = args
        .first()
        .map(|word| word.trim())
        .filter(|word| !word.is_empty())
    {
        if let Some(result) =
            set_permissions_mode_command(state, word, args.get(1), active_session_id)
        {
            return result;
        }
    }
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

/// The mode spellings `/permissions` accepts. `bypass`, `yolo` and the like are recognized only to be refused:
/// this command never raises a session to the mode that skips every approval.
pub(crate) fn permissions_command_mode(
    word: &str,
) -> Option<Result<rightkit_agent_policy::PermissionMode, ()>> {
    let lower = word.trim().to_lowercase();
    match lower.as_str() {
        "plan" | "ask" | "accept-edits" | "accept_edits" | "auto-safe" | "auto_safe" | "auto" => {
            rightkit_agent_policy::PermissionMode::from_user_name(&lower)
                .or_else(|| rightkit_agent_policy::parse_permission_mode_strict(&lower))
                .map(Ok)
        }
        "bypass" | "yolo" | "bypass-permissions" | "dangerously-skip-permissions" => Some(Err(())),
        _ => None,
    }
}

/// `None` when `word` is not a mode word (the caller then treats it as a session id).
fn set_permissions_mode_command(
    state: &DaemonState,
    word: &str,
    explicit_session: Option<&String>,
    active_session_id: Option<&str>,
) -> Option<CommandResult> {
    let requested = permissions_command_mode(word)?;
    let mode = match requested {
        Ok(mode) => mode,
        Err(()) => {
            return Some(command_err(
                "`/permissions` never selects the mode that skips every approval. Use plan, ask, accept-edits or auto-safe.",
            ));
        }
    };
    let Some(session_id) = explicit_session
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .or_else(|| active_session_id.map(str::trim).filter(|id| !id.is_empty()))
    else {
        return Some(command_err(
            "`/permissions <mode>` needs an active session.",
        ));
    };
    let Some(live) = state.get_session(session_id) else {
        return Some(command_err(format!(
            "No live session named `{session_id}`."
        )));
    };
    if let Some(message) = permission_mode_refusal(state, &live, mode) {
        return Some(command_err(message));
    }
    set_permission_mode_and_tighten_descendants(state, &live, mode);
    Some(command_ok(format!(
        "**Permissions:** {session_id}\nMode: {}\nThe change applies from the next turn.",
        permission_mode_label(mode)
    )))
}

#[cfg(test)]
mod permissions_command_mode_tests {
    use super::*;

    #[test]
    fn mode_words_resolve_and_bypass_is_recognized_only_to_be_refused() {
        assert_eq!(
            permissions_command_mode("plan"),
            Some(Ok(rightkit_agent_policy::PermissionMode::Plan))
        );
        assert_eq!(
            permissions_command_mode("Ask"),
            Some(Ok(rightkit_agent_policy::PermissionMode::Ask))
        );
        assert_eq!(
            permissions_command_mode("accept-edits"),
            Some(Ok(rightkit_agent_policy::PermissionMode::AcceptEdits))
        );
        assert_eq!(
            permissions_command_mode("auto-safe"),
            Some(Ok(rightkit_agent_policy::PermissionMode::AutoSafe))
        );
        assert_eq!(permissions_command_mode("bypass"), Some(Err(())));
        assert_eq!(permissions_command_mode("yolo"), Some(Err(())));
        assert_eq!(permissions_command_mode("sess-1234"), None);
    }
}
