//! Slash commands that read or change one chat using only what the daemon already holds
//! (wave 25, B-compactargs, B-cxcommands, B-recap, B-skilldoctor).
//!
//! Every command here is a plain read of live state, a call to an existing route handler, or (for `/recap` and
//! `/compact <instructions>`) one model call that the user's own typing asks for. None of them sends anything to a
//! service of ours, and none runs on its own.

use super::*;

use crate::api::session_workspace_root;

fn ok(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: true,
        output: output.into(),
        action: CommandAction::None,
    }
}

fn fail(output: impl Into<String>) -> CommandResult {
    CommandResult {
        success: false,
        output: output.into(),
        action: CommandAction::None,
    }
}

fn need_session(command: &str) -> CommandResult {
    fail(format!(
        "/{command} works on one chat; run it inside a session, or pass `sessionId` to POST /commands/execute."
    ))
}

/// The session's workspace folder, or a ready failure result.
async fn workspace_of(state: &DaemonState, session_id: &str) -> Result<String, CommandResult> {
    match session_workspace_root(state, session_id).await {
        Ok(Some(root)) if !root.trim().is_empty() => Ok(root),
        Ok(_) => Err(fail(format!(
            "Session {session_id} has no workspace folder."
        ))),
        Err(status) => Err(fail(format!(
            "Could not read session {session_id}: HTTP {status}"
        ))),
    }
}

// ── /pwd ────────────────────────────────────────────────────────────────────

pub(crate) async fn command_pwd(state: &DaemonState, session_id: Option<&str>) -> CommandResult {
    let Some(session_id) = session_id else {
        return need_session("pwd");
    };
    match workspace_of(state, session_id).await {
        Ok(root) => ok(root),
        Err(result) => result,
    }
}

// ── /rename and /title ──────────────────────────────────────────────────────

pub(crate) async fn command_rename(
    state: &DaemonState,
    args: &[String],
    session_id: Option<&str>,
) -> CommandResult {
    let Some(session_id) = session_id else {
        return need_session("rename");
    };
    let title = args.join(" ");
    let title = title.trim();
    if title.is_empty() {
        return fail("usage: /rename <new title>  (also /title <new title>)");
    }
    let Ok(body) = serde_json::from_value::<PatchSession>(json!({ "title": title })) else {
        return fail("Could not build the rename request.");
    };
    match crate::api_sessions::lifecycle::patch_session(
        axum::extract::State(state.clone()),
        axum::extract::Path(session_id.to_string()),
        axum::http::HeaderMap::new(),
        axum::extract::Json(body),
    )
    .await
    {
        Ok(_) => ok(format!("Renamed session to \"{title}\"")),
        Err(status) => fail(format!("Cannot rename: session API returned HTTP {status}")),
    }
}

// ── /clone ──────────────────────────────────────────────────────────────────

