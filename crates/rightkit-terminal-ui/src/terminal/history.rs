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

#[cfg(test)]
mod tests {
    #[test]
    fn a_bottom_anchored_shrink_slides_history_down_so_no_gap_opens() {
        // 20-row screen: history on rows 6..=9, a 10-row footprint at row 10
        // whose top 4 rows are a live tail that settles into history.
        let mut parser = vt100::Parser::new(20, 40, 0);
        for (row, text) in ["h1", "h2", "h3", "h4"].iter().enumerate() {
            parser.process(format!("[{};1H{text}", row + 7).as_bytes());
        }
        for row in 10..20 {
            parser.process(format!("[{};1Hlive{row}", row + 1).as_bytes());
        }
        // Footprint shrinks 10 -> 6 rows: erase the released band, close it.
        let mut wire = Vec::new();
        for row in 10..14 {
            write!(wire, "[{};1H[K", row + 1).unwrap();
        }
        close_released_band(&mut wire, 20, 10, 10, 14, 6).unwrap();
        parser.process(&wire);
        let screen = parser.screen();
        let row_text = |row: u16| screen.contents_between(row, 0, row, 40);
        // History now ends on the row right above the new top (row 14).
        assert_eq!(row_text(10), "h1");
        assert_eq!(row_text(13), "h4");
        assert_eq!(row_text(14), "live14");
    }

    #[test]
    fn floating_viewports_and_growth_are_not_slid() {
        let mut wire = Vec::new();
        close_released_band(&mut wire, 20, 5, 6, 8, 6).unwrap();
        assert!(
            wire.is_empty(),
            "a viewport with free rows below is left alone"
        );
        close_released_band(&mut wire, 20, 12, 8, 8, 12).unwrap();
        assert!(wire.is_empty(), "growth is the scroll path's job");
    }

    use super::*;

    fn hl(text: &str) -> HyperlinkLine {
        HyperlinkLine::from(Line::from(text.to_string()))
    }

    fn rendered(bytes: &[u8]) -> String {
        String::from_utf8(bytes.to_vec()).expect("escapes are valid utf-8")
    }

    /// True when the stream contains a scroll-region control (`CSI … r`, the
    /// DECSTBM set/reset final byte). A cursor-positioning `CSI n;m H` like
    /// `\x1b[1;1H` is NOT a region; scanning CSI final bytes keeps the check
    /// semantic instead of matching parameter text.
    fn sets_scroll_region(s: &str) -> bool {
        let bytes = s.as_bytes();
        let mut i = 0;
        while i + 1 < bytes.len() {
            if bytes[i] == 0x1b && bytes[i + 1] == b'[' {
                let mut j = i + 2;
                // Skip parameter (0x30-0x3f) and intermediate (0x20-0x2f)
                // bytes; the first byte outside those ranges is the final.
                while j < bytes.len() && matches!(bytes[j], 0x20..=0x3f) {
                    j += 1;
                }
                if bytes.get(j) == Some(&b'r') {
                    return true;
                }
                i = j.max(i + 2);
            } else {
                i += 1;
            }
        }
        false
    }

    fn insert_lines(
        out: &mut Vec<u8>,
        mode: InsertMode,
        vp: Viewport,
        screen_height: u16,
        lines: &[HyperlinkLine],
        cursor: (u16, u16),
    ) -> u16 {
        insert(
            out,
            mode,
            vp,
            /*screen_width*/ 80,
            screen_height,
            lines,
            HistoryLineWrapPolicy::PreWrap,
            cursor,
        )
        .unwrap()
    }

    #[test]
    fn nothing_is_written_for_an_empty_batch() {
        let mut out = Vec::new();
        let vp = Viewport { top: 20, height: 4 };
        insert_lines(&mut out, InsertMode::ScrollRegion, vp, 24, &[], (3, 21));
        assert!(out.is_empty());
    }

    #[test]
    fn the_region_covers_exactly_the_rows_above_the_viewport() {
        let mut out = Vec::new();
        let vp = Viewport { top: 20, height: 4 };
        insert_lines(
            &mut out,
            InsertMode::ScrollRegion,
            vp,
            24,
            &[hl("hello")],
            (0, 20),
        );

        // 1-based inclusive: rows 1..=20, where 20 is the last row above a
        // viewport that starts at 0-based row 20.
        assert!(
            rendered(&out).contains("\x1b[1;20r"),
            "expected region 1;20, got {:?}",
            rendered(&out)
        );
    }

