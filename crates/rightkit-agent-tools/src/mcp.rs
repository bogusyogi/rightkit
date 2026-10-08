//! MCP server adapter for the typed agent toolbox.
//!
//! The toolbox remains the single admission point: MCP only translates
//! descriptors, arguments, results & transport errors.

use crate::{Caller, Effect, ToolError, ToolSpec, Toolbox};
use rightkit_mcp::{
    McpError, Result, ServerHost, ToolAnnotations, ToolDescriptor, ToolProvider, ToolResult,
};
use serde_json::Value;
use std::sync::Arc;

/// Exposes one toolbox to an MCP host under one toolbox caller identity.
pub struct ToolboxProvider {
    toolbox: Arc<Toolbox>,
    caller: Caller,
}

impl ToolboxProvider {
    pub fn new(toolbox: Arc<Toolbox>, caller: impl Into<String>) -> Self {
        Self {
            toolbox,
            caller: Caller::new(caller),
        }
    }

    pub fn toolbox(&self) -> &Arc<Toolbox> {
        &self.toolbox
    }

    fn descriptor(spec: ToolSpec) -> ToolDescriptor {
        let effect = spec.effect;
        ToolDescriptor {
            name: spec.name,
            description: spec.description,
            input_schema: spec.input_schema,
            annotations: ToolAnnotations {
                title: None,
                read_only: Some(matches!(effect, Effect::Observe | Effect::Capture)),
                destructive: Some(matches!(effect, Effect::Act | Effect::Launch)),
                idempotent: Some(matches!(effect, Effect::Observe)),
                open_world: Some(matches!(effect, Effect::Network)),
            },
        }
    }

    fn tool_error(error: ToolError) -> ToolResult {
        let mut result = ToolResult::error(error.message.clone());
        result.structured = Some(error.to_value());
        result
    }
}

impl ToolProvider for ToolboxProvider {
    fn list(&self) -> Vec<ToolDescriptor> {
        self.toolbox
            .list_for(&self.caller.app)
            .into_iter()
            .map(Self::descriptor)
            .collect()
    }

    fn call(&self, name: &str, args: Value) -> Result<ToolResult> {
        if !self.list().iter().any(|tool| tool.name == name) {
            return Err(McpError::ToolNotFound(name.into()));
        }
        match self.toolbox.call(&self.caller, name, args) {
            Ok(value) => Ok(ToolResult::json(value)),
            Err(error) => Ok(Self::tool_error(error)),
        }
    }
}

/// Serve toolbox over newline-delimited MCP JSON-RPC on stdin/stdout.
pub fn serve_stdio(toolbox: Arc<Toolbox>, caller: impl Into<String>) -> Result<()> {
    let provider = Arc::new(ToolboxProvider::new(toolbox, caller));
    ServerHost::new("right-ctl", env!("CARGO_PKG_VERSION"), provider).serve_stdio()
}
