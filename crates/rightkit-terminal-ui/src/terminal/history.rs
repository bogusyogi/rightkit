//! Writing finalized lines into the terminal's own scrollback.
//!
//! Lifted from the Codex donor `codex-rs/tui/src/insert_history.rs` at
//! revision `444da310e108da16aaeb18fd790b0ac464f08aca` (Apache-2.0), adapted at
//! one seam: the donor drives `custom_terminal::Terminal` while the host app's
//! `Surface` owns viewport geometry, so this module operates on
//! `impl Write` + explicit `Viewport`/screen-size parameters instead of
//! terminal state. `Surface::insert_history` supplies those parameters.
//!
//! Two rules govern everything in this module (unchanged from the first-pass
//! port and enforced identically by the donor):
//!
//! 1. **The scroll region is always restored**, on every path including write
//!    failures. A leaked region means every later write scrolls a
//!    sub-rectangle of the screen. `ScrollRegion` enforces this with `Drop`,
//!    and the donor's `queue!`-based [`SetScrollRegion`]/[`ResetScrollRegion`]
//!    are used where the algorithm pairs them manually.
//! 2. **The cursor is restored by raw movement**, never through a tracked
//!    cursor API. History insertion must be invisible to the frame renderer's
//!    idea of where the cursor is.
//!
//! Untrusted content reaches this module only through styled `ratatui` spans
//! produced by the transcript renderer, which routes all provider text through
//! `render::sanitize`. Semantic OSC-8 hyperlinks are emitted by
//! `super::hyperlinks::decorate_spans` as *trusted* renderer-generated
//! escapes around already-sanitized span text.

use std::fmt;
use std::io::{self, Write};

use crossterm::cursor::{MoveDown, MoveToColumn, RestorePosition, SavePosition};
use crossterm::style::{
    Color as CColor, Colors, Print, SetAttribute, SetBackgroundColor, SetColors, SetForegroundColor,
};
use crossterm::terminal::{Clear, ClearType};
use crossterm::{Command, queue};
use ratatui::backend::IntoCrossterm;
use ratatui::style::{Color, Modifier};
use ratatui::text::{Line, Span};

use super::hyperlinks::{HyperlinkLine, decorate_spans, remap_wrapped_line};
use crate::terminal::line_utils::line_to_static;
use crate::transcript::wrapping::{
    RtOptions, adaptive_wrap_line, line_contains_url_like, line_has_mixed_url_and_non_url_tokens,
};

/// How to get lines into scrollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertMode {
    /// Set a scroll region above the viewport and print into it. Correct
    /// everywhere the region is honoured, which is every real terminal.
    /// (Donor `InsertHistoryMode::Standard`.)
    ScrollRegion,
    /// Print at the top of the screen and let the terminal's own wrapping
    /// scroll naturally, then repaint the viewport.
    ///
    /// Zellij emulates scroll regions but does not confine soft-wrapped
    /// continuation rows to them, so the region approach leaks composer rows
    /// into history. Screen and tmux are also safer here.
    MultiplexerSafe,
    /// Scroll the WHOLE screen with newline sequences instead of a partial
    /// DECSTBM region.
    ///
    /// Donor `InsertHistoryMode::FullScreen`: Windows Terminal can DISCARD
    /// rows that leave a partial scroll region instead of pushing them into
    /// scrollback, so on WT the viewport rows are cleared and real `\r\n`s
    /// at the screen bottom do the scrolling -- newlines always commit to
    /// terminal-native scrollback.
    FullScreen,
}

/// Whether the caller already wrapped lines to the terminal width or the
/// terminal should own soft wrapping (donor `HistoryLineWrapPolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryLineWrapPolicy {
    /// The transcript renderer pre-wraps; re-wrap here only to repair
    /// width (adaptive wrap that keeps URLs intact).
    PreWrap,
    /// Hand raw lines to the terminal and let it soft-wrap (used where
    /// terminal-managed wrapping is the correct behavior, e.g. Zellij).
    Terminal,
}