    #[test]
    fn the_region_is_always_reset_after_use() {
        let mut out = Vec::new();
        let vp = Viewport { top: 10, height: 3 };
        // Screen bottom == viewport bottom: no free rows, no slide-down.
        insert_lines(
            &mut out,
            InsertMode::ScrollRegion,
            vp,
            13,
            &[hl("a")],
            (0, 10),
        );

        let s = rendered(&out);
        let set = s.find("\x1b[1;10r").expect("region set");
        let reset = s.find("\x1b[r").expect("region reset");
        assert!(reset > set, "reset must follow set");
    }

    /// The guard exists so that an error partway through cannot leave the
    /// terminal scrolling a sub-region forever.
    #[test]
    fn the_region_is_reset_even_when_writing_fails_midway() {
        struct FailAfter {
            budget: usize,
            written: Vec<u8>,
        }
        impl Write for FailAfter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                if self.budget == 0 {
                    // The Drop impl still gets to write; record it.
                    self.written.extend_from_slice(buf);
                    return Err(io::Error::other("device full"));
                }
                self.budget -= 1;
                self.written.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut w = FailAfter {
            budget: 6,
            written: Vec::new(),
        };
        let vp = Viewport { top: 10, height: 3 };
        let _ = insert(
            &mut w,
            InsertMode::ScrollRegion,
            vp,
            80,
            13,
            &[hl("a"), hl("b")],
            HistoryLineWrapPolicy::PreWrap,
            (0, 10),
        );

        assert!(
            rendered(&w.written).contains("\x1b[r"),
            "scroll region leaked after a write error: {:?}",
            rendered(&w.written)
        );
    }

    #[test]
    fn the_cursor_returns_to_where_it_started() {
        let mut out = Vec::new();
        let vp = Viewport { top: 10, height: 3 };
        insert_lines(
            &mut out,
            InsertMode::ScrollRegion,
            vp,
            13,
            &[hl("a")],
            (7, 11),
        );

        // 0-based (7, 11) is 1-based row 12, column 8.
        assert!(
            rendered(&out).ends_with("\x1b[12;8H"),
            "expected a trailing cursor restore, got {:?}",
            rendered(&out)
        );
    }

    #[test]
    fn newline_precedes_content_so_the_vacated_row_is_filled() {
        let mut out = Vec::new();
        let vp = Viewport { top: 5, height: 2 };
        insert_lines(
            &mut out,
            InsertMode::ScrollRegion,
            vp,
            7,
            &[hl("first")],
            (0, 5),
        );

        let s = rendered(&out);
        let nl = s.find("\r\n").expect("newline emitted");
        let content = s.find("first").expect("content emitted");
        assert!(
            nl < content,
            "content must follow the scroll, not precede it"
        );
    }

    /// Donor slide-down: a floating viewport first scrolls the free rows
    /// below it into scrollback, then writes into the widened band above
    /// its slid position.
    #[test]
    fn free_rows_below_the_viewport_are_scrolled_away_before_inserting() {
        let mut out = Vec::new();
        let vp = Viewport { top: 5, height: 2 };
        let lines = vec![hl("a"), hl("b"), hl("c")];
        let slide = insert_lines(&mut out, InsertMode::ScrollRegion, vp, 24, &lines, (0, 8));

        assert_eq!(slide, 3, "one slide row per inserted line");
        let s = rendered(&out);
        // Slide phase: region top+1..screen (1-based 6..24) reverse-scrolled
        // three times at the viewport's old top row.
        let slide_region = s.find("\x1b[6;24r").expect("slide region set");
        let reverse = s.find("\x1bM").expect("reverse scroll emitted");
        assert!(slide_region < reverse);
        assert_eq!(s.matches("\x1bM").count(), 3);
        // Insert phase: the slid top is 5+3=8, so the band is rows 1..=8.
        assert!(
            s.contains("\x1b[1;8r"),
            "insert region above slid top: {s:?}"
        );
        for line in &lines {
            assert!(s.contains(line.line.spans[0].content.as_ref()));
        }
    }

