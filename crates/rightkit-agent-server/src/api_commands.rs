use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::{get, post},
    Router,
};
use rightkit_agent_host_traits::skill_host::{
    CommandAction, CommandRegistry, CommandResult, SlashCommand,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::{Path as FsPath, PathBuf};

use crate::api::{execute_daemon_command, DaemonState};
use crate::api_permissions::enforce_trusted_source_authority;
use crate::api_plugins::plugin_manifests_with_roots;

pub fn command_routes() -> Router<DaemonState> {
    Router::new()
        .route("/commands", get(list_commands_handler))
        .route("/commands/execute", post(execute_command_handler))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CommandRow {
    name: String,
    description: String,
    usage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    plugin_id: Option<String>,
}

/// List all available slash commands.
///
/// CR-24/P1-1 unification: the shared `COMMAND_SPECS` table is the ONE
/// authority for what this daemon surface recognizes, so the listing is a
/// pure projection of its interactive entries (rendered in the daemon's
/// bare-name `CommandRow` shape). There is no second built-in list to append:
/// every daemon-side builtin carries its executor on its table row. Plugin
/// commands are appended after the shared rows.
async fn list_commands_handler(State(state): State<DaemonState>) -> Json<Vec<CommandRow>> {
    let mut rows = shared_table_command_rows();
    rows.extend(plugin_command_rows(&state.plugin_dir));
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    Json(rows)
}

/// Project the interactive surface of the shared command table into the
/// daemon's bare-name `CommandRow` shape. Free function so tests can assert
/// on exactly what the route serves without standing up daemon state.
fn shared_table_command_rows() -> Vec<CommandRow> {
    crate::cli::command_registry::entries_for_surface(false)
        .map(|entry| CommandRow {
            name: entry.name.trim_start_matches('/').to_string(),
            description: entry.summary.to_string(),
            usage: entry.usage.to_string(),
            plugin_id: None,
        })
        .collect()
}

pub(crate) fn command_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();

    schemas.insert(
        "CommandRow".to_string(),
        json!({
            "type": "object",
            "required": ["name", "description", "usage"],
            "properties": {
                "name": {"type": "string"},
                "description": {"type": "string"},
                "usage": {"type": "string"},
                "pluginId": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );

    schemas.insert(
        "ExecuteCommandRequest".to_string(),
        json!({
            "type": "object",
            "required": ["input"],
            "properties": {
                "input": {"type": "string"},
                "sessionId": {"type": "string"}
            },
            "additionalProperties": false
        }),
    );

    schemas.insert(
        "CommandAction".to_string(),
        json!({
            "oneOf": [
                {
                    "type": "string",
                    "enum": ["none", "clear_chat", "compact_context", "list_sessions"]
                },
                {
                    "type": "object",
                    "required": ["switch_session"],
                    "properties": {"switch_session": {"type": "string"}},
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "required": ["submit_message"],
                    "properties": {"submit_message": {"type": "string"}},
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "required": ["conduct_goal"],
                    "properties": {"conduct_goal": {"type": "string"}},
                    "additionalProperties": false
                }
            ]
        }),
    );

    schemas.insert(
        "CommandResult".to_string(),
        json!({
            "type": "object",
            "required": ["success", "output", "action"],
            "properties": {
                "success": {"type": "boolean"},
                "output": {"type": "string"},
                "action": {"$ref": "#/components/schemas/CommandAction"}
            },
            "additionalProperties": false
        }),
    );

    schemas
}

pub(crate) fn list_commands_response_schema() -> Value {
    json!({"type": "array", "items": {"$ref": "#/components/schemas/CommandRow"}})
}

pub(crate) fn execute_command_request_schema() -> Value {
    json!({"$ref": "#/components/schemas/ExecuteCommandRequest"})
}

pub(crate) fn execute_command_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/CommandResult"})
}

/// Request body for executing a slash command.
#[derive(Deserialize)]
struct ExecuteCommandRequest {
    /// The full slash command string, e.g. "/help foo"
    input: String,
    /// Optional active session id supplied by app/web clients. Commands that
    /// naturally operate on the current session can use this without making
    /// users paste ids into the composer.
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
}

/// Execute a slash command and return the result.
async fn execute_command_handler(
    State(state): State<DaemonState>,
    headers: HeaderMap,
    Json(body): Json<ExecuteCommandRequest>,
) -> Result<Json<CommandResult>, StatusCode> {
    enforce_trusted_source_authority(&headers)?;
    Ok(Json(
        execute_command_on_state(&state, &body.input, body.session_id.as_deref()).await,
    ))
}

/// The single slash-command dispatch flow shared by the ServiceHost HTTP
/// route above and `SessionExecutionHost::execute_command` under InlineHost
/// (`main_runtime_session_host.rs`). There is no second command executor:
/// both hosts run parse -> shared-table canonical-name normalization ->
/// daemon executors (`execute_daemon_command`) -> shared `CommandRegistry`
/// -> plugin commands. Non-command input and unknown names return
/// `success: false` results rather than errors, matching the route
/// contract; per-command failures already surface inside `CommandResult`.
pub(crate) async fn execute_command_on_state(
    state: &DaemonState,
    input: &str,
    session_id: Option<&str>,
) -> CommandResult {
    let Some(cmd) = SlashCommand::parse(input) else {
        return CommandResult {
            success: false,
            output: "Not a slash command (must start with /)".into(),
            action: CommandAction::None,
        };
    };
    // P1-1/CR-24 authority, applied BEFORE execution (not only before
    // listing): the typed name is normalized to its canonical bare form
    // through the shared table, so aliases (`/sessions`, `/history`, `/q`,
    // `tbench`, …) dispatch exactly as their canonical names would. Names
    // the table does not know (plugin commands, unknown words) pass through
    // untouched.
    let mut normalized = cmd.clone();
    if let Some(bare) = crate::cli::command_registry::canonical_bare_name(&cmd.name) {
        normalized.name = bare.to_string();
    }
    let registry = CommandRegistry::new();
    if let Some(result) = execute_daemon_command(state, &normalized, session_id).await {
        result
    } else if let Some(result) = registry.execute(&normalized.name, &normalized.args) {
        dispatch_command_action(state, result, session_id).await
    } else if let Some((manifest, root)) = find_plugin_command(&state.plugin_dir, &cmd.name) {
        execute_plugin_command(manifest, root, &cmd).await
    } else {
        CommandResult {
            success: false,
            output: format!(
                "Unknown command: /{}\n\nType /help for available commands.",
                cmd.name
            ),
            action: CommandAction::None,
        }
    }
}

/// Execute registry actions whose authority lives in RightKit's session API.
/// UI-only actions remain in the result for their caller; backend actions are
/// consumed here so CLI/TUI callers cannot report success while dropping work.
async fn dispatch_command_action(
    state: &DaemonState,
    result: CommandResult,
    session_id: Option<&str>,
) -> CommandResult {
    match result.action.clone() {
        CommandAction::CompactContext => {
            let Some(session_id) = session_id else {
                return action_error("compact context", "no active session");
            };
            let response = crate::api_sessions::approvals::compact_session(
                axum::extract::State(state.clone()),
                axum::extract::Path(session_id.to_string()),
                HeaderMap::new(),
            )
            .await;
            match response {
                Ok(axum::response::Json(body)) => CommandResult {
                    success: true,
                    output: format!(
                        "Context compacted: {}",
                        body.get("summary")
                            .and_then(Value::as_str)
                            .filter(|summary| !summary.trim().is_empty())
                            .unwrap_or("completed")
                    ),
                    action: CommandAction::None,
                },
                Err(status) => action_error(
                    "compact context",
                    &format!("session API returned HTTP {status}"),
                ),
            }
        }
        CommandAction::ConductGoal(goal) => {
            let Some(session_id) = session_id else {
                return action_error("conduct goal", "no active session");
            };
            let response = crate::api_sessions::orchestration::orchestrate_session(
                axum::extract::State(state.clone()),
                axum::extract::Path(session_id.to_string()),
                HeaderMap::new(),
                axum::extract::Json(crate::api::OrchestrateBody {
                    goal,
                    dry_run: false,
                    request_label: None,
                    nonce: None,
                }),
            )
            .await;
            match response {
                Ok(_) => CommandResult {
                    success: true,
                    output: result.output,
                    action: CommandAction::None,
                },
                Err(status) => action_error(
                    "conduct goal",
                    &format!("session API returned HTTP {status}"),
                ),
            }
        }
        CommandAction::DreamNow(_) => action_error(
            "memory dream",
            "no RightKit memory-dream command consumer is available",
        ),
        _ => result,
    }
}

fn action_error(action: &str, detail: &str) -> CommandResult {
    CommandResult {
        success: false,
        output: format!("Cannot {action}: {detail}"),
        action: CommandAction::None,
    }
}

fn plugin_command_rows(plugin_dir: &FsPath) -> Vec<CommandRow> {
    plugin_manifests_with_roots(plugin_dir)
        .into_iter()
        .filter(|(m, _)| m.enabled)
        .flat_map(|(manifest, _root)| {
            let command_metadata = manifest.commands.clone();
            let plugin_id = manifest.id;
            manifest.provides_commands.into_iter().map(move |name| {
                let metadata = command_metadata.get(&name);
                let description = metadata
                    .map(|m| m.description.trim())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("Plugin command from {}", plugin_id));
                let usage = metadata
                    .map(|m| m.usage.trim())
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("/{name} [args...]"));
                CommandRow {
                    name,
                    description,
                    usage,
                    plugin_id: Some(plugin_id.clone()),
                }
            })
        })
        .collect()
}

fn find_plugin_command(
    plugin_dir: &FsPath,
    command: &str,
) -> Option<(rightkit_agent_plugins::PluginManifest, PathBuf)> {
    plugin_manifests_with_roots(plugin_dir)
        .into_iter()
        .find(|(manifest, _)| {
            manifest.enabled && manifest.provides_commands.iter().any(|c| c == command)
        })
}

async fn execute_plugin_command(
    manifest: rightkit_agent_plugins::PluginManifest,
    root: PathBuf,
    cmd: &SlashCommand,
) -> CommandResult {
    let entry = PathBuf::from(&manifest.entry);
    let workspace_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let adapter = match plugin_command_adapter() {
        Ok(adapter) => adapter,
        Err(error) => {
            return CommandResult {
                success: false,
                output: format!("Plugin command /{} denied: {error}", cmd.name),
                action: CommandAction::None,
            };
        }
    };
    match rightkit_agent_plugins::run_plugin_entry_with_policy_and_adapter(
        &entry,
        &root,
        rightkit_agent_plugins::PluginInvocation {
            kind: "command".into(),
            name: cmd.name.clone(),
            args: serde_json::json!(cmd.args),
            session_id: "command".into(),
            workspace_root,
        },
        manifest.sandbox,
        adapter,
    )
    .await
    {
        Ok(result) => CommandResult {
            success: !result.is_error,
            output: result.content,
            action: CommandAction::None,
        },
        Err(err) => CommandResult {
            success: false,
            output: format!("Plugin command /{} failed: {err}", cmd.name),
            action: CommandAction::None,
        },
    }
}

fn plugin_command_adapter(
) -> Result<std::sync::Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>, String> {
    {
        crate::main_runtime::governed_child_process_launcher()
    }
}
