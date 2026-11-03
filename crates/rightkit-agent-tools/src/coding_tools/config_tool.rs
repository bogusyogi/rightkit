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
