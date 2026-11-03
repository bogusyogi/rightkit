//! Patch presentation: file-change summaries and bounded diff previews.
//!
//! Donor lineage: `codex-rs/tui/src/diff_render.rs` and the
//! `history_cell/patches.rs` `PatchHistoryCell` at
//! `444da310e108da16aaeb18fd790b0ac464f08aca`, adapted to this crate's
//! boundaries:
//!
//! - **Donor parser retained.** Unified diffs use Codex's `diffy::Patch`
//!   validation; malformed hunks never produce invented line numbers.
//! - **Semantic roles, not literal colours.** Additions resolve through
//!   [`Role::Success`], deletions through [`Role::Failure`]; the gutter uses
//!   [`Role::Dim`]. The donor's tinted diff backgrounds apply only when the
//!   terminal supports indexed/true colour (`theme.depth() >= Ansi256`) so
//!   16-colour and monochrome terminals still get foreground-only diffs.
//! - **Sanitizer boundary preserved.** File paths and diff content pass
//!   through [`super::sanitize::sanitize`] before they are stored or drawn.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use unicode_segmentation::UnicodeSegmentation;

use crate::component::{Role, Theme};
use crate::terminal::{Background, ColorDepth};
use crate::transcript::wrap::display_width;

use super::sanitize::sanitize;

/// Preview cap for collapsed views — codex
/// `history_cell/activity_preview.rs`'s `DETAIL_PREVIEW_LINES`.
pub const DETAIL_PREVIEW_LINES: usize = 3;

/// A change to one file. `Add`/`Delete` carry whole-file content; `Update`
/// carries a unified diff. All content is sanitized on construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChange {
    Add { content: String },
    Delete { content: String },
    Update { unified_diff: String },
}

impl FileChange {
    pub fn add(content: impl Into<String>) -> Self {
        Self::Add {
            content: sanitize(&content.into()),
        }
    }

    pub fn delete(content: impl Into<String>) -> Self {
        Self::Delete {
            content: sanitize(&content.into()),
        }
    }

    pub fn update(unified_diff: impl Into<String>) -> Self {
        Self::Update {
            unified_diff: sanitize(&unified_diff.into()),
        }
    }

    /// An update from the whole text before and after, with real line numbers.
    pub fn replacement(before: &str, after: &str) -> Self {
        Self::update(diffy::create_patch(before, after).to_string())
    }

