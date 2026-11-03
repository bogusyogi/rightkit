//! The surface: viewport geometry, screen-mode transitions, and the event loop.

use std::io::{self, Write};
use std::time::{Duration, Instant};

use crate::component::{Event, Outcome};
use crate::terminal::history::{self, HistoryLineWrapPolicy};
use crate::terminal::hyperlinks::HyperlinkLine;
use crate::terminal::{InsertMode, Viewport};

/// Which screen buffer the UI is currently drawing to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenMode {
    /// The normal buffer. Finalized history goes to the terminal's own
    /// scrollback, so it survives us exiting and scrolls natively. This is the
    /// default and where the conversation lives.
    Inline,
    /// The alternate buffer, taken over entirely. Used only by named modal
    /// screens -- a session picker, a diff viewer, an approval dialog -- and
    /// always returned from.
    Alternate,
}

/// Reasons a modal may own the alternate screen.
///
/// Deliberately a closed set. Anything that wants the whole screen must be
/// named here, which keeps "temporarily takes over the terminal" from spreading
/// into ordinary UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalKind {
    SessionPicker,
    AgentPicker,
    DiffViewer,
    ApprovalDialog,
    Transcript,
    Fullscreen,
}

/// Detects Claude-style Esc-Esc while leaving ordinary Escape available to
/// the composer and modal owners. The host calls [`Self::press`] only for a
/// plain Escape key press and passes the current eligibility state.
#[derive(Debug, Clone)]
pub struct DoubleEscape {
    last: Option<Instant>,
    window: Duration,
}

impl Default for DoubleEscape {
    fn default() -> Self {
        Self::new(Duration::from_millis(450))
    }
}

impl DoubleEscape {
    pub fn new(window: Duration) -> Self {
        Self { last: None, window }
    }

    /// Return true for a second eligible Escape within `window`; any
    /// ineligible Escape clears the pending first press.
    pub fn press(&mut self, now: Instant, eligible: bool) -> bool {
        if !eligible {
            self.last = None;
            return false;
        }
        match self.last.take() {
            Some(previous) if now.saturating_duration_since(previous) <= self.window => true,
            _ => {
                self.last = Some(now);
                false
            }
        }
    }

    pub fn clear(&mut self) {
        self.last = None;
    }
}

/// Viewport geometry and screen mode.
///
/// Kept free of I/O so the transitions -- the part that corrupts the display
/// when it goes wrong -- can be tested exhaustively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Surface {
    mode: ScreenMode,
    viewport: Viewport,
    /// The inline viewport, saved while a modal owns the screen.
    ///
    /// Saving and restoring this is a strict pair. Losing it means the inline
    /// viewport's row and height are gone, and the conversation redraws in the
    /// wrong place for the rest of the session.
    saved_viewport: Option<Viewport>,
    modal: Option<ModalKind>,
    screen_width: u16,
    screen_height: u16,
    insert_mode: InsertMode,
    /// A growth the screen still owes scrollback for, as
    /// `(region boundary, row count)`: the viewport's logical top already
    /// accounts for it. Emitted via [`Self::emit_pending_scroll`] before the
    /// next repaint so the displaced rows land in real scrollback (donor
    /// `grow_viewport`); dropped on resize/modal, where the terminal reflows
    /// or the alternate-buffer restore re-created the rows.
    pending_scroll: Option<(u16, u16)>,
    /// Rows the viewport's top moved down inside `insert_history` since
    /// [`Self::take_history_slide`] last ran. The band they released holds
    /// real inserted history and must not be erased as stale paint.
    history_slide: u16,
    /// Normal chat chrome stays at the terminal bottom after launch history.
    bottom_anchored: bool,
    /// Codex Tui::pending_history_lines: insert only after frame geometry settles.
    pending_history_lines: Vec<HyperlinkLine>,
}

impl Surface {
    /// Create an inline surface anchored at the launch cursor row.
    ///
    /// Launch-anchored geometry (Codex `custom_terminal.rs` viewport at
    /// `cursor_pos.y`, donor commit 3074be908ac7f270b1b4781f2f8c55662067b96a,
    /// Apache-2.0): the viewport starts where the shell left the cursor and
    /// grows DOWNWARD into the free rows below it, so shell output above the
    /// launch point is never cleared or painted over. When `height` would
    /// overflow the bottom the settled top moves up and the difference is
    /// recorded in `pending_scroll` -- the caller emits it (scrolling the
    /// displaced rows into real scrollback) before the first repaint.
    pub fn new(
        screen_width: u16,
        screen_height: u16,
        launch_row: u16,
        height: u16,
        insert_mode: InsertMode,
    ) -> Self {
        let height = height.min(screen_height);
        let mut surface = Self {
            mode: ScreenMode::Inline,
            viewport: Viewport {
                top: launch_row.min(screen_height.saturating_sub(1)),
                height,
            },
            saved_viewport: None,
            modal: None,
            screen_width,
            screen_height,
            insert_mode,
            pending_scroll: None,
            history_slide: 0,
            bottom_anchored: false,
            pending_history_lines: Vec::new(),
        };
        surface.settle_overflow();
        surface
    }

