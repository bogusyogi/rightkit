//! `config` — read/set daemon config keys (get/set/list).
//!
//! Operates on a simple in-memory serde_json::Map passed via ToolContext or a config path.
//! Real get/set/list operations.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::lock_recover::lock_recover;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub struct ConfigTool {
    config: Arc<Mutex<serde_json::Map<String, Value>>>,
}

impl ConfigTool {
    pub fn new() -> Self {
        Self {
            config: Arc::new(Mutex::new(serde_json::Map::new())),
        }
    }

    pub fn with_initial_config(config: serde_json::Map<String, Value>) -> Self {
        Self {
            config: Arc::new(Mutex::new(config)),
        }
    }
}

impl Default for ConfigTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for ConfigTool {
    fn name(&self) -> &str {
        "config"
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn description(&self) -> &str {
        "Read/set/list daemon config keys. Ops: get(key), set(key, value), list(). Returns JSON values. Fail: key not found (get), invalid key format (set)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["operation"],
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["get", "set", "list"],
                    "description": "Operation to perform"
                },
                "key": {
                    "type": "string",
                    "description": "Configuration key (get/set operations)"
                },
                "value": {
                    "description": "Value to set (set operation, any JSON type)"
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let operation = match args["operation"].as_str() {
            Some(op) => op,
            None => return ToolResult::error("Missing required argument: operation"),
        };

        match operation {
            "get" => {
                let key = match args["key"].as_str() {
                    Some(k) => k,
                    None => return ToolResult::error("Missing key for get"),
                };
                self.get_config(key, ctx)
            }
            "set" => {
                let key = match args["key"].as_str() {
                    Some(k) => k,
                    None => return ToolResult::error("Missing key for set"),
                };
                if !args.get("value").is_some() {
                    return ToolResult::error("Missing value for set");
                }
                let value = args["value"].clone();
                self.set_config(key, value, ctx)
            }
            "list" => self.list_config(ctx),
            other => ToolResult::error(format!("Unknown operation: {}", other)),
        }
    }
}

impl ConfigTool {
    fn get_config(&self, key: &str, ctx: &ToolContext) -> ToolResult {
        let config = match self.load_effective_config(ctx) {
            Ok(c) => c,
            Err(e) => return ToolResult::error(e),
        };

        match config.get(key) {
            Some(value) => ToolResult::ok(value.to_string()),
            None => ToolResult::error(format!("Configuration key '{}' not found", key)),
        }
    }

    fn set_config(&self, key: &str, value: Value, ctx: &ToolContext) -> ToolResult {
        // Validate key format
        if key.is_empty() || key.len() > 256 {
            return ToolResult::error("Key must be non-empty and <= 256 characters");
        }

        if !key
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
        {
            return ToolResult::error(
                "Key must contain only alphanumeric characters, underscores, and dots",
            );
        }

        let mut config = match self.load_effective_config(ctx) {
            Ok(c) => c,
            Err(e) => return ToolResult::error(e),
        };
        config.insert(key.to_string(), value.clone());

        if let Err(e) = save_workspace_config(ctx, &config) {
            return ToolResult::error(e);
        }

        let mut memory = lock_recover(&self.config, "config_tool.config");
        *memory = config;

        ToolResult::ok(format!("Set config key '{}' to {}", key, value.to_string()))
    }

    fn list_config(&self, ctx: &ToolContext) -> ToolResult {
        let config = match self.load_effective_config(ctx) {
            Ok(c) => c,
            Err(e) => return ToolResult::error(e),
        };

        if config.is_empty() {
            return ToolResult::ok("No configuration keys set");
        }

        let mut output = String::from("Configuration:\n");
        for (key, value) in config.iter() {
            output.push_str(&format!("  {}: {}\n", key, value));
        }

        ToolResult::ok(output)
    }