/// An armed scroll region that restores itself when dropped.
///
/// Holding the writer for the region's lifetime is deliberate: it makes it
/// impossible to write outside the region while it is set, and impossible to
/// forget the reset.
struct ScrollRegion<'a, W: Write> {
    writer: &'a mut W,
}

impl<'a, W: Write> ScrollRegion<'a, W> {
    /// Set the scroll region to `top..=bottom`, in 1-based inclusive rows.
    ///
    /// The guard is armed *before* the escape is written, deliberately. A
    /// `write!` of a format string reaches the writer in fragments, so a
    /// failure partway through can leave an incomplete escape on the wire with
    /// the region in an indeterminate state. Arming first means that case still
    /// emits a reset; arming after would leave the terminal scrolling a
    /// sub-rectangle until the user runs `reset`.
    fn set(writer: &'a mut W, top: u16, bottom: u16) -> io::Result<Self> {
        let region = Self { writer };
        write!(region.writer, "\x1b[{top};{bottom}r")?;
        Ok(region)
    }

    fn writer(&mut self) -> &mut W {
        self.writer
    }
}

impl<W: Write> Drop for ScrollRegion<'_, W> {
    fn drop(&mut self) {
        // Best-effort: a failure here cannot be reported, but leaving the
        // region set would be far worse than a dropped byte.
        let _ = self.writer.write_all(b"\x1b[r");
    }
}

/// Where the live viewport sits, in 0-based screen rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    pub top: u16,
    pub height: u16,
}

impl Viewport {
    /// The row just below the viewport's last row.
    pub fn bottom_exclusive(&self) -> u16 {
        self.top.saturating_add(self.height)
    }
}

/// Move the cursor to a 0-based `(col, row)` using a raw escape.
///
/// Deliberately not routed through the terminal's cursor bookkeeping: history
/// insertion must leave the renderer's tracked cursor position untouched.
fn move_to(writer: &mut impl Write, col: u16, row: u16) -> io::Result<()> {
    write!(
        writer,
        "\x1b[{};{}H",
        row.saturating_add(1),
        col.saturating_add(1)
    )
}

/// Insert `lines` into scrollback above `viewport`, restoring the cursor.
///
/// `lines` are styled [`HyperlinkLine`]s from the transcript renderer; they
/// are (re-)wrapped inside this function at `screen_width` under
/// `wrap_policy` -- the donor behaviour, which keeps URL tokens intact for
/// terminal link detection and preserves each line's leading whitespace on
/// continuation rows.
///
/// `restore_cursor` is the 0-based `(col, row)` the cursor occupied before
/// the call and must occupy after it. `screen_width`/`screen_height` are the
/// terminal's column/row counts.
///
/// Returns the number of rows the viewport's top slid DOWN while making
/// room. The caller adds it to `Viewport.top`; the band it released holds
/// real inserted history and must not be erased as stale paint.
#[allow(
    clippy::too_many_arguments,
    reason = "one scrollback insert needs the writer, mode, viewport, screen size, lines, wrap policy and cursor together"
)]
pub fn insert(
    writer: &mut impl Write,
    mode: InsertMode,
    viewport: Viewport,
    screen_width: u16,
    screen_height: u16,
    lines: &[HyperlinkLine],
    wrap_policy: HistoryLineWrapPolicy,
    restore_cursor: (u16, u16),
) -> io::Result<u16> {
    if lines.is_empty() {
        return Ok(0);
    }

    // Pre-wrap lines for terminal scrollback (donor
    // `wrap_history_hyperlink_lines` policy): URL-only-ish lines stay intact
    // so terminals can match them as links; mixed and plain lines wrap
    // adaptively at the screen width.
    let wrap_width = (screen_width as usize).max(1);
    let (wrapped, wrapped_rows) = wrap_history_hyperlink_lines(lines, wrap_width, wrap_policy);
    let wrapped_lines = wrapped_rows as u16;

    match mode {
        InsertMode::ScrollRegion => insert_scroll_region(
            writer,
            viewport,
            screen_height,
            &wrapped,
            wrapped_lines,
            wrap_width,
            restore_cursor,
        ),
        InsertMode::MultiplexerSafe => {
            insert_multiplexer_safe(writer, viewport, &wrapped, wrap_width, restore_cursor)?;
            Ok(0)
        }
        InsertMode::FullScreen => insert_full_screen(
            writer,
            viewport,
            screen_height,
            &wrapped,
            wrapped_lines,
            wrap_width,
            restore_cursor,
        ),
    }
}