    /// An update from exact-substring replacements when the file itself is
    /// not at hand: one hunk per pair, numbered from line 1, showing exactly
    /// the lines that go away and the lines that replace them.
    pub fn from_replacements(pairs: &[(String, String)]) -> Self {
        let mut diff = String::from("--- original\n+++ modified\n");
        for (old, new) in pairs {
            let old_lines: Vec<&str> = old.lines().collect();
            let new_lines: Vec<&str> = new.lines().collect();
            diff.push_str(&format!(
                "@@ -1,{} +1,{} @@\n",
                old_lines.len(),
                new_lines.len()
            ));
            for line in &old_lines {
                diff.push_str(&format!("-{line}\n"));
            }
            for line in &new_lines {
                diff.push_str(&format!("+{line}\n"));
            }
        }
        Self::update(diff)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffLineKind {
    Context,
    Insert,
    Delete,
}

#[derive(Debug, Clone)]
struct DiffBodyLine {
    kind: DiffLineKind,
    text: String,
}

#[derive(Debug)]
struct Hunk {
    old_start: usize,
    new_start: usize,
    lines: Vec<DiffBodyLine>,
}

/// Codex `diff_render`'s validated patch/hunk projection. the host app may
/// supply a hunk-only preview; synthetic file headers add no file authority.
fn parse_unified_diff(diff: &str) -> Vec<Hunk> {
    let normalized = if diff.starts_with("@@") {
        format!("--- a/preview\n+++ b/preview\n{diff}")
    } else {
        diff.to_owned()
    };
    let Ok(patch) = diffy::Patch::from_str(&normalized) else {
        return Vec::new();
    };
    patch
        .hunks()
        .iter()
        .map(|hunk| Hunk {
            old_start: hunk.old_range().start(),
            new_start: hunk.new_range().start(),
            lines: hunk
                .lines()
                .iter()
                .map(|line| {
                    let (kind, text) = match line {
                        diffy::Line::Context(text) => (DiffLineKind::Context, text),
                        diffy::Line::Insert(text) => (DiffLineKind::Insert, text),
                        diffy::Line::Delete(text) => (DiffLineKind::Delete, text),
                    };
                    DiffBodyLine {
                        kind,
                        text: sanitize(text.trim_end_matches('\n')),
                    }
                })
                .collect(),
        })
        .collect()
}

/// Donor lineage: `diff_render::calculate_add_remove_from_diff`.
/// Counts `+`/`-` body lines, skipping `+++`/`---` file headers.
fn count_add_remove(diff: &str) -> (usize, usize) {
    parse_unified_diff(diff)
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .fold((0, 0), |(adds, dels), line| match line.kind {
            DiffLineKind::Insert => (adds + 1, dels),
            DiffLineKind::Delete => (adds, dels + 1),
            DiffLineKind::Context => (adds, dels),
        })
}

/// Diff-tinted backgrounds from the donor's fallback palettes
/// (`diff_render::create_diff_summary`'s `DEFAULT_LIGHT`/`DEFAULT_DARK`
/// tail). Applied only when the terminal reports at least 256-colour depth;
/// below that the sign and role foreground carry the meaning.
fn diff_bg(kind: DiffLineKind, theme: &Theme) -> Option<Color> {
    if theme.depth() < ColorDepth::Ansi256 {
        return None;
    }
    let rgb = match (kind, theme.background()) {
        (DiffLineKind::Insert, Background::Dark) => (33, 58, 43),
        (DiffLineKind::Delete, Background::Dark) => (74, 34, 29),
        (DiffLineKind::Insert, Background::Light) => (232, 245, 233),
        (DiffLineKind::Delete, Background::Light) => (253, 235, 235),
        (DiffLineKind::Context, _) => return None,
    };
    Some(match theme.depth() {
        ColorDepth::TrueColor => Color::Rgb(rgb.0, rgb.1, rgb.2),
        _ => Color::Indexed(crate::terminal::capability::rgb_to_xterm256(
            rgb.0, rgb.1, rgb.2,
        )),
    })
}

fn gutter_bg(theme: &Theme) -> Option<Color> {
    if theme.depth() < ColorDepth::Ansi256 {
        return None;
    }
    let rgb = match theme.background() {
        Background::Dark => (44, 50, 58),
        Background::Light => (240, 241, 243),
    };
    Some(match theme.depth() {
        ColorDepth::TrueColor => Color::Rgb(rgb.0, rgb.1, rgb.2),
        _ => Color::Indexed(crate::terminal::capability::rgb_to_xterm256(
            rgb.0, rgb.1, rgb.2,
        )),
    })
}

fn gutter_fg(theme: &Theme) -> Style {
    Style::default().fg(theme.color(Role::Dim))
}

/// Display path: relativized to `cwd` when possible, sanitized always.
/// Windows separators are preserved as-is (this lane is Windows-native);
/// the sanitize pass is what keeps a hostile path from reaching the screen.
fn display_path(path: &Path, cwd: &Path) -> String {
    let shown = path
        .strip_prefix(cwd)
        .map(|rel| rel.display().to_string())
        .unwrap_or_else(|_| path.display().to_string());
    sanitize(&shown)
}

/// Wrap styled spans to `width` display columns.
///
/// Donor lineage: `diff_render::wrap_styled_spans` — greedily pack
/// graphemes into lines while carrying each span's style, hard-breaking
/// over-wide words rather than letting the terminal wrap desynchronize row
/// accounting. Newline inside a span forces a break.
fn wrap_styled_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Vec<Span<'static>>> {
    let width = width.max(1);
    let mut lines: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0usize;

    for span in spans {
        // Iterate graphemes so wide chars and clusters move atomically.
        let mut piece = String::new();
        let flush_piece =
            |piece: &mut String, style: Style, lines: &mut Vec<Vec<Span<'static>>>| {
                if !piece.is_empty() {
                    lines
                        .last_mut()
                        .expect("lines is never empty")
                        .push(Span::styled(std::mem::take(piece), style));
                }
            };
        for grapheme in span.content.graphemes(true) {
            if grapheme == "\n" {
                flush_piece(&mut piece, span.style, &mut lines);
                lines.push(Vec::new());
                used = 0;
                continue;
            }
            let w = display_width(grapheme);
            if used + w > width && used > 0 {
                flush_piece(&mut piece, span.style, &mut lines);
                lines.push(Vec::new());
                used = 0;
            }
            piece.push_str(grapheme);
            used += w;
        }
        flush_piece(&mut piece, span.style, &mut lines);
    }
    lines
}

