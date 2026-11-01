//! The tool-call block: a bordered frame that fills in as a tool streams,
//! then recolours as one whole to signal success or failure.
//!
//! This is the single visual detail the product owner asked for by name:
//! the frame is `Role::Running` while output is arriving, then the *entire*
//! block -- border, title, and body -- switches to `Role::Success` or
//! `Role::Failure` on completion. Colour is never the only signal: the
//! title glyph (`theme.glyph`) changes with it, per `component.rs`'s
//! accessibility test.

use crate::component::RenderFrameExt as _;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::component::{Component, Role, Theme};
use crate::transcript::wrap::wrap;

use super::RenderDensity;
use super::cell::{Cell, clipped_line};
use super::hint::{CallHint, ResultHint};
use super::registry::{RenderClass, classify};
use super::sanitize::sanitize;
use super::truncate::{Truncation, truncate};

/// Streamed-line cap for a classified (known) tool. Generous, but never
/// unbounded: a misbehaving built-in still cannot grow a block forever.
const KNOWN_MAX_LINES: usize = 200;
const KNOWN_LINE_CAP: usize = 400;

/// Streamed-line cap for [`RenderClass::Generic`] -- unknown and dynamic
/// tools. Tighter, because we have no prior knowledge of what "reasonable
/// output" looks like for a tool we didn't write.
const GENERIC_MAX_LINES: usize = 20;
const GENERIC_LINE_CAP: usize = 200;

/// Where a tool call currently stands. Drives both the frame colour and the
/// title glyph -- see [`ToolState::role`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Success,
    Failure,
}

impl ToolState {
    /// The semantic role this state maps to. Never referenced as a literal
    /// colour -- always resolved through `theme.color`/`theme.glyph`.
    pub fn role(self) -> Role {
        match self {
            ToolState::Running => Role::Running,
            ToolState::Success => Role::Success,
            ToolState::Failure => Role::Failure,
        }
    }
}

/// One tool call, rendered as a bordered block.
pub struct ToolCallBlock {
    call: CallHint,
    class: RenderClass,
    state: ToolState,
    /// Lines accumulated while streaming, before a final result lands.
    streamed: Vec<String>,
    result: Option<ResultHint>,
    density: RenderDensity,
}

impl ToolCallBlock {
    pub fn new(call: CallHint) -> Self {
        let class = classify(&call.tool);
        Self {
            call,
            class,
            state: ToolState::Running,
            streamed: Vec::new(),
            result: None,
            density: RenderDensity::Expanded,
        }
    }

    pub fn class(&self) -> RenderClass {
        self.class
    }

    pub fn state(&self) -> ToolState {
        self.state
    }

    pub fn set_density(&mut self, density: RenderDensity) {
        self.density = density;
    }

    pub fn density(&self) -> RenderDensity {
        self.density
    }

    fn line_cap(&self) -> usize {
        match self.class {
            RenderClass::Generic => GENERIC_MAX_LINES,
            _ => KNOWN_MAX_LINES,
        }
    }

    fn char_cap(&self) -> usize {
        match self.class {
            RenderClass::Generic => GENERIC_LINE_CAP,
            _ => KNOWN_LINE_CAP,
        }
    }

    /// Appends one streamed chunk while the call is still running. The
    /// chunk is sanitized and bounded exactly like a [`ResultHint`] --
    /// there is no separate, less-safe path for "output that arrived
    /// early" versus "output that arrived at the end".
    pub fn push_chunk(&mut self, chunk: &str) {
        let sanitized = sanitize(chunk);
        let cap = self.line_cap();
        let char_cap = self.char_cap();
        for line in sanitized.lines() {
            if self.streamed.len() >= cap {
                break;
            }
            let (text, _) = truncate(line, char_cap);
            self.streamed.push(text);
        }
    }

    /// Settles the block: the whole frame recolours to success or failure,
    /// and any final result content is attached.
    pub fn finish(&mut self, result: ResultHint, ok: bool) {
        self.state = if ok {
            ToolState::Success
        } else {
            ToolState::Failure
        };
        self.result = Some(result);
    }

    /// Title text. Known tools show the call's sanitized summary; an
    /// unrecognized/dynamic tool falls back to its bare name only -- a
    /// smaller, more predictable surface than trusting an attacker-supplied
    /// summary string to be display-safe, even after sanitization.
    fn title(&self, theme: &Theme) -> String {
        let glyph = theme.glyph(self.state.role());
        match self.class {
            RenderClass::Generic => format!("{glyph} {}", self.call.tool),
            _ => format!("{glyph} {}", self.call.summary),
        }
    }

