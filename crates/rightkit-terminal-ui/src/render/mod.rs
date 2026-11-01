//! Tool-call rendering.
//!
//! A tool call renders as a bordered block that fills in as the tool
//! streams, then the whole block turns green on success or red on failure
//! (resolved through [`crate::component::Theme`], never a literal colour).
//!
//! # Shape
//!
//! - [`hint`] -- the only data shape a tool's call/result may take. Every
//!   constructor sanitizes and bounds its input; this is the security
//!   boundary render hints cross.
//! - [`registry`] -- the finite set of render classes built-in tools map
//!   to, with unknown/dynamic tools always falling back to
//!   [`RenderClass::Generic`].
//! - [`sanitize`] -- strips control bytes (including every ANSI escape
//!   opener) from untrusted text before it is stored anywhere.
//! - [`truncate`] -- bounds display size while recording the original size
//!   and the cap applied, never a bare ellipsis.
//! - [`tool_call`] -- [`ToolCallBlock`], the `Component` that draws the
//!   frame described above.
//!
//! Mirrors pi's `ToolExecutionComponent`
//! (`packages/tui/src/components/tool-execution.ts:16-388`): per-tool
//! classification resolved from a registry, a shell choice between the
//! generic frame and a tool-drawn one, and a bounded fallback for anything
//! not recognized.

pub mod allow_rule;
pub mod approval;
pub mod cell;
pub mod destructive;
pub mod diff;
pub mod elicitation_form;
pub mod exec;
mod hint;
mod registry;
mod sanitize;
pub mod status;
mod tool_call;
mod truncate;

pub use hint::{CallHint, ResultHint};
pub use registry::{RenderClass, RenderShell, classify};
pub use sanitize::{display_path_str, sanitize};
pub use tool_call::{ToolCallBlock, ToolPresentation, ToolState};
pub use truncate::{Truncation, truncate};

/// Presentation density is a projection choice only; it never changes the
/// semantic content captured by a render document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RenderDensity {
    #[default]
    Expanded,
    Compact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Context,
    Addition,
    Deletion,
}

/// Frontend-neutral semantic content. Ratatui and plain CLI consumers can
/// project this same value without sharing widget or terminal types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticBlock {
    Heading(String),
    Paragraph(String),
    Code(String),
    Link { label: String, target: String },
    Diff { kind: DiffKind, text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SemanticDocument {
    blocks: Vec<SemanticBlock>,
}

impl SemanticDocument {
    pub fn new(blocks: impl IntoIterator<Item = SemanticBlock>) -> Self {
        Self {
            blocks: blocks.into_iter().collect(),
        }
    }

    pub fn blocks(&self) -> &[SemanticBlock] {
        &self.blocks
    }

    /// Plain projection used by CLI/non-Ratatui consumers.
    pub fn plain_lines(&self) -> Vec<String> {
        self.blocks
            .iter()
            .map(|block| match block {
                SemanticBlock::Heading(text)
                | SemanticBlock::Paragraph(text)
                | SemanticBlock::Code(text) => text.clone(),
                SemanticBlock::Diff { kind, text } => {
                    let marker = match kind {
                        DiffKind::Context => " ",
                        DiffKind::Addition => "+",
                        DiffKind::Deletion => "-",
                    };
                    format!("{marker}{text}")
                }
                SemanticBlock::Link { label, target } => format!("{label} ({target})"),
            })
            .collect()
    }
}
