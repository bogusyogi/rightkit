//! `synthetic_output` — emit a structured synthetic result for testing/evaluation.
//!
//! Used for testing harness behavior, mocking tool outputs, and evaluation.
//! Echoes the provided JSON payload as a tool result.

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
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