    pub fn mode(&self) -> ScreenMode {
        self.mode
    }

    pub fn viewport(&self) -> Viewport {
        self.viewport
    }

    pub fn modal(&self) -> Option<ModalKind> {
        self.modal
    }

    pub fn screen_size(&self) -> (u16, u16) {
        (self.screen_width, self.screen_height)
    }

    /// Pin chat chrome without taking over the terminal's scrollback.
    /// Call after launch history is inserted so that history keeps its row.
    pub fn anchor_to_bottom(&mut self) {
        self.bottom_anchored = true;
        if self.mode == ScreenMode::Inline {
            self.viewport.top = self.screen_height.saturating_sub(self.viewport.height);
        }
        if let Some(saved) = self.saved_viewport.as_mut() {
            saved.top = self.screen_height.saturating_sub(saved.height);
        }
    }

    pub fn queue_history(&mut self, lines: Vec<HyperlinkLine>) {
        self.pending_history_lines.extend(lines);
    }

    pub fn take_pending_history(&mut self) -> Vec<HyperlinkLine> {
        if self.can_insert_history() {
            std::mem::take(&mut self.pending_history_lines)
        } else {
            Vec::new()
        }
    }

    /// Take the alternate screen for a modal.
    ///
    /// Idempotent per modal: entering twice without leaving keeps the *first*
    /// saved viewport, so a nested modal cannot overwrite the inline geometry
    /// with the outer modal's full-screen rect.
    pub fn enter_modal(&mut self, kind: ModalKind) {
        if self.mode == ScreenMode::Inline {
            self.saved_viewport = Some(self.viewport);
        }
        // The alternate-buffer save/restore invalidates any owed scroll and
        // any unreconciled slide: on return the saved viewport repaints over
        // whatever the buffer restored.
        self.pending_scroll = None;
        self.history_slide = 0;
        self.mode = ScreenMode::Alternate;
        self.modal = Some(kind);
        self.viewport = Viewport {
            top: 0,
            height: self.screen_height,
        };
    }

    /// Return to inline, restoring the exact viewport the modal displaced.
    pub fn leave_modal(&mut self) {
        if let Some(saved) = self.saved_viewport.take() {
            self.viewport = saved;
        }
        self.pending_scroll = None;
        self.history_slide = 0;
        self.mode = ScreenMode::Inline;
        self.modal = None;
    }

    /// Adopt a new terminal size.
    ///
    /// A resized terminal reflowed its scrollback already, so no scroll
    /// bytes are owed and nothing is scrolled here (donor skips the growth
    /// scroll on shrink for the same reason). The launch anchor survives a
    /// resize that still fits; a viewport that no longer fits bottom-anchors,
    /// and a grown screen keeps a bottom-pinned viewport pinned.
    pub fn resize(&mut self, width: u16, height: u16) {
        if width == self.screen_width && height == self.screen_height {
            return;
        }
        let old_height = self.screen_height;
        self.screen_width = width;
        self.screen_height = height;
        self.pending_scroll = None;
        self.history_slide = 0;

        self.viewport = Self::reseat(self.viewport, old_height, height);
        // Keep the saved inline geometry valid for the eventual restore.
        if let Some(saved) = self.saved_viewport.as_mut() {
            *saved = Self::reseat(*saved, old_height, height);
        }
    }

    /// Reposition a viewport for a new screen height, preserving the launch
    /// anchor: keep `top` unless the viewport no longer fits (bottom-anchor
    /// it), and keep a bottom-pinned viewport pinned when the screen grows.
    fn reseat(viewport: Viewport, old_height: u16, new_height: u16) -> Viewport {
        let height = viewport.height.min(new_height);
        let was_bottom_pinned = viewport.bottom_exclusive() == old_height;
        let mut top = viewport.top;
        if top.saturating_add(height) > new_height || (new_height > old_height && was_bottom_pinned)
        {
            top = new_height.saturating_sub(height);
        }
        Viewport { top, height }
    }

    /// Grow or shrink the inline viewport -- the composer gaining a line, say.
    ///
    /// Growth extends the bottom edge DOWNWARD into free rows; only when the
    /// new height would overflow the terminal bottom does the top move up,
    /// and the displaced rows are owed a scroll (recorded for
    /// [`Self::emit_pending_scroll`]). Shrinkage keeps the top -- the freed
    /// rows below are released for the caller to erase.
    pub fn set_viewport_height(&mut self, height: u16) {
        if self.mode != ScreenMode::Inline {
            return;
        }
        self.viewport.height = height.clamp(1, self.screen_height);
        self.settle_overflow();
        if self.bottom_anchored {
            self.viewport.top = self.screen_height.saturating_sub(self.viewport.height);
        }
    }

