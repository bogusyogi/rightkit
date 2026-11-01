//! Shared single-line row rendering for the composer popups.
//!
//! Lifted from the Codex donor `bottom_pane::{popup_consts,
//! selection_popup_common, picker_rows}` @ 444da310, reduced to the single-line
//! row path the composer popups use. The donor's wrapped/two-column row layout
//! and palette-aware styling live behind the parent's render infrastructure;
//! here selection uses reverse-video and match indices render bold, preserving
//! the donor's windowing, selection, and disabled-row semantics.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Widget;

use super::key_hint::ShortcutHint;
use super::scroll_state::ScrollState;

/// Maximum rows any composer popup shows at once.
pub(crate) const MAX_POPUP_ROWS: usize = 8;

/// Render-ready representation of one row in a selection popup.
///
/// This type contains presentation-focused fields that are intentionally more
/// concrete than source domain models. `match_indices` are character offsets
/// into `name`.
#[derive(Default)]
pub(crate) struct GenericDisplayRow {
    pub name: String,
    /// Optional full-width selection treatment; other rows use the shared text accent.
    pub selection_style: Option<Style>,
    pub name_prefix_spans: Vec<Span<'static>>,
    pub display_shortcut: Option<ShortcutHint>,
    pub match_indices: Option<Vec<usize>>, // indices to bold (char positions)
    pub description: Option<String>,       // optional grey text after the name
    /// Required result identity, retained even when the description column hides.
    pub category_tag: Option<String>,
    pub disabled_reason: Option<String>, // optional disabled message
    pub is_disabled: bool,
}

/// Reports content hidden outside the viewport.
#[derive(Clone, Copy, Default)]
pub(crate) struct RenderedRows {
    pub(crate) has_above: bool,
    /// Includes a description whose final wrapped lines were clipped.
    pub(crate) has_below: bool,
}

/// Selection treatment for picker rows.
///
/// The donor resolves this through the terminal palette; the composer-local
/// seam uses reverse video so selection stays visible on any background until
/// the parent's theme-aware palette lands.
pub(crate) fn selection_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

/// First visible item index that keeps `state.selected_idx` inside a window of
/// `max_items` — verbatim from donor `compute_item_window_start`.
fn compute_item_window_start(
    rows_all: &[GenericDisplayRow],
    state: &ScrollState,
    max_items: usize,
) -> usize {
    if rows_all.is_empty() || max_items == 0 {
        return 0;
    }

    let mut start_idx = state.scroll_top.min(rows_all.len().saturating_sub(1));
    if let Some(sel) = state.selected_idx {
        if sel < start_idx {
            start_idx = sel;
        } else {
            let bottom = start_idx.saturating_add(max_items.saturating_sub(1));
            if sel > bottom {
                start_idx = sel + 1 - max_items;
            }
        }
    }
    start_idx
}

/// Build the display line for one row: prefix spans, name with bolded match
/// indices, optional dimmed description, optional disabled reason.
fn row_line(row: &GenericDisplayRow, width: usize) -> Line<'static> {
    let mut spans = row.name_prefix_spans.clone();
    if let Some(indices) = &row.match_indices {
        let iter = row.name.chars().enumerate().peekable();
        let mut run = String::new();
        let mut run_is_match = indices.contains(&0);
        for (idx, ch) in iter {
            let is_match = indices.contains(&idx);
            if is_match == run_is_match {
                run.push(ch);
            } else {
                spans.push(if run_is_match {
                    Span::styled(std::mem::take(&mut run), Style::default().bold())
                } else {
                    Span::raw(std::mem::take(&mut run))
                });
                run.push(ch);
                run_is_match = is_match;
            }
        }
        if !run.is_empty() {
            spans.push(if run_is_match {
                Span::styled(run, Style::default().bold())
            } else {
                Span::raw(run)
            });
        }
    } else {
        spans.push(row.name.clone().into());
    }

    if row.is_disabled {
        spans.push(" (disabled)".dim());
        if let Some(reason) = &row.disabled_reason {
            spans.push(format!(": {reason}").dim());
        }
    } else if let Some(description) = &row.description {
        spans.push(format!("  {description}").dim());
    } else if let Some(tag) = &row.category_tag {
        spans.push(format!("  {tag}").dim());
    }
    if let Some(shortcut) = &row.display_shortcut {
        spans.push("  ".into());
        spans.push(Span::from(*shortcut));
    }

    // Truncate to the renderable width on display cells.
    let line = Line::from(spans);
    super::line_truncation::truncate_line_with_ellipsis_if_overflow(line, width)
}