/// Donor `InsertHistoryMode::Standard`: scroll-region insertion.
///
/// A terminal cannot scroll content above its scroll region, so when the
/// viewport floats over free rows the region `top+1..screen_height` is first
/// reverse-scrolled (`\x1bM`) -- pushing those rows into real scrollback and
/// sliding the viewport down into the freed space. The lines are then printed
/// into the region above the slid viewport.
fn insert_scroll_region(
    writer: &mut impl Write,
    viewport: Viewport,
    screen_height: u16,
    wrapped: &[HyperlinkLine],
    wrapped_lines: u16,
    wrap_width: usize,
    restore_cursor: (u16, u16),
) -> io::Result<u16> {
    // Slide-down: free rows below the viewport become room for history.
    let free_below = screen_height.saturating_sub(viewport.bottom_exclusive());
    let slide = free_below.min(wrapped_lines);
    if slide > 0 {
        {
            let mut region = ScrollRegion::set(writer, viewport.top + 1, screen_height)?;
            move_to(region.writer(), 0, viewport.top)?;
            for _ in 0..slide {
                write!(region.writer(), "\x1bM")?;
            }
            // Region restored here by Drop, before the next region is set.
        }
    }
    let top = viewport.top.saturating_add(slide);

    // The region is everything above the viewport, in 1-based rows. A
    // viewport still at row 0 leaves nowhere to insert.
    if top == 0 {
        return Ok(slide);
    }
    let region_bottom = top; // 1-based bottom == 0-based top

    {
        let mut region = ScrollRegion::set(writer, 1, region_bottom)?;

        // Park on the region's last row. Printing a newline here scrolls the
        // region rather than moving down into the viewport.
        move_to(region.writer(), 0, top.saturating_sub(1))?;

        // Every row must reach terminal scrollback, including batches taller
        // than this region. Its height bounds visibility, never delivery.
        for line in wrapped {
            // Newline first, then content: this scrolls, *then* fills the
            // freshly vacated row. Reversing the order overwrites the row
            // that is about to scroll away.
            region.writer().write_all(b"\r\n")?;
            write_history_line(region.writer(), line, wrap_width)?;
        }
        // Region restored here by Drop, before the cursor is moved back.
    }

    move_to(writer, restore_cursor.0, restore_cursor.1)?;
    writer.flush()?;
    Ok(slide)
}

fn insert_multiplexer_safe(
    writer: &mut impl Write,
    viewport: Viewport,
    wrapped: &[HyperlinkLine],
    wrap_width: usize,
    restore_cursor: (u16, u16),
) -> io::Result<()> {
    // Clear the viewport first, so the terminal's own scrolling cannot drag
    // stale composer rows up into history.
    for row in viewport.top..viewport.bottom_exclusive() {
        move_to(writer, 0, row)?;
        writer.write_all(b"\x1b[K")?;
    }

    move_to(writer, 0, viewport.top)?;
    for line in wrapped {
        write_history_line(writer, line, wrap_width)?;
        writer.write_all(b"\r\n")?;
    }

    move_to(writer, restore_cursor.0, restore_cursor.1)?;
    writer.flush()
}

