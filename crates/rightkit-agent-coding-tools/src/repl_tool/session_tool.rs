// `repl` — evaluate snippets in a persistent language REPL session.
//
// Keep a session map keyed by id. Ops: start(lang), eval(id, code), status,
// reset(id), close(id).
// Use tokio process for subprocess management (python, node).

use super::helpers::available_languages;
use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use rightkit_sandbox::execution::ManagedChild;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};

pub(super) const MAX_ARTIFACT_SCAN_FILES: usize = 2_000;
pub(super) const MAX_REPL_ARTIFACTS: usize = 20;
/// One REPL protocol frame is a single JSON line; bound it so a runaway
/// helper response cannot grow the session's memory without limit.
pub(super) const MAX_REPL_FRAME_BYTES: usize = 4 * 1024 * 1024;

pub struct ReplTool {
    pub(super) sessions: Arc<tokio::sync::Mutex<HashMap<String, ReplSession>>>,
}

pub(super) struct ReplSession {
    pub(super) id: String,
    pub(super) language: String,
    pub(super) workspace_root: PathBuf,
    pub(super) process: Box<dyn ManagedChild>,
    pub(super) stdin: Pin<Box<dyn AsyncWrite + Send>>,
    pub(super) stdout: BufReader<Pin<Box<dyn AsyncRead + Send>>>,
    pub(super) next_request_id: u64,
}

impl ReplTool {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }
}

impl Default for ReplTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Tool for ReplTool {
    fn name(&self) -> &str {
        "repl"
    }

    fn is_read_only(&self) -> bool {
        false
    }

    /// REPL sessions spawn interpreter child processes, so admission must go
    /// through the existing child-process resource authority.
    fn process_resource_kind(
        &self,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        Some(rightkit_agent_engine_runtime_resources::ResourceKind::ChildProcess)
    }

    fn description(&self) -> &str {
        "Evaluate code snippets in persistent REPL sessions. Ops: start(language), eval(session_id, code), status([session_id]), reset(session_id), close(session_id). Languages: only runtimes resolvable on this host are advertised (python, node). Fail: unsupported language, session not found."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["operation"],
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["start", "eval", "status", "reset", "close"],
                    "description": "Operation to perform"
                },
                "language": {
                    "type": "string",
                    "enum": available_languages(),
                    "description": "Programming language for start operation (only runtimes resolvable on this host are listed)"
                },
                "session_id": {
                    "type": "string",
                    "description": "Session ID for eval, status, reset, and close operations"
                },
                "code": {
                    "type": "string",
                    "description": "Code to evaluate (eval operation)"
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
            "start" => {
                let language = match args["language"].as_str() {
                    Some(l) => l,
                    None => return ToolResult::error("Missing language for start"),
                };
                if let Some(message) = ctx.nonlocal_execution_backend_error("repl") {
                    return ToolResult::error(message);
                }
                if let Some(message) = ctx.unenforced_network_profile_error("repl") {
                    return ToolResult::error(message);
                }
                self.start_session(language, ctx).await
            }
            "eval" => {
                let session_id = match args["session_id"].as_str() {
                    Some(s) => s,
                    None => return ToolResult::error("Missing session_id for eval"),
                };
                let code = match args["code"].as_str() {
                    Some(c) => c,
                    None => return ToolResult::error("Missing code for eval"),
                };
                self.eval_code(session_id, code).await
            }
            "status" => {
                let session_id = args["session_id"].as_str();
                self.status(session_id).await
            }
            "reset" => {
                let session_id = match args["session_id"].as_str() {
                    Some(s) => s,
                    None => return ToolResult::error("Missing session_id for reset"),
                };
                self.reset_session(session_id, ctx).await
            }
            "close" => {
                let session_id = match args["session_id"].as_str() {
                    Some(s) => s,
                    None => return ToolResult::error("Missing session_id for close"),
                };
                self.close_session(session_id).await
            }
            other => ToolResult::error(format!("Unknown operation: {}", other)),
        }
    }
}
