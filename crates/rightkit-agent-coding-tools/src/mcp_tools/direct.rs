// A tool of a server configured with `expose = "direct"`: it is in the model's tool list under
// its qualified name (`mcp__<server>__<tool>`), with the server's own input schema, instead of
// behind `tool_search` and `mcp_call`. It calls through `mcp_call`'s own path, so the channel
// and mutating-tool policy, the untrusted-output wrapper and the schema hint on a failed call
// are the ones every MCP call gets.

use rightkit_mcp::agent_mcp::McpToolDef;

/// A tool of an MCP server in the model's own tool list, called through [`McpCallTool`].
pub struct McpDirectTool {
    name: String,
    description: String,
    input_schema: Value,
    read_only: bool,
    deferred: bool,
}

impl McpDirectTool {
    /// The tool for `def`, its schema in the first request.
    pub fn new(def: &McpToolDef) -> Self {
        Self::build(def, false)
    }

    /// The tool for `def`, its schema deferred until `tool_search` scopes it.
    pub fn new_deferred(def: &McpToolDef) -> Self {
        Self::build(def, true)
    }

    fn build(def: &McpToolDef, deferred: bool) -> Self {
        let description = if def.description.trim().is_empty() {
            format!("Tool {} of an MCP server.", def.name)
        } else {
            def.description.clone()
        };
        // A server may send a schema with no `type`; the model's tool list needs an object.
        let input_schema = if def.input_schema.get("type").is_some() {
            def.input_schema.clone()
        } else {
            json!({"type": "object", "properties": {}})
        };
        Self {
            name: def.name.clone(),
            description,
            input_schema,
            read_only: def.is_read_only_hint(),
            deferred,
        }
    }
}

#[async_trait::async_trait]
impl Tool for McpDirectTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        self.input_schema.clone()
    }

    fn defer_schema_by_default(&self) -> bool {
        self.deferred
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn process_resource_kind(
        &self,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        Some(rightkit_agent_engine_runtime_resources::ResourceKind::Mcp)
    }

    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        Some(rightkit_agent_policy::CapabilityClass::Mcp)
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        McpCallTool
            .execute(json!({ "name": self.name, "arguments": args }), ctx)
            .await
    }
}
