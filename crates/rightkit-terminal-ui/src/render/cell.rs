//! History cells: the line-producing projections a transcript renders.
//!
//! A cell is *not* a [`crate::component::Component`]: it produces
//! `Vec<Line<'static>>` at a width for a transcript/history projection to
//! place, rather than drawing into a `Frame` itself. This mirrors the codex
//! donor's `HistoryCell` split between display (compact scrollback),
//! transcript (expanded), and raw (plain-text dump) renderings
//! (`codex-rs/tui/src/history_cell/base.rs`,
//! `codex-rs/tui/src/render/line_utils.rs` at
//! `444da310e108da16aaeb18fd790b0ac464f08aca`).
//!
//! Two invariants hold for every cell in this module:
//!
//! - **Untrusted text is sanitized at construction.** `UserPromptCell`,
//!   `AssistantCell`, `ReasoningCell`, and every cell that stores external
//!   strings routes them through [`super::sanitize::sanitize`] before
//!   storing. No cell renders bytes that can move the cursor, clear the
//!   screen, or spoof UI chrome.
//! - **Wrapping is width-aware.** Cells wrap with
//!   [`crate::transcript::wrap`]'s grapheme/width tables, so a wrapped line's
//!   display width matches the column it is drawn into on any terminal.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::component::{Role, Theme};
use crate::transcript::wrap::{display_width, wrap};

use super::sanitize::sanitize;

/// A history-cell projection: plain display lines at a given width, plus the
/// expanded transcript and plain-text renderings.
///
/// Implementations are expected to hold sanitized, bounded data only.
/// `Send + Sync` lets a settled cell live inside a [`crate::transcript`]
/// entry that the host may move across threads.
pub trait Cell: Send + Sync {
    /// Lines for the live scrollback display at `width` columns.
    fn display_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>>;

    /// Lines for the expanded transcript (defaults to the display
    /// projection). Cells that show a bounded preview on screen override
    /// this to expose their full retained content.
    fn transcript_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        self.display_lines(width, theme)
    }

    /// Unstyled text lines for a plain-text dump or copy.
    fn plain_lines(&self) -> Vec<Line<'static>> {
        self.display_lines(
            u16::MAX,
            &Theme::new(
                crate::terminal::Background::Dark,
                crate::terminal::ColorDepth::Monochrome,
            ),
        )
    }

    /// Rows this cell wants at `width`; used for viewport sizing.
    fn desired_height(&self, width: u16, theme: &Theme) -> u16 {
        self.display_lines(width, theme).len() as u16
    }

    /// True while this cell is a continuation fragment of a still-streaming
    /// entry (matched against the entry above it during consolidation).
    fn is_stream_continuation(&self) -> bool {
        false
    }
}

