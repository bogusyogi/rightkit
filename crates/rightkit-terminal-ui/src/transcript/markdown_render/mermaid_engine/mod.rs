//! Bounded, terminal-native Mermaid diagram renderer.
//!
//! Lifted from `repos/codex/codex-rs/mermaid/src/lib.rs` at
//! `444da310e108da16aaeb18fd790b0ac464f08aca` (Apache-2.0; Codex NOTICE applies).
//! Absorbed as a local submodule of `markdown_render::mermaid`, not a separate
//! crate. Adaptations from the donor file: module docs reworded, `pub` surface
//! kept for the parent module only, `render` (string join helper) omitted as
//! the caller consumes spans directly, and the donor `#[path]` test
//! declarations are replaced by `engine_tests.rs`. Child sources are verbatim
//! copies; their `use super::` paths resolve against this module.
//!
//! Supports flowcharts, sequences, states, classes, and entity relationships.
//! Graph edges own separate lanes and endpoint positions. Crossings never join
//! routes. Unsupported syntax and outputs exceeding the caller's width return
//! errors, leaving source fallback to the caller. This module performs no I/O
//! and does not depend on a Mermaid implementation.

mod draw;
mod output;
mod parse;
mod relations;
mod sequence;
mod state;
mod syntax;

pub use output::Role;
pub use output::Span;
use std::fmt;

const MAX_SOURCE: usize = 16 * 1024;
const MAX_NODES: usize = 16;
const MAX_EDGES: usize = 24;
const MAX_LABEL: usize = 40;
const MAX_CELLS: usize = 64 * 1024;

/// A diagram cannot be faithfully represented by this bounded prototype.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderError {
    /// Syntax or text is outside the explicitly supported subset.
    Unsupported,
    /// A source, diagram, label, or canvas limit was exceeded.
    Limit,
    /// The complete diagram exceeds the supplied display width.
    TooWide,
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unsupported => "unsupported Mermaid syntax or label",
            Self::Limit => "diagram exceeds prototype limits",
            Self::TooWide => "diagram exceeds available display width",
        })
    }
}

impl std::error::Error for RenderError {}

/// Render the same bounded diagram as lines of semantic spans for caller-provided styling.
///
/// Supports flowcharts, sequence diagrams, flat state diagrams, class diagrams, and ER diagrams.
/// Unknown syntax, unsafe terminal text, and diagrams exceeding `max_width` return errors; no
/// partial result is returned.
pub fn render_spans(source: &str, max_width: usize) -> Result<Vec<Vec<Span>>, RenderError> {
    if source.len() > MAX_SOURCE {
        return Err(RenderError::Limit);
    }
    let statements = syntax::statements(source)?;
    let (header, body) = statements.split_first().ok_or(RenderError::Unsupported)?;
    match *header {
        "sequenceDiagram" => sequence::render(body, max_width),
        "stateDiagram-v2" | "stateDiagram" | "classDiagram" | "erDiagram" => {
            draw::render(&relations::parse(header, body)?, max_width)
        }
        _ => draw::render(&parse::parse(header, body)?, max_width),
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Direction {
    #[default]
    Down,
    Up,
    Right,
    Left,
}

impl Direction {
    fn parse(text: &str) -> Result<Self, RenderError> {
        match text {
            "TD" | "TB" => Ok(Self::Down),
            "BT" => Ok(Self::Up),
            "LR" => Ok(Self::Right),
            "RL" => Ok(Self::Left),
            _ => Err(RenderError::Unsupported),
        }
    }
}

/// Node shapes supported by the terminal renderer.
#[derive(Debug, PartialEq, Eq)]
enum Shape {
    /// A rectangular node, such as `A[Work]`.
    Rectangle,
    /// A decision node (`A{Done?}`), rendered with a `◇` marker inside a box.
    Decision,
    /// A stadium node (`A([Start])`), rendered as a box with rounded corners.
    Stadium,
}

#[derive(Debug, PartialEq, Eq)]
struct Node {
    id: String,
    label: String,
    shape: Shape,
    declared: bool,
    members: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct Edge {
    from: usize,
    to: usize,
    label: String,
    target_label: String,
    source_tip: char,
    target_tip: char,
    dashed: bool,
}

impl Edge {
    fn directed(from: usize, to: usize, label: String) -> Self {
        Self {
            from,
            to,
            label,
            target_label: String::new(),
            source_tip: '─',
            target_tip: '◄',
            dashed: false,
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Graph {
    direction: Direction,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Graph {
    fn node(&mut self, id: &str) -> Result<usize, RenderError> {
        if let Some(index) = self.nodes.iter().position(|node| node.id == id) {
            return Ok(index);
        }
        if self.nodes.len() == MAX_NODES {
            return Err(RenderError::Limit);
        }
        self.nodes.push(Node {
            id: id.to_owned(),
            label: id.to_owned(),
            shape: Shape::Rectangle,
            declared: false,
            members: Vec::new(),
        });
        Ok(self.nodes.len() - 1)
    }
}