/// Windows Terminal insertion (donor `InsertHistoryMode::FullScreen`).
///
/// Partial DECSTBM regions are unsafe on WT -- rows scrolled out of the
/// region can be dropped instead of reaching scrollback. So the viewport rows
/// are cleared outright, the lines are written at the viewport's top row, and
/// one `\r\n` per viewport row scrolls the screen back down so the caller can
/// repaint the viewport at its settled row. Every displaced row reaches real
/// terminal scrollback.
///
/// Returns how far the viewport's top moved down: it lands at
/// `min(top + wrapped_rows, screen_height - height)`.
fn insert_full_screen(
    writer: &mut impl Write,
    viewport: Viewport,
    screen_height: u16,
    wrapped: &[HyperlinkLine],
    wrapped_lines: u16,
    wrap_width: usize,
    restore_cursor: (u16, u16),
) -> io::Result<u16> {
    // Clear the viewport and everything below the insertion point so stale
    // cells cannot linger where the newlines scroll.
    move_to(writer, 0, viewport.top)?;
    writer.write_all(b"\x1b[J")?;
    for (index, line) in wrapped.iter().enumerate() {
        if index > 0 {
            writer.write_all(b"\r\n")?;
        }
        write_history_line(writer, line, wrap_width)?;
        commit_native_history(writer)?;
    }
    // Writing raw source text through the terminal preserves its soft-wrap
    // metadata. Advance through empty rows for the viewport so history ends
    // immediately above the composer even when a replay batch is taller than
    // the visible history region.
    for _ in 0..viewport.height {
        writer.write_all(b"\r\n")?;
        writer.write_all(b"\x1b[K")?;
    }
    commit_native_history(writer)?;
    move_to(writer, restore_cursor.0, restore_cursor.1)?;
    writer.flush()?;

    let new_top = viewport
        .top
        .saturating_add(wrapped_lines)
        .min(screen_height.saturating_sub(viewport.height));
    Ok(new_top.saturating_sub(viewport.top))
}

/// Commit rows before another scroll or viewport repaint can replace them.
/// ConPTY's deferred renderer can coalesce a whole insertion into its final
/// screen, losing intermediate rows even after stdout is flushed. Its
/// AdaptDispatch::DoFinalTermAction flushes the frame before handling OSC 133.
/// An empty action creates no shell marks or replies, but provides that barrier.
fn commit_native_history(writer: &mut impl Write) -> io::Result<()> {
    #[cfg(windows)]
    {
        writer.write_all(b"\x1b]133;\x1b\\")?;
        writer.flush()?;
    }
    #[cfg(not(windows))]
    let _ = writer;
    Ok(())
}

/// Scroll screen rows up so a *growing* viewport can occupy them, donor
/// `grow_viewport` (`codex-rs/tui/src/tui/scrollback.rs`,
/// `ScrollbackStrategy::grow_viewport`).
///
/// `viewport_top` is the 0-based top in effect *before* the growth -- the
/// scroll-region boundary. `count` is how many rows the settled viewport
/// needs committed: those rows leave the top of the region and land in real
/// terminal scrollback.
///
/// - `ScrollRegion`: scroll only the region above the boundary. Below row 1
///   there is no meaningful region, so the full-screen path is used.
/// - `MultiplexerSafe`: noop -- the host-line insertion path owns
///   scrollback under tmux/screen/zellij and accepts degraded growth.
/// - `FullScreen`: Windows Terminal can DISCARD rows scrolled out of a
///   partial DECSTBM region, so the stale viewport is cleared and the whole
///   screen is scrolled with `\r\n` from the last row.
pub fn grow_viewport(
    writer: &mut impl Write,
    mode: InsertMode,
    viewport_top: u16,
    screen_height: u16,
    count: u16,
    restore_cursor: (u16, u16),
) -> io::Result<()> {
    // Donor guard (`scroll_by == 0 || viewport_top == 0`): a viewport anchored
    // at row 0 has no band above it, so growth is a cursor-neutral no-op.
    if count == 0 || viewport_top == 0 || mode == InsertMode::MultiplexerSafe {
        return Ok(());
    }
    if mode == InsertMode::ScrollRegion && viewport_top > 1 {
        // Once `viewport_top` rows have scrolled the region is all blank;
        // further scrolls would only churn blanks.
        let count = count.min(viewport_top);
        {
            let mut region = ScrollRegion::set(writer, 1, viewport_top)?;
            // Park on the region's last row; each newline scrolls the region
            // by one, pushing its top row into real scrollback.
            move_to(region.writer(), 0, viewport_top.saturating_sub(1))?;
            for _ in 0..count {
                region.writer().write_all(b"\r\n")?;
            }
        }
    } else {
        // FullScreen, or the region boundary has fallen to the first rows:
        // clear the stale viewport rows, then scroll the whole screen with
        // newlines emitted on the last row.
        move_to(writer, 0, viewport_top)?;
        writer.write_all(b"\x1b[J")?;
        move_to(writer, 0, screen_height.saturating_sub(1))?;
        for _ in 0..count {
            writer.write_all(b"\r\n")?;
        }
        commit_native_history(writer)?;
    }
    move_to(writer, restore_cursor.0, restore_cursor.1)?;
    writer.flush()
}

