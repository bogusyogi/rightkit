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
                    "description": "Optional default concurrency cap for this team when used as a team preset."
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding_tools::context::{TeamDefinition, TeamDirectory};
    use rightkit_agent_policy::{PermissionMode, WorkspaceTrust};
    use std::collections::HashMap;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct RecordingTeamDirectory {
        teams: Mutex<HashMap<String, TeamDefinition>>,
        deleted: Mutex<Vec<String>>,
    }

    impl TeamDirectory for RecordingTeamDirectory {
        fn create_team(
            &self,
            team: TeamDefinition,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
            self.teams.lock().unwrap().insert(team.name.clone(), team);
            Box::pin(async { Ok("live team registered".to_string()) })
        }

        fn delete_team(
            &self,
            name: String,
        ) -> Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>> {
            self.deleted.lock().unwrap().push(name.clone());
            self.teams.lock().unwrap().remove(&name);
            Box::pin(async { Ok("live team deleted".to_string()) })
        }
    }

    fn live_ctx(directory: Arc<RecordingTeamDirectory>) -> ToolContext {
        ToolContext::new(
            "s1",
            std::env::temp_dir(),
            PermissionMode::Bypass,
            WorkspaceTrust::Trusted,
        )
        .with_team_directory(directory)
    }

    #[tokio::test]
    async fn team_create_registers_live_team_when_available() {
        let directory = Arc::new(RecordingTeamDirectory::default());

        let result = TeamCreateTool
            .execute(
                json!({
                    "name": "review-squad",
                    "description": "review changes",
                    "members": ["architect", "tester"],
                    "max_workers": 2,
                    "budget_usd": 1.25
                }),
                &live_ctx(directory.clone()),
            )
            .await;

        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("live team registered"));
        let teams = directory.teams.lock().unwrap();
        let team = teams.get("review-squad").expect("team registered");
        assert_eq!(team.description.as_deref(), Some("review changes"));
        assert_eq!(team.members, vec!["architect", "tester"]);
        assert_eq!(team.max_workers, Some(2));
        assert_eq!(team.budget_usd, Some(1.25));
    }

    #[tokio::test]
    async fn team_delete_removes_live_team_when_available() {
        let directory = Arc::new(RecordingTeamDirectory::default());
        directory.teams.lock().unwrap().insert(
            "review-squad".to_string(),
            TeamDefinition {
                name: "review-squad".to_string(),
                description: None,
                members: vec!["architect".to_string()],
                max_workers: None,
                budget_usd: None,
            },
        );

        let result = TeamDeleteTool
            .execute(
                json!({ "name": "review-squad" }),
                &live_ctx(directory.clone()),
            )
            .await;

        assert!(!result.is_error, "{}", result.content);
        assert!(result.content.contains("live team deleted"));
        assert!(!directory.teams.lock().unwrap().contains_key("review-squad"));
        assert_eq!(
            directory.deleted.lock().unwrap().as_slice(),
            &["review-squad".to_string()]
        );
    }
}