/// The style pair a span resolves to once at render time. Cells hold
/// [`Role`]s, never literal colors, so they can reproject on theme changes.
fn dim(theme: &Theme) -> Style {
    Style::default().fg(theme.color(Role::Dim))
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

/// Wrap a styled `Line` to `width` columns, preserving span styles.
///
/// Donor lineage: `codex-rs/tui/src/history_cell/base.rs`'s
/// `PrefixedWrappedHistoryCell` + `wrapping::adaptive_wrap_line` behavior —
/// word-boundary wrap with a hard grapheme break for words wider than the
/// column budget. Implemented over `crate::transcript::wrap::wrap` (this
/// crate's width-aware wrapper) on the flattened text, then re-styled per
/// span range, so wide glyphs never overshoot the column.
fn wrap_line(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    // Flatten to plain text, wrap, then re-slice each wrapped row back into
    // per-span styled fragments so the wrap is display-width exact but the
    // styling follows the original span boundaries.
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let wrapped = wrap(&text, width.min(u16::MAX as usize) as u16);

    // Byte offsets in `text` where each span starts/ends.
    let mut spans: Vec<(usize, usize, Style)> = Vec::with_capacity(line.spans.len());
    let mut cursor = 0usize;
    for span in &line.spans {
        let start = cursor;
        cursor += span.content.len();
        spans.push((start, cursor, span.style));
    }

    // `wrap` may drop whitespace it introduces at a break, so row positions
    // in `text` are located by ordered search rather than cumulative length.
    let mut out: Vec<Line<'static>> = Vec::with_capacity(wrapped.len());
    let mut cursor_pos = 0usize; // first unclaimed byte offset in `text`
    for row in wrapped {
        let row_start = if row.is_empty() {
            cursor_pos
        } else {
            text[cursor_pos..]
                .find(row.as_str())
                .map(|offset| cursor_pos + offset)
                .unwrap_or(cursor_pos)
        };
        let row_end = row_start + row.len();
        let mut row_spans: Vec<Span<'static>> = Vec::new();
        for (start, end, style) in &spans {
            let s = (*start).max(row_start);
            let e = (*end).min(row_end);
            if s < e {
                row_spans.push(Span::styled(text[s..e].to_string(), *style));
            }
        }
        if row_spans.is_empty() {
            row_spans.push(Span::raw(String::new()));
        }
        out.push(Line::from(row_spans));
        cursor_pos = row_end;
    }
    out
}

/// Clip a line to `width` columns, appending `…` when it does not fit.
///
/// Donor lineage: `line_truncation::truncate_line_with_ellipsis_if_overflow`
/// and `history_cell::activity_preview::clipped_line` — clip at the display
/// boundary so a visible row never exceeds the column budget.
pub fn clipped_line(line: Line<'static>, width: u16) -> Line<'static> {
    crate::composer::line_truncation::truncate_line_with_ellipsis_if_overflow(
        line,
        usize::from(width.max(1)),
    )
}

/// Prefix each line of `lines` — `initial_prefix` on the first line,
/// `subsequent_prefix` on every line after — without changing their styles.
///
/// Donor lineage: `render::line_utils::prefix_lines` in codex
/// (`codex-rs/tui/src/render/line_utils.rs`); same shape, same precedence.
pub fn prefix_lines(
    lines: Vec<Line<'static>>,
    initial_prefix: Span<'static>,
    subsequent_prefix: Span<'static>,
) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .enumerate()
        .map(|(i, mut line)| {
            line.spans.insert(
                0,
                if i == 0 {
                    initial_prefix.clone()
                } else {
                    subsequent_prefix.clone()
                },
            );
            line
        })
        .collect()
}

/// A cell that owns fully-formed lines. Used for static headings, blank
/// separators, and content rendered by a caller that already respects the
/// display width. `PlainCell` is still bounded: its lines are stored as
/// given, so callers must have sanitized any untrusted text themselves.
#[derive(Debug)]
pub struct PlainCell {
    lines: Vec<Line<'static>>,
}

impl PlainCell {
    pub fn new(lines: Vec<Line<'static>>) -> Self {
        Self { lines }
    }
}

impl Cell for PlainCell {
    fn display_lines(&self, _width: u16, _theme: &Theme) -> Vec<Line<'static>> {
        self.lines.clone()
    }
}

/// A single logical line wrapped under a gutter: `initial_prefix` on the
/// first wrapped row, `subsequent_prefix` on the rest (the codex
/// `PrefixedWrappedHistoryCell` shape — e.g. `✔ ` + `"  "` hanging indent).
#[derive(Debug)]
pub struct PrefixedWrappedCell {
    line: Line<'static>,
    initial_prefix: Span<'static>,
    subsequent_prefix: Span<'static>,
}

impl PrefixedWrappedCell {
    pub fn new(
        line: Line<'static>,
        initial_prefix: Span<'static>,
        subsequent_prefix: Span<'static>,
    ) -> Self {
        Self {
            line,
            initial_prefix,
            subsequent_prefix,
        }
    }
}

impl Cell for PrefixedWrappedCell {
    fn display_lines(&self, width: u16, _theme: &Theme) -> Vec<Line<'static>> {
        let initial_w = display_width(&self.initial_prefix.content);
        let subsequent_w = display_width(&self.subsequent_prefix.content);
        let prefix_w = initial_w.max(subsequent_w);
        let wrap_width = (width as usize).saturating_sub(prefix_w).max(1);
        let wrapped = wrap_line(&self.line, wrap_width);
        wrapped
            .into_iter()
            .enumerate()
            .map(|(i, mut line)| {
                line.spans.insert(
                    0,
                    if i == 0 {
                        self.initial_prefix.clone()
                    } else {
                        self.subsequent_prefix.clone()
                    },
                );
                line
            })
            .collect()
    }

    fn plain_lines(&self) -> Vec<Line<'static>> {
        let mut lines = self.display_lines(
            u16::MAX,
            &Theme::new(
                crate::terminal::Background::Dark,
                crate::terminal::ColorDepth::Monochrome,
            ),
        );
        for line in &mut lines {
            for span in &mut line.spans {
                span.style = Style::default();
            }
        }
        lines
    }
}

/// A cell made of other cells, concatenated in order — the codex
/// `CompositeHistoryCell` shape used to pair a prompt echo with its output.
#[derive(Default)]
pub struct CompositeCell {
    cells: Vec<Box<dyn Cell>>,
}

impl CompositeCell {
    pub fn new(cells: Vec<Box<dyn Cell>>) -> Self {
        Self { cells }
    }
}

impl Cell for CompositeCell {
    fn display_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for cell in &self.cells {
            out.extend(cell.display_lines(width, theme));
        }
        out
    }

    fn transcript_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for cell in &self.cells {
            out.extend(cell.transcript_lines(width, theme));
        }
        out
    }
}