    fn body(&self, theme: &Theme) -> Vec<Line<'static>> {
        let presentation = self.presentation(self.density);
        let truncated = presentation.truncation;
        let mut body: Vec<Line> = presentation.lines.into_iter().map(Line::from).collect();
        if let Some(truncation) = truncated {
            let dim = Style::default().fg(theme.color(Role::Dim));
            body.push(Line::from(Span::styled(truncation.marker(), dim)));
        }
        body
    }

    /// Stable, widget-free projection for CLI/non-Ratatui consumers. Compact
    /// and expanded output draw from the same bounded semantic content.
    pub fn presentation(&self, density: RenderDensity) -> ToolPresentation {
        let mut lines = Vec::new();
        match density {
            RenderDensity::Expanded => {
                lines.extend(self.streamed.iter().cloned());
                if let Some(result) = &self.result {
                    lines.extend(result.lines.iter().cloned());
                }
            }
            RenderDensity::Compact => {
                if let Some(first) = self.streamed.first() {
                    lines.push(first.clone());
                }
                if self.streamed.len() > 1 {
                    lines.push(format!(
                        "… {} streamed lines hidden",
                        self.streamed.len() - 1
                    ));
                }
                if let Some(result) = &self.result {
                    if let Some(first) = result.lines.first() {
                        lines.push(first.clone());
                    }
                    if result.lines.len() > 1 {
                        lines.push(format!("… {} result lines hidden", result.lines.len() - 1));
                    }
                }
            }
        }
        let truncation = self.result.as_ref().and_then(|result| result.truncation);
        ToolPresentation {
            title: match self.class {
                RenderClass::Generic => self.call.tool.clone(),
                _ => self.call.summary.clone(),
            },
            class: self.class,
            state: self.state,
            lines,
            truncation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPresentation {
    pub title: String,
    pub class: RenderClass,
    pub state: ToolState,
    pub lines: Vec<String>,
    pub truncation: Option<Truncation>,
}

impl ToolPresentation {
    pub fn plain_lines(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.lines.len() + 1);
        out.push(self.title.clone());
        out.extend(self.lines.iter().cloned());
        if let Some(truncation) = self.truncation {
            out.push(truncation.marker());
        }
        out
    }
}

impl Component for ToolCallBlock {
    fn render(&mut self, frame: &mut dyn crate::component::RenderFrame, area: Rect, theme: &Theme) {
        let color = theme.color(self.state.role());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(color))
            .title(self.title(theme));
        let paragraph = Paragraph::new(self.body(theme))
            .block(block)
            .style(Style::default().fg(color));
        frame.render_widget(paragraph, area);
    }

    fn desired_height(&self, _width: u16, available: u16) -> u16 {
        let presentation = self.presentation(self.density);
        let content_lines = presentation.lines.len() + presentation.truncation.is_some() as usize;
        // +2 for the top/bottom border.
        (content_lines as u16).saturating_add(2).min(available)
    }
}

/// History-cell projection of the same bounded [`ToolPresentation`], so a
/// tool call can also sit in the cell-based transcript/scrollback without
/// duplicating its sanitization or truncation rules. The widget frame's
/// colour cue becomes a `•`/`✓`-glyph gutter on the title row; body rows
/// wrap to the column budget and clip like every other cell.
impl Cell for ToolCallBlock {
    fn display_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        self.presentation_lines(self.density, width, theme)
    }

    fn transcript_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        // The expanded projection keeps the full retained content
        // regardless of the compact display density.
        self.presentation_lines(RenderDensity::Expanded, width, theme)
    }
}

impl ToolCallBlock {
    fn presentation_lines(
        &self,
        density: RenderDensity,
        width: u16,
        theme: &Theme,
    ) -> Vec<Line<'static>> {
        let role = self.state.role();
        let color = theme.color(role);
        let dim = Style::default().fg(theme.color(Role::Dim));
        let presentation = self.presentation(density);

