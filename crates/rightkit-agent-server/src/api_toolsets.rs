use axum::{http::StatusCode, response::Json, routing::get, Router};
use rightkit_app_settings::agent_config::{HostConfig, ToolsetConfig};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashSet};

use crate::api::DaemonState;

pub(crate) const SCHEDULE_ADMIN_TOOLSET: &str = "schedule-admin";

pub(crate) fn toolset_routes() -> Router<DaemonState> {
    Router::new()
        .route("/toolsets", get(list_toolsets_handler))
        .route("/toolsets/gateways", get(list_toolset_gateways_handler))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolsetProfile {
    pub id: String,
    pub name: String,
    pub description: String,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub read_only: bool,
    pub default_behavioral_skills: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolGatewayStatus {
    id: String,
    name: String,
    toolset: String,
    description: String,
    status: String,
    provider: String,
    secret_status: String,
    tools: Vec<ToolGatewayToolStatus>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolGatewayToolStatus {
    id: String,
    available: bool,
}

async fn list_toolsets_handler() -> Json<Vec<ToolsetProfile>> {
    let config = rightkit_app_settings::agent_config::ConfigLoader::new().load_files_default();
    Json(toolset_profiles_for_config(&config))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolGatewayStatusResponse {
    gateways: Vec<ToolGatewayStatus>,
}

async fn list_toolset_gateways_handler() -> Json<ToolGatewayStatusResponse> {
    let config = rightkit_app_settings::agent_config::ConfigLoader::new().load_files_default();
    Json(ToolGatewayStatusResponse {
        gateways: tool_gateway_statuses(&config),
    })
}

pub(crate) fn toolset_openapi_components() -> Map<String, Value> {
    let mut schemas = Map::new();
    schemas.insert(
        "ToolsetProfile".to_string(),
        json!({
            "type": "object",
            "required": ["id", "name", "description", "allowedTools", "disallowedTools", "readOnly", "defaultBehavioralSkills"],
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "description": {"type": "string"},
                "allowedTools": {"type": "array", "items": {"type": "string"}},
                "disallowedTools": {"type": "array", "items": {"type": "string"}},
                "readOnly": {"type": "boolean"},
                "defaultBehavioralSkills": {"type": "array", "items": {"type": "string"}}
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ToolGatewayStatusResponse".to_string(),
        json!({
            "type": "object",
            "required": ["gateways"],
            "properties": {
                "gateways": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/ToolGatewayStatus"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ToolGatewayStatus".to_string(),
        json!({
            "type": "object",
            "required": ["id", "name", "toolset", "description", "status", "provider", "secretStatus", "tools"],
            "properties": {
                "id": {"type": "string"},
                "name": {"type": "string"},
                "toolset": {"type": "string"},
                "description": {"type": "string"},
                "status": {"type": "string", "enum": ["ready", "degraded"]},
                "provider": {"type": "string"},
                "secretStatus": {"type": "string"},
                "tools": {
                    "type": "array",
                    "items": {"$ref": "#/components/schemas/ToolGatewayToolStatus"}
                }
            },
            "additionalProperties": false
        }),
    );
    schemas.insert(
        "ToolGatewayToolStatus".to_string(),
        json!({
            "type": "object",
            "required": ["id", "available"],
            "properties": {
                "id": {"type": "string"},
                "available": {"type": "boolean"}
            },
            "additionalProperties": false
        }),
    );
    schemas
}

pub(crate) fn toolsets_list_response_schema() -> Value {
    json!({"type": "array", "items": {"$ref": "#/components/schemas/ToolsetProfile"}})
}

pub(crate) fn toolset_gateways_response_schema() -> Value {
    json!({"$ref": "#/components/schemas/ToolGatewayStatusResponse"})
}

fn tool_gateway_statuses(config: &HostConfig) -> Vec<ToolGatewayStatus> {
    let available_tools = crate::session_tool_catalog::default_tool_capabilities()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<HashSet<_>>();
    let web_provider = config
        .tools
        .web_search_provider
        .as_deref()
        .filter(|provider| !provider.trim().is_empty())
        .unwrap_or("default");
    vec![
        gateway_status(
            &available_tools,
            "research-gateway",
            "Research gateway",
            "research-gateway",
            "Read-only local, web, recursive-fetch, MCP discovery, and context helper tools.",
            web_provider,
            "not_required",
            &[
                "web_search",
                "browser_fetch",
                "recursive_fetch",
                "research_report",
                "list_mcp_tools",
                "list_mcp_sources",
                "list_mcp_resources",
                "read_mcp_resource",
                "context7",
                "tool_search",
            ],
        ),
        gateway_status(
            &available_tools,
            "web-research",
            "Web research",
            "safe-research",
            "Provider-backed web/search tools usable from safe research profiles.",
            web_provider,
            "not_required",
            &[
                "web_search",
                "browser_fetch",
                "recursive_fetch",
                "research_report",
            ],
        ),
        gateway_status(
            &available_tools,
            "mcp-discovery",
            "MCP discovery",
            "research-gateway",
            "Read-only MCP discovery and resource access without granting mutating MCP calls.",
            "mcp",
            "not_required",
            &[
                "list_mcp_tools",
                "list_mcp_sources",
                "list_mcp_resources",
                "read_mcp_resource",
            ],
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn gateway_status(
    available_tools: &HashSet<String>,
    id: &str,
    name: &str,
    toolset: &str,
    description: &str,
    provider: &str,
    secret_status: &str,
    tools: &[&str],
) -> ToolGatewayStatus {
    let tools = tools
        .iter()
        .map(|tool| ToolGatewayToolStatus {
            id: (*tool).to_string(),
            available: available_tools.contains(*tool),
        })
        .collect::<Vec<_>>();
    let status = if tools.iter().all(|tool| tool.available) {
        "ready"
    } else {
        "degraded"
    };
    ToolGatewayStatus {
        id: id.to_string(),
        name: name.to_string(),
        toolset: toolset.to_string(),
        description: description.to_string(),
        status: status.to_string(),
        provider: provider.to_string(),
        secret_status: secret_status.to_string(),
        tools,
    }
}

pub(crate) fn resolve_toolset_profile(id: &str) -> Result<ToolsetProfile, StatusCode> {
    builtin_toolset_profiles()
        .into_iter()
        .find(|profile| profile.id == id)
        .ok_or(StatusCode::BAD_REQUEST)
}

pub(crate) fn resolve_toolset_profile_with_config(
    id: &str,
    config: &HostConfig,
) -> Result<ToolsetProfile, StatusCode> {
    toolset_profiles_for_config(config)
        .into_iter()
        .find(|profile| profile.id == id)
        .ok_or(StatusCode::BAD_REQUEST)
}

pub(crate) fn toolset_profiles_for_config(config: &HostConfig) -> Vec<ToolsetProfile> {
    let mut profiles = builtin_toolset_profiles();
    let configured = config.toolsets.iter().collect::<BTreeMap<_, _>>();
    for (id, config) in configured {
        let profile = toolset_profile_from_config(id, config);
        if let Some(existing) = profiles
            .iter_mut()
            .find(|candidate| candidate.id == profile.id)
        {
            *existing = profile;
        } else {
            profiles.push(profile);
        }
    }
    profiles
}

fn toolset_profile_from_config(id: &str, config: &ToolsetConfig) -> ToolsetProfile {
    let trimmed_id = id.trim();
    ToolsetProfile {
        id: trimmed_id.to_string(),
        name: if config.name.trim().is_empty() {
            trimmed_id.to_string()
        } else {
            config.name.trim().to_string()
        },
        description: config.description.trim().to_string(),
        allowed_tools: config.allowed_tools.clone(),
        disallowed_tools: config.disallowed_tools.clone(),
        read_only: config.read_only,
        default_behavioral_skills: config.default_behavioral_skills.clone(),
    }
}

pub(crate) fn apply_toolset_profile(
    profile: Option<&ToolsetProfile>,
    requested_allowed_tools: Vec<String>,
    requested_disallowed_tools: Vec<String>,
) -> (Vec<String>, Vec<String>) {
    let Some(profile) = profile else {
        return (requested_allowed_tools, requested_disallowed_tools);
    };

    let allowed = if profile.allowed_tools.is_empty() {
        requested_allowed_tools
    } else if requested_allowed_tools.is_empty() {
        profile.allowed_tools.clone()
    } else {
        requested_allowed_tools
            .into_iter()
            .filter(|tool| profile.allowed_tools.contains(tool))
            .collect()
    };

    let mut disallowed = profile.disallowed_tools.clone();
    for tool in requested_disallowed_tools {
        if !disallowed.contains(&tool) {
            disallowed.push(tool);
        }
    }
    let allowed = if disallowed.is_empty() {
        allowed
    } else {
        allowed
            .into_iter()
            .filter(|tool| !disallowed.contains(tool))
            .collect()
    };
    (allowed, disallowed)
}

pub(crate) fn builtin_toolset_profiles() -> Vec<ToolsetProfile> {
    vec![
        ToolsetProfile {
            id: "full".to_string(),
            name: "Full".to_string(),
            description: "Default RightKit tool surface with normal permission policy enforcement."
                .to_string(),
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            read_only: false,
            default_behavioral_skills: baseline_behavioral_skills(),
        },
        ToolsetProfile {
            id: "desktop-full".to_string(),
            name: "Desktop full".to_string(),
            description:
                "Full local desktop tool surface with normal permission policy enforcement."
                    .to_string(),
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            read_only: false,
            default_behavioral_skills: baseline_behavioral_skills(),
        },
        ToolsetProfile {
            id: "read-only".to_string(),
            name: "Read-only".to_string(),
            description:
                "Read/search/inspect only; no writes, shell execution, scheduling, or delegation."
                    .to_string(),
            allowed_tools: read_only_tool_names(),
            disallowed_tools: mutating_tool_names(),
            read_only: true,
            default_behavioral_skills: baseline_behavioral_skills(),
        },
        ToolsetProfile {
            id: "safe-research".to_string(),
            name: "Safe research".to_string(),
            description:
                "Read-only local and web research profile for inspecting context without mutation."
                    .to_string(),
            allowed_tools: names(&[
                "read_file",
                "search",
                "list_files",
                "git_read",
                "git_diff",
                "browser_fetch",
                "web_search",
                "recursive_fetch",
                "research_report",
                "tool_search",
                "context7",
                "get_time",
                "sequentialthinking",
                "ask_user",
            ]),
            disallowed_tools: mutating_tool_names(),
            read_only: true,
            default_behavioral_skills: baseline_behavioral_skills(),
        },
        ToolsetProfile {
            id: "review".to_string(),
            name: "Review".to_string(),
            description:
                "Code review profile for reading files, searching, inspecting git state, and checklists."
                    .to_string(),
            allowed_tools: names(&[
                "read_file",
                "search",
                "list_files",
                "git_read",
                "git_diff",
                "tool_search",
                "todo_write",
                "ask_user",
            ]),
            disallowed_tools: mutating_tool_names(),
            read_only: true,
            default_behavioral_skills: baseline_behavioral_skills(),
        },
        ToolsetProfile {
            id: "dev-safe".to_string(),
            name: "Dev safe".to_string(),
            description:
                "File-editing profile without shell, scheduling, browser control, messaging, or spawning."
                    .to_string(),
            allowed_tools: names(&[
                "read_file",
                "search",
                "list_files",
                "git_read",
                "git_diff",
                "write_file",
                "edit_file",
                "multi_edit",
                "apply_patch",
                "undo_edit",
                "tool_search",
                "todo_write",
                "ask_user",
                "finalize_artifact",
            ]),
            disallowed_tools: names(&[
                "shell",
                "powershell",
                "schedule_cron",
                "spawn_subagent",
                "task_create",
                "send_message",
                "browser",
            ]),
            read_only: false,
            default_behavioral_skills: baseline_behavioral_skills(),
        },
        ToolsetProfile {
            id: "coding-local".to_string(),
            name: "Coding local".to_string(),
            description:
                "Trusted local coding profile with file edits, git inspection, and local execution."
                    .to_string(),
            allowed_tools: names(&[
                "read_file",
                "search",
                "list_files",
                "lsp",
                "git_read",
                "git_diff",
                "write_file",
                "edit_file",
                "multi_edit",
                "apply_patch",
                "undo_edit",
                "shell",
                "powershell",
                "get_shell_output",
                "kill_shell",
                "repl",
                "task_create",
                "task_update",
                "task_get",
                "task_list",
                "task_output",
                "task_stop",
                "tool_search",
                "todo_write",
                "ask_user",
                "finalize_artifact",
            ]),
            disallowed_tools: names(&["schedule_cron", "spawn_subagent", "send_message", "browser"]),
            read_only: false,
            default_behavioral_skills: baseline_behavioral_skills(),
        },
        ToolsetProfile {
            id: "cron-safe".to_string(),
            name: "Cron safe".to_string(),
            description:
                "Scheduled-agent profile for deterministic review/research jobs without schedule administration."
                    .to_string(),
            allowed_tools: names(&[
                "read_file",
                "search",
                "list_files",
                "git_read",
                "git_diff",
                "browser_fetch",
                "web_search",
                "tool_search",
                "todo_write",
                "ask_user",
                "finalize_artifact",
            ]),
            disallowed_tools: names(&[
                "shell",
                "powershell",
                "schedule_cron",
                "spawn_subagent",
                "task_create",
                "send_message",
                "browser",
            ]),
            read_only: false,
            default_behavioral_skills: baseline_behavioral_skills(),
        },
        ToolsetProfile {
            id: "telegram-remote".to_string(),
            name: "Telegram remote".to_string(),
            description:
                "Remote-control profile for Telegram sessions: inspect, summarize, and ask back only."
                    .to_string(),
            allowed_tools: names(&[
                "read_file",
                "search",
                "list_files",
                "git_diff",
                "browser_fetch",
                "web_search",
                "tool_search",
                "ask_user",
            ]),
            disallowed_tools: mutating_tool_names(),
            read_only: true,
            default_behavioral_skills: baseline_behavioral_skills(),
        },
        ToolsetProfile {
            id: "research-gateway".to_string(),
            name: "Research gateway".to_string(),
            description:
                "Higher-level research gateway profile with read-only local, web, and MCP discovery."
                    .to_string(),
            allowed_tools: names(&[
                "read_file",
                "search",
                "list_files",
                "git_read",
                "git_diff",
                "browser_fetch",
                "web_search",
                "recursive_fetch",
                "research_report",
                "list_mcp_tools",
                "list_mcp_sources",
                "list_mcp_resources",
                "read_mcp_resource",
                "tool_search",
                "context7",
                "get_time",
                "sequentialthinking",
                "ask_user",
            ]),
            disallowed_tools: mutating_tool_names(),
            read_only: true,
            default_behavioral_skills: baseline_behavioral_skills(),
        },
        ToolsetProfile {
            id: SCHEDULE_ADMIN_TOOLSET.to_string(),
            name: "Schedule admin".to_string(),
            description:
                "Explicit scheduled-job profile allowed to create, list, and cancel schedules."
                    .to_string(),
            allowed_tools: names(&[
                "read_file",
                "search",
                "list_files",
                "git_read",
                "git_diff",
                "schedule_cron",
                "tool_search",
                "todo_write",
                "ask_user",
                "finalize_artifact",
            ]),
            disallowed_tools: mutating_tool_names()
                .into_iter()
                .filter(|tool| tool != "schedule_cron")
                .collect(),
            read_only: false,
            default_behavioral_skills: baseline_behavioral_skills(),
        },
    ]
}

fn baseline_behavioral_skills() -> Vec<String> {
    vec!["cr-baseline-behavior".to_string()]
}

fn read_only_tool_names() -> Vec<String> {
    let mut names = crate::session_tool_catalog::default_tool_capabilities()
        .into_iter()
        .filter(|tool| tool.read_only)
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn mutating_tool_names() -> Vec<String> {
    let mut names = crate::session_tool_catalog::default_tool_capabilities()
        .into_iter()
        .filter(|tool| !tool.read_only)
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}