    /// If the viewport extends past the terminal bottom, move its logical
    /// top up and record the screen scroll the growth owes (donor
    /// `grow_viewport` from `tui/scrollback.rs`). Successive growths before
    /// a flush accumulate into one scroll against the original boundary.
    fn settle_overflow(&mut self) {
        let overflow = self
            .viewport
            .bottom_exclusive()
            .saturating_sub(self.screen_height);
        if overflow == 0 {
            return;
        }
        let boundary = self.viewport.top;
        self.viewport.top = boundary.saturating_sub(overflow);
        match &mut self.pending_scroll {
            Some((_, total)) => *total = total.saturating_add(overflow),
            None => self.pending_scroll = Some((boundary, overflow)),
        }
    }

    /// Emit the screen scroll a launch anchor or growth owes, if any, as
    /// `(boundary, count)` consumed from `pending_scroll`. The `count` top
    /// rows of the region above `boundary` commit to real scrollback; the
    /// viewport then repaints at its already-settled top. Returns whether a
    /// scroll was emitted.
    ///
    /// `cursor` is the 0-based position the cursor must hold afterwards.
    pub fn emit_pending_scroll(
        &mut self,
        writer: &mut impl Write,
        cursor: (u16, u16),
    ) -> io::Result<bool> {
        let Some((boundary, count)) = self.pending_scroll.take() else {
            return Ok(false);
        };
        history::grow_viewport(
            writer,
            self.insert_mode,
            boundary,
            self.screen_height,
            count,
            cursor,
        )?;
        Ok(true)
    }

    /// Drop any owed growth scroll without emitting it -- used on frames
    /// where the screen contents were reflowed (resize) or restored
    /// (modal leave) and the scroll must not run.
    pub fn drop_pending_scroll(&mut self) {
        self.pending_scroll = None;
    }

    /// Rows the viewport's top moved down inside `insert_history` since the
    /// last call -- the released band above the new top holds real history
    /// and must not be erased. Drains the count.
    pub fn take_history_slide(&mut self) -> u16 {
        std::mem::take(&mut self.history_slide)
    }

    /// Whether finalized lines may be pushed to scrollback right now.
    ///
    /// Never while a modal owns the alternate screen: that buffer has its own
    /// scrollback, and writing there loses the lines when the modal closes.
    /// Inline needs a band to write into -- rows above the viewport, or free
    /// rows below it the insertion can slide the viewport down over (donor
    /// slide-down in `insert_history.rs`).
    pub fn can_insert_history(&self) -> bool {
        self.mode == ScreenMode::Inline
            && (self.viewport.top > 0 || self.viewport.bottom_exclusive() < self.screen_height)
    }

    /// Push finalized lines into scrollback above the viewport.
    ///
    /// `lines` are styled [`HyperlinkLine`]s (a `Line` plus semantic
    /// hyperlink metadata). They are wrapped inside `history::insert` at the
    /// screen width under `wrap_policy` -- the donor behaviour, which keeps
    /// URL tokens intact and preserves each line's leading whitespace on
    /// continuation rows.
    ///
    /// The viewport's top may slide DOWN to make room (scroll-region
    /// slide-down, or the FullScreen newline path on Windows Terminal); the
    /// surface records the slide so the renderer erases the released band's
    /// stale paint without touching the inserted history.
    pub fn insert_history(
        &mut self,
        writer: &mut impl Write,
        lines: &[HyperlinkLine],
        wrap_policy: HistoryLineWrapPolicy,
        cursor: (u16, u16),
    ) -> io::Result<()> {
        if !self.can_insert_history() {
            return Ok(());
        }
        let slide = history::insert(
            writer,
            self.insert_mode,
            self.viewport,
            self.screen_width,
            self.screen_height,
            lines,
            wrap_policy,
            cursor,
        )?;
        if slide > 0 {
            self.viewport.top = self.viewport.top.saturating_add(slide);
            self.history_slide = self.history_slide.saturating_add(slide);
        }
        Ok(())
    }
}

/// Routes an event to the lane that should render it.
///
/// Input is echoed immediately because typing latency is felt directly.
/// Everything else coalesces.
pub fn lane_for(event: &Event) -> Lane {
    match event {
        Event::Key(_) | Event::Paste(_) | Event::Mouse(_) => Lane::Immediate,
        Event::Focus(_) | Event::Resize { .. } | Event::Resume => Lane::Coalesced,
    }
}

/// Which scheduler lane should carry a redraw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// Draw as soon as the frame budget allows.
    Immediate,
    /// Fold into the next coalesced frame.
    Coalesced,
}

/// Dispatch an event through components in focus order, stopping at the first
/// that handles it.
pub fn dispatch(components: &mut [&mut dyn crate::component::Component], event: &Event) -> Outcome {
    for component in components.iter_mut() {
        let outcome = component.handle(event);
        if outcome.is_terminal() {
            return outcome;
        }
    }
    Outcome::Ignored
}