/// Pick an insertion mode for the detected environment.
///
/// A multiplexer wins over Windows Terminal (WT + tmux keeps the
/// host-line strategy); a bare Windows Terminal session takes the
/// newline-scrolling `FullScreen` mode. Native Windows consoles need the
/// same mode even without WT_SESSION: ConPTY also drops rows scrolled out
/// of a partial region. Other platforms scroll regions.
///
/// Under Zellij with terminal-managed wrapping the donor upgrades to
/// `FullScreen` (`ScrollbackStrategy::history_insertion_mode`); our mux path
/// already pre-wraps, so the dedicated `MultiplexerSafe` mode stays correct.
pub fn mode_for(
    multiplexer: Option<super::capability::Multiplexer>,
    windows_terminal: bool,
) -> InsertMode {
    match (multiplexer, windows_terminal || cfg!(windows)) {
        (Some(_), _) => InsertMode::MultiplexerSafe,
        (None, true) => InsertMode::FullScreen,
        (None, false) => InsertMode::ScrollRegion,
    }
}

// ── donor insert_history.rs verbatim block ────────────────────────────────

/// Pre-wrap history lines per `HistoryLineWrapPolicy`, returning the wrapped
/// lines and the total number of physical rows they occupy at `wrap_width`.
pub fn wrap_history_hyperlink_lines(
    lines: &[HyperlinkLine],
    wrap_width: usize,
    wrap_policy: HistoryLineWrapPolicy,
) -> (Vec<HyperlinkLine>, usize) {
    let mut wrapped = Vec::new();
    let mut wrapped_rows = 0usize;

    for line in lines {
        let line_wrapped = match wrap_policy {
            HistoryLineWrapPolicy::Terminal => vec![line.clone()],
            HistoryLineWrapPolicy::PreWrap
                if line_contains_url_like(&line.line)
                    && !line_has_mixed_url_and_non_url_tokens(&line.line) =>
            {
                vec![line.clone()]
            }
            HistoryLineWrapPolicy::PreWrap => remap_wrapped_line(
                line,
                adaptive_wrap_line(
                    &line.line,
                    RtOptions::new(wrap_width)
                        .subsequent_indent(leading_whitespace_prefix(&line.line)),
                )
                .into_iter()
                .map(|line| line_to_static(&line))
                .collect(),
            ),
        };
        wrapped_rows += line_wrapped
            .iter()
            .map(|wrapped_line| wrapped_line.width().max(1).div_ceil(wrap_width))
            .sum::<usize>();
        wrapped.extend(line_wrapped);
    }

    (wrapped, wrapped_rows)
}

