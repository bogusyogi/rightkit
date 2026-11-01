// `lsp` — semantic code navigation.
//
// The preferred path is a real Language Server Protocol subprocess through the
// existing `host_mcp` LSP bridge. Definition and hover retain a local
// navigation fallback; references are semantic-only because lexical matches are
// not a truthful substitute for an LSP reference graph.

use crate::coding_tools::context::ToolContext;
use crate::coding_tools::tool_trait::{Tool, ToolResult};
use rightkit_mcp::agent_mcp::{LspPosition, LspServerConfig};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub struct LspTool;

async fn live_bridge(
    server: &LspServerConfig,
    ctx: &ToolContext,
) -> Result<super::workspace_manager::SharedLspBridge, String> {
    let mut launch_context = ctx.process_launch_context.clone();
    if launch_context.workspace_root.trim().is_empty() {
        launch_context.workspace_root = ctx.workspace_root.to_string_lossy().into_owned();
    }
    super::workspace_manager::initialized_bridge_with_launcher(
        server,
        ctx.process_launch_adapter.clone(),
        launch_context,
    )
    .await
}

/// The prefix `LspTool`'s `diagnostics` op emits when problems exist. Shared so
/// consumers (`post_edit_diagnostics`) parse exactly what is produced — a format
/// change here breaks both sides loudly instead of silently.
pub const DIAGNOSTICS_PREFIX: &str = "Diagnostics:\n";