/// Render rows in the `[scroll_top, scroll_top + max_results)` window while
/// keeping the selected item visible — the donor's single-line row path
/// (`picker_rows::render_rows_single_line`): when the popup area is tall
/// enough, one row is reserved above and below the list for scroll
/// indicators, and `↑`/`↓` markers report hidden content.
pub(crate) fn render_rows_single_line(
    area: Rect,
    buf: &mut Buffer,
    rows_all: &[GenericDisplayRow],
    state: &ScrollState,
    max_results: usize,
    empty_message: &str,
) -> RenderedRows {
    let inset = u16::from(area.height >= 3);
    let body = Rect {
        x: area.x,
        y: area.y.saturating_add(inset),
        width: area.width,
        height: area.height.saturating_sub(inset.saturating_mul(2)),
    };
    let rendered =
        render_rows_single_line_body(body, buf, rows_all, state, max_results, empty_message);
    render_scroll_indicators(area, buf, rendered);
    rendered
}

/// `picker_style::render_scroll_indicators` reduced to a palette-independent
/// marker: `↑`/`↓` on the reserved rows when content hides above or below.
fn render_scroll_indicators(area: Rect, buf: &mut Buffer, rendered: RenderedRows) {
    if area.height < 3 || area.width == 0 {
        return;
    }
    for (visible, symbol, y) in [
        (rendered.has_above, "↑", area.y),
        (rendered.has_below, "↓", area.bottom().saturating_sub(1)),
    ] {
        if visible {
            Line::from(symbol)
                .bold()
                .render(Rect::new(area.x, y, /*width*/ 1, /*height*/ 1), buf);
        }
    }
}

fn render_rows_single_line_body(
    area: Rect,
    buf: &mut Buffer,
    rows_all: &[GenericDisplayRow],
    state: &ScrollState,
    max_results: usize,
    empty_message: &str,
) -> RenderedRows {
    if area.height == 0 || area.width == 0 {
        return RenderedRows::default();
    }
    if rows_all.is_empty() {
        Line::from(empty_message.dim().italic()).render(area, buf);
        return RenderedRows {
            ..Default::default()
        };
    }

    let max_items = max_results.min(rows_all.len()).min(area.height as usize);
    if max_items == 0 {
        return RenderedRows::default();
    }
    let start_idx = compute_item_window_start(rows_all, state, max_items);

    let mut cur_y = area.y;
    let mut rendered_lines: u16 = 0;
    let mut rendered_items = 0;
    for (i, row) in rows_all.iter().enumerate().skip(start_idx).take(max_items) {
        if cur_y >= area.y + area.height {
            break;
        }
        let mut line = row_line(row, display_width_cap(area));
        let selected = Some(i) == state.selected_idx && !row.is_disabled;
        if selected {
            if let Some(style) = row.selection_style {
                line.style = style;
                for span in line.spans.iter_mut() {
                    span.style = style.patch(span.style).not_dim();
                }
            } else {
                for span in line.spans.iter_mut() {
                    span.style = span.style.bold();
                }
            }
        }
        if row.is_disabled {
            for span in line.spans.iter_mut() {
                span.style = span.style.dim();
            }
        }
        line.render(
            Rect {
                x: area.x,
                y: cur_y,
                width: area.width,
                height: 1,
            },
            buf,
        );
        cur_y = cur_y.saturating_add(1);
        rendered_lines = rendered_lines.saturating_add(1);
        rendered_items += 1;
    }

    RenderedRows {
        has_above: start_idx > 0,
        has_below: start_idx + rendered_items < rows_all.len(),
    }
}

fn display_width_cap(area: Rect) -> usize {
    usize::from(area.width).max(1)
}
