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
/// 2. The legacy static [`BUILTIN_TOOLS`] table.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_tool_maps_to_a_specific_class_not_generic() {
        for (name, class) in BUILTIN_TOOLS {
            assert_ne!(
                classify(name),
                RenderClass::Generic,
                "builtin tool {name:?} must not fall back to Generic"
            );
            assert_eq!(classify(name), *class);
        }
    }

    #[test]
    fn an_unknown_or_dynamic_tool_falls_back_to_bounded_generic() {
        assert_eq!(
            classify("some_mcp_tool_nobody_registered"),
            RenderClass::Generic
        );
        assert_eq!(classify(""), RenderClass::Generic);
        assert_eq!(classify("<script>alert(1)</script>"), RenderClass::Generic);
    }

    #[test]
    fn classification_is_case_insensitive() {
        assert_eq!(classify("BASH"), RenderClass::Shell);
        assert_eq!(classify("Read"), RenderClass::FileRead);
    }

    // The bug this seam exists to fix: a production tool name that is NOT on
    // the legacy static list used to fall through to Generic forever. Once
    // registered, it must resolve to exactly the intentional class.
    #[test]
    fn a_registered_specific_class_does_not_fall_through_to_generic() {
        // Unique names: tests in one binary run in parallel and share the
        // process-wide registration map; nothing else may depend on these.
        assert_eq!(classify("read_file"), RenderClass::Generic);
        crate::register_render_class("read_file", RenderClass::FileRead);
        assert_ne!(
            classify("read_file"),
            RenderClass::Generic,
            "a registered tool must not fall back to Generic"
        );
        assert_eq!(classify("read_file"), RenderClass::FileRead);
    }

    #[test]
    fn bulk_registration_registers_every_entry_and_last_write_wins() {
        let probe = "lane_b_bulk_probe";
        assert_eq!(classify(probe), RenderClass::Generic);
        crate::register_render_classes([
            (probe, RenderClass::Network),
            (probe, RenderClass::Shell),
        ]);
        assert_eq!(
            classify(probe),
            RenderClass::Shell,
            "last registration wins"
        );
    }

    #[test]
    fn registered_lookup_is_case_insensitive_like_the_static_table() {
        let probe = "lane_b_case_probe";
        crate::register_render_class(probe, RenderClass::Search);
        assert_eq!(classify("LANE_B_CASE_PROBE"), RenderClass::Search);
        assert_eq!(classify(probe), RenderClass::Search);
    }

    #[test]
    fn registration_refines_without_unbounding_unknown_tools() {
        crate::register_render_class("lane_b_one_real_tool", RenderClass::FileWrite);
        // Registration is additive only: names nobody registered still hit
        // the bounded Generic fallback.
        assert_eq!(classify("never_registered_anywhere"), RenderClass::Generic);
        assert_eq!(classify(""), RenderClass::Generic);
    }

    #[test]
    fn every_class_currently_resolves_to_the_default_shell() {
        for class in [
            RenderClass::FileRead,
            RenderClass::FileWrite,
            RenderClass::Shell,
            RenderClass::Search,
            RenderClass::Network,
            RenderClass::Generic,
        ] {
            assert_eq!(class.shell(), RenderShell::Default);
        }
    }
}