    #[test]
    fn slide_down_is_bounded_by_the_free_rows_not_the_batch() {
        let mut out = Vec::new();
        let vp = Viewport { top: 5, height: 2 };
        // Screen 9 leaves 2 free rows under a viewport ending at row 7.
        let lines = vec![hl("a"), hl("b"), hl("c")];
        let slide = insert_lines(&mut out, InsertMode::ScrollRegion, vp, 9, &lines, (0, 8));

        assert_eq!(slide, 2);
        let s = rendered(&out);
        assert_eq!(s.matches("\x1bM").count(), 2);
        // Slid top = 7; the insertion band is rows 1..=7.
        assert!(
            s.contains("\x1b[1;7r"),
            "insert region above slid top: {s:?}"
        );
    }

    #[test]
    fn a_full_screen_viewport_has_nowhere_to_insert_and_writes_nothing() {
        let mut out = Vec::new();
        let vp = Viewport { top: 0, height: 5 };
        insert_lines(
            &mut out,
            InsertMode::ScrollRegion,
            vp,
            5,
            &[hl("a")],
            (0, 0),
        );
        assert!(out.is_empty());
    }

    /// A viewport at row 0 WITH free space below still gains room by
    /// sliding down -- it is only a truly full-screen viewport that cannot
    /// accept history.
    #[test]
    fn a_top_row_viewport_slides_down_to_accept_history() {
        let mut out = Vec::new();
        let vp = Viewport { top: 0, height: 5 };
        let slide = insert_lines(
            &mut out,
            InsertMode::ScrollRegion,
            vp,
            24,
            &[hl("a")],
            (0, 5),
        );

        assert_eq!(slide, 1);
        let s = rendered(&out);
        assert!(s.contains("\x1b[1;24r"), "slide region covers the screen");
        assert!(s.contains("\x1bM"));
        // After sliding to top=1 the band above is exactly row 1.
        assert!(s.contains("\x1b[1;1r"));
        assert!(s.contains("a"));
    }

    /// A batch taller than the scroll band still delivers every row to
    /// scrollback, in order: the band bounds visibility of the in-flight
    /// writes, never delivery (see `insert_scroll_region`).
    #[test]
    fn insertion_delivers_every_line_even_when_taller_than_the_band() {
        let mut out = Vec::new();
        let vp = Viewport { top: 3, height: 2 };
        let lines: Vec<HyperlinkLine> = (0..5).map(|i| hl(&format!("line{i}"))).collect();
        insert_lines(&mut out, InsertMode::ScrollRegion, vp, 5, &lines, (0, 3));
        let s = rendered(&out);
        let mut cursor = 0;
        for i in 0..5 {
            let tag = format!("line{i}");
            let at = s[cursor..]
                .find(&tag)
                .unwrap_or_else(|| panic!("expected {tag} in order in {s:?}"));
            cursor += at + tag.len();
        }
    }

    #[test]
    fn multiplexer_mode_clears_the_viewport_and_never_sets_a_region() {
        let mut out = Vec::new();
        let vp = Viewport { top: 8, height: 2 };
        insert_lines(
            &mut out,
            InsertMode::MultiplexerSafe,
            vp,
            10,
            &[hl("x")],
            (0, 8),
        );

        let s = rendered(&out);
        assert!(
            !sets_scroll_region(&s),
            "must not set a scroll region: {s:?}"
        );
        assert!(s.contains("\x1b[K"), "must clear the viewport rows");
    }

    #[test]
    fn every_multiplexer_selects_the_safe_mode() {
        use super::super::capability::Multiplexer;
        // Native Windows consoles take FullScreen without WT_SESSION
        // (`mode_for` doc comment); the expectation is platform-keyed, not
        // read from the ambient environment.
        let bare = if cfg!(windows) {
            InsertMode::FullScreen
        } else {
            InsertMode::ScrollRegion
        };
        assert_eq!(mode_for(None, false), bare);
        for mux in [Multiplexer::Tmux, Multiplexer::Screen, Multiplexer::Zellij] {
            assert_eq!(mode_for(Some(mux), false), InsertMode::MultiplexerSafe);
        }
    }

