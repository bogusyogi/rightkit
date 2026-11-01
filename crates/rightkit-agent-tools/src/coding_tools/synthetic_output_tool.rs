//! `synthetic_output` — emit a structured synthetic result for testing/evaluation.
//!
//! Used for testing harness behavior, mocking tool outputs, and evaluation.
//! Echoes the provided JSON payload as a tool result.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use serde_json::{json, Value};

pub struct SyntheticOutputTool;

#[async_trait::async_trait]
impl Tool for SyntheticOutputTool {
    fn name(&self) -> &str {
        "synthetic_output"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Emit a structured synthetic result for testing/evaluation. Echoes a provided JSON payload as a tool result. Use for mocking, test harnesses, eval. Fail: output field missing."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["output"],
            "properties": {
                "output": {
                    "type": "string",
                    "description": "The output content to emit as a result (can be JSON, plain text, etc.)"
                },
                "is_error": {
                    "type": "boolean",
                    "description": "Whether to mark this as an error result (default: false)",
                    "default": false
                }
            }
        })
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let output = match args["output"].as_str() {
            Some(o) => o.to_string(),
            None => return ToolResult::error("Missing required argument: output"),
        };

        let is_error = args["is_error"].as_bool().unwrap_or(false);

        if is_error {
            ToolResult::error(output)
        } else {
            ToolResult::ok(output)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_agent_policy::PermissionMode;
    use rightkit_agent_policy::WorkspaceTrust;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_synthetic_output_success() {
        let tool = SyntheticOutputTool;
        let ctx = ToolContext::new(
            "test",
            PathBuf::from("/tmp"),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let result = tool.execute(json!({"output": "test result"}), &ctx).await;
        assert!(!result.is_error);
        assert_eq!(result.content, "test result");
    }

    #[tokio::test]
    async fn test_synthetic_output_error() {
        let tool = SyntheticOutputTool;
        let ctx = ToolContext::new(
            "test",
            PathBuf::from("/tmp"),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let result = tool
            .execute(json!({"output": "error message", "is_error": true}), &ctx)
            .await;
        assert!(result.is_error);
        assert_eq!(result.content, "error message");
    }

    #[tokio::test]
    async fn test_synthetic_output_json() {
        let tool = SyntheticOutputTool;
        let ctx = ToolContext::new(
            "test",
            PathBuf::from("/tmp"),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let result = tool
            .execute(json!({"output": r#"{"key": "value"}"#}), &ctx)
            .await;
        assert!(!result.is_error);
        assert!(result.content.contains("key"));
    }

    #[tokio::test]
    async fn test_synthetic_output_missing_output() {
        let tool = SyntheticOutputTool;
        let ctx = ToolContext::new(
            "test",
            PathBuf::from("/tmp"),
            PermissionMode::Plan,
            WorkspaceTrust::Trusted,
        );

        let result = tool.execute(json!({"is_error": false}), &ctx).await;
        assert!(result.is_error);
    }
}