pub(crate) async fn command_clone(state: &DaemonState, session_id: Option<&str>) -> CommandResult {
    let Some(session_id) = session_id else {
        return need_session("clone");
    };
    let Ok(body) = serde_json::from_value(json!({})) else {
        return fail("Could not build the clone request.");
    };
    match crate::api_sessions::lifecycle::fork_session_handler(
        axum::extract::State(state.clone()),
        axum::extract::Path(session_id.to_string()),
        axum::http::HeaderMap::new(),
        axum::extract::Json(body),
    )
    .await
    {
        Ok(axum::response::Json(value)) => {
            let id = value
                .get("id")
                .or_else(|| value.get("sessionId"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            if id.is_empty() {
                fail("The clone was created but its id was not returned.")
            } else {
                ok(format!("Forked session {id}"))
            }
        }
        Err(StatusCode::CONFLICT) => fail(
            "Cannot clone: the chat has a turn in flight (or the new id is taken). Wait for the turn to finish.",
        ),
        Err(status) => fail(format!("Cannot clone: session API returned HTTP {status}")),
    }
}

// ── /reload ─────────────────────────────────────────────────────────────────

pub(crate) async fn command_reload(state: &DaemonState, session_id: Option<&str>) -> CommandResult {
    let Some(session_id) = session_id else {
        return need_session("reload");
    };
    let root = match workspace_of(state, session_id).await {
        Ok(root) => root,
        Err(result) => return result,
    };
    let query = WorkspaceQuery {
        workspace_root: root,
    };
    match crate::api_settings::reload_settings(
        axum::extract::State(state.clone()),
        axum::extract::Query(query),
        axum::http::HeaderMap::new(),
    )
    .await
    {
        Ok(axum::response::Json(value)) => {
            let files = value
                .pointer("/configStatus/files")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let found = files
                .iter()
                .filter(|file| file.get("exists").and_then(Value::as_bool).unwrap_or(false))
                .count();
            ok(format!(
                "Reloaded the configuration: {found} of {} files found. A running turn keeps the settings it started with; the next turn uses the new ones.",
                files.len()
            ))
        }
        Err(status) => fail(format!(
            "Cannot reload: settings API returned HTTP {status}"
        )),
    }
}

// ── /ps ─────────────────────────────────────────────────────────────────────

pub(crate) fn command_ps(state: &DaemonState, session_id: Option<&str>) -> CommandResult {
    let Some(session_id) = session_id else {
        return need_session("ps");
    };
    let Some(live) = state.get_session(session_id) else {
        return fail(format!("No live session named `{session_id}`."));
    };
    let shells: Vec<(
        String,
        std::sync::Arc<rightkit_agent_coding_tools::background_shell::BgShell>,
    )> = lock_recover(&live.background_shells, "session.background_shells")
        .iter()
        .map(|(id, shell)| (id.clone(), shell.clone()))
        .collect();
    if shells.is_empty() {
        return ok("No background terminals in this chat.");
    }
    let mut rows: Vec<String> = shells
        .iter()
        .map(|(id, shell)| {
            let status = format!("{:?}", shell.status());
            let pid = shell
                .pid()
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "-".into());
            let last = shell.last_line();
            let last = if last.trim().is_empty() {
                String::new()
            } else {
                format!("  last: {}", last.trim())
            };
            format!("- {id} [{status}] pid {pid}  {}{last}", shell.command)
        })
        .collect();
    rows.sort();
    ok(format!(
        "**Background terminals:** {}\n{}\n\nStop one with /stop.",
        rows.len(),
        rows.join("\n")
    ))
}

// ── /agents and /subagents ──────────────────────────────────────────────────

pub(crate) async fn command_agents(state: &DaemonState, session_id: Option<&str>) -> CommandResult {
    let Some(session_id) = session_id else {
        return need_session("agents");
    };
    let rows = match crate::api_sessions::orchestration::get_session_agents(
        axum::extract::State(state.clone()),
        axum::extract::Path(session_id.to_string()),
    )
    .await
    {
        Ok(axum::response::Json(value)) => value.as_array().cloned().unwrap_or_default(),
        Err(StatusCode::NOT_FOUND) => {
            return fail(format!("No live session named `{session_id}`."))
        }
        Err(status) => {
            return fail(format!(
                "Cannot list agents: session API returned HTTP {status}"
            ))
        }
    };
    if rows.is_empty() {
        return ok("No sub-agents in this chat.");
    }
    let field = |row: &Value, names: &[&str]| -> String {
        names
            .iter()
            .find_map(|name| row.get(*name).and_then(Value::as_str))
            .unwrap_or("-")
            .to_string()
    };
    let lines: Vec<String> = rows
        .iter()
        .map(|row| {
            format!(
                "- {} role {} [{}] model {}",
                field(row, &["subSessionId", "id"]),
                field(row, &["role", "name"]),
                field(row, &["status", "state"]),
                field(row, &["model", "modelId"]),
            )
        })
        .collect();
    ok(format!(
        "**Sub-agents:** {}\n{}",
        lines.len(),
        lines.join("\n")
    ))
}

// ── /experimental ───────────────────────────────────────────────────────────

pub(crate) async fn command_experimental(
    state: &DaemonState,
    session_id: Option<&str>,
) -> CommandResult {
    let config = match config_for(state, session_id).await {
        Ok(config) => config,
        Err(result) => return result,
    };
    let features = serde_json::to_value(&config.features).unwrap_or(Value::Null);
    let Some(object) = features.as_object() else {
        return fail("The feature flags could not be read.");
    };
    let mut rows: Vec<String> = object
        .iter()
        .filter(|(_, value)| value.is_null() || value.is_boolean())
        .map(|(name, value)| {
            let state = match value.as_bool() {
                Some(true) => "on",
                Some(false) => "off",
                None => "unset (the product default applies)",
            };
            format!("- {name}: {state}")
        })
        .collect();
    rows.sort();
    ok(format!(
        "**Feature flags** (set under `[features]` in the config):\n{}",
        rows.join("\n")
    ))
}

/// The merged configuration for the session's workspace, or the daemon's current folder with no session.
async fn config_for(
    state: &DaemonState,
    session_id: Option<&str>,
) -> Result<rightkit_agent_config::HostConfig, CommandResult> {
    let root = match session_id {
        Some(id) => workspace_of(state, id).await?,
        None => std::env::current_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default(),
    };
    Ok(crate::api::load_workspace_config_with_env(FsPath::new(
        &root,
    )))
}

// ── /debug-config ───────────────────────────────────────────────────────────

pub(crate) async fn command_debug_config(
    state: &DaemonState,
    session_id: Option<&str>,
) -> CommandResult {
    let root = match session_id {
        Some(id) => match workspace_of(state, id).await {
            Ok(root) => root,
            Err(result) => return result,
        },
        None => std::env::current_dir()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default(),
    };
    let lines = crate::api_settings::config_layer_lines(&root);
    ok(format!(
        "**Configuration files** for {root} (later files override earlier ones):\n{}",
        lines.join("\n")
    ))
}

// ── /statusline ─────────────────────────────────────────────────────────────

/// The footer items the terminal draws, in footer order.
pub(crate) const STATUSLINE_ITEMS: &[&str] =
    &["cwd", "branch", "model", "ctx", "cost", "cache", "goal"];

pub(crate) async fn command_statusline(
    state: &DaemonState,
    session_id: Option<&str>,
) -> CommandResult {
    let config = match config_for(state, session_id).await {
        Ok(config) => config,
        Err(result) => return result,
    };
    let chosen: Vec<String> = config
        .tui
        .statusline
        .iter()
        .map(|item| item.trim().to_ascii_lowercase())
        .filter(|item| !item.is_empty())
        .collect();
    let shown = if chosen.is_empty() || chosen.iter().any(|item| item == "all") {
        "all".to_string()
    } else {
        chosen.join(", ")
    };
    ok(format!(
        "**Footer items:** {}\nShown now: {shown}\nChoose with `/statusline cwd,model,ctx` in the terminal, or `statusline = [..]` under `[tui]`.",
        STATUSLINE_ITEMS.join(", ")
    ))
}

// ── /daemon ─────────────────────────────────────────────────────────────────

pub(crate) fn command_daemon(state: &DaemonState) -> CommandResult {
    let host = if state.bound_port == 0 {
        "in-process host (no daemon port)".to_string()
    } else {
        format!("daemon on 127.0.0.1:{}", state.bound_port)
    };
    ok(format!(
        "**Host:** {host}\nPort: {}\nVersion: {}\nProcess id: {}\nDatabase: {}",
        state.bound_port,
        env!("CARGO_PKG_VERSION"),
        std::process::id(),
        state.database_path
    ))
}

// ── /rollout ────────────────────────────────────────────────────────────────

pub(crate) async fn command_rollout(
    state: &DaemonState,
    session_id: Option<&str>,
) -> CommandResult {
    let Some(session_id) = session_id else {
        return need_session("rollout");
    };
    let events = match super::api_slash_session_commands::session_events_for_usage(
        state, session_id,
    )
    .await
    {
        Ok(events) => events,
        Err(error) => return fail(error),
    };
    let last_seq = events
        .iter()
        .filter_map(|event| serde_json::to_value(event).ok())
        .filter_map(|value| value.get("seq").and_then(Value::as_i64))
        .max()
        .unwrap_or(0);
    ok(format!(
        "**Event log** for {session_id}\nStored in: {} (table `events`, rows for this session)\nEvents: {} (last sequence {last_seq})\nRead it with GET /sessions/{session_id}/events or `{export}`.",
        state.database_path,
        events.len(),
        export = crate::host_identity::command(&format!("export {session_id}"))
    ))
}

// ── /warnings ───────────────────────────────────────────────────────────────

/// Event kinds that are a warning to the user: something retried, failed, hit a limit or refused work.
const WARNING_KINDS: &[&str] = &[
    "persistenceFailed",
    "overloaded",
    "providerRetrying",
    "runFailed",
    "budgetExceeded",
    "accountRateLimited",
    "streamResyncRequired",
    "runGovernanceRejected",
];

pub(crate) async fn command_warnings(
    state: &DaemonState,
    session_id: Option<&str>,
) -> CommandResult {
    let Some(session_id) = session_id else {
        return need_session("warnings");
    };
    let events = match super::api_slash_session_commands::session_events_for_usage(
        state, session_id,
    )
    .await
    {
        Ok(events) => events,
        Err(error) => return fail(error),
    };
    let lines: Vec<String> = events
        .iter()
        .filter_map(|event| serde_json::to_value(event).ok())
        .filter_map(|value| warning_line(&value))
        .collect();
    if lines.is_empty() {
        return ok("No warnings in this chat.");
    }
    // The newest 30; older ones stay in the event log.
    let skipped = lines.len().saturating_sub(30);
    let shown = lines[skipped..].join("\n");
    let header = if skipped > 0 {
        format!("**Warnings:** {} ({skipped} older not shown)", lines.len())
    } else {
        format!("**Warnings:** {}", lines.len())
    };
    ok(format!("{header}\n{shown}"))
}

/// One line for a warning event, or `None` for any other event.
fn warning_line(event: &Value) -> Option<String> {
    let kind = event.get("$kind").and_then(Value::as_str)?;
    if !WARNING_KINDS.contains(&kind) {
        return None;
    }
    let at = event
        .get("createdAt")
        .and_then(Value::as_str)
        .unwrap_or("")
        .replace('T', " ");
    let at: String = at.chars().take(19).collect();
    let detail = ["reason", "error", "scope", "hint", "message"]
        .iter()
        .find_map(|key| {
            event
                .get(*key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
        })
        .unwrap_or("");
    Some(
        format!("- {at} {kind}: {detail}")
            .trim_end()
            .trim_end_matches(':')
            .to_string(),
    )
}

// ── /feedback ───────────────────────────────────────────────────────────────

/// Longest note kept, in characters.
const FEEDBACK_MAX_CHARS: usize = 8_000;

/// Write the user's note, with the version and the chat's id, to a local file. Nothing leaves the machine; the
/// user attaches the file wherever they choose to report.
pub(crate) fn command_feedback(args: &[String], session_id: Option<&str>) -> CommandResult {
    let text = args.join(" ");
    let text = text.trim();
    if text.is_empty() {
        return fail("usage: /feedback <what you want to report>. It is written to a local file; nothing is sent anywhere.");
    }
    let Some(root) = rightkit_agent_config::state_root() else {
        return fail("Cannot save feedback: the state folder is unavailable.");
    };
    let dir = root.join("feedback");
    if let Err(error) = std::fs::create_dir_all(&dir) {
        return fail(format!("Cannot save feedback: {error}"));
    }
    let stamp = now_iso().replace([':', '.'], "-");
    let path = dir.join(format!("feedback-{stamp}.md"));
    let note: String = text.chars().take(FEEDBACK_MAX_CHARS).collect();
    let body = format!(
        "# {} feedback\n\nVersion: {}\nSession: {}\nWritten: {}\n\n{note}\n",
        rightkit_agent_config::app_name(),
        env!("CARGO_PKG_VERSION"),
        session_id.unwrap_or("-"),
        now_iso()
    );
    match std::fs::write(&path, body) {
        Ok(()) => ok(format!(
            "Saved your note to {}. Nothing was sent anywhere; attach the file where you report.",
            path.display()
        )),
        Err(error) => fail(format!("Cannot save feedback: {error}")),
    }
}

// ── /skill-doctor ───────────────────────────────────────────────────────────

/// Skill slugs the chat's tool calls named through `invoke_skill`.
fn invoked_skill_slugs(events: &[ProtocolEvent]) -> std::collections::HashSet<String> {
    let mut used = std::collections::HashSet::new();
    for event in events {
        if let ProtocolEvent::ToolCallStarted { call, .. } = event {
            if call.name != "invoke_skill" {
                continue;
            }
            if let Ok(arguments) = serde_json::from_str::<Value>(&call.arguments_json) {
                if let Some(slug) = arguments.get("skill").and_then(Value::as_str) {
                    used.insert(slug.trim().to_ascii_lowercase());
                }
            }
        }
    }
    used
}

pub(crate) async fn command_skill_doctor(
    state: &DaemonState,
    session_id: Option<&str>,
) -> CommandResult {
    let Some(session_id) = session_id else {
        return need_session("skill-doctor");
    };
    let events = match super::api_slash_session_commands::session_events_for_usage(
        state, session_id,
    )
    .await
    {
        Ok(events) => events,
        Err(error) => return fail(error),
    };
    let used = invoked_skill_slugs(&events);
    let skills = crate::api_skills::skill_context_costs(&state.skill_dirs);
    if skills.is_empty() {
        return ok("No skills are installed.");
    }
    let mut unused: Vec<(String, usize)> = skills
        .into_iter()
        .filter(|(slug, _)| !used.contains(&slug.to_ascii_lowercase()))
        .collect();
    unused.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    if unused.is_empty() {
        return ok("Every installed skill was used in this chat.");
    }
    let total: usize = unused.iter().map(|(_, tokens)| tokens).sum();
    let rows: Vec<String> = unused
        .iter()
        .map(|(slug, tokens)| format!("unused: {slug} ~{tokens} tokens"))
        .collect();
    ok(format!(
        "**Skills this chat never used:** {} (about {total} tokens, estimated as characters / 4)\n{}",
        unused.len(),
        rows.join("\n")
    ))
}

// ── /recap ──────────────────────────────────────────────────────────────────

/// Longest transcript text sent for a recap, in bytes (the helper bounds it again).
const RECAP_MAX_BYTES: usize = 48_000;

/// The chat's user and assistant text, oldest first, newest kept when it must be cut.
fn recap_source(events: &[ProtocolEvent]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for event in events {
        match event {
            ProtocolEvent::UserMessageSubmitted { text, .. } if !text.trim().is_empty() => {
                lines.push(format!("User: {}", text.trim()));
            }
            ProtocolEvent::AssistantMessageCompleted {
                message: TranscriptMessage::Assistant { content, .. },
                ..
            } if !content.trim().is_empty() => {
                lines.push(format!("Assistant: {}", content.trim()));
            }
            _ => {}
        }
    }
    let mut kept: Vec<&String> = Vec::new();
    let mut size = 0usize;
    for line in lines.iter().rev() {
        size += line.len() + 1;
        if size > RECAP_MAX_BYTES && !kept.is_empty() {
            break;
        }
        kept.push(line);
    }
    kept.reverse();
    kept.iter()
        .map(|line| line.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `/recap`: one small-model request, made because the user typed the command. Never automatic.
pub(crate) async fn command_recap(state: &DaemonState, session_id: Option<&str>) -> CommandResult {
    use rightkit_agent_orchestration::coding::small_model_utility::{
        SmallModelBudget, DEFAULT_CALL_TOKEN_ESTIMATE,
    };

    let Some(session_id) = session_id else {
        return need_session("recap");
    };
    let live = match state.get_session(session_id) {
        Some(live) => live,
        None => match state.resume_session(session_id).await {
            Some(live) => live,
            None => return fail(format!("No session named `{session_id}`.")),
        },
    };
    let Some(route) = live.model_route.clone() else {
        return fail("This chat has no model route to recap with.");
    };
    let events = match super::api_slash_session_commands::session_events_for_usage(
        state, session_id,
    )
    .await
    {
        Ok(events) => events,
        Err(error) => return fail(error),
    };
    let source = recap_source(&events);
    if source.trim().is_empty() {
        return ok("Nothing to recap yet: this chat has no messages.");
    }
    let small_model = crate::api::load_workspace_config_with_env(FsPath::new(&live.workspace_root))
        .model
        .small_model;
    let client =
        crate::provider_client::DaemonSmallModelClient::new(route).with_model(&small_model);
    let budget = SmallModelBudget::new(1, DEFAULT_CALL_TOKEN_ESTIMATE);
    match rightkit_agent_orchestration::coding::session_recap::summarize_session(
        &client, &source, &budget,
    )
    .await
    {
        Ok(Some(recap)) => ok(format!("**Recap:** {}", recap.summary)),
        Ok(None) => ok("The model returned no recap."),
        Err(error) => fail(format!("Cannot recap: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn warning_lines_come_from_warning_kinds_only() {
        let retry = json!({"$kind": "providerRetrying", "createdAt": "2026-11-13T10:11:12.000Z", "reason": "HTTP 529"});
        assert_eq!(
            warning_line(&retry).as_deref(),
            Some("- 2026-11-13 10:11:12 providerRetrying: HTTP 529")
        );
        let done = json!({"$kind": "sessionTurnFinished", "createdAt": "2026-11-13T10:11:12.000Z"});
        assert_eq!(warning_line(&done), None);
        let bare = json!({"$kind": "persistenceFailed", "createdAt": "2026-11-13T10:11:12.000Z"});
        assert_eq!(
            warning_line(&bare).as_deref(),
            Some("- 2026-11-13 10:11:12 persistenceFailed")
        );
    }

    #[test]
    fn recap_source_keeps_the_newest_text_when_it_must_cut() {
        let events: Vec<ProtocolEvent> = (0..4)
            .map(|index| ProtocolEvent::UserMessageSubmitted {
                session_id: "s".into(),
                seq: index,
                created_at: "t".into(),
                text: format!("{index}{}", "x".repeat(20_000)),
                nonce: None,
                trust: None,
                attachments: Vec::new(),
            })
            .collect();
        let source = recap_source(&events);
        assert!(source.len() <= RECAP_MAX_BYTES + 20_100);
        assert!(source.contains("User: 3x"));
        assert!(!source.contains("User: 0x"));
    }

    #[test]
    fn feedback_without_text_is_a_usage_error_and_writes_nothing() {
        let result = command_feedback(&[], Some("s-1"));
        assert!(!result.success);
        assert!(result.output.contains("usage: /feedback"));
    }
}