    /// Codex `ScrollbackStrategy::detect`: Windows Terminal takes the
    /// newline-scrolling FullScreen strategy; a multiplexer keeps its own.
    #[test]
    fn windows_terminal_selects_full_screen_but_a_mux_still_wins() {
        use super::super::capability::Multiplexer;
        assert_eq!(mode_for(None, true), InsertMode::FullScreen);
        assert_eq!(
            mode_for(Some(Multiplexer::Tmux), true),
            InsertMode::MultiplexerSafe
        );
    }

    /// The FullScreen byte contract (Codex `insert_history.rs`): clear the
    /// viewport downward, write the lines, then newline-scroll the screen
    /// down by the viewport height, clearing each row as it goes.
    #[test]
    fn full_screen_insertion_clears_then_newline_scrolls() {
        // Deterministic wire bytes: force color output so crossterm's memoized
        // NO_COLOR suppression cannot replace SGR colors with bare resets.
        crossterm::style::force_color_output(true);
        let mut out = Vec::new();
        let vp = Viewport { top: 5, height: 3 };
        let lines = vec![hl("alpha"), hl("beta")];
        let slide = insert_lines(&mut out, InsertMode::FullScreen, vp, 24, &lines, (0, 8));

        // New top = min(5 + 2, 24 - 3) = 7.
        assert_eq!(slide, 2);
        let s = rendered(&out);
        // Move to the viewport's top row (1-based row 6) and clear downward.
        assert!(
            s.starts_with("\x1b[6;1H\x1b[J"),
            "must clear viewport first: {s:?}"
        );
        // Lines: no leading newline on the first, `\r\n` separators after.
        // Each line ends with its SGR style resets, so assert ordering of the
        // content and the separating newline rather than contiguity.
        let alpha = s.find("alpha").expect("first line written");
        let separator = s[alpha..]
            .find("\r\n")
            .map(|offset| alpha + offset)
            .expect("separator newline after first line");
        let beta = s[separator..]
            .find("beta")
            .map(|offset| separator + offset)
            .expect("second line written");
        assert!(
            alpha < separator && separator < beta,
            "a newline scroll must separate the lines: {s:?}"
        );
        // Exactly `height` newline+clear pairs scroll the screen back down.
        assert_eq!(s.matches("\r\n\x1b[K").count(), 3);
        // No scroll region or index scroll anywhere -- that is the whole
        // point of the mode.
        assert!(!sets_scroll_region(&s), "no scroll region: {s:?}");
        assert!(!s.contains("\x1bM"));
        assert!(s.ends_with("\x1b[9;1H"), "cursor restored: {s:?}");
    }

    /// Clamped at the bottom: when the lines would push past the screen the
    /// viewport bottom-anchors instead.
    #[test]
    fn full_screen_insertion_bottom_anchors_when_lines_overflow() {
        let mut out = Vec::new();
        let vp = Viewport { top: 18, height: 3 };
        let lines = vec![hl("a"), hl("b"), hl("c"), hl("d")];
        let slide = insert_lines(&mut out, InsertMode::FullScreen, vp, 24, &lines, (0, 20));

        // New top = min(18 + 4, 24 - 3) = 21; slide delta = 3.
        assert_eq!(slide, 3);
        assert_eq!(rendered(&out).matches("\r\n\x1b[K").count(), 3);
    }

    #[test]
    fn detail_rows_are_inserted_into_scrollback_like_any_line() {
        // Detail rows (thinking/tool/approval) are rendered as plain lines
        // before insertion; this proves they survive the same scroll-region
        // path as the main transcript without disturbing the viewport.
        let mut out = Vec::new();
        let vp = Viewport { top: 15, height: 4 };
        let detail_rows = vec![
            hl("▸ thinking: considering edge cases"),
            hl("▸ tool shell (2s): cargo test"),
            hl("▸ approval: edit src/main.rs"),
        ];
        insert_lines(
            &mut out,
            InsertMode::ScrollRegion,
            vp,
            19,
            &detail_rows,
            (0, 15),
        );
        let s = rendered(&out);
        for row in &detail_rows {
            assert!(
                s.contains(row.line.spans[0].content.as_ref()),
                "detail row must be in scrollback"
            );
        }
        // Region must still be reset even for detail rows.
        assert!(s.contains("\x1b[r"));
    }