    fn load_effective_config(
        &self,
        ctx: &ToolContext,
    ) -> Result<serde_json::Map<String, Value>, String> {
        let mut merged = lock_recover(&self.config, "config_tool.config").clone();
        let persisted = load_workspace_config(ctx)?;
        for (key, value) in persisted {
            merged.insert(key, value);
        }
        Ok(merged)
    }
}

fn config_path(ctx: &ToolContext) -> PathBuf {
    ctx.workspace_root
        .join(".rightkit")
        .join("agent-config.json")
}

fn load_workspace_config(ctx: &ToolContext) -> Result<serde_json::Map<String, Value>, String> {
    let path = config_path(ctx);
    if !path.exists() {
        return Ok(serde_json::Map::new());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("Failed to read config '{}': {e}", path.display()))?;
    serde_json::from_str::<serde_json::Map<String, Value>>(&text)
        .map_err(|e| format!("Invalid config JSON '{}': {e}", path.display()))
}

fn save_workspace_config(
    ctx: &ToolContext,
    config: &serde_json::Map<String, Value>,
) -> Result<(), String> {
    let path = config_path(ctx);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "Failed to create config directory '{}': {e}",
                parent.display()
            )
        })?;
    }
    let text = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize config: {e}"))?;
    std::fs::write(&path, text)
        .map_err(|e| format!("Failed to write config '{}': {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::PermissionMode;
    use rightkit_agent_policy::WorkspaceTrust;
    use std::path::PathBuf;

    fn test_workspace(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("rightkit-agent-test-tmp")
            .join(format!("config-{label}-{nanos}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[tokio::test]
    async fn test_set_and_get_config() {
        let tool = ConfigTool::new();
        let ctx = ToolContext::new(
            "test",
            test_workspace("set-get"),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let set_result = tool
            .execute(
                json!({"operation": "set", "key": "test_key", "value": "test_value"}),
                &ctx,
            )
            .await;

        assert!(!set_result.is_error, "{}", set_result.content);

        let get_result = tool
            .execute(json!({"operation": "get", "key": "test_key"}), &ctx)
            .await;

        assert!(!get_result.is_error);
        assert!(get_result.content.contains("test_value"));
    }

    #[tokio::test]
    async fn test_get_nonexistent_key() {
        let tool = ConfigTool::new();
        let ctx = ToolContext::new(
            "test",
            test_workspace("missing"),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let result = tool
            .execute(json!({"operation": "get", "key": "nonexistent"}), &ctx)
            .await;

        assert!(result.is_error);
    }

    #[tokio::test]
    async fn test_list_config() {
        let tool = ConfigTool::new();
        let ctx = ToolContext::new(
            "test",
            test_workspace("list"),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let _ = tool
            .execute(
                json!({"operation": "set", "key": "key1", "value": 123}),
                &ctx,
            )
            .await;

        let result = tool.execute(json!({"operation": "list"}), &ctx).await;

        assert!(!result.is_error);
        assert!(result.content.contains("key1"));
    }

    #[tokio::test]
    async fn test_invalid_key_format() {
        let tool = ConfigTool::new();
        let ctx = ToolContext::new(
            "test",
            test_workspace("invalid-key"),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let result = tool
            .execute(
                json!({"operation": "set", "key": "invalid-key!", "value": "value"}),
                &ctx,
            )
            .await;

        assert!(result.is_error);
    }

    #[tokio::test]
    async fn test_set_various_value_types() {
        let tool = ConfigTool::new();
        let ctx = ToolContext::new(
            "test",
            test_workspace("unknown-op"),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        // String
        let _ = tool
            .execute(
                json!({"operation": "set", "key": "str_key", "value": "string_val"}),
                &ctx,
            )
            .await;

        // Number
        let _ = tool
            .execute(
                json!({"operation": "set", "key": "num_key", "value": 42}),
                &ctx,
            )
            .await;

        // Object
        let _ = tool
            .execute(
                json!({"operation": "set", "key": "obj_key", "value": {"nested": "value"}}),
                &ctx,
            )
            .await;

        let list_result = tool.execute(json!({"operation": "list"}), &ctx).await;

        assert!(!list_result.is_error);
        assert!(list_result.content.contains("str_key"));
        assert!(list_result.content.contains("num_key"));
        assert!(list_result.content.contains("obj_key"));
    }

    #[tokio::test]
    async fn test_config_persists_in_workspace_between_tool_instances() {
        let temp_dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(
            "test",
            temp_dir.path(),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let first = ConfigTool::new();
        let set_result = first
            .execute(
                json!({"operation": "set", "key": "agent.review.depth", "value": "deep"}),
                &ctx,
            )
            .await;
        assert!(!set_result.is_error);

        let second = ConfigTool::new();
        let get_result = second
            .execute(
                json!({"operation": "get", "key": "agent.review.depth"}),
                &ctx,
            )
            .await;

        assert!(!get_result.is_error);
        assert!(get_result.content.contains("deep"));
    }
}
