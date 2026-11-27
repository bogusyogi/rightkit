// Dynamic MCP tools — let the agent discover and call tools exposed by
// configured MCP servers (Claude-Code dynamic MCP parity).
//
// - `list_mcp_tools` → `manager.discover_all()` rendered as a list.
// - `mcp_call` → `manager.invoke(qualified_name, args, Some(session_id))`.
// - `McpDirectTool` (direct.rs) → one server tool in the model's own tool list, called through
//   `mcp_call`'s path.
// - `list_mcp_sources` → `manager.connection_states()` classified by `rightkit_mcp::session_status`;
//   reads held state only, never starts a server or lists or calls a tool.

use crate::context::ToolContext;
use crate::tool_trait::{Tool, ToolResult};
use rightkit_mcp::agent_mcp::{parse_qualified_name, McpManager};
use serde_json::{json, Value};

pub struct ListMcpToolsTool;

#[async_trait::async_trait]
impl Tool for ListMcpToolsTool {
    fn name(&self) -> &str {
        "list_mcp_tools"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn process_resource_kind(
        &self,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        Some(rightkit_agent_engine_runtime_resources::ResourceKind::Mcp)
    }

    fn description(&self) -> &str {
        if crate::tool_options::mcp_listing_newer() {
            "List the tools available from configured MCP servers, with their qualified names and \
             descriptions. Pass `server` and/or `query` to narrow a long list; a narrowed list (or a \
             short one) includes each tool's input schema. Call mcp_call to invoke one."
        } else {
            "List the tools available from configured MCP servers, with their qualified names and \
             descriptions. Call mcp_call to invoke one."
        }
    }

    fn input_schema(&self) -> Value {
        if !crate::tool_options::mcp_listing_newer() {
            return json!({ "type": "object", "properties": {} });
        }
        json!({
            "type": "object",
            "properties": {
                "server": {
                    "type": "string",
                    "description": "Only list tools from this MCP server."
                },
                "query": {
                    "type": "string",
                    "description": "Only list tools whose name or description contains this text (case-insensitive)."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        // `server` and `query` exist only with `set_mcp_listing_newer`.
        let newer = crate::tool_options::mcp_listing_newer();
        let server_filter = args
            .get("server")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| newer && !s.is_empty())
            .map(str::to_string);
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_lowercase())
            .filter(|s| newer && !s.is_empty());
        let manager = match &ctx.mcp {
            Some(m) => m,
            None => return ToolResult::ok("No MCP servers are configured."),
        };

        if manager.is_empty() {
            return ToolResult::ok("No MCP servers are configured.");
        }

        // A server the model named is waited for past the short discovery bound, so a cold
        // start does not answer "still starting" and cost a turn.
        if let Some(wanted) = server_filter.as_deref() {
            manager.wait_for_server(wanted).await;
        }
        let tools: Vec<_> = manager
            .discover_all()
            .await
            .into_iter()
            .filter(|tool| {
                parse_qualified_name(&tool.name)
                    .map(|(server, _)| manager.is_channel_allowed(&server, &ctx.mcp_channel))
                    .unwrap_or(true)
            })
            .filter(|tool| {
                parse_qualified_name(&tool.name)
                    .map(|(server, _)| manager.is_tool_allowed_by_mutating_policy(&server, tool))
                    .unwrap_or(true)
            })
            .filter(|tool| {
                server_filter.as_deref().is_none_or(|wanted| {
                    parse_qualified_name(&tool.name)
                        .map(|(server, _)| server.eq_ignore_ascii_case(wanted))
                        .unwrap_or(false)
                })
            })
            .filter(|tool| {
                query.as_deref().is_none_or(|q| {
                    tool.name.to_lowercase().contains(q)
                        || tool.description.to_lowercase().contains(q)
                })
            })
            .collect();
        // A tool whose definition changed since it was pinned is withheld (with
        // `set_mcp_definition_pins`; with the option off nothing is withheld).
        let (tools, withheld): (Vec<_>, Vec<_>) = tools
            .into_iter()
            .partition(|tool| mcp_definition_pin(tool).is_none());

        let starting = manager.starting_servers();
        let starting_note = if starting.is_empty() {
            String::new()
        } else {
            format!(
                "\nMCP server(s) still starting (tools will appear when ready; call list_mcp_tools again later): {}",
                starting.join(", ")
            )
        };

        let withheld_note = if withheld.is_empty() {
            String::new()
        } else {
            format!(
                "\n{} MCP tool(s) withheld: {}. {}",
                withheld.len(),
                withheld
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                mcp_pin_remedy()
            )
        };
        let starting_note = format!("{starting_note}{withheld_note}");

        if tools.is_empty() {
            return ToolResult::ok(format!(
                "No MCP tools are available (servers returned no tools).{starting_note}"
            ));
        }

        let mut lines = Vec::with_capacity(tools.len() + 1);
        lines.push(format!("{} MCP tool(s) available:\n", tools.len()));

        // A long unfiltered list stays on parameter-name hints to fit the output cap; a short or
        // narrowed one carries the schema so the model can build a correct call.
        let with_schema =
            newer && (server_filter.is_some() || query.is_some() || tools.len() <= 20);
        if newer && !with_schema {
            lines.push(
                "(parameter names only; pass `server` or `query` to see full input schemas)\n"
                    .to_string(),
            );
        }
        for tool in &tools {
            let schema_hint = if with_schema {
                compact_schema(&tool.input_schema)
            } else {
                build_schema_hint(&tool.input_schema)
            };
            // A server's description is its own text: with `set_mcp_listing_newer` the listing
            // carries it bounded, as the tools block does, so one chatty server cannot fill the
            // model's context.
            let description = if newer {
                bounded_listing_description(&tool.description)
            } else {
                tool.description.clone()
            };
            if schema_hint.is_empty() {
                lines.push(format!("  {} — {}", tool.name, description));
            } else {
                lines.push(format!(
                    "  {} — {}  [{}]",
                    tool.name, description, schema_hint
                ));
            }
        }

        if !withheld_note.is_empty() {
            lines.push(withheld_note.trim_start().to_string());
        }
        ToolResult::ok(wrap_mcp_output(&lines.join("\n")))
    }
}

/// Most bytes of one MCP tool description a listing carries (the tools block's own bound).
const LISTING_DESCRIPTION_LIMIT: usize = 2_048;

/// `description` cut to [`LISTING_DESCRIPTION_LIMIT`] bytes on a character boundary, with a
/// marker that says how long it was.
fn bounded_listing_description(description: &str) -> String {
    if description.len() <= LISTING_DESCRIPTION_LIMIT {
        return description.to_string();
    }
    let mut end = LISTING_DESCRIPTION_LIMIT;
    while !description.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{} [description truncated: {} bytes total]",
        &description[..end],
        description.len()
    )
}

/// `schema` as the tool listing and the failed-call hint show it: through
/// `registry::enum_for_strict_validators` with `set_tool_schema_strict_enums`, as the tools block
/// of a provider request is.
fn listing_schema(schema: &Value) -> Value {
    if crate::tool_options::tool_schema_strict_enums() {
        crate::registry::enum_for_strict_validators(schema.clone())
    } else {
        schema.clone()
    }
}

/// Compact JSON of an input schema with no properties omitted; empty when it takes no arguments.
fn compact_schema(schema: &Value) -> String {
    let has_properties = schema
        .get("properties")
        .and_then(|p| p.as_object())
        .is_some_and(|p| !p.is_empty());
    if !has_properties {
        return String::new();
    }
    // With `set_tool_schema_strict_enums` the model sees `enum`, not a `oneOf` of string consts,
    // as with the built-in tools.
    let text = listing_schema(schema).to_string();
    if text.chars().count() > 2000 {
        let cut: String = text.chars().take(2000).collect();
        format!("schema: {cut}…")
    } else {
        format!("schema: {text}")
    }
}

/// Returns the pinned digest when `def` differs from the definition pinned at first use; a
/// first sight pins it and returns `None`. Always `None` unless `set_mcp_definition_pins`.
fn mcp_definition_pin(def: &rightkit_mcp::agent_mcp::McpToolDef) -> Option<String> {
    if !crate::tool_options::mcp_definition_pins() {
        return None;
    }
    let (server, _) = parse_qualified_name(&def.name)?;
    let store =
        rightkit_agent_config::definition_pins::pin_store_path("mcp-tool-definitions.json")?;
    let annotations = serde_json::to_string(&def.annotations).unwrap_or_default();
    let schema = serde_json::to_string(&def.input_schema).unwrap_or_default();
    let digest = rightkit_agent_config::definition_pins::digest_of(&[
        def.name.as_str(),
        def.description.as_str(),
        annotations.as_str(),
        schema.as_str(),
    ]);
    match rightkit_agent_config::definition_pins::check_or_pin(
        &store,
        &format!("mcp:{}", server.to_ascii_lowercase()),
        &def.name.to_ascii_lowercase(),
        &digest,
    ) {
        rightkit_agent_config::definition_pins::PinCheck::Changed { pinned } => Some(pinned),
        _ => None,
    }
}

/// True when `def` changed since it was first pinned (the session catalog skips such a tool).
/// Always `false` unless `tool_options::set_mcp_definition_pins`.
pub fn mcp_definition_changed(def: &rightkit_mcp::agent_mcp::McpToolDef) -> bool {
    mcp_definition_pin(def).is_some()
}

/// What to do about a tool whose definition changed since it was first used.
fn mcp_pin_remedy() -> String {
    format!(
        "Its definition (name, description, annotations or input schema) changed since it was first used. Review it, then remove the server (`{} mcp remove` followed by the server name) and add it again to approve the new definition.",
        rightkit_agent_config::host_slug()
    )
}

pub struct ListMcpSourcesTool;

#[async_trait::async_trait]
impl Tool for ListMcpSourcesTool {
    fn name(&self) -> &str {
        "list_mcp_sources"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Report the state of each MCP tool source in this session: ready, no-tools, failed, or \
         pending (still starting or not connected yet), with the tool count. It reads state the \
         session already holds; it does not start a server, call a tool, or show a URL, header \
         or token. Use it to tell a server that is not connected from one that has no tools. \
         Call list_mcp_tools for the tools themselves."
    }

    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn execute(&self, _args: Value, ctx: &ToolContext) -> ToolResult {
        let manager = match &ctx.mcp {
            Some(m) => m,
            None => return ToolResult::ok("No MCP servers are configured."),
        };
        if manager.is_empty() {
            return ToolResult::ok("No MCP servers are configured.");
        }
        let held: Vec<rightkit_mcp::EngineSource> = manager
            .connection_states()
            .into_iter()
            .filter(|source| manager.is_channel_allowed(&source.name, &ctx.mcp_channel))
            .collect();
        let statuses = rightkit_mcp::engine_source_status(&held);
        if statuses.is_empty() {
            return ToolResult::ok("No MCP servers are available to this session.");
        }
        let sources: Vec<Value> = statuses
            .iter()
            .map(|status| {
                json!({
                    "name": status.name,
                    "state": status.state,
                    "tools": status.tools,
                })
            })
            .collect();
        ToolResult::ok(json!({ "sources": sources }).to_string())
    }
}

/// Produce a compact hint string from an input schema, e.g. `required: query; optional: limit`.
fn build_schema_hint(schema: &Value) -> String {
    let properties = match schema.get("properties").and_then(|p| p.as_object()) {
        Some(p) => p,
        None => return String::new(),
    };

    if properties.is_empty() {
        return String::new();
    }

    let required: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let optional: Vec<&str> = properties
        .keys()
        .filter(|k| !required.contains(&k.as_str()))
        .map(|k| k.as_str())
        .collect();

    let mut parts = Vec::new();
    if !required.is_empty() {
        parts.push(format!("required: {}", required.join(", ")));
    }
    if !optional.is_empty() {
        parts.push(format!("optional: {}", optional.join(", ")));
    }
    parts.join("; ")
}

pub struct McpCallTool;

#[async_trait::async_trait]
impl Tool for McpCallTool {
    fn name(&self) -> &str {
        "mcp_call"
    }

    fn process_resource_kind(
        &self,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        Some(rightkit_agent_engine_runtime_resources::ResourceKind::Mcp)
    }

    fn description(&self) -> &str {
        "Invoke a tool on a configured MCP server by its qualified name (mcp__server__tool). Use \
         list_mcp_tools first to discover names and schemas."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["name"],
            "properties": {
                "name": { "type": "string", "description": "Qualified tool name, e.g. 'mcp__github__search_issues'." },
                "arguments": { "type": "object", "description": "Arguments object for the tool." }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let manager = match &ctx.mcp {
            Some(m) => m,
            None => return ToolResult::error("No MCP servers are configured."),
        };

        let name = match args.get("name").and_then(|v| v.as_str()) {
            Some(n) => n.to_string(),
            None => return ToolResult::error("Missing required argument: 'name'."),
        };

        let mut arguments = args.get("arguments").cloned().unwrap_or_else(|| json!({}));

        if manager.is_membrane_public_tool_name(&name) {
            if let Err(error) = bind_trusted_membrane_caller(manager, &name, &mut arguments, ctx) {
                return ToolResult::error(error);
            }
        }

        if let Some(denial) = deny_if_mcp_channel_disallowed(manager, &name, &ctx.mcp_channel) {
            return denial;
        }
        if let Some(denial) = deny_if_mcp_mutating_disallowed(manager, &name).await {
            return denial;
        }
        if crate::tool_options::mcp_definition_pins() {
            if let Some(listed) = manager
                .discover_all()
                .await
                .into_iter()
                .find(|tool| tool.name.eq_ignore_ascii_case(&name))
            {
                if mcp_definition_pin(&listed).is_some() {
                    return ToolResult::error(format!(
                        "MCP tool {name} was not called. {}",
                        mcp_pin_remedy()
                    ));
                }
            }
        }

        // Only the bound canonical Membrane Pull may skip the untrusted wrapper; any server
        // can name a tool `pull`.
        let pull_program_result = manager.is_canonical_membrane_public_tool(&name)
            && name.ends_with("__pull")
            && arguments.get("operation").and_then(Value::as_str) == Some("program_result");
        match manager
            .invoke(&name, arguments, Some(ctx.session_id.as_str()))
            .await
        {
            Ok(result) => {
                if result.is_error {
                    // A model that called the tool without listing it first learns the
                    // shape from the failure: with `set_mcp_listing_newer` the cached
                    // listing's input schema follows the server's own error text (inside
                    // the untrusted wrapper, since the schema is the server's too).
                    let hint = if crate::tool_options::mcp_listing_newer() {
                        expected_schema_hint(manager, &name).await
                    } else {
                        String::new()
                    };
                    ToolResult::error(wrap_mcp_output(&format!("{}{hint}", result.content)))
                } else if pull_program_result {
                    // Private run_program bridge consumes the canonical typed
                    // result envelope; do not convert it into generic untrusted
                    // prose before Pull handles its recovery metadata.
                    ToolResult::ok(result.content)
                } else {
                    ToolResult::ok(wrap_mcp_output(&result.content))
                }
            }
            Err(e) => {
                let text = format!("MCP call failed: {e} [{}]", e.kind());
                ToolResult::error(wrap_mcp_output(&text))
            }
        }
    }
}

/// Largest schema text appended to a failed call, in characters.
const EXPECTED_SCHEMA_LIMIT: usize = 2000;

/// `"\nExpected input schema:\n<compact JSON>"` for `qualified_name` from the manager's
/// discovered (cached) listing, cut at [`EXPECTED_SCHEMA_LIMIT`] characters; empty when the
/// tool is not listed or takes no arguments.
async fn expected_schema_hint(manager: &McpManager, qualified_name: &str) -> String {
    let tools = manager.discover_all().await;
    let Some(tool) = tools
        .iter()
        .find(|tool| tool.name.eq_ignore_ascii_case(qualified_name))
    else {
        return String::new();
    };
    format_expected_schema(&tool.input_schema)
}

fn format_expected_schema(schema: &Value) -> String {
    let has_properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .is_some_and(|properties| !properties.is_empty());
    if !has_properties {
        return String::new();
    }
    let text = listing_schema(schema).to_string();
    if text.chars().count() > EXPECTED_SCHEMA_LIMIT {
        let cut: String = text.chars().take(EXPECTED_SCHEMA_LIMIT).collect();
        format!("\nExpected input schema:\n{cut}…")
    } else {
        format!("\nExpected input schema:\n{text}")
    }
}

fn bind_trusted_membrane_caller(
    manager: &McpManager,
    qualified_name: &str,
    arguments: &mut Value,
    ctx: &ToolContext,
) -> Result<(), String> {
    if !manager.is_canonical_membrane_public_tool(qualified_name) {
        return Err("canonical Membrane transport is unavailable or replaced".into());
    }
    let caller = manager
        .canonical_membrane_caller()
        .ok_or_else(|| "canonical Membrane repository identity is unavailable".to_string())?;
    let selected_root = ctx
        .workspace_root
        .canonicalize()
        .map_err(|_| "session repository root is unavailable".to_string())?;
    let caller_root = std::path::Path::new(&caller.root)
        .canonicalize()
        .map_err(|_| "canonical Membrane repository root is unavailable".to_string())?;
    if !crate::repository_root::is_same_repository(&selected_root, &caller_root) {
        return Err("canonical Membrane identity does not match session repository".into());
    }
    let object = arguments
        .as_object_mut()
        .ok_or_else(|| "canonical Membrane call requires object arguments".to_string())?;
    if object
        .get("repository")
        .is_some_and(|repository| repository.as_str() != Some(caller.repository_id.as_str()))
    {
        return Err("repository must match trusted session repository identity".into());
    }
    if let Some(model_caller) = object.get("caller") {
        let model_caller = model_caller
            .as_object()
            .ok_or_else(|| "caller must be an object".to_string())?;
        for (key, expected) in [
            ("repositoryId", caller.repository_id.as_str()),
            ("scopeId", caller.scope_id.as_str()),
        ] {
            if model_caller
                .get(key)
                .is_some_and(|value| value.as_str() != Some(expected))
            {
                return Err(format!("caller.{key} must match trusted session identity"));
            }
        }
        if let Some(model_root) = model_caller.get("root") {
            let Some(model_root) = model_root.as_str() else {
                return Err("caller.root must match trusted session repository".into());
            };
            let model_root = std::path::Path::new(model_root)
                .canonicalize()
                .map_err(|_| "caller.root must match trusted session repository".to_string())?;
            let matches = if cfg!(windows) {
                model_root
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&caller_root.to_string_lossy())
            } else {
                model_root == caller_root
            };
            if !matches {
                return Err("caller.root must match trusted session repository".into());
            }
        }
        if let Some(model_descriptor) = model_caller.get("scopeDescriptor") {
            if Some(model_descriptor) != caller.scope_descriptor.as_ref() {
                return Err("caller.scopeDescriptor must match trusted session identity".into());
            }
        }
    }
    let mut trusted_caller = serde_json::Map::new();
    trusted_caller.insert("root".into(), Value::String(caller.root.clone()));
    trusted_caller.insert(
        "repositoryId".into(),
        Value::String(caller.repository_id.clone()),
    );
    trusted_caller.insert("scopeId".into(), Value::String(caller.scope_id.clone()));
    if let Some(descriptor) = caller.scope_descriptor.as_ref() {
        trusted_caller.insert("scopeDescriptor".into(), descriptor.clone());
    }
    object.insert(
        "repository".into(),
        Value::String(caller.repository_id.clone()),
    );
    object.insert("caller".into(), Value::Object(trusted_caller));
    if qualified_name.ends_with("__pull") {
        let program_result =
            object.get("operation").and_then(Value::as_str) == Some("program_result");
        let expected_session = if program_result {
            caller.scope_id.as_str()
        } else {
            ctx.session_id.as_str()
        };
        if object
            .get("sessionId")
            .and_then(Value::as_str)
            .is_some_and(|session| session != expected_session)
        {
            return Err(if program_result {
                "sessionId must match trusted caller.scopeId".into()
            } else {
                "sessionId must match the active session".into()
            });
        }
        object.insert(
            "sessionId".into(),
            Value::String(expected_session.to_string()),
        );
    }
    Ok(())
}

pub struct ListMcpResourcesTool;

#[async_trait::async_trait]
impl Tool for ListMcpResourcesTool {
    fn name(&self) -> &str {
        "list_mcp_resources"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn process_resource_kind(
        &self,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        Some(rightkit_agent_engine_runtime_resources::ResourceKind::Mcp)
    }

    fn description(&self) -> &str {
        "List the resources available from a configured MCP server. Use read_mcp_resource to \
         fetch the content of a specific resource by URI."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["server"],
            "properties": {
                "server": {
                    "type": "string",
                    "description": "The MCP server name/id to list resources from."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let manager = match &ctx.mcp {
            Some(m) => m,
            None => return ToolResult::error("No MCP servers are configured."),
        };

        let server = match args.get("server").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::error("Missing required argument: 'server'."),
        };

        if let Some(denial) =
            deny_server_if_mcp_channel_disallowed(manager, &server, &ctx.mcp_channel)
        {
            return denial;
        }

        match manager.list_resources(&server).await {
            Ok(resources) => {
                if resources.is_empty() {
                    return ToolResult::ok(format!("Server '{server}' returned no resources."));
                }
                match serde_json::to_string(&resources) {
                    Ok(json) => ToolResult::ok(wrap_mcp_output(&json)),
                    Err(e) => ToolResult::error(format!("Failed to serialize resources: {e}")),
                }
            }
            Err(e) => ToolResult::error(format!("Failed to list resources from '{server}': {e}")),
        }
    }
}

pub struct ReadMcpResourceTool;

#[async_trait::async_trait]
impl Tool for ReadMcpResourceTool {
    fn name(&self) -> &str {
        "read_mcp_resource"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn process_resource_kind(
        &self,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        Some(rightkit_agent_engine_runtime_resources::ResourceKind::Mcp)
    }

    fn description(&self) -> &str {
        "Read a resource from a configured MCP server by URI. Use list_mcp_resources to discover \
         available URIs first."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["server", "uri"],
            "properties": {
                "server": {
                    "type": "string",
                    "description": "The MCP server name/id that owns the resource."
                },
                "uri": {
                    "type": "string",
                    "description": "The resource URI as returned by list_mcp_resources."
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let manager = match &ctx.mcp {
            Some(m) => m,
            None => return ToolResult::error("No MCP servers are configured."),
        };

        let server = match args.get("server").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::error("Missing required argument: 'server'."),
        };

        let uri = match args.get("uri").and_then(|v| v.as_str()) {
            Some(u) => u.to_string(),
            None => return ToolResult::error("Missing required argument: 'uri'."),
        };

        if let Some(denial) =
            deny_server_if_mcp_channel_disallowed(manager, &server, &ctx.mcp_channel)
        {
            return denial;
        }

        match manager.read_resource(&server, &uri).await {
            Ok(content) => {
                if let Some(text) = &content.text {
                    ToolResult::ok(wrap_mcp_output(text))
                } else if let Some(blob) = &content.blob {
                    let body = format!("[base64 binary, mimeType={}]\n{}", content.mime_type, blob);
                    ToolResult::ok(wrap_mcp_output(&body))
                } else {
                    ToolResult::ok(String::new())
                }
            }
            Err(e) => {
                let text = format!("Failed to read resource '{uri}' from '{server}': {e}");
                ToolResult::error(wrap_mcp_output(&text))
            }
        }
    }
}

/// Bound and wrap every byte an MCP server sends us.
///
/// Two separate concerns, both required. `bound_foreign_output` masks
/// secret-shaped spans and caps size with an explicit marker; the wrapper marks
/// the content as untrusted so a prompt-injection attempt inside it is framed
/// rather than obeyed.
///
/// Masking previously did not happen here at all: native tools each mask at
/// their own call site, and MCP output -- which we did not author and cannot
/// vouch for -- was the one path that skipped it. A server relaying a config
/// file or a log could hand the model a live credential.
fn wrap_mcp_output(content: &str) -> String {
    let bounded = crate::output_compaction::bound_foreign_output(content);
    crate::browser_fetch::wrap_untrusted_web_text("mcp_tools", &bounded)
}

fn deny_if_mcp_channel_disallowed(
    manager: &McpManager,
    qualified_name: &str,
    channel: &str,
) -> Option<ToolResult> {
    let (server, _) = parse_qualified_name(qualified_name)?;
    deny_server_if_mcp_channel_disallowed(manager, &server, channel)
}

fn deny_server_if_mcp_channel_disallowed(
    manager: &McpManager,
    server: &str,
    channel: &str,
) -> Option<ToolResult> {
    if manager.is_channel_allowed(server, channel) {
        return None;
    }
    Some(ToolResult::error(format!(
        "MCP server '{server}' is not allowed for channel '{channel}'. Update mcpChannelPermissions.allowedChannels to permit this channel."
    )))
}

async fn deny_if_mcp_mutating_disallowed(
    manager: &McpManager,
    qualified_name: &str,
) -> Option<ToolResult> {
    let (server, tool_name) = parse_qualified_name(qualified_name)?;
    match manager
        .is_qualified_tool_allowed_by_mutating_policy(qualified_name)
        .await
    {
        Ok(true) => None,
        Ok(false) => Some(ToolResult::error(format!(
            "MCP server '{server}' disallows mutating tools, and '{tool_name}' is not marked with annotations.readOnlyHint=true."
        ))),
        // An unknown tool/server is not a policy question: the call itself
        // fails closed with the precise "tool not found" error (and the
        // manager re-applies the mutating/trust policy before dispatching any
        // call), so do not mask it behind a policy message.
        Err(rightkit_mcp::agent_mcp::McpError::ToolNotFound(_)) => None,
        Err(error) => Some(ToolResult::error(format!(
            "MCP server '{server}' disallows mutating tools, but {} could not verify annotations.readOnlyHint for '{tool_name}': {error}",
            rightkit_agent_config::app_name()
        ))),
    }
}