/// A user turn: a `› ` gutter before each wrapped line and blank padding
/// above and below so the prompt reads as a distinct unit in scrollback.
///
/// Donor lineage: codex `UserHistoryCell::display_lines`
/// (`history_cell/messages.rs`) — same `›` marker, same surrounding blank
/// line, with the sanitization moved to construction via
/// `sanitize_user_text` (there) / `sanitize` (here).
#[derive(Debug)]
pub struct UserPromptCell {
    message: String,
}

impl UserPromptCell {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: sanitize(&message.into()),
        }
    }
}

impl Cell for UserPromptCell {
    fn display_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        let gutter = "› ";
        let gutter_w = display_width(gutter);
        let wrap_width = (width as usize).saturating_sub(gutter_w).max(1);
        let message = self.message.trim_end_matches(['\r', '\n']);

        let mut out: Vec<Line<'static>> = Vec::new();
        out.push(Line::from(""));
        if !message.is_empty() {
            let marker = Span::styled(gutter.to_string(), bold());
            let indent = Span::raw("  ");
            for (i, logical) in message.split('\n').enumerate() {
                for (j, wrapped) in wrap(logical, wrap_width as u16).into_iter().enumerate() {
                    let mut spans: Vec<Span<'static>> = Vec::new();
                    if i == 0 && j == 0 {
                        spans.push(marker.clone());
                    } else {
                        spans.push(indent.clone());
                    }
                    spans.push(Span::styled(wrapped, Style::default()));
                    out.push(Line::from(spans));
                }
            }
        }
        out.push(Line::from(""));
        // Quiet blank padding so the turn separates from surrounding rows.
        for line in &mut out {
            if line.spans.is_empty() {
                line.spans.push(Span::styled(String::new(), dim(theme)));
            }
        }
        out
    }
}

/// One assistant text block: `• ` on the first line of the entry, `  ` on
/// the rest — the codex `AgentMessageCell` gutter convention.
#[derive(Debug)]
pub struct AssistantCell {
    text: String,
    /// False while the cell continues the block above it during streaming.
    is_first_line: bool,
}

impl AssistantCell {
    pub fn new(text: impl Into<String>, is_first_line: bool) -> Self {
        Self {
            text: sanitize(&text.into()),
            is_first_line,
        }
    }
}

impl Cell for AssistantCell {
    fn display_lines(&self, width: u16, _theme: &Theme) -> Vec<Line<'static>> {
        let gutter_w = 2usize; // "• " or "  "
        let wrap_width = (width as usize).saturating_sub(gutter_w).max(1);
        let mut out = Vec::new();
        let mut first_row = true;
        for logical in self.text.split('\n') {
            for wrapped in wrap(logical, wrap_width as u16) {
                let marker = if first_row && self.is_first_line {
                    "• "
                } else {
                    "  "
                };
                first_row = false;
                out.push(Line::from(vec![
                    Span::styled(
                        marker.to_string(),
                        Style::default().add_modifier(Modifier::DIM),
                    ),
                    Span::raw(wrapped),
                ]));
            }
        }
        out
    }

    fn is_stream_continuation(&self) -> bool {
        !self.is_first_line
    }
}

/// A reasoning/thinking block. Settled, it is one dim line,
/// `• Thought: <heading> · ctrl+o expand`; the expanded transcript renders the
/// full text through the markdown renderer (headings as real paragraphs, not
/// raw `**bold**`). `streaming` is the in-flight variant (`• Thinking: …`).
#[derive(Debug)]
pub struct ReasoningCell {
    text: String,
    transcript_only: bool,
    streaming: bool,
}

impl ReasoningCell {
    pub fn new(text: impl Into<String>, transcript_only: bool) -> Self {
        Self {
            text: sanitize(&text.into()),
            transcript_only,
            streaming: false,
        }
    }

    /// The still-open block: same one line, but `Thinking` and no expand hint.
    pub fn streaming(text: impl Into<String>) -> Self {
        Self {
            text: sanitize(&text.into()),
            transcript_only: false,
            streaming: true,
        }
    }

    /// Short label: the first `**heading**` (or, failing that, the first
    /// non-empty line) with markdown markers removed.
    pub fn heading(text: &str) -> String {
        let text = normalize_reasoning(text);
        let raw = text
            .split("**")
            .nth(1)
            .map(str::trim)
            .filter(|h| !h.is_empty() && !h.contains('\n') && text.trim_start().starts_with("**"))
            .map(str::to_string)
            .unwrap_or_else(|| {
                text.lines()
                    .map(|l| l.trim())
                    .find(|l| !l.is_empty())
                    .unwrap_or("")
                    .to_string()
            });
        raw.replace("**", "")
    }
}