/// Render one diff body line at `width` with optional wrapping — sign
/// gutter, old/new line numbers, styled content. Donor lineage:
/// `diff_render::render_wrapped_diff_line`.
fn render_diff_line(
    line: &DiffBodyLine,
    ln_old: usize,
    ln_new: usize,
    line_number_width: usize,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let sign = match line.kind {
        DiffLineKind::Context => ' ',
        DiffLineKind::Insert => '+',
        DiffLineKind::Delete => '-',
    };

    // Skip file-header lines like ordinary diffs do; the caller's parser
    // never produces them, but a literal "---"/"+++" content line in an
    // Add/Delete body is still a body line, not a header — no skipping here.

    let mut gutter_spans: Vec<Span<'static>> = Vec::new();
    let gutter_style = match gutter_bg(theme) {
        Some(bg) => gutter_fg(theme).bg(bg),
        None => gutter_fg(theme),
    };

    let mut old_number_str = String::new();
    let mut new_number_str = String::new();
    match line.kind {
        DiffLineKind::Insert => new_number_str = format!("{ln_new:>line_number_width$}"),
        DiffLineKind::Delete => old_number_str = format!("{ln_old:>line_number_width$}"),
        DiffLineKind::Context => {
            old_number_str = format!("{ln_old:>line_number_width$}");
            new_number_str = format!("{ln_new:>line_number_width$}");
        }
    }

    gutter_spans.push(Span::styled(format!("{sign} "), gutter_style));
    gutter_spans.push(Span::styled(format!("{old_number_str} "), gutter_style));
    gutter_spans.push(Span::styled(format!("{new_number_str} "), gutter_style));

    let fg = match line.kind {
        DiffLineKind::Insert => theme.color(Role::Success),
        DiffLineKind::Delete => theme.color(Role::Failure),
        DiffLineKind::Context => theme.color(Role::Text),
    };
    let mut content_style = Style::default().fg(fg);
    if let Some(bg) = diff_bg(line.kind, theme) {
        content_style = content_style.bg(bg);
        for span in &mut gutter_spans {
            span.style = span.style.bg(bg);
        }
    }
    let content_span = Span::styled(line.text.clone(), content_style);

    if width <= 4 {
        return vec![Line::from(Span::styled(sign.to_string(), content_style))];
    }

    let mut all_spans = gutter_spans;
    let gutter_width: usize = all_spans.iter().map(|s| display_width(&s.content)).sum();
    all_spans.push(content_span);

    let wrap_width = width.saturating_sub(gutter_width).max(1);
    let wrapped = wrap_styled_spans(vec![all_spans.pop().expect("content span")], wrap_width);
    // Reattach the gutter to the first wrapped row; continuation rows get a
    // matching blank gutter so columns stay aligned.
    let blank_gutter: Vec<Span<'static>> = vec![
        Span::styled("  ".to_string(), gutter_style),
        Span::styled(
            format!("{:>width$} ", "", width = line_number_width),
            gutter_style,
        ),
        Span::styled(
            format!("{:>width$} ", "", width = line_number_width),
            gutter_style,
        ),
    ];

    let gutter_line_spans = all_spans; // sign + numbers
    let mut out: Vec<Line<'static>> = Vec::new();
    for (i, content_row) in wrapped.into_iter().enumerate() {
        let mut spans = if i == 0 {
            gutter_line_spans.clone()
        } else {
            blank_gutter.clone()
        };
        spans.extend(content_row);
        out.push(Line::from(spans));
    }
    out
}