        let mut out: Vec<Line<'static>> = Vec::new();
        let title = format!("{} {}", theme.glyph(role), presentation.title);
        for (i, row) in wrap(&title, width.max(1)).into_iter().enumerate() {
            let marker = if i == 0 { "" } else { "  " };
            out.push(Line::from(vec![
                Span::styled(marker.to_string(), dim),
                Span::styled(row, Style::default().fg(color)),
            ]));
        }
        let body_width = width.saturating_sub(2).max(1);
        for line in &presentation.lines {
            for row in wrap(line, body_width) {
                out.push(clipped_line(
                    Line::from(vec![
                        Span::styled("  ".to_string(), dim),
                        Span::styled(row, dim),
                    ]),
                    width.max(1),
                ));
            }
        }
        if let Some(truncation) = presentation.truncation {
            out.push(clipped_line(
                Line::from(Span::styled(format!("  {}", truncation.marker()), dim)),
                width.max(1),
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::{Background, ColorDepth};

    fn theme() -> Theme {
        Theme::new(Background::Dark, ColorDepth::TrueColor)
    }

    #[test]
    fn a_new_block_starts_running() {
        let block = ToolCallBlock::new(CallHint::new("bash", "ls -la"));
        assert_eq!(block.state(), ToolState::Running);
    }

    #[test]
    fn known_tools_resolve_to_their_specific_class_not_generic() {
        let block = ToolCallBlock::new(CallHint::new("bash", "ls"));
        assert_eq!(block.class(), RenderClass::Shell);
    }

    #[test]
    fn an_unknown_tool_renders_as_bounded_generic() {
        let block =
            ToolCallBlock::new(CallHint::new("totally_unregistered_mcp_tool", "do a thing"));
        assert_eq!(block.class(), RenderClass::Generic);
    }

    #[test]
    fn success_and_failure_map_to_distinct_theme_roles() {
        assert_eq!(ToolState::Success.role(), Role::Success);
        assert_eq!(ToolState::Failure.role(), Role::Failure);
        assert_ne!(ToolState::Success.role(), ToolState::Failure.role());
    }

    #[test]
    fn finishing_recolours_the_whole_block_state_to_success_or_failure() {
        let mut block = ToolCallBlock::new(CallHint::new("read", "read foo.rs"));
        block.finish(ResultHint::new("ok"), true);
        assert_eq!(block.state(), ToolState::Success);

        let mut block = ToolCallBlock::new(CallHint::new("read", "read foo.rs"));
        block.finish(ResultHint::new("boom"), false);
        assert_eq!(block.state(), ToolState::Failure);
    }

    #[test]
    fn streaming_fills_the_body_progressively() {
        let mut block = ToolCallBlock::new(CallHint::new("bash", "build"));
        assert!(block.body(&theme()).is_empty());
        block.push_chunk("compiling...\n");
        assert_eq!(block.body(&theme()).len(), 1);
        block.push_chunk("linking...\n");
        assert_eq!(block.body(&theme()).len(), 2);
    }

    #[test]
    fn an_unknown_tools_generic_title_uses_only_the_bare_tool_name() {
        let block = ToolCallBlock::new(CallHint::new("mystery_tool", "arbitrary attacker text"));
        let title = block.title(&theme());
        assert!(title.contains("mystery_tool"));
        assert!(!title.contains("arbitrary attacker text"));
    }

    #[test]
    fn a_hostile_streamed_chunk_cannot_emit_raw_escape_sequences() {
        let mut block = ToolCallBlock::new(CallHint::new("bash", "run"));
        block.push_chunk("\x1b[2J\x1b[H\x1b]0;pwned\x07innocuous text\n");
        for line in &block.streamed {
            assert!(
                !line.contains('\u{1b}'),
                "escape byte leaked into a streamed line: {line:?}"
            );
            assert!(
                !line.contains('\u{7}'),
                "bell byte leaked into a streamed line: {line:?}"
            );
        }
    }

    #[test]
    fn a_generic_tool_cannot_grow_its_streamed_lines_without_bound() {
        let mut block = ToolCallBlock::new(CallHint::new("unregistered_tool", "spam"));
        for i in 0..(GENERIC_MAX_LINES * 5) {
            block.push_chunk(&format!("line {i}\n"));
        }
        assert_eq!(block.streamed.len(), GENERIC_MAX_LINES);
    }

    #[test]
    fn a_known_tool_still_has_a_finite_line_cap_even_though_it_is_generous() {
        let mut block = ToolCallBlock::new(CallHint::new("bash", "spam"));
        for i in 0..(KNOWN_MAX_LINES * 3) {
            block.push_chunk(&format!("line {i}\n"));
        }
        assert_eq!(block.streamed.len(), KNOWN_MAX_LINES);
    }

    #[test]
    fn an_oversized_single_streamed_line_is_truncated_per_line_cap() {
        let mut block = ToolCallBlock::new(CallHint::new("unregistered_tool", "spam"));
        block.push_chunk(&format!("{}\n", "z".repeat(10_000)));
        assert_eq!(block.streamed.len(), 1);
        assert!(block.streamed[0].len() <= GENERIC_LINE_CAP);
    }

    #[test]
    fn desired_height_is_bounded_by_the_available_area() {
        let mut block = ToolCallBlock::new(CallHint::new("bash", "spam"));
        for i in 0..1000 {
            block.push_chunk(&format!("line {i}\n"));
        }
        assert_eq!(Component::desired_height(&block, 80, 10), 10);
    }

    #[test]
    fn compact_and_expanded_are_projections_of_same_bounded_call() {
        let mut block = ToolCallBlock::new(CallHint::new("bash", "run"));
        block.push_chunk("first\nsecond\nthird\n");
        block.finish(ResultHint::new("result\nmore"), true);
        let compact = block.presentation(RenderDensity::Compact);
        let expanded = block.presentation(RenderDensity::Expanded);
        assert_eq!(compact.title, expanded.title);
        assert_eq!(compact.state, expanded.state);
        assert!(compact.lines.len() < expanded.lines.len());
        assert!(
            compact
                .plain_lines()
                .iter()
                .any(|line| line.contains("hidden"))
        );
    }

    #[test]
    fn a_truncated_result_shows_a_marker_naming_the_original_size_and_cap() {
        let mut block = ToolCallBlock::new(CallHint::new("bash", "spam"));
        let raw = (0..500)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        block.finish(ResultHint::new(&raw), true);
        let body = block.body(&theme());
        let last = body
            .last()
            .expect("truncated result must append a marker line");
        let rendered: String = last
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(rendered.contains('…'));
        let truncation = block.result.as_ref().unwrap().truncation.unwrap();
        assert!(
            truncation
                .marker()
                .contains(&truncation.original_len.to_string())
        );
    }
}
