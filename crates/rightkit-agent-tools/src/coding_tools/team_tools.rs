//! `team_create` and `team_delete` — manage agent team config files.

use crate::coding_tools::context::{TeamDefinition, ToolContext};
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};

pub struct TeamCreateTool;
pub struct TeamDeleteTool;

fn teams_dir() -> String {
    match rightkit_app_settings::agent_config::state_root() {
        Some(root) => root.join("teams").to_string_lossy().into_owned(),
        None => "/tmp/.rightkit/teams".to_string(),
    }
}

#[async_trait::async_trait]
impl Tool for TeamCreateTool {
    fn name(&self) -> &str {
        "team_create"
    }

    fn description(&self) -> &str {
        "Create an agent team configuration file at ~/.host/teams/<name>.toml. \
         Defines a named group of agent roles that can be spawned together. \
         Returns 'Team created: {name}' on success."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["name", "members"],
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Team name (used as filename slug)."
                },
                "members": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of agent role names in this team."
                },
                "description": {
                    "type": "string",
                    "description": "Optional human-readable description of the team's purpose."
                },
                "max_workers": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional default concurrency cap for this team when used as a planning template."
                },
                "budget_usd": {
                    "type": "number",
                    "minimum": 0,
                    "description": "Optional default dollar budget hint for this team's work."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let name = match args["name"].as_str() {
            Some(n) => n,
            None => return ToolResult::error("team_create: 'name' is required"),
        };
        let members = match args["members"].as_array() {
            Some(m) => m,
            None => return ToolResult::error("team_create: 'members' must be an array"),
        };
        let description = args["description"].as_str().unwrap_or("");
        let member_names: Vec<String> = members
            .iter()
            .filter_map(|m| m.as_str().map(|s| s.to_string()))
            .collect();
        if member_names.len() != members.len() {
            return ToolResult::error("team_create: 'members' must contain only strings");
        }
        let max_workers = match args.get("max_workers").and_then(|v| v.as_u64()) {
            Some(0) => return ToolResult::error("team_create: 'max_workers' must be at least 1"),
            Some(value) if value > u32::MAX as u64 => {
                return ToolResult::error("team_create: 'max_workers' is too large")
            }
            Some(value) => Some(value as u32),
            None => None,
        };
        let budget_usd = match args.get("budget_usd").and_then(|v| v.as_f64()) {
            Some(value) if value < 0.0 => {
                return ToolResult::error("team_create: 'budget_usd' cannot be negative")
            }
            Some(value) => Some(value),
            None => None,
        };

        if let Some(directory) = &ctx.team_directory {
            let team = TeamDefinition {
                name: name.to_string(),
                description: if description.is_empty() {
                    None
                } else {
                    Some(description.to_string())
                },
                members: member_names,
                max_workers,
                budget_usd,
            };
            return match directory.create_team(team).await {
                Ok(summary) => ToolResult::ok(format!("Team created: {name} ({summary})")),
                Err(e) => ToolResult::error(format!("team_create: live registration failed: {e}")),
            };
        }

        let dir = teams_dir();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return ToolResult::error(format!("team_create: failed to create teams dir: {e}"));
        }

        let member_lines: Vec<String> = members
            .iter()
            .filter_map(|m| m.as_str())
            .map(|m| format!("  \"{m}\""))
            .collect();
        let members_toml = member_lines.join(",\n");

        let mut preset_lines = String::new();
        if let Some(max_workers) = max_workers {
            preset_lines.push_str(&format!("max_workers = {max_workers}\n"));
        }
        if let Some(budget_usd) = budget_usd {
            preset_lines.push_str(&format!("budget_usd = {budget_usd}\n"));
        }

        let toml = if description.is_empty() {
            format!("[team]\nname = \"{name}\"\n{preset_lines}members = [\n{members_toml}\n]\n")
        } else {
            format!(
                "[team]\nname = \"{name}\"\ndescription = \"{description}\"\n{preset_lines}members = [\n{members_toml}\n]\n"
            )
        };

        let path = std::path::Path::new(&dir).join(format!("{name}.toml"));
        if let Err(e) = std::fs::write(&path, toml) {
            return ToolResult::error(format!("team_create: failed to write team file: {e}"));
        }

        ToolResult::ok(format!("Team created: {name}"))
    }
}

#[async_trait::async_trait]
impl Tool for TeamDeleteTool {
    fn name(&self) -> &str {
        "team_delete"
    }

    fn description(&self) -> &str {
        "Delete an agent team configuration file at ~/.host/teams/<name>.toml. \
         Returns 'Team deleted: {name}' on success or an error if not found."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Team name to delete."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let name = match args["name"].as_str() {
            Some(n) => n,
            None => return ToolResult::error("team_delete: 'name' is required"),
        };

        if let Some(directory) = &ctx.team_directory {
            return match directory.delete_team(name.to_string()).await {
                Ok(summary) => ToolResult::ok(format!("Team deleted: {name} ({summary})")),
                Err(e) => ToolResult::error(format!("team_delete: live deletion failed: {e}")),
            };
        }

        let dir = teams_dir();
        let path = std::path::Path::new(&dir).join(format!("{name}.toml"));

        if !path.exists() {
            return ToolResult::error(format!("team_delete: team not found: {name}"));
        }

        if let Err(e) = std::fs::remove_file(&path) {
            return ToolResult::error(format!("team_delete: failed to delete team file: {e}"));
        }

        ToolResult::ok(format!("Team deleted: {name}"))
    }
}
