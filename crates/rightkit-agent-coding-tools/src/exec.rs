use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use serde_json::Value;

pub const EXEC_TOOL_DESCRIPTION: &str = "Execute a governed read-only composition over registered capabilities. \
Two forms: {capability, schema_digest, args?} invokes one read-only capability through governed dispatch; \
{source, timeout_ms?} runs a bounded JavaScript async function body whose `tools.read`/`tools.search` calls \
execute governed nested reads. Mutating capabilities and recursive exec are refused.";

pub struct ExecTool;

impl ExecTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ExecTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for ExecTool {
    fn name(&self) -> &str {
        "exec"
    }

    fn description(&self) -> &str {
        EXEC_TOOL_DESCRIPTION
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "source": {
                    "type": "string",
                    "maxLength": 65536,
                    "description": "JavaScript async function body. Receives `tools` with `read(args)` and `search(args)` returning {ok, content}. The return value is JSON-serialized as the tool result."
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 120000,
                    "description": "Bounded execution timeout in milliseconds (default and maximum 120000)."
                },
                "capability": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Capability id to invoke, exactly `tool:<name>`."
                },
                "schema_digest": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Expected schema digest for the capability; the call fails on a stale digest before any nested effect."
                },
                "args": {
                    "type": "object",
                    "default": {},
                    "description": "Arguments passed to the capability."
                }
            },
            "oneOf": [
                {
                    "required": ["source"],
                    "not": {
                        "anyOf": [
                            { "required": ["capability"] },
                            { "required": ["schema_digest"] },
                            { "required": ["args"] }
                        ]
                    }
                },
                {
                    "required": ["capability", "schema_digest"],
                    "not": {
                        "anyOf": [
                            { "required": ["source"] },
                            { "required": ["timeout_ms"] }
                        ]
                    }
                }
            ]
        })
    }

    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
        ToolResult::error(
            "exec requires the SessionEngine governed composition boundary; direct tool execution is not supported",
        )
    }

    fn is_read_only(&self) -> bool {
        true
    }
}