/// The leading whitespace of `line` as a styled `Line`, used as the
/// continuation indent when the line wraps (donor `leading_whitespace_prefix`).
pub fn leading_whitespace_prefix(line: &Line<'_>) -> Line<'static> {
    let mut spans = Vec::new();
    for span in &line.spans {
        let prefix_end = span
            .content
            .char_indices()
            .find_map(|(idx, ch)| (!ch.is_whitespace()).then_some(idx))
            .unwrap_or(span.content.len());
        if prefix_end > 0 {
            spans.push(Span::styled(
                span.content[..prefix_end].to_string(),
                span.style,
            ));
        }
        if prefix_end < span.content.len() {
            break;
        }
    }
    Line::from(spans).style(line.style)
}

/// Render a single wrapped history line: clear continuation rows for wide lines,
/// set foreground/background colors, and write styled spans. Caller is responsible
/// for cursor positioning and any leading `\r\n`.
fn write_history_line<W: Write>(
    writer: &mut W,
    line: &HyperlinkLine,
    wrap_width: usize,
) -> io::Result<()> {
    let physical_rows = line.width().max(1).div_ceil(wrap_width) as u16;
    if physical_rows > 1 {
        queue!(writer, SavePosition)?;
        for _ in 1..physical_rows {
            queue!(writer, MoveDown(1), MoveToColumn(0))?;
            queue!(writer, Clear(ClearType::UntilNewLine))?;
        }
        queue!(writer, RestorePosition)?;
    }
    queue!(
        writer,
        SetColors(Colors::new(
            line.line
                .style
                .fg
                .map(IntoCrossterm::into_crossterm)
                .unwrap_or(CColor::Reset),
            line.line
                .style
                .bg
                .map(IntoCrossterm::into_crossterm)
                .unwrap_or(CColor::Reset)
        ))
    )?;
    queue!(writer, Clear(ClearType::UntilNewLine))?;
    // Merge line-level style into each span so that ANSI colors reflect
    // line styles (e.g., blockquotes with green fg).
    let merged_spans: Vec<Span> = line
        .line
        .spans
        .iter()
        .map(|s| Span {
            style: s.style.patch(line.line.style),
            content: s.content.clone(),
        })
        .collect();
    let merged_line = HyperlinkLine {
        source: None,
        line: Line::from(merged_spans),
        hyperlinks: line.hyperlinks.clone(),
    };
    let decorated = decorate_spans(&merged_line);
    write_spans(writer, decorated.iter())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetScrollRegion(pub std::ops::Range<u16>);

impl Command for SetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[{};{}r", self.0.start, self.0.end)
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        panic!("tried to execute SetScrollRegion command using WinAPI, use ANSI instead");
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResetScrollRegion;

impl Command for ResetScrollRegion {
    fn write_ansi(&self, f: &mut impl fmt::Write) -> fmt::Result {
        write!(f, "\x1b[r")
    }

    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        panic!("tried to execute ResetScrollRegion command using WinAPI, use ANSI instead");
    }

    #[cfg(windows)]
    fn is_ansi_code_supported(&self) -> bool {
        true
    }
}

struct ModifierDiff {
    pub from: Modifier,
    pub to: Modifier,
}

impl ModifierDiff {
    fn queue<W>(self, mut w: W) -> io::Result<()>
    where
        W: io::Write,
    {
        use crossterm::style::Attribute as CAttribute;
        let removed = self.from - self.to;
        if removed.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::NoReverse))?;
        }
        if removed.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
            if self.to.contains(Modifier::DIM) {
                queue!(w, SetAttribute(CAttribute::Dim))?;
            }
        }
        if removed.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::NoItalic))?;
        }
        if removed.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::NoUnderline))?;
        }
        if removed.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::NormalIntensity))?;
        }
        if removed.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(CAttribute::NotCrossedOut))?;
        }
        if removed.contains(Modifier::SLOW_BLINK) || removed.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(CAttribute::NoBlink))?;
        }

        let added = self.to - self.from;
        if added.contains(Modifier::REVERSED) {
            queue!(w, SetAttribute(CAttribute::Reverse))?;
        }
        if added.contains(Modifier::BOLD) {
            queue!(w, SetAttribute(CAttribute::Bold))?;
        }
        if added.contains(Modifier::ITALIC) {
            queue!(w, SetAttribute(CAttribute::Italic))?;
        }
        if added.contains(Modifier::UNDERLINED) {
            queue!(w, SetAttribute(CAttribute::Underlined))?;
        }
        if added.contains(Modifier::DIM) {
            queue!(w, SetAttribute(CAttribute::Dim))?;
        }
        if added.contains(Modifier::CROSSED_OUT) {
            queue!(w, SetAttribute(CAttribute::CrossedOut))?;
        }
        if added.contains(Modifier::SLOW_BLINK) {
            queue!(w, SetAttribute(CAttribute::SlowBlink))?;
        }
        if added.contains(Modifier::RAPID_BLINK) {
            queue!(w, SetAttribute(CAttribute::RapidBlink))?;
        }

        Ok(())
    }
}

