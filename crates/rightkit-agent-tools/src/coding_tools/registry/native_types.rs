// Tool registry — manages the set of available tools.
//
// The `Tool` impl for `rightkit_ffmpeg::agent_media::NativeMediaTool` lives in
// `crate::coding_tools::media_tools` (WS-7, `media_tools.rs`) — that module is the
// canonical home of the bridge between `host_media`'s
// `NativeMediaTool` and the registry's own `Tool` trait; media tools are
// registered individually by `builtin_tool_factories()` (CQ-16).
//
use crate::coding_tools::tool_trait::Tool;
use serde::Serialize;

/// Identity of a disposable dynamic-registration owner (EXT-001).
///
/// Every dynamically registered tool belongs to exactly one scope. Unloading
/// the scope removes every registration it owns, so a disconnected extension
/// or MCP server can never leave a stale tool behind. Built-in tools carry no
/// scope and are never removed by an unload.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RegistrationScopeId(pub String);

impl RegistrationScopeId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RegistrationScopeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityDescriptor {
    pub id: String,
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub schema_digest: String,
    pub read_only: bool,
    pub availability: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ModelToolInterface {
    #[default]
    LegacyPromoted,
    FixedV1,
}

pub struct ToolRegistry {
    pub(super) tools: Vec<Box<dyn Tool>>,
    /// Owner scope for each dynamically registered tool, keyed by tool name.
    /// Absent means a built-in registration with no disposable owner.
    pub(super) scope_owners: std::collections::BTreeMap<String, RegistrationScopeId>,
    /// Deferred/lazy tool loading (par §#157): when true, non-core tools are
    /// withheld from the default provider tool block until `tool_search`
    /// scopes them by name. OFF by default — preserves the existing
    /// full-prelude behavior (back-compat) until a caller opts in via
    /// `with_lazy_loading(true)`.
    pub(super) lazy_loading: bool,
    pub(super) model_interface: ModelToolInterface,
}

/// Why a scoped registration was refused. Refusing is deliberate: silent
/// name-stealing would break `unload_scope`'s total-removal guarantee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopedRegistrationError {
    OwnedByAnotherScope {
        tool: String,
        owner: RegistrationScopeId,
    },
    ShadowsBuiltin {
        tool: String,
    },
}

impl std::fmt::Display for ScopedRegistrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OwnedByAnotherScope { tool, owner } => {
                write!(f, "tool `{tool}` is already registered by scope `{owner}`")
            }
            Self::ShadowsBuiltin { tool } => {
                write!(f, "tool `{tool}` would shadow a built-in registration")
            }
        }
    }
}

impl std::error::Error for ScopedRegistrationError {}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCapability {
    pub name: String,
    pub description: String,
    pub category: String,
    pub read_only: bool,
    /// `read`, `write`, `external`, `spend` or `destroy` (rule 50a).
    pub effect_class: String,
    /// `none`, `yes` or `in_app` (rule 50a).
    pub approval_class: String,
}

/// Finite render class for a tool call, owned by the production registry.
/// TUI consumes this via `ToolRegistry::render_classes()`; unknown/dynamic
/// tools fall back to `Generic` with bounded presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolRenderClass {
    FileRead,
    FileWrite,
    Shell,
    Search,
    Network,
    Generic,
}

/// Built-in tool name -> render class, case-insensitive.
/// This is the single source of truth for P1-4: adding a built-in tool
/// requires classifying it here, not in a separate TUI list.
pub fn tool_render_class(name: &str) -> ToolRenderClass {
    let lower = name.to_ascii_lowercase();
    match lower.as_str() {
        "read_file" | "view_image" | "list_files" | "git_read" | "git_diff" | "lsp"
        | "get_shell_output" => ToolRenderClass::FileRead,
        "write_file" | "edit_file" | "multi_edit" | "undo_edit" | "apply_patch" | "todo_write"
        | "notebook" | "file_service" | "git_write" => ToolRenderClass::FileWrite,
        "shell" | "powershell" | "repl" | "kill_shell" | "process" | "ocr" | "ocr_onnx"
        | "web_extract" | "media_fetch" | "ffmpeg_extract" => ToolRenderClass::Shell,
        "search" | "tool_search" => ToolRenderClass::Search,
        "web_search" | "browser_fetch" | "browser_open" | "browser_read" | "browser_act"
        | "browser_screenshot" | "browser_console" | "browser_close" | "recursive_fetch"
        | "github" | "context7" | "mcp_call" | "list_mcp_tools" | "list_mcp_resources"
        | "read_mcp_resource" | "cloudflare" => ToolRenderClass::Network,
        _ => ToolRenderClass::Generic,
    }
}

pub fn render_tool_capabilities_markdown(capabilities: &[ToolCapability]) -> String {
    let read_only = capabilities.iter().filter(|cap| cap.read_only).count();
    let mut out = format!(
        "**Agent capabilities:** {} tools ({} read-only, {} mutating)",
        capabilities.len(),
        read_only,
        capabilities.len().saturating_sub(read_only)
    );
    let mut current_category = "";
    for cap in capabilities {
        if cap.category != current_category {
            current_category = &cap.category;
            out.push_str(&format!("\n\n### {}", current_category));
        }
        let mode = if cap.read_only {
            "read-only"
        } else {
            "mutating"
        };
        out.push_str(&format!(
            "\n- `{}` ({}) - {}",
            cap.name, mode, cap.description
        ));
    }
    out
}