/// Shared summary/preview body, parameterized by the line cap.
/// Donor lineage: `create_diff_summary_with_links` (hyperlink extraction
/// removed — the host app emits no OSC-8 from untrusted content).
fn create_diff_lines(
    changes: &BTreeMap<PathBuf, FileChange>,
    cwd: &Path,
    width: usize,
    max_lines: Option<usize>,
    theme: &Theme,
) -> Vec<Line<'static>> {
    // `remaining` starts at usize::MAX for the uncapped summary.
    let mut remaining = max_lines.unwrap_or(usize::MAX);
    let mut rendered: Vec<Line<'static>> = Vec::new();
    rendered.push(Line::from(vec![
        Span::styled("•".to_string(), dim_style(theme)),
        Span::raw(" "),
        Span::styled(
            "Proposed Diff".to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
    ]));
    rendered.push(Line::from(""));

    let block_indent = "    ";
    let mut changed_files: Vec<(String, PathBuf, FileChange, usize, usize)> = Vec::new();
    let mut max_change_width = 0usize;
    for (path, change) in changes.iter() {
        let (adds, dels) = match change {
            FileChange::Add { content } => (content.lines().count(), 0),
            FileChange::Delete { content } => (0, content.lines().count()),
            FileChange::Update { unified_diff } => count_add_remove(unified_diff),
        };
        let change_text = format!("(+{adds} -{dels})");
        max_change_width = max_change_width.max(display_width(&change_text));
        changed_files.push((change_text, path.clone(), change.clone(), adds, dels));
    }

    let mut is_first_file = true;
    for (change_text, original_path, change, _, _) in changed_files.iter() {
        let header_text = format!(
            "{change_text:>max_change_width$} {}",
            display_path(original_path, cwd)
        );
        for line in wrap_styled_spans(
            vec![Span::styled(header_text, Style::default())],
            width.saturating_sub(display_width(block_indent)),
        ) {
            let mut spans = vec![Span::styled(block_indent.to_string(), dim_style(theme))];
            spans.extend(line);
            if remaining > 0 {
                rendered.push(Line::from(spans));
                remaining = remaining.saturating_sub(1);
            }
        }
        if !is_first_file && remaining > 0 {
            rendered.push(Line::from(""));
            remaining = remaining.saturating_sub(1);
        }
        is_first_file = false;

        match change {
            FileChange::Add { content } => {
                if !content.is_empty() {
                    let num_width = content.lines().count().to_string().len();
                    let mut idx = 0usize;
                    for content_line in content.lines() {
                        idx += 1;
                        let body = DiffBodyLine {
                            kind: DiffLineKind::Insert,
                            text: content_line.to_string(),
                        };
                        let wrapped = render_diff_line(&body, 0, idx, num_width, width, theme);
                        for line in wrapped {
                            if remaining == 0 {
                                break;
                            }
                            rendered.push(line);
                            remaining = remaining.saturating_sub(1);
                        }
                    }
                }
            }
            FileChange::Delete { content } => {
                if !content.is_empty() {
                    let num_width = content.lines().count().to_string().len();
                    let mut idx = 0usize;
                    for content_line in content.lines() {
                        idx += 1;
                        let body = DiffBodyLine {
                            kind: DiffLineKind::Delete,
                            text: content_line.to_string(),
                        };
                        let wrapped = render_diff_line(&body, idx, 0, num_width, width, theme);
                        for line in wrapped {
                            if remaining == 0 {
                                break;
                            }
                            rendered.push(line);
                            remaining = remaining.saturating_sub(1);
                        }
                    }
                }
            }
            FileChange::Update { unified_diff } => {
                let hunks = parse_unified_diff(unified_diff);
                if hunks.is_empty() && remaining > 0 {
                    rendered.push(Line::from(Span::styled(
                        format!("{block_indent}(unable to parse diff)"),
                        dim_style(theme),
                    )));
                    remaining = remaining.saturating_sub(1);
                }
                for hunk in hunks {
                    if remaining == 0 {
                        break;
                    }
                    let line_number_width = {
                        let mut ln_old = hunk.old_start;
                        let mut ln_new = hunk.new_start;
                        for l in &hunk.lines {
                            match l.kind {
                                DiffLineKind::Context => {
                                    ln_old += 1;
                                    ln_new += 1;
                                }
                                DiffLineKind::Delete => ln_old += 1,
                                DiffLineKind::Insert => ln_new += 1,
                            }
                        }
                        format!("{}", ln_old.max(ln_new).saturating_sub(1)).len()
                    };
                    let mut ln_old = hunk.old_start;
                    let mut ln_new = hunk.new_start;
                    for l in &hunk.lines {
                        if remaining == 0 {
                            break;
                        }
                        let lines =
                            render_diff_line(l, ln_old, ln_new, line_number_width, width, theme);
                        for line in lines {
                            if remaining == 0 {
                                break;
                            }
                            rendered.push(line);
                            remaining = remaining.saturating_sub(1);
                        }
                        match l.kind {
                            DiffLineKind::Context => {
                                ln_old += 1;
                                ln_new += 1;
                            }
                            DiffLineKind::Delete => ln_old += 1,
                            DiffLineKind::Insert => ln_new += 1,
                        }
                    }
                }
            }
        }
    }

    // If the preview ran out of rows mid-diff, close it explicitly rather
    // than let the block trail off — matching the donor's elided marker.
    if remaining == 0 {
        rendered.push(Line::from(Span::styled(
            "  ⋯".to_string(),
            dim_style(theme),
        )));
    }
    rendered
}

