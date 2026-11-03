//! The finite set of render classes a tool call can resolve to.
//!
//! Mirrors pi's `ToolExecutionComponent` registry
//! (`packages/tui/src/components/tool-execution.ts:84-102`): every built-in
//! tool is classified up front into one of a small, closed set of classes,
//! and anything not on the list -- including every dynamic/MCP tool, which
//! by definition we cannot enumerate ahead of time -- falls back to
//! [`RenderClass::Generic`]. There is no path from "unrecognized tool name"
//! to "unbounded rendering": `classify` always returns *something* finite.

/// Who draws the frame around a tool call: the generic bordered shell, or
/// the tool's own drawing (reserved for a future built-in that needs bespoke
/// layout, e.g. a diff view). Every class here uses `Default` today; the
/// enum exists so adding a self-drawn tool later is a data change, not a
/// new code path. Mirrors `renderShell: "default" | "self"`
/// (`tool-execution.ts:108-116`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderShell {
    /// The generic bordered frame this module draws.
    Default,
    /// The tool draws its own framing inside the allotted area.
    SelfDrawn,
}

/// The finite set of ways a tool call can be presented. Adding a tool never
/// adds a rendering code path -- it maps to one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderClass {
    /// Reads from the workspace: `read`, `glob`'s listing, etc.
    FileRead,
    /// Writes to the workspace: `write`, `edit`.
    FileWrite,
    /// Runs a subprocess: `bash`, `shell`.
    Shell,
    /// Searches content: `grep`, `search`.
    Search,
    /// Talks to the network: `fetch`, `webfetch`.
    Network,
    /// Anything unrecognized -- unknown built-ins and every dynamic/MCP
    /// tool. Bounded: see `render::tool_call`'s line and character caps.
    Generic,
}

impl RenderClass {
    /// Every class currently uses the generic bordered frame. Kept as a
    /// method (rather than inlined at call sites) so a future self-drawn
    /// class is a one-line change here.
    pub fn shell(self) -> RenderShell {
        RenderShell::Default
    }
}

/// Built-in tool name -> render class. Names are matched case-insensitively.
/// This list is the host app's own; it intentionally does not import
/// `command_registry` (a binary crate this lane cannot depend on) and instead
/// mirrors the concept locally, per the task's read-only reference to it.
const BUILTIN_TOOLS: &[(&str, RenderClass)] = &[
    ("read", RenderClass::FileRead),
    ("write", RenderClass::FileWrite),
    ("edit", RenderClass::FileWrite),
    ("multiedit", RenderClass::FileWrite),
    ("bash", RenderClass::Shell),
    ("shell", RenderClass::Shell),
    ("exec", RenderClass::Shell),
    ("grep", RenderClass::Search),
    ("glob", RenderClass::Search),
    ("search", RenderClass::Search),
    ("webfetch", RenderClass::Network),
    ("fetch", RenderClass::Network),
    ("websearch", RenderClass::Network),
];

/// Resolves a tool name to its render class.
///
/// Order, most specific first:
///
/// 1. A class registered at runtime for this exact name (the production
///    seam: the host binary registers every real tool name at boot -- see
///    `crate::register_render_class`). This is what makes `read_file` render
///    as [`RenderClass::FileRead`] instead of falling through to Generic.
/// 2. The legacy static `BUILTIN_TOOLS` table.
/// 3. [`RenderClass::Generic`] -- unknown or dynamic tools, always. Never an
///    unbounded default.
pub fn classify(tool_name: &str) -> RenderClass {
    if let Some(class) = crate::lookup_registered_render_class(tool_name) {
        return class;
    }
    let lower = tool_name.to_ascii_lowercase();
    BUILTIN_TOOLS
        .iter()
        .find(|(name, _)| *name == lower)
        .map(|(_, class)| *class)
        .unwrap_or(RenderClass::Generic)
}
