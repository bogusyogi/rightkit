//! Pie charts: `pie [showData] [title <text>]` followed by `"label" : value` rows, drawn as one
//! labelled bar per slice with its share to one decimal (B-mermaidpie). Local to this crate; the
//! donor engine has no pie family.
//!
//! Supported: the `pie` header with `showData` and an inline or following `title` line, quoted
//! labels, non-negative decimal values. Everything else (a negative or non-numeric value, an
//! unquoted label, a `style` or `click` line, a total of zero) is `Unsupported`; more than
//! [`MAX_SLICES`] slices is a `Limit`, and a chart wider than the caller's width is `TooWide`.

use super::output::{Cell, finish};
use super::{MAX_LABEL, RenderError};

const MAX_SLICES: usize = 12;
const BAR_WIDTH: usize = 24;
const MIN_BAR_WIDTH: usize = 4;

struct Slice {
    label: String,
    value: f64,
    shown: String,
}

fn safe_text(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| !c.is_control())
}

/// `"Dogs" : 75` -> (`Dogs`, `75`).
fn parse_row(line: &str) -> Result<Slice, RenderError> {
    let rest = line.strip_prefix('"').ok_or(RenderError::Unsupported)?;
    let (label, after) = rest.split_once('"').ok_or(RenderError::Unsupported)?;
    let value_text = after
        .trim_start()
        .strip_prefix(':')
        .ok_or(RenderError::Unsupported)?
        .trim();
    if !safe_text(label) {
        return Err(RenderError::Unsupported);
    }
    if label.chars().count() > MAX_LABEL {
        return Err(RenderError::Limit);
    }
    // Digits with an optional single point: no sign, exponent, NaN or infinity.
    let digits_ok = !value_text.is_empty()
        && value_text.chars().all(|c| c.is_ascii_digit() || c == '.')
        && value_text.matches('.').count() <= 1
        && value_text.chars().any(|c| c.is_ascii_digit());
    if !digits_ok {
        return Err(RenderError::Unsupported);
    }
    let value: f64 = value_text.parse().map_err(|_| RenderError::Unsupported)?;
    if !value.is_finite() {
        return Err(RenderError::Unsupported);
    }
    Ok(Slice {
        label: label.to_owned(),
        value,
        shown: value_text.to_owned(),
    })
}

pub(super) fn render(
    header: &str,
    body: &[&str],
    max_width: usize,
) -> Result<Vec<Vec<super::Span>>, RenderError> {
    // Header: `pie`, then any of `showData` and `title <text>`.
    let mut rest = header
        .strip_prefix("pie")
        .ok_or(RenderError::Unsupported)?
        .trim();
    let mut show_data = false;
    let mut title = String::new();
    if let Some(after) = rest.strip_prefix("showData")
        && (after.is_empty() || after.starts_with(char::is_whitespace))
    {
        show_data = true;
        rest = after.trim();
    }
    if let Some(text) = rest.strip_prefix("title")
        && (text.is_empty() || text.starts_with(char::is_whitespace))
    {
        title = text.trim().to_owned();
        rest = "";
    }
    if !rest.is_empty() {
        return Err(RenderError::Unsupported);
    }
    let mut slices: Vec<Slice> = Vec::new();
    for line in body {
        if let Some(text) = line.strip_prefix("title")
            && (text.is_empty() || text.starts_with(char::is_whitespace))
        {
            title = text.trim().to_owned();
            continue;
        }
        if slices.len() == MAX_SLICES {
            return Err(RenderError::Limit);
        }
        slices.push(parse_row(line)?);
    }
    if !title.is_empty() && (!safe_text(&title) || title.chars().count() > 4 * MAX_LABEL) {
        return Err(RenderError::Unsupported);
    }
    if slices.is_empty() {
        return Err(RenderError::Unsupported);
    }
    let total: f64 = slices.iter().map(|slice| slice.value).sum();
    if total <= 0.0 {
        return Err(RenderError::Unsupported);
    }

    let label_width = slices
        .iter()
        .map(|s| s.label.chars().count())
        .max()
        .unwrap_or(0);
    let share_text = |slice: &Slice| format!("{:.1}%", slice.value * 100.0 / total);
    let tail_width = |slice: &Slice| {
        let share = share_text(slice).chars().count();
        if show_data {
            share + 3 + slice.shown.chars().count()
        } else {
            share
        }
    };
    let widest_tail = slices.iter().map(tail_width).max().unwrap_or(0);
    // label, space, bar, space, share (and the value in brackets).
    let fixed = label_width + 1 + 1 + widest_tail;
    let bar_width = BAR_WIDTH.min(max_width.saturating_sub(fixed));
    if bar_width < MIN_BAR_WIDTH {
        return Err(RenderError::TooWide);
    }

    let mut rows: Vec<Vec<Cell>> = Vec::new();
    let text_cell = |symbol: char| Cell {
        symbol,
        role: super::Role::Text,
    };
    if !title.is_empty() {
        rows.push(title.chars().map(text_cell).collect());
    }
    for slice in &slices {
        let mut row: Vec<Cell> = Vec::new();
        row.extend(slice.label.chars().map(text_cell));
        row.extend(std::iter::repeat_n(
            text_cell(' '),
            label_width - slice.label.chars().count() + 1,
        ));
        let filled = ((slice.value / total) * bar_width as f64).round() as usize;
        let filled = filled.min(bar_width);
        row.extend(std::iter::repeat_n(Cell::node('█'), filled));
        row.extend(std::iter::repeat_n(Cell::edge('░'), bar_width - filled));
        row.push(text_cell(' '));
        row.extend(share_text(slice).chars().map(text_cell));
        if show_data {
            row.extend(format!(" ({})", slice.shown).chars().map(text_cell));
        }
        rows.push(row);
    }
    Ok(finish(rows))
}
