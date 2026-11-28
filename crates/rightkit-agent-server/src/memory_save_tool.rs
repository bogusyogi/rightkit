//! memory_save — the model-facing WRITE path into sealed memory backend.
//! The read side is the `membrane context pull` envelope, the only automatic source
//! (contract rule 42). Without this tool the model has no way to persist a fact on purpose.

use rightkit_agent_coding_tools::context::ToolContext;
use rightkit_agent_coding_tools::{Tool, ToolResult};
use rightkit_memory_client::backend::{path_to_scope, MemoryBackend, MemoryTier};
use serde_json::Value;
use std::sync::Arc;

pub(crate) const MAX_MEMORY_NAME_BYTES: usize = 128;
pub(crate) const MAX_MEMORY_CONTENT_BYTES: usize = 8192;

pub struct MemorySaveTool {
    memory: Arc<dyn MemoryBackend>,
    workspace_scope: String,
}

impl MemorySaveTool {
    pub fn new(memory: Arc<dyn MemoryBackend>, workspace_root: &str) -> Self {
        Self {
            memory,
            workspace_scope: path_to_scope(workspace_root),
        }
    }
}

#[async_trait::async_trait]
impl Tool for MemorySaveTool {
    fn name(&self) -> &str {
        "memory_save"
    }

    fn description(&self) -> &str {
        "Save a durable memory to the cross-session memory store. Use for facts worth \
         recalling in future sessions: decisions, conventions, gotchas, environment quirks. \
         Saved memories are retrieved automatically when relevant."
    }

    fn input_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_MEMORY_NAME_BYTES,
                    "description": "Short kebab-case identifier for the memory"
                },
                "content": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_MEMORY_CONTENT_BYTES,
                    "description": "The fact to remember, written to stand alone"
                },
                "scope": {
                    "type": "string",
                    "enum": ["workspace", "global"],
                    "description": "workspace (default): recalled in this workspace only; global: recalled everywhere"
                }
            },
            "required": ["name", "content"]
        })
    }

    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        // Mutates durable state; WriteFile is the closest mutation class so gating
        // never depends on name inference.
        Some(rightkit_agent_policy::CapabilityClass::WriteFile)
    }

    fn durable_write_kind(
        &self,
        _args: &serde_json::Value,
    ) -> Option<rightkit_agent_policy::DurableWriteKind> {
        Some(rightkit_agent_policy::DurableWriteKind::Memory)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        // Contract rules 16 and 45: Membrane stores no memory provenance yet and
        // the host keeps no provenance table, so a write whose origin is not the
        // user's own session (tool, web or MCP content influenced it) is refused
        // unless the operator approved this exact call. The engine asks for that
        // approval before the call reaches this sink; this is the backstop.
        if !rightkit_agent_policy::memory_write_is_authorized(
            &ctx.content_provenance,
            ctx.durable_write_approved,
        ) {
            return ToolResult::error(format!(
                "memory_save refuses this write: it is influenced by {} content from '{}', and Membrane stores no memory provenance yet; it needs the operator's approval of this call",
                ctx.content_provenance.origin.label(),
                ctx.content_provenance.source
            ));
        }
        let name = args
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if name.is_empty() || content.is_empty() {
            return ToolResult::error("memory_save requires non-empty 'name' and 'content'");
        }
        if name.len() > MAX_MEMORY_NAME_BYTES {
            return ToolResult::error(format!(
                "memory_save 'name' exceeds {MAX_MEMORY_NAME_BYTES} bytes"
            ));
        }
        if content.len() > MAX_MEMORY_CONTENT_BYTES {
            return ToolResult::error(format!(
                "memory_save 'content' exceeds {MAX_MEMORY_CONTENT_BYTES} bytes"
            ));
        }
        let scope = match args.get("scope").and_then(Value::as_str) {
            Some("global") => "global",
            None | Some("workspace") => self.workspace_scope.as_str(),
            Some(_) => {
                return ToolResult::error(
                    "memory_save 'scope' must be 'workspace' (default) or explicit 'global'",
                )
            }
        };
        let request_context = rightkit_memory_client::api_memright::current_memory_call_context();
        let cancellation = request_context.cancellation().child_token();
        let cancel_watcher = ctx.cancel_rx.clone().map(|mut cancel_rx| {
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                loop {
                    match cancel_rx.changed().await {
                        Ok(()) if *cancel_rx.borrow() => {
                            cancellation.cancel();
                            break;
                        }
                        Ok(()) => {}
                        Err(_) => break,
                    }
                }
            })
        });
        let context = request_context.with_cancellation(cancellation);
        let result = self
            .memory
            .try_put(&context, name, content, scope, MemoryTier::Semantic)
            .await;
        if let Some(task) = cancel_watcher {
            task.abort();
        }
        if ctx.cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
            return ToolResult::error("memory_save cancelled");
        }
        match result {
            Ok(id) => ToolResult::ok(format!("saved memory {id}")),
            Err(error) => ToolResult::error(format!("memory_save failed: {error}")),
        }
    }
}