    #[test]
    fn detail_rows_survive_multiplexer_safe_insertion() {
        let mut out = Vec::new();
        let vp = Viewport { top: 10, height: 3 };
        let rows = vec![hl("▸ thinking detail")];
        insert_lines(
            &mut out,
            InsertMode::MultiplexerSafe,
            vp,
            13,
            &rows,
            (0, 10),
        );
        let s = rendered(&out);
        assert!(s.contains("thinking detail"));
        assert!(
            !s.contains("\x1b[1;10r"),
            "multiplexer-safe must not set region"
        );
    }

    // ── donor insert_history.rs styled-span tests ─────────────────────

    #[test]
    fn writes_bold_then_regular_spans() {
        use ratatui::style::Stylize;

        let spans = ["A".bold(), "B".into()];

        let mut actual: Vec<u8> = Vec::new();
        write_spans(&mut actual, spans.iter()).unwrap();

        let mut expected: Vec<u8> = Vec::new();
        queue!(
            expected,
            SetAttribute(crossterm::style::Attribute::Bold),
            Print("A"),
            SetAttribute(crossterm::style::Attribute::NormalIntensity),
            Print("B"),
            SetForegroundColor(CColor::Reset),
            SetBackgroundColor(CColor::Reset),
            SetAttribute(crossterm::style::Attribute::Reset),
        )
        .unwrap();

        assert_eq!(
            String::from_utf8(actual).unwrap(),
            String::from_utf8(expected).unwrap()
        );
    }

    /// Untrusted span content (embedded CSI/OSC/OSC-52/C1 controls) is emitted
    /// verbatim only when the *caller* failed to sanitize; the trust boundary
    /// remains `render::sanitize` upstream. What this path DOES own: trusted
    /// OSC-8 hyperlinks are emitted only around already-sanitized text, and
    /// the destination set is derived from parsed markdown link events, never
    /// from raw content bytes.
    #[test]
    fn writes_semantic_web_link_without_emitting_untrusted_controls() {
        let destination = "https://example.com/long/path";
        for linked in [false, true] {
            let mut line =
                crate::terminal::hyperlinks::annotate_web_urls_in_line(Line::from(vec![
                    "\x1b[2J\x1b]52;c;Y2xpcA==\x07\u{009d}hidden\u{009c}\r\n\t ".into(),
                    destination.into(),
                ]));
            let mut safe =
                crate::terminal::hyperlinks::annotate_web_urls_in_line(Line::from(vec![
                    "[2J]52;c;Y2xpcA==hidden ".into(),
                    destination.into(),
                ]));
            if !linked {
                line.hyperlinks.clear();
                safe.hyperlinks.clear();
            }
            let original = line.clone();
            let mut actual = Vec::new();
            let mut expected = Vec::new();
            write_history_line(&mut actual, &line, /*wrap_width*/ 80).unwrap();
            write_history_line(&mut expected, &safe, /*wrap_width*/ 80).unwrap();
            assert_eq!(line, original);
            let output = String::from_utf8(actual).unwrap();
            assert_eq!(
                output.contains(&format!(
                    "\x1b]8;;{destination}\x07{destination}\x1b]8;;\x07"
                )),
                linked,
            );
        }
    }