fn dim_style(theme: &Theme) -> Style {
    Style::default().fg(theme.color(Role::Dim))
}

/// Full per-file diff summary: header, aligned `+/-` counts, and the diff
/// bodies at `width`. This is the expanded/transcript projection.
pub fn create_diff_summary(
    changes: &BTreeMap<PathBuf, FileChange>,
    cwd: &Path,
    width: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    create_diff_lines(changes, cwd, width, None, theme)
}

/// Bounded preview: at most `max_lines` rows, closed with an explicit
/// elision marker when content is cut. Codex uses this for the compact
/// `PatchHistoryCell`/`ApprovalRequest::Patch` view.
pub fn create_diff_preview(
    changes: &BTreeMap<PathBuf, FileChange>,
    cwd: &Path,
    width: usize,
    max_lines: usize,
    theme: &Theme,
) -> Vec<Line<'static>> {
    if max_lines == 0 {
        return Vec::new();
    }
    let mut lines = create_diff_lines(changes, cwd, width, Some(max_lines), theme);
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        lines[max_lines - 1] = Line::from(Span::styled("  ⋯", dim_style(theme)));
    }
    lines
}

/// One inline-diff row for the tool ledger: `      41 - text`, tinted across
/// the full `width` at 256 colours or better (sign + foreground role carry the
/// meaning below that). `text` is sanitized and clipped to the row.
pub fn ledger_diff_row(
    insert: bool,
    number: Option<usize>,
    text: &str,
    width: usize,
    theme: &Theme,
) -> Line<'static> {
    let kind = if insert {
        DiffLineKind::Insert
    } else {
        DiffLineKind::Delete
    };
    let sign = if insert { '+' } else { '-' };
    let number = number.map_or_else(|| "    ".to_string(), |n| format!("{n:>4}"));
    let prefix = format!("      {number} {sign} ");
    let budget = width.saturating_sub(display_width(&prefix)).max(1);
    let mut body = String::new();
    let mut used = 0;
    let clean = sanitize(text);
    let mut clipped = false;
    for g in clean.graphemes(true) {
        let w = display_width(g);
        if used + w > budget.saturating_sub(1) && display_width(&clean) > budget {
            clipped = true;
            break;
        }
        used += w;
        body.push_str(g);
    }
    if clipped {
        body.push('…');
        used += 1;
    }
    let pad = width.saturating_sub(display_width(&prefix) + used);
    let fg = theme.color(if insert { Role::Success } else { Role::Failure });
    let mut gutter = Style::default().fg(theme.color(Role::Dim));
    let mut content = Style::default().fg(fg);
    if let Some(bg) = diff_bg(kind, theme) {
        gutter = gutter.bg(bg);
        content = content.bg(bg);
    }
    let mut spans = vec![Span::styled(prefix, gutter), Span::styled(body, content)];
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), content));
    }
    Line::from(spans)
}

/// Tinted ledger rows for a unified diff, at most `max_rows`, with context
/// lines dropped (the ledger shows only what changed). Returns the rows plus
/// the full `(added, removed)` counts so a result column can state them even
/// when the rows are capped. Malformed diffs yield no rows, never invented
/// ones.
pub fn ledger_diff_rows(
    unified_diff: &str,
    max_rows: usize,
    width: usize,
    theme: &Theme,
) -> (Vec<Line<'static>>, usize, usize) {
    let sanitized = sanitize(unified_diff);
    let hunks = parse_unified_diff(&sanitized);
    let mut rows = Vec::new();
    let (mut adds, mut dels) = (0usize, 0usize);
    for hunk in &hunks {
        let (mut old, mut new) = (hunk.old_start, hunk.new_start);
        for line in &hunk.lines {
            match line.kind {
                DiffLineKind::Context => {
                    old += 1;
                    new += 1;
                }
                DiffLineKind::Delete => {
                    dels += 1;
                    if rows.len() < max_rows {
                        rows.push(ledger_diff_row(false, Some(old), &line.text, width, theme));
                    }
                    old += 1;
                }
                DiffLineKind::Insert => {
                    adds += 1;
                    if rows.len() < max_rows {
                        rows.push(ledger_diff_row(true, Some(new), &line.text, width, theme));
                    }
                    new += 1;
                }
            }
        }
    }
    (rows, adds, dels)
}