fn write_spans<'a, I>(mut writer: &mut impl Write, content: I) -> io::Result<()>
where
    I: IntoIterator<Item = &'a Span<'a>>,
{
    let mut fg = Color::Reset;
    let mut bg = Color::Reset;
    let mut last_modifier = Modifier::empty();
    for span in content {
        let mut modifier = Modifier::empty();
        modifier.insert(span.style.add_modifier);
        modifier.remove(span.style.sub_modifier);
        if modifier != last_modifier {
            let diff = ModifierDiff {
                from: last_modifier,
                to: modifier,
            };
            diff.queue(&mut writer)?;
            last_modifier = modifier;
        }
        let next_fg = span.style.fg.unwrap_or(Color::Reset);
        let next_bg = span.style.bg.unwrap_or(Color::Reset);
        if next_fg != fg || next_bg != bg {
            queue!(
                writer,
                SetColors(Colors::new(
                    next_fg.into_crossterm(),
                    next_bg.into_crossterm()
                ))
            )?;
            fg = next_fg;
            bg = next_bg;
        }

        queue!(writer, Print(super::glyphs::for_terminal(&span.content)))?;
    }

    queue!(
        writer,
        SetForegroundColor(CColor::Reset),
        SetBackgroundColor(CColor::Reset),
        SetAttribute(crossterm::style::Attribute::Reset),
    )
}

/// Slide everything above row `above_rows` down by `rows`: reverse-index inside
/// the region `1..=above_rows`, so the `rows` blank rows just above the
/// viewport drop out of the bottom and the history meets the viewport.
pub fn slide_history_down(writer: &mut impl Write, above_rows: u16, rows: u16) -> io::Result<()> {
    let rows = rows.min(above_rows);
    if rows == 0 {
        return Ok(());
    }
    write!(writer, "\x1b[1;{above_rows}r\x1b[1;1H")?;
    for _ in 0..rows {
        writer.write_all(b"\x1bM")?;
    }
    // Restore the full-screen region; the draw that follows repositions the cursor.
    write!(writer, "\x1b[r")?;
    Ok(())
}

/// A bottom-anchored viewport that shrinks leaves its released rows blank
/// between the history above and the smaller footprint below. Slide the
/// history down over them (reverse-index inside the region above the new top)
/// so the transcript stays contiguous with the input: spare rows then sit at
/// the top of the screen, never between the transcript and the prompt.
/// Floating viewports (free rows below them) and growth are left alone.
///
/// `old_*`/`new_*` are the previous and next footprint `y`/`height`.
pub fn close_released_band(
    writer: &mut impl Write,
    screen_height: u16,
    old_y: u16,
    old_height: u16,
    new_y: u16,
    new_height: u16,
) -> io::Result<()> {
    if old_y.saturating_add(old_height) != screen_height
        || new_y.saturating_add(new_height) != screen_height
        || new_y <= old_y
    {
        return Ok(());
    }
    slide_history_down(writer, new_y, new_y - old_y)
}