    /// Donor vt100 lineage, adapted: inserting styled lines through the
    /// scroll-region path into a real terminal parser leaves the blockquote
    /// green on the screen.
    #[test]
    fn vt100_blockquote_line_emits_green_fg() {
        let width: u16 = 40;
        let height: u16 = 10;
        let mut backend = crate::terminal::test_backend::VT100Backend::new(width, height);
        let vp = Viewport {
            top: height - 1,
            height: 1,
        };

        let mut line: Line<'static> = Line::from(vec!["> ".into(), "Hello world".into()]);
        line = line.style(Color::Green);
        let lines = crate::terminal::hyperlinks::plain_hyperlink_lines(vec![line]);
        insert(
            &mut backend,
            InsertMode::ScrollRegion,
            vp,
            width,
            height,
            &lines,
            HistoryLineWrapPolicy::PreWrap,
            (0, height - 1),
        )
        .expect("insert history");

        let mut saw_colored = false;
        'outer: for row in 0..height {
            for col in 0..width {
                if let Some(cell) = backend.vt100().screen().cell(row, col)
                    && cell.has_contents()
                    && cell.fgcolor() != vt100::Color::Default
                {
                    saw_colored = true;
                    break 'outer;
                }
            }
        }
        assert!(
            saw_colored,
            "expected at least one colored cell in vt100 output"
        );
    }

    /// Donor vt100 lineage: a wrapped blockquote keeps its color on every
    /// wrapped row.
    #[test]
    fn vt100_blockquote_wrap_preserves_color_on_all_wrapped_lines() {
        let width: u16 = 20;
        let height: u16 = 8;
        let mut backend = crate::terminal::test_backend::VT100Backend::new(width, height);
        let vp = Viewport {
            top: height - 1,
            height: 1,
        };

        let mut line: Line<'static> = Line::from(vec![
            "> ".into(),
            "This is a long quoted line that should wrap".into(),
        ]);
        line = line.style(Color::Green);
        let lines = crate::terminal::hyperlinks::plain_hyperlink_lines(vec![line]);

        insert(
            &mut backend,
            InsertMode::ScrollRegion,
            vp,
            width,
            height,
            &lines,
            HistoryLineWrapPolicy::PreWrap,
            (0, height - 1),
        )
        .expect("insert history");

        let screen = backend.vt100().screen();
        let mut non_empty_rows: Vec<u16> = Vec::new();
        for row in 0..height {
            let mut any = false;
            for col in 0..width {
                if let Some(cell) = screen.cell(row, col)
                    && cell.has_contents()
                    && cell.contents() != "\0"
                    && cell.contents() != " "
                {
                    any = true;
                    break;
                }
            }
            if any {
                non_empty_rows.push(row);
            }
        }

        assert!(
            non_empty_rows.len() >= 2,
            "expected wrapped output to span >=2 rows, got {non_empty_rows:?}",
        );

        for row in non_empty_rows {
            for col in 0..width {
                if let Some(cell) = screen.cell(row, col) {
                    let contents = cell.contents();
                    if !contents.is_empty() && contents != " " {
                        assert!(
                            cell.fgcolor() != vt100::Color::Default,
                            "expected non-default fg on row {row} col {col}, got {:?}",
                            cell.fgcolor()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn vt100_prefixed_url_keeps_prefix_and_url_on_same_row() {
        let width: u16 = 48;
        let height: u16 = 8;
        let mut backend = crate::terminal::test_backend::VT100Backend::new(width, height);
        let vp = Viewport {
            top: height - 1,
            height: 1,
        };

        let url = "http://a-long-url.com/this/that/blablablab/new.aspx/many_people_like_how";
        let line: Line<'static> = Line::from(vec!["  │ ".into(), url.into()]);
        let lines = crate::terminal::hyperlinks::plain_hyperlink_lines(vec![line]);

        insert(
            &mut backend,
            InsertMode::ScrollRegion,
            vp,
            width,
            height,
            &lines,
            HistoryLineWrapPolicy::PreWrap,
            (0, height - 1),
        )
        .expect("insert history");

        let rows: Vec<String> = backend.vt100().screen().rows(0, width).collect();
        assert!(
            rows.iter().any(|r| r.contains("│ http://a-long-url.com")),
            "expected prefix and URL on same row, rows: {rows:?}"
        );
        assert!(
            !rows.iter().any(|r| r.trim_end() == "│"),
            "unexpected orphan prefix row, rows: {rows:?}"
        );
    }

    #[test]
    fn vt100_prefixed_non_url_line_preserves_prefix_on_wrapped_rows() {
        let width: u16 = 32;
        let height: u16 = 10;
        let mut backend = crate::terminal::test_backend::VT100Backend::new(width, height);
        let vp = Viewport {
            top: height - 1,
            height: 1,
        };

        let line = Line::from(
            "      dog while this deliberately long string tests code block scrolling versus soft wrapping",
        );
        let lines = crate::terminal::hyperlinks::plain_hyperlink_lines(vec![line]);

        insert(
            &mut backend,
            InsertMode::ScrollRegion,
            vp,
            width,
            height,
            &lines,
            HistoryLineWrapPolicy::PreWrap,
            (0, height - 1),
        )
        .expect("insert prefixed history");

        let rows: Vec<String> = backend.vt100().screen().rows(0, width).collect();
        let continuation_row = rows
            .iter()
            .find(|row| row.contains("tests code block scrolling"))
            .unwrap_or_else(|| panic!("expected continuation row in screen rows: {rows:?}"));

        assert!(
            continuation_row.starts_with("      "),
            "expected wrapped continuation row to keep the original prefix, rows: {rows:?}"
        );
    }

    // ── viewport-growth scroll (donor grow_viewport) ──────────────────

    #[test]
    fn growth_scrolls_the_region_above_the_old_top() {
        let mut out = Vec::new();
        grow_viewport(&mut out, InsertMode::ScrollRegion, 20, 24, 3, (0, 17)).unwrap();
        let s = rendered(&out);
        let set = s.find("\x1b[1;20r").expect("region set to old top");
        let scrolls = s.matches('\n').count();
        let reset = s.find("\x1b[r").expect("region reset");
        assert!(set < reset, "set must precede reset");
        assert_eq!(scrolls, 3, "three scrolls commit three covered rows");
        assert!(
            s.ends_with("\x1b[18;1H"),
            "cursor restored to new top, got {s:?}"
        );
    }

    #[test]
    fn growth_without_rows_writes_nothing() {
        let mut out = Vec::new();
        grow_viewport(&mut out, InsertMode::ScrollRegion, 20, 24, 0, (0, 0)).unwrap();
        assert!(out.is_empty());
        grow_viewport(&mut out, InsertMode::MultiplexerSafe, 20, 24, 5, (0, 0)).unwrap();
        assert!(out.is_empty(), "mux growth stays a noop");
    }

    /// Below the second row there is no scroll band worth a region: at row 0
    /// the donor returns without writing anything; at row 1 (a single history
    /// row, where DECSTBM requires two distinct rows) it falls back to the
    /// full-screen newline path.
    #[test]
    fn growth_at_the_first_rows_falls_back_to_full_screen() {
        let mut out = Vec::new();
        grow_viewport(&mut out, InsertMode::ScrollRegion, 0, 24, 2, (0, 0)).unwrap();
        assert!(
            out.is_empty(),
            "row 0 has no band above it: {:?}",
            rendered(&out)
        );

        let mut out = Vec::new();
        grow_viewport(&mut out, InsertMode::ScrollRegion, 1, 24, 2, (0, 0)).unwrap();
        let s = rendered(&out);
        assert!(!sets_scroll_region(&s), "no partial region at top=1: {s:?}");
        assert!(s.contains("\x1b[J"), "stale viewport cleared: {s:?}");
        assert_eq!(s.matches("\r\n").count(), 2);
    }

    /// Windows Terminal growth (Codex `grow_viewport` FullScreen branch):
    /// clear the stale viewport, park on the last row, newline-scroll.
    #[test]
    fn full_screen_growth_clears_viewport_then_scrolls_from_the_bottom() {
        let mut out = Vec::new();
        grow_viewport(&mut out, InsertMode::FullScreen, 5, 24, 3, (0, 2)).unwrap();
        let s = rendered(&out);
        // Clear from the old viewport top (1-based row 6) downward.
        assert!(s.starts_with("\x1b[6;1H\x1b[J"), "{s:?}");
        // Then park on the last row and emit three newlines.
        assert!(s.contains("\x1b[24;1H"));
        assert_eq!(s.matches("\r\n").count(), 3);
        assert!(s.ends_with("\x1b[3;1H"), "cursor restored: {s:?}");
        assert!(
            !sets_scroll_region(&s),
            "FullScreen never sets a region: {s:?}"
        );
    }

    #[test]
    fn growth_writes_nothing_in_multiplexer_safe_mode() {
        let mut out = Vec::new();
        grow_viewport(&mut out, InsertMode::MultiplexerSafe, 20, 24, 4, (0, 16)).unwrap();
        assert!(out.is_empty());
    }
}
