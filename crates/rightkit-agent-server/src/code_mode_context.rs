//! Authenticated context bridge used by bounded code-mode programs.

use rightkit_agent_tools::coding_tools::{
    code_mode::{ContextToolDispatcher, NestedToolDispatcher, NestedToolFuture},
    context::ToolContext,
    ToolResult,
};
use serde_json::json;

/// Pull-backed compression via engine's internal-only nested child, preserving
/// EFF-001 admission and receipts without advertising it as a model tool.
/// Caller identity comes from the session's attested context manager.
pub(crate) struct CodeModeContextAdapter;

impl ContextToolDispatcher for CodeModeContextAdapter {
    fn compress<'a>(
        &'a self,
        output: String,
        context: &'a ToolContext,
        dispatcher: &'a dyn NestedToolDispatcher,
        parent_task_id: &'a str,
    ) -> NestedToolFuture<'a> {
        Box::pin(async move {
            let Some(manager) = context.mcp.as_ref() else {
                return ToolResult::error("Pull result compression unavailable: no context worker");
            };
            let Some(caller) = manager.canonical_membrane_caller() else {
                return ToolResult::error(
                    "Pull result compression unavailable: caller identity is unbound",
                );
            };
            let mut caller_identity = json!({
                "root": caller.root,
                "repositoryId": caller.repository_id,
                "scopeId": caller.scope_id,
            });
            if let Some(descriptor) = caller.scope_descriptor.as_ref() {
                caller_identity["scopeDescriptor"] = descriptor.clone();
            }
            let request = json!({
                "operation": "program_result",
                "caller": caller_identity,
                "repository": caller.repository_id,
                "taskId": parent_task_id,
                "sessionId": caller.scope_id,
                "programResult": {
                    "schemaVersion": 1,
                    "action": "compress",
                    "sourceText": output,
                    "budgetTokens": 2000,
                    "protectedSpans": [],
                },
            });
            dispatcher
                .invoke("pull_program_result", request, context)
                .await
        })
    }
}