/// Reasoning summaries arrive as `**Heading**` sections that the provider
/// concatenates with no separator (`...content.**Exploring**`). Open a
/// paragraph before each heading glued to preceding text, and one after a
/// heading that runs straight into its body.
pub fn normalize_reasoning(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut rest = text;
    while let Some(start) = rest.find("**") {
        let after = &rest[start + 2..];
        let heading_end = after
            .find("**")
            .filter(|end| *end > 0 && *end <= 120 && !after[..*end].contains('\n'));
        let Some(end) = heading_end else {
            out.push_str(&rest[..start + 2]);
            rest = after;
            continue;
        };
        let before = &rest[..start];
        let prev = before.chars().last().or_else(|| out.chars().last());
        let line_start = matches!(prev, None | Some('\n'));
        // A heading opens at line start or glued to a sentence end; inline
        // bold in the middle of prose (preceded by a space) is left alone.
        let sentence_end = matches!(prev, Some('.' | '!' | '?' | ':' | ')' | '"'));
        if !(line_start || sentence_end) {
            out.push_str(&rest[..start + 2 + end + 2]);
            rest = &after[end + 2..];
            continue;
        }
        out.push_str(before);
        if !line_start {
            out.push_str("\n\n");
        } else if !out.is_empty() && !out.ends_with("\n\n") {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        out.push_str(&rest[start..start + 2 + end + 2]);
        let tail = &after[end + 2..];
        if tail.starts_with('\n') || tail.is_empty() {
            rest = tail;
        } else {
            out.push_str("\n\n");
            rest = tail.trim_start_matches(' ');
        }
    }
    out.push_str(rest);
    out
}

impl Cell for ReasoningCell {
    fn display_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        if self.transcript_only {
            return Vec::new();
        }
        self.collapsed(width, theme)
    }

    fn transcript_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        if self.streaming {
            return self.collapsed(width, theme);
        }
        self.expanded(width, theme)
    }
}

impl ReasoningCell {
    fn collapsed(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        let style = dim(theme);
        let label = if self.streaming {
            "Thinking"
        } else {
            "Thought"
        };
        let heading = Self::heading(&self.text);
        let mut text = if heading.is_empty() {
            label.to_string()
        } else {
            format!("{label}: {heading}")
        };
        if !self.streaming {
            text.push_str(" \u{b7} ctrl+o expand");
        }
        let budget = (width as usize).saturating_sub(2).max(1);
        if display_width(&text) > budget {
            let mut cut = String::new();
            for ch in text.chars() {
                let mut buf = [0u8; 4];
                if display_width(&cut) + display_width(ch.encode_utf8(&mut buf)) + 1 > budget {
                    break;
                }
                cut.push(ch);
            }
            cut.push('\u{2026}');
            text = cut;
        }
        vec![Line::from(vec![
            Span::styled("\u{2022} ".to_string(), style),
            Span::styled(text, style),
        ])]
    }

    fn expanded(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        let dim_fg = Style::default().fg(theme.color(Role::Dim));
        let inner = (width as usize).saturating_sub(2).max(1);
        let normalized = normalize_reasoning(&self.text);
        let body = crate::transcript::markdown_render::render_markdown_lines_with_width_and_cwd(
            &normalized,
            Some(inner),
            None,
        );
        let mut out = Vec::new();
        for (index, rendered) in body.into_iter().enumerate() {
            let mut line = rendered.into_line();
            for span in &mut line.spans {
                span.style = span.style.patch(dim_fg);
            }
            let marker = if index == 0 { "\u{2022} " } else { "  " };
            line.spans
                .insert(0, Span::styled(marker.to_string(), dim_fg));
            out.push(line);
        }
        out
    }
}

/// A one-line dim notice/status row, wrapped under a `• `/`  ` gutter — the
/// shared shape codex `history_cell` uses for system notices, warnings, and
/// review-status lines.
#[derive(Debug)]
pub struct NoticeCell {
    text: String,
    role: Role,
}

impl NoticeCell {
    pub fn new(text: impl Into<String>, role: Role) -> Self {
        Self {
            text: sanitize(&text.into()),
            role,
        }
    }
}

impl Cell for NoticeCell {
    fn display_lines(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        let color = theme.color(self.role);
        let wrap_width = (width as usize).saturating_sub(2).max(1);
        let mut out = Vec::new();
        let mut first = true;
        for logical in self.text.split('\n') {
            for wrapped in wrap(logical, wrap_width as u16) {
                let marker = if first { "• " } else { "  " };
                first = false;
                out.push(Line::from(vec![
                    Span::styled(marker.to_string(), Style::default().fg(color)),
                    Span::styled(wrapped, Style::default().fg(color)),
                ]));
            }
        }
        out
    }
}
