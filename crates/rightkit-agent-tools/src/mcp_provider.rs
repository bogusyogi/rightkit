//! [`ToolboxProvider`]: serves a [`Toolbox`] over MCP as a `rightkit_mcp::ToolProvider`.
//!
//! Ownership split (M22, `docs/plans/2026-10-20-rightkit-m1-m22-package-spec-astra-r1.md`):
//! `rightkit-mcp` owns the transport (framing, protocol versions, batching, per-call deadline,
//! cancellation, per-tool admission hooks); the [`Toolbox`] owns policy and schema (hard exposure
//! rules, per-caller allowlist, argument validation, the caller's admission hook, journal). This
//! adapter owns neither: it renders `Toolbox::list_for` as descriptors and sends every call
//! through `Toolbox::call_cancellable`, so MCP clients cannot reach a tool the toolbox would
//! refuse, and no policy is copied into the transport layer.

use crate::spec::{Caller, CancelToken, Effect, ToolError, ERR_UNKNOWN_TOOL};
use crate::toolbox::Toolbox;
use rightkit_mcp::{
    CallHandle, Content, McpError, ToolDescriptor, ToolProvider, ToolResult as McpToolResult,
};
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// How often a running call checks whether the MCP request was cancelled.
const CANCEL_POLL: Duration = Duration::from_millis(20);

/// One MCP-facing view of a [`Toolbox`] for one caller identity.
///
/// The caller's allowlist decides what `tools/list` shows and what `tools/call` may run; an app
/// that serves several MCP peers with different rights builds one provider per caller.
pub struct ToolboxProvider {
    toolbox: Arc<Toolbox>,
    caller: Caller,
}

impl ToolboxProvider {
    pub fn new(toolbox: Arc<Toolbox>, caller: Caller) -> Self {
        Self { toolbox, caller }
    }

    fn render_error(error: &ToolError) -> McpToolResult {
        let value = error.to_value();
        McpToolResult {
            content: vec![Content::Text {
                text: value.to_string(),
            }],
            structured: Some(value),
            is_error: true,
        }
    }

    fn run(
        &self,
        name: &str,
        args: Value,
        cancel: &CancelToken,
    ) -> Result<McpToolResult, McpError> {
        match self
            .toolbox
            .call_cancellable(&self.caller, name, args, cancel)
        {
            Ok(value) => Ok(McpToolResult::json(value)),
            Err(error) if error.code == ERR_UNKNOWN_TOOL => {
                Err(McpError::ToolNotFound(name.to_string()))
            }
            Err(error) => Ok(Self::render_error(&error)),
        }
    }
}

impl ToolProvider for ToolboxProvider {
    fn list(&self) -> Vec<ToolDescriptor> {
        self.toolbox
            .list_for(&self.caller.app)
            .into_iter()
            .map(|spec| {
                ToolDescriptor::new(&spec.name, &spec.description, spec.input_schema)
                    .read_only(spec.effect == Effect::Observe)
            })
            .collect()
    }

    fn call(&self, name: &str, args: Value) -> Result<McpToolResult, McpError> {
        self.run(name, args, &CancelToken::new())
    }

    fn call_with_handle(
        &self,
        name: &str,
        args: Value,
        call: &CallHandle,
    ) -> Result<McpToolResult, McpError> {
        let cancel = CancelToken::new();
        if call.is_cancelled() {
            cancel.cancel();
        }
        let finished = AtomicBool::new(false);
        // A short-lived watcher forwards the MCP request's cancellation to the tool's token and
        // ends as soon as the call returns.
        std::thread::scope(|scope| {
            scope.spawn(|| {
                while !finished.load(Ordering::SeqCst) {
                    if call.is_cancelled() {
                        cancel.cancel();
                        return;
                    }
                    std::thread::sleep(CANCEL_POLL);
                }
            });
            let result = self.run(name, args, &cancel);
            finished.store(true, Ordering::SeqCst);
            result
        })
    }
}