/// Canonical one-line rendering of a single diagnostic. Shared producer for the
/// `diagnostics` op output and for `post_edit_diagnostics` test fixtures.
pub fn format_diagnostic_line(
    uri: &str,
    line_1based: impl std::fmt::Display,
    character: impl std::fmt::Display,
    severity: impl std::fmt::Display,
    message: &str,
) -> String {
    format!("{uri}:{line_1based}:{character} severity={severity} {message}")
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LspWorkspaceConfig {
    #[serde(default)]
    pub servers: Vec<LspWorkspaceServer>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LspWorkspaceServer {
    pub language: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    pub server_path: String,
    #[serde(default)]
    pub server_args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LspServerStatus {
    pub language: String,
    pub executable: String,
    pub extensions: Vec<String>,
    pub configured_path: Option<String>,
    pub discovered_path: Option<String>,
    pub effective_path: Option<String>,
    pub server_args: Vec<String>,
    pub source: String,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LspStatusView {
    pub workspace_root: String,
    pub config_path: String,
    pub fallback_enabled: bool,
    pub servers: Vec<LspServerStatus>,
}

#[async_trait::async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str {
        "lsp"
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn process_resource_kind(&self) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        Some(rightkit_agent_engine_runtime_resources::ResourceKind::Lsp)
    }

    fn description(&self) -> &str {
        "Semantic code navigation. Ops: status, workspace_symbols(query), goto_definition(path,line,character), references(path,line,character), hover(path,line,character), diagnostics(path). Reuses one live language server per workspace. Definition and hover can use a local fallback; workspace symbols, references and diagnostics require a real server. Line is 1-based to match read_file output."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["operation"],
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["status", "workspace_symbols", "goto_definition", "references", "hover", "diagnostics"]
                },
                "query": { "type": "string", "description": "Workspace symbol search query" },
                "path": { "type": "string", "description": "Workspace-relative or absolute file path" },
                "line": { "type": "integer", "description": "1-based line number" },
                "character": { "type": "integer", "description": "0-based character offset on the line" },
                "server_path": { "type": "string", "description": "Optional explicit language-server executable path" },
                "server_args": { "type": "array", "items": { "type": "string" } }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        match args["operation"].as_str().unwrap_or("") {
            "status" => ToolResult::ok(render_status(&ctx.workspace_root)),
            "workspace_symbols" => workspace_symbols(args, ctx).await,
            "goto_definition" => goto_definition(args, ctx).await,
            "references" => references(args, ctx).await,
            "hover" => hover(args, ctx).await,
            "diagnostics" => diagnostics(args, ctx).await,
            "" => ToolResult::error("Missing required argument: operation"),
            other => ToolResult::error(format!("Unknown lsp operation: {other}")),
        }
    }
}

async fn workspace_symbols(args: Value, ctx: &ToolContext) -> ToolResult {
    let Some(query) = args["query"].as_str() else {
        return ToolResult::error("Missing required argument: query");
    };
    let path = if args["path"].is_string() {
        match path_arg(&args, ctx) {
            Ok(path) => path,
            Err(error) => return ToolResult::error(error),
        }
    } else {
        ctx.workspace_root.clone()
    };
    let Some(server) = server_config_from_args_or_path(&args, &path, ctx) else {
        return ToolResult::error(
            "Workspace symbols require a configured or discovered language server.",
        );
    };
    let bridge = match live_bridge(&server, ctx).await {
        Ok(bridge) => bridge,
        Err(error) => {
            return ToolResult::error(format!("LSP workspace symbols unavailable: {error}"))
        }
    };
    let result = bridge.lock().await.workspace_symbols(query).await;
    match result {
        Ok(value) if value.is_null() => ToolResult::ok("No workspace symbols found."),
        Ok(Value::Array(mut symbols)) => {
            let truncated = symbols.len() > 500;
            symbols.truncate(500);
            ToolResult::ok(json!({"symbols": symbols, "truncated": truncated}).to_string())
        }
        Ok(_) => ToolResult::error("LSP workspace symbols returned an invalid result."),
        Err(error) => {
            super::workspace_manager::invalidate_generation(&server, &bridge).await;
            ToolResult::error(format!("LSP workspace symbols failed: {error}"))
        }
    }
}

async fn goto_definition(args: Value, ctx: &ToolContext) -> ToolResult {
    let target = match target_from_args(&args, ctx) {
        Ok(target) => target,
        Err(e) => return ToolResult::error(e),
    };
    if let Some(server) = server_config_from_args_or_path(&args, &target.path, ctx) {
        match bridge_goto_definition(server, &target, ctx).await {
            Ok(result) if !result.trim().is_empty() => return ToolResult::ok(result),
            Ok(_) => {}
            Err(e) => {
                let fallback = local_goto_definition(&target, ctx);
                return if fallback.is_error {
                    ToolResult::error(format!(
                        "LSP server failed ({e}); local fallback also failed: {}",
                        fallback.content
                    ))
                } else {
                    ToolResult::ok(format!(
                        "[LSP server failed: {e}; using local fallback]\n{}",
                        fallback.content
                    ))
                };
            }
        }
    }
    local_goto_definition(&target, ctx)
}

async fn references(args: Value, ctx: &ToolContext) -> ToolResult {
    let target = match target_from_args(&args, ctx) {
        Ok(target) => target,
        Err(e) => return ToolResult::error(e),
    };
    let Some(server) = server_config_from_args_or_path(&args, &target.path, ctx) else {
        return ToolResult::error("References require a configured or discovered language server.");
    };
    match bridge_references(server, &target, ctx).await {
        Ok(locations) if locations.is_empty() => ToolResult::ok("No references found."),
        Ok(locations) => ToolResult::ok(format!("References:\n{}", locations.join("\n"))),
        Err(error) => ToolResult::error(format!("LSP references failed: {error}")),
    }
}

async fn hover(args: Value, ctx: &ToolContext) -> ToolResult {
    let target = match target_from_args(&args, ctx) {
        Ok(target) => target,
        Err(e) => return ToolResult::error(e),
    };
    if let Some(server) = server_config_from_args_or_path(&args, &target.path, ctx) {
        match bridge_hover(server, &target, ctx).await {
            Ok(content) if !content.trim().is_empty() => return ToolResult::ok(content),
            Ok(_) => {}
            Err(_) => {}
        }
    }
    let def = local_goto_definition(&target, ctx);
    if def.is_error {
        def
    } else {
        ToolResult::ok(format!("Local hover:\n{}", def.content))
    }
}

async fn diagnostics(args: Value, ctx: &ToolContext) -> ToolResult {
    let path = match path_arg(&args, ctx) {
        Ok(path) => path,
        Err(e) => return ToolResult::error(e),
    };
    if let Some(server) = server_config_from_args_or_path(&args, &path, ctx) {
        let uri = file_uri(&path);
        let bridge = match live_bridge(&server, ctx).await {
            Ok(bridge) => bridge,
            Err(e) => {
                return ToolResult::error(format!("LSP diagnostics unavailable: {e}"));
            }
        };
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Err(error) =
                super::workspace_manager::sync_document(&server, &bridge, &uri, &text).await
            {
                return ToolResult::error(format!("LSP diagnostics unavailable: {error}"));
            }
        }
        let result = bridge.lock().await.diagnostics(&uri).await;
        if result.is_err() {
            super::workspace_manager::invalidate_generation(&server, &bridge).await;
        }
        match result {
            Ok(diags) if diags.is_empty() => ToolResult::ok("No diagnostics reported."),
            Ok(diags) => ToolResult::ok(format!(
                "{DIAGNOSTICS_PREFIX}{}",
                diags
                    .iter()
                    .map(|d| format_diagnostic_line(
                        &d.uri,
                        d.start.line + 1,
                        d.start.character,
                        &d.severity,
                        &d.message
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            )),
            Err(e) => ToolResult::error(format!("LSP diagnostics failed: {e}")),
        }
    } else {
        ToolResult::ok("Diagnostics require a language server; none was configured or discovered.")
    }
}

async fn bridge_goto_definition(
    server: LspServerConfig,
    target: &Target,
    ctx: &ToolContext,
) -> Result<String, String> {
    let uri = file_uri(&target.path);
    let bridge = live_bridge(&server, ctx).await?;
    if let Ok(text) = std::fs::read_to_string(&target.path) {
        super::workspace_manager::sync_document(&server, &bridge, &uri, &text).await?;
    }
    let mut guard = bridge.lock().await;
    let result = guard
        .goto_definition(
            &uri,
            LspPosition {
                line: target.line_index as u32,
                character: target.character as u32,
            },
        )
        .await;
    if result.is_err() {
        drop(guard);
        super::workspace_manager::invalidate_generation(&server, &bridge).await;
    } else {
        drop(guard);
    }
    let result = result.map_err(|e| e.to_string())?;
    if result.locations.is_empty() {
        return Ok(String::new());
    }
    Ok(format!(
        "Definitions:\n{}",
        result
            .locations
            .iter()
            .map(|loc| format!("{}:{}:{}", loc.uri, loc.start.line + 1, loc.start.character))
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

async fn bridge_hover(
    server: LspServerConfig,
    target: &Target,
    ctx: &ToolContext,
) -> Result<String, String> {
    let uri = file_uri(&target.path);
    let bridge = live_bridge(&server, ctx).await?;
    if let Ok(text) = std::fs::read_to_string(&target.path) {
        super::workspace_manager::sync_document(&server, &bridge, &uri, &text).await?;
    }
    let mut guard = bridge.lock().await;
    let result = guard
        .hover(
            &uri,
            LspPosition {
                line: target.line_index as u32,
                character: target.character as u32,
            },
        )
        .await;
    if result.is_err() {
        drop(guard);
        super::workspace_manager::invalidate_generation(&server, &bridge).await;
    } else {
        drop(guard);
    }
    result.map(|h| h.content).map_err(|e| e.to_string())
}

async fn bridge_references(
    server: LspServerConfig,
    target: &Target,
    ctx: &ToolContext,
) -> Result<Vec<String>, String> {
    let uri = file_uri(&target.path);
    let bridge = live_bridge(&server, ctx).await?;
    if let Ok(text) = std::fs::read_to_string(&target.path) {
        super::workspace_manager::sync_document(&server, &bridge, &uri, &text).await?;
    }
    let mut guard = bridge.lock().await;
    let result = guard
        .references(
            &uri,
            LspPosition {
                line: target.line_index as u32,
                character: target.character as u32,
            },
        )
        .await;
    if result.is_err() {
        drop(guard);
        super::workspace_manager::invalidate_generation(&server, &bridge).await;
    } else {
        drop(guard);
    }
    result
        .map(|locations| {
            locations
                .into_iter()
                .map(|loc| format!("{}:{}:{}", loc.uri, loc.start.line + 1, loc.start.character))
                .collect()
        })
        .map_err(|e| e.to_string())
}

/// Files above this size are refused by the local fallback's own-file read,
/// mirroring the bound the fallback scan applies to every other file it
/// visits so a single oversized target cannot bypass it.
const MAX_LOCAL_TARGET_FILE_BYTES: u64 = 2 * 1024 * 1024;

fn local_goto_definition(target: &Target, ctx: &ToolContext) -> ToolResult {
    if let Ok(metadata) = std::fs::metadata(&target.path) {
        if metadata.len() > MAX_LOCAL_TARGET_FILE_BYTES {
            return ToolResult::error(format!(
                "'{}' is {} bytes; local fallback refuses files over {MAX_LOCAL_TARGET_FILE_BYTES} bytes",
                target.path.display(),
                metadata.len()
            ));
        }
    }
    let content = match std::fs::read_to_string(&target.path) {
        Ok(content) => content,
        Err(e) => {
            return ToolResult::error(format!("Failed to read '{}': {e}", target.path.display()))
        }
    };
    let symbol = match symbol_at(&content, target.line_index, target.character) {
        Some(symbol) => symbol,
        None => return ToolResult::error("Could not identify symbol at the requested position"),
    };
    let definitions = find_definitions(&ctx.workspace_root, &symbol);
    if definitions.is_empty() {
        ToolResult::ok(format!("No definition found for '{symbol}'."))
    } else {
        ToolResult::ok(format!(
            "Definitions for '{symbol}':\n{}",
            render_matches(&definitions)
        ))
    }
}
