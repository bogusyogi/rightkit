//! Conversation rendering: user messages, streaming and finalized assistant
//! output, and system notices.
//!
//! # Shape
//!
//! A [`Transcript`] holds every message for the session, split at
//! `flushed` into two halves:
//!
//! - `entries[..flushed]` have already been pushed into the terminal's own
//!   scrollback (see `terminal::history::insert`) and are kept only so a
//!   later feature (scrollback recall, a `Transcript` modal) can still see
//!   them.
//! - `entries[flushed..]` are the *live* messages: still streaming, or
//!   finalized but not yet drained into scrollback lines. Only these are
//!   drawn by [`Component::render`].
//!
//! [`Transcript::drain_finalized`] moves the finalized prefix of the live
//! entries into scrollback-ready `Vec<String>` and advances `flushed`, which
//! is the boundary `terminal::history::insert` expects the caller to
//! maintain.
//!
//! # Why streaming a token is cheap
//!
//! Each entry owns its own [`cache::WidthCache`]. Appending a token touches
//! only the last entry's content hash, so `Transcript::ensure_rendered`
//! recomputes markdown parsing and wrapping for that one entry and reuses the
//! cached render for everything else -- the core requirement this module
//! exists to satisfy. See the `transcript_streaming` test module for the
//! proof (cache hit/miss counts, not a comment).

pub mod cache;
pub mod find;
pub mod history_pager;
pub mod markdown;
pub mod timeline;
pub mod wrap;

// Donor markdown renderer closure (codex-rs/tui, 444da310, Apache-2.0).
// `markdown` keeps the REN-003 streaming gate plus these entry points;
// `markdown_render` + helpers are the actual lifted implementation.
pub(crate) mod assistant_directives;
pub(crate) mod color;
pub(crate) mod highlight;
pub(crate) mod inline_viz;
pub(crate) mod line_truncation;
pub(crate) mod live_wrap;
pub(crate) mod markdown_copy;
pub(crate) mod markdown_render;
pub(crate) mod markdown_stream;
pub(crate) mod markdown_text_merge;
pub(crate) mod model_themes;
pub(crate) mod style;
pub(crate) mod table_detect;
pub(crate) mod terminal_palette;
pub(crate) mod width;
pub(crate) mod wrapping;

use crate::component::RenderFrameExt as _;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::component::{Component, Event, Outcome, Role, Theme};
use crate::render::cell::Cell;
use crate::render::sanitize;
use crate::terminal::hyperlinks::HyperlinkLine;
use cache::WidthCache;

/// Who produced a message. Drives the header line and, for the assistant,
/// whether that header reads as in-progress or settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Speaker {
    User,
    Assistant,
    System,
}

/// One turn in the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Message {
    pub speaker: Speaker,
    pub content: String,
    /// Whether this message can still receive appended tokens. A finalized
    /// message is eligible for `drain_finalized`; a streaming one is not.
    pub finalized: bool,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            speaker: Speaker::User,
            content: content.into(),
            finalized: true,
        }
    }

    /// A streaming assistant message, empty until tokens are appended.
    pub fn assistant_streaming() -> Self {
        Self {
            speaker: Speaker::Assistant,
            content: String::new(),
            finalized: false,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            speaker: Speaker::Assistant,
            content: content.into(),
            finalized: true,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            speaker: Speaker::System,
            content: content.into(),
            finalized: true,
        }
    }
}

/// One wrapped, styled output line: a [`HyperlinkLine`] (styled spans plus
/// hyperlink metadata) plus an optional theme role for header lines that the
/// ambient theme still owns.
#[derive(Debug, Clone, PartialEq)]
struct RenderedLine {
    line: HyperlinkLine,
    /// `Some((role, bold))` for lines the theme styles at draw time (the
    /// speaker header); `None` for donor-rendered content lines whose spans
    /// already carry their final styles.
    theme_role: Option<(Role, bool)>,
}

impl RenderedLine {
    fn styled(line: HyperlinkLine) -> Self {
        Self {
            line,
            theme_role: None,
        }
    }
}

/// A settled typed cell (exec, reasoning, decision, notice…) plus the theme
/// ambient at push time. The terminal's theme is fixed for a session, and
/// `Cell::display_lines` re-resolves roles through it on every projection,
/// so width-aware reflow never reparses text or loses donor styling.
struct CellEntry {
    cell: Box<dyn Cell>,
    theme: Theme,
}

/// What one transcript entry actually holds: a markdown `Message` or a
/// settled typed cell. Cells enter through [`Transcript::push_cell`] already
/// finalized; their own `display_lines` is the only renderer, so styled
/// spans survive flush, resize, and eviction without a markdown reparse.
enum EntryBody {
    Message(Message),
    Cell(CellEntry),
}

impl std::fmt::Debug for EntryBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Message(m) => f.debug_tuple("Message").field(m).finish(),
            Self::Cell(_) => f.debug_struct("Cell").finish_non_exhaustive(),
        }
    }
}

/// A message or cell plus the memoized render of its last-seen width and
/// content.
#[derive(Debug)]
struct Entry {
    body: EntryBody,
    cache: WidthCache<Vec<RenderedLine>>,
    /// A user/assistant message that follows a settled cell (tool rows, a
    /// thought) opens with one blank row, so blocks never run together.
    lead_gap: bool,
}

/// Cache key for cell entries. A pushed cell is immutable — its content
/// never changes after settlement — so a constant fingerprint is exact;
/// the `WidthCache` still keys on width, so reflow recomputes correctly.
const CELL_CACHE_HASH: u64 = 0;

/// Markdown-parse, header-prefix, and width-wrap one message.
///
/// Free function (not a method) so both the memoized path in
/// [`Transcript::ensure_rendered`] and the unmemoized estimate in
/// `desired_height` share exactly one implementation.
fn render_message(message: &Message, width: u16, theme: &Theme) -> Vec<RenderedLine> {
    // The user's own message is a raised panel carrying the brand rail, the
    // same `▌` the composer wears — no speaker header. A system notice is a
    // plain dim line, also without a header. Only the assistant keeps one.
    match message.speaker {
        Speaker::User => return render_user_panel(&message.content, width, theme),
        Speaker::System => return render_system_notice(&message.content, width, theme),
        Speaker::Assistant => {}
    }
    // Assistant prose is plain text: no speaker prefix. A route divider is
    // drawn by the host only when the actor or model actually changes.
    let mut out: Vec<RenderedLine> = Vec::new();

    // The donor markdown renderer (pulldown-cmark → styled HyperlinkLines)
    // is the production path. Message content was already sanitized at
    // ingestion, so spans here contain no control bytes; OSC-8 links are
    // renderer-emitted metadata, not source escapes. Agent messages take the
    // fenced-table-normalizing agent path.
    let body = markdown::render_markdown_agent_with_list_spacing(
        &message.content,
        Some(usize::from(width).saturating_sub(2).max(1)),
        /*cwd*/ None,
        /*inline_visualization_context*/ None,
        markdown_render::ListSpacing::Uniform,
    );
    out.extend(
        body.into_iter()
            .map(|line| RenderedLine::styled(line.indented(2))),
    );
    if message.finalized {
        out.push(RenderedLine::styled(HyperlinkLine::new(Line::from(
            Span::styled("  copy · c", Style::default().fg(theme.color(Role::Dim))),
        ))));
    }
    // A settled answer ends with one blank row so the next prompt, tool row
    // or notice never runs into its last line.
    if message.finalized {
        out.push(RenderedLine::styled(HyperlinkLine::new(Line::from(""))));
    }

    out
}

/// `▌ text` rows on the raised panel, padded to the full width so the panel
/// reads as one block. Text is literal (wrapped, never markdown-parsed): what
/// the user typed is what they see. A blank row follows to separate the turn.
fn render_user_panel(content: &str, width: u16, theme: &Theme) -> Vec<RenderedLine> {
    let panel = |style: Style| match theme.panel_bg() {
        Some(bg) => style.bg(bg),
        None => style,
    };
    let rail = panel(Style::default().fg(theme.accent()));
    let body = panel(Style::default());
    let wrap_width = width.saturating_sub(2).max(1);
    let content = content.trim_end_matches(['\r', '\n']);
    let mut rows: Vec<String> = Vec::new();
    for logical in content.split('\n') {
        rows.extend(wrap::wrap(logical, wrap_width));
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    let mut out: Vec<RenderedLine> = rows
        .into_iter()
        .map(|row| {
            let used = 2 + wrap::display_width(&row);
            let pad = usize::from(width).saturating_sub(used);
            let mut spans = vec![
                Span::styled("▌".to_string(), rail),
                Span::styled(" ".to_string(), body),
                Span::styled(row, body),
            ];
            if pad > 0 {
                spans.push(Span::styled(" ".repeat(pad), body));
            }
            RenderedLine::styled(HyperlinkLine::new(Line::from(spans)))
        })
        .collect();
    out.push(RenderedLine::styled(HyperlinkLine::new(Line::from(""))));
    out
}

/// A system notice: dim lines indented to the transcript gutter (the same two
/// columns tool rows use), no `System` header.
fn render_system_notice(content: &str, width: u16, theme: &Theme) -> Vec<RenderedLine> {
    let dim = theme.color(Role::Dim);
    let inner = width.saturating_sub(2).max(1);
    let content = escape_backslashes_outside_code(content);
    markdown_render::render_markdown_lines_with_width_and_cwd(
        &content,
        Some(usize::from(inner)),
        None,
    )
    .into_iter()
    .map(|line| RenderedLine::styled(line.indented(2).style(Style::default().fg(dim))))
    .collect()
}

/// Runtime notices carry Windows paths (`\?\D:\Claude\.audit`). Markdown reads
/// a backslash before punctuation as an escape and eats it, so double every
/// backslash that is not inside a code span or fence, where it is already literal.
fn escape_backslashes_outside_code(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    let mut in_fence = false;
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push_str(line);
            continue;
        }
        if in_fence {
            out.push_str(line);
            continue;
        }
        let mut in_code = false;
        for ch in line.chars() {
            match ch {
                '`' => {
                    in_code = !in_code;
                    out.push(ch);
                }
                '\\' if !in_code => out.push_str("\\\\"),
                _ => out.push(ch),
            }
        }
    }
    out
}

/// Maximum live (unflushed) entries retained in memory — enforced strictly,
/// regardless of flush state (P2-5). Because this caps the WHOLE structure
/// (flushed entries are a prefix of `entries`), it is simultaneously the
/// total bound: RSS no longer grows with transcript length, and older
/// ranges are paged back from SQLite via `before`/`limit`
/// (`api_session_config_messages_title.rs:AfterQuery`,
/// `paging_window` below).
const BOUNDED_LIVE_MAX: usize = 150;

/// Conversation content: user messages, assistant output, and system
/// notices.
#[derive(Debug, Default)]
pub struct Transcript {
    entries: Vec<Entry>,
    /// Markdown that is still unsafe to render for the open assistant turn.
    /// Stable prefixes are appended to the message as deltas arrive; the
    /// retained tail is forced into the final message at settlement.
    streaming_markdown: markdown::StreamingMarkdownBuffer,
    /// Entries at indices `[0, flushed)` are already in scrollback.
    flushed: usize,
    /// Number of entries evicted due to bounding. Exposed for paging tests
    /// and for the daemon's `before` cursor (oldest retained seq = evicted).
    evicted: usize,
    /// Entries evicted while still unflushed (never shown in scrollback).
    /// Strict bounding (P2-5) may evict below the flush boundary; those
    /// entries are stashed here — NOT dropped — until the host drains them
    /// into terminal scrollback, so eviction never destroys unseen content.
    /// Whole `Entry`s are stashed, so an unflushed *cell* keeps its styled
    /// renderer instead of degrading to plain text.
    overflow: Vec<Entry>,
    /// Theme the message panels resolve their colours through. Fixed for a
    /// session; defaults to a dark true-colour palette until the host sets it.
    theme: Option<Theme>,
}

impl Transcript {
    pub fn new() -> Self {
        Self {
            theme: None,
            entries: Vec::new(),
            streaming_markdown: markdown::StreamingMarkdownBuffer::new(),
            flushed: 0,
            evicted: 0,
            overflow: Vec::new(),
        }
    }

    /// Bind the theme message panels (user rail, dim notices) resolve through.
    /// Entries already rendered keep their cached width-keyed output, so call
    /// this before the first push.
    pub fn set_theme(&mut self, theme: &Theme) {
        self.theme = Some(*theme);
    }

    fn message_theme(&self) -> Theme {
        self.theme.unwrap_or_else(|| {
            Theme::new(
                crate::terminal::Background::Dark,
                crate::terminal::ColorDepth::TrueColor,
            )
        })
    }

    pub fn push(&mut self, mut message: Message) {
        // A system/user message can arrive between assistant deltas. Preserve
        // the preceding assistant's retained tail before changing the last
        // entry, so no streamed bytes are stranded in the buffer.
        self.flush_stream_tail();
        // Trust boundary: untrusted content is sanitized once here so the
        // renderer and scrollback writer never see control bytes (ESC, C1
        // CSI, BEL). Newlines and tabs survive as structure.
        message.content = sanitize(&message.content);
        let lead_gap =
            self.last_is_cell() && matches!(message.speaker, Speaker::User | Speaker::Assistant);
        self.entries.push(Entry {
            body: EntryBody::Message(message),
            cache: WidthCache::new(),
            lead_gap,
        });
        self.enforce_bounds();
    }

    /// Push a settled typed cell (exec block, reasoning summary, approval
    /// decision, notice) as one finalized entry.
    ///
    /// This is the styled-history seam: the cell's own width-aware
    /// `display_lines` renders it in the live tail, at the final flush into
    /// scrollback, on resize reflow, and inside unflushed-eviction salvage —
    /// the styled spans are never flattened through a `Message::system`
    /// plain-text round-trip or a markdown reparse. Cells sanitize untrusted
    /// text at construction (see `render::cell`), so no second pass happens
    /// here. `theme` is captured only to satisfy `&self` projections; the
    /// terminal theme is fixed for a session.
    pub fn push_cell(&mut self, cell: Box<dyn Cell>, theme: &Theme) {
        // Same stream-tail contract as `push`: a cell can arrive between
        // assistant deltas, so settle the retained tail first.
        self.flush_stream_tail();
        self.entries.push(Entry {
            body: EntryBody::Cell(CellEntry {
                cell,
                theme: *theme,
            }),
            cache: WidthCache::new(),
            lead_gap: false,
        });
        self.enforce_bounds();
    }

    fn last_is_cell(&self) -> bool {
        matches!(
            self.entries.last(),
            Some(Entry {
                body: EntryBody::Cell(_),
                ..
            })
        )
    }

    /// Oldest evicted count (for paging cursor).
    pub fn evicted_count(&self) -> usize {
        self.evicted
    }

    /// Total retained entries (flushed + live).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Page window for older ranges: returns `(before_seq_hint, limit)` when
    /// older history has been evicted and the live window is bounded. The
    /// production caller (`tui_host`'s maintenance tick) uses this both as
    /// the trigger and the page size when fetching older durable history
    /// from the daemon's SQLite ledger.
    pub fn paging_window(&self) -> Option<(usize, usize)> {
        if self.evicted > 0 {
            Some((self.evicted, BOUNDED_LIVE_MAX))
        } else {
            None
        }
    }

    fn enforce_bounds(&mut self) {
        // P2-5: strict live bound regardless of flush state. The old logic
        // capped eviction by `flushed`, so memory could hold up to 300
        // entries and more than 150 unflushed; the authored 150-live-bound
        // test could logically never pass. Now the oldest entries are evicted
        // even while unflushed — but never silently: unflushed evictees have
        // never been shown, so they are stashed for the host to insert into
        // terminal scrollback before they leave memory.
        while self.entries.len() > BOUNDED_LIVE_MAX {
            let evict = self.entries.len() - BOUNDED_LIVE_MAX;
            let flushed_evict = evict.min(self.flushed);
            for (idx, entry) in self.entries.drain(0..evict).enumerate() {
                if idx >= self.flushed {
                    self.overflow.push(entry);
                }
            }
            self.flushed -= flushed_evict;
            self.evicted += evict;
        }
    }

    /// Entries evicted while still unflushed, awaiting scrollback salvage by
    /// the host.
    pub fn eviction_overflow_len(&self) -> usize {
        self.overflow.len()
    }

    /// Render every stashed unflushed evictee at `width` into scrollback-
    /// ready lines (headers included) and empty the stash. The host inserts
    /// these through its ordinary `Surface::insert_history` flow so content
    /// evicted before being shown stays visible in terminal scrollback.
    pub fn drain_eviction_overflow_lines(&mut self, width: u16) -> Vec<HyperlinkLine> {
        let stashed = std::mem::take(&mut self.overflow);
        let theme = self.message_theme();
        let mut out = Vec::new();
        for entry in &stashed {
            out.extend(entry.render(width, &theme).into_iter().map(|l| l.line));
        }
        out
    }

    /// Drop every entry and reset the flush boundary, so a session switch can
    /// start a clean view of the new session's transcript (P1-5: switching
    /// root sessions must not keep the old tree's messages on screen).
    pub fn clear(&mut self) {
        self.entries.clear();
        self.streaming_markdown = markdown::StreamingMarkdownBuffer::new();
        self.flushed = 0;
        self.evicted = 0;
        self.overflow.clear();
    }

    /// Append a streamed token to the last message, if it is an
    /// unfinalized assistant message. No-op otherwise -- a caller streaming
    /// into a transcript with no open assistant turn has a bug, but it is
    /// not this method's job to panic over it.
    pub fn append_stream(&mut self, token: &str) {
        let Some(index) = self.entries.len().checked_sub(1) else {
            return;
        };
        let open = matches!(
            &self.entries[index].body,
            EntryBody::Message(m) if m.speaker == Speaker::Assistant && !m.finalized
        );
        if !open {
            return;
        }

        let released = self.streaming_markdown.push(token);
        if let Some(released) = released {
            // Same trust boundary as `push`, applied to released stream chunks.
            if let EntryBody::Message(m) = &mut self.entries[index].body {
                m.content.push_str(&sanitize(&released));
            }
        }
    }

    /// Reconcile completed prose with its open stream, or insert a completed
    /// message when no deltas arrived. The canonical text replaces the stream.
    pub fn complete_assistant(&mut self, text: &str) {
        self.streaming_markdown.flush();
        if let Some(index) = self.pending_assistant_index()
            && let EntryBody::Message(message) = &mut self.entries[index].body
        {
            message.content = sanitize(text);
            message.finalized = true;
            return;
        }
        if !text.is_empty() {
            self.push(Message::assistant(text));
        }
    }

    /// Find the current turn's assistant across intervening notices/cells.
    /// Cache accounting can arrive between its final delta and completion.
    /// A user boundary or an already-settled assistant ends the search.
    fn pending_assistant_index(&self) -> Option<usize> {
        for (index, entry) in self.entries.iter().enumerate().rev() {
            if let EntryBody::Message(message) = &entry.body {
                match message.speaker {
                    Speaker::User => return None,
                    Speaker::Assistant => return (!message.finalized).then_some(index),
                    Speaker::System => {}
                }
            }
        }
        None
    }

    /// Mark the current assistant finalized, making it eligible for
    /// [`Self::drain_finalized`].
    pub fn finalize_last(&mut self) {
        let Some(index) = self.pending_assistant_index() else {
            self.streaming_markdown.flush();
            return;
        };
        let can_finalize = matches!(
            &self.entries[index].body,
            EntryBody::Message(m) if m.speaker == Speaker::Assistant && !m.finalized
        );
        let tail = self.streaming_markdown.flush();
        if can_finalize && let EntryBody::Message(m) = &mut self.entries[index].body {
            m.content.push_str(&sanitize(&tail));
            m.finalized = true;
        }
    }

    /// Append any retained unsafe suffix to the currently open assistant.
    /// This is used before another message enters the transcript so the
    /// buffer remains associated with exactly one streaming turn.
    fn flush_stream_tail(&mut self) {
        let tail = self.streaming_markdown.flush();
        if tail.is_empty() {
            return;
        }
        if let Some(entry) = self.entries.last_mut()
            && let EntryBody::Message(m) = &mut entry.body
            && m.speaker == Speaker::Assistant
            && !m.finalized
        {
            m.content.push_str(&sanitize(&tail));
        }
    }

    /// Recompute (or reuse from cache) the render of every live entry at
    /// `width`. Shared by `render` and the test suite, which inspects cache
    /// hit/miss counts this leaves behind.
    fn ensure_rendered(&mut self, width: u16) {
        let theme = self.message_theme();
        for entry in &mut self.entries[self.flushed..] {
            let hash = entry.cache_key();
            entry.cache.get_or_compute(width, hash, || {
                render_body(&entry.body, entry.lead_gap, width, &theme)
            });
        }
    }

    /// Sum of (hits, misses) across every live entry's cache. Exposed so a
    /// test can prove memoization is doing something, not just assert it.
    pub fn cache_stats(&self) -> (u64, u64) {
        self.entries[self.flushed..]
            .iter()
            .fold((0, 0), |(h, m), e| {
                (h + e.cache.hits(), m + e.cache.misses())
            })
    }

    /// Move the finalized prefix of the live entries into scrollback-ready,
    /// pre-wrapped lines, and advance the flush boundary past them.
    ///
    /// Stops at the first still-streaming message: flushing must preserve
    /// message order, and a message later in the conversation cannot be
    /// pushed to scrollback while an earlier one is still open.
    /// Returns styled [`HyperlinkLine`]s for `Surface::insert_history` — the
    /// lines carry their donor-rendered spans plus hyperlink metadata, which
    /// `history::insert` emits as trusted OSC-8 decorations.
    pub fn drain_finalized(&mut self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = Vec::new();
        let theme = self.message_theme();

        while self.flushed < self.entries.len() && self.entries[self.flushed].is_finalized() {
            let entry = &mut self.entries[self.flushed];
            let hash = entry.cache_key();
            let rendered = entry.cache.get_or_compute(width, hash, || {
                render_body(&entry.body, entry.lead_gap, width, &theme)
            });
            lines.extend(rendered.iter().map(|l| l.line.clone()));
            self.flushed += 1;
        }

        lines
    }

    /// Rendered height of the live (unflushed) tail at `width`, capped at
    /// `cap` rows. Shares the width-keyed render cache with `render` and
    /// `drain_finalized`, so calling it every frame is hits-only once a frame
    /// has drawn at this width — unlike `desired_height`, which recomputes
    /// and is for `&self` contexts only.
    pub fn live_tail_height(&mut self, width: u16, cap: u16) -> u16 {
        self.ensure_rendered(width);
        let total: usize = self.entries[self.flushed..]
            .iter()
            .map(|e| e.cache.peek().map_or(0, |r| r.len()))
            .sum();
        (total as u16).min(cap)
    }

    /// Live (unflushed) messages, for tests and for a future scrollback/nav
    /// view. Cell entries are not `Message`s and are skipped here; their
    /// rendering goes through `display_lines` on every projection.
    pub fn live_messages(&self) -> impl Iterator<Item = &Message> {
        self.entries[self.flushed..]
            .iter()
            .filter_map(|e| e.message())
    }

    /// Latest complete assistant source as Markdown for the copy action.
    pub fn latest_assistant_markdown(&self) -> Option<&str> {
        self.entries.iter().rev().find_map(|entry| {
            entry.message().and_then(|message| {
                (message.speaker == Speaker::Assistant && message.finalized)
                    .then_some(message.content.as_str())
            })
        })
    }

    /// Number of settled assistant messages available to keyboard copy.
    pub fn assistant_markdown_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| {
                entry.message().is_some_and(|message| {
                    message.speaker == Speaker::Assistant && message.finalized
                })
            })
            .count()
    }

    /// Settled assistant Markdown by chronological assistant-message index.
    pub fn assistant_markdown_at(&self, index: usize) -> Option<&str> {
        self.entries
            .iter()
            .filter_map(|entry| {
                entry.message().and_then(|message| {
                    (message.speaker == Speaker::Assistant && message.finalized)
                        .then_some(message.content.as_str())
                })
            })
            .nth(index)
    }

    /// Code block payloads in latest settled assistant Markdown, in source order.
    pub fn latest_assistant_code_blocks(&self) -> Vec<(Option<String>, String)> {
        self.latest_assistant_markdown()
            .map_or_else(Vec::new, Self::code_blocks_for)
    }

    pub fn assistant_markdown_at_line(&self, line_index: usize, width: u16) -> Option<&str> {
        let theme = self.message_theme();
        let mut start = 0usize;
        for entry in &self.entries {
            let rendered = entry.render(width, &theme);
            let end = start + rendered.len();
            if (start..end).contains(&line_index) {
                return entry.message().and_then(|message| {
                    (message.speaker == Speaker::Assistant && message.finalized)
                        .then_some(message.content.as_str())
                });
            }
            start = end;
        }
        None
    }

    pub fn assistant_code_blocks_at_line(
        &self,
        line_index: usize,
        width: u16,
    ) -> Vec<(Option<String>, String)> {
        self.assistant_markdown_at_line(line_index, width)
            .map_or_else(Vec::new, Self::code_blocks_for)
    }

    fn code_blocks_for(markdown: &str) -> Vec<(Option<String>, String)> {
        markdown::extract_copy_targets(markdown)
            .into_iter()
            .filter_map(|target| match target {
                markdown::CopyTarget::Code { language, content } => {
                    Some((language, content.to_string()))
                }
                markdown::CopyTarget::Quote(_) => None,
            })
            .collect()
    }

    /// Plain-text snapshot for the full-screen transcript reader. This is a
    /// projection of the existing bounded transcript, including entries
    /// already flushed to terminal scrollback; it owns no additional history.
    pub fn inspection_lines(&self, width: u16) -> Vec<String> {
        let theme = self.message_theme();
        self.entries
            .iter()
            .flat_map(|entry| entry.render(width, &theme))
            .map(|rendered| {
                rendered
                    .line
                    .line()
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }
}

impl Entry {
    /// The message body, when this entry is one.
    fn message(&self) -> Option<&Message> {
        match &self.body {
            EntryBody::Message(m) => Some(m),
            EntryBody::Cell(_) => None,
        }
    }

    /// Cells are always settled; messages carry their own `finalized` flag.
    fn is_finalized(&self) -> bool {
        match &self.body {
            EntryBody::Message(m) => m.finalized,
            EntryBody::Cell(_) => true,
        }
    }

    /// Fingerprint for the width cache. A cell's content is immutable after
    /// push, so its key is a constant; a message keys on its full content so
    /// a single appended token invalidates only its own entry.
    fn cache_key(&self) -> u64 {
        match &self.body {
            EntryBody::Message(m) => cache::hash_content(m) ^ u64::from(self.lead_gap),
            EntryBody::Cell(_) => CELL_CACHE_HASH,
        }
    }

    /// Render this entry at `width`.
    fn render(&self, width: u16, theme: &Theme) -> Vec<RenderedLine> {
        render_body(&self.body, self.lead_gap, width, theme)
    }
}

/// Render an entry body at `width`. Messages take the markdown path (with a
/// themed speaker header); cells delegate to their own `display_lines`,
/// whose spans already carry final styles and need no theme role. Free
/// function so `WidthCache::get_or_compute` closures borrow only `body`
/// alongside the mutable `cache` borrow.
fn render_body(body: &EntryBody, lead_gap: bool, width: u16, theme: &Theme) -> Vec<RenderedLine> {
    match body {
        EntryBody::Message(m) => {
            let mut rows = render_message(m, width, theme);
            if lead_gap {
                rows.insert(0, RenderedLine::styled(HyperlinkLine::new(Line::from(""))));
            }
            rows
        }
        EntryBody::Cell(c) => c
            .cell
            .display_lines(width, &c.theme)
            .into_iter()
            .map(|line| RenderedLine::styled(HyperlinkLine::new(line)))
            .collect(),
    }
}

impl Component for Transcript {
    fn render(&mut self, frame: &mut dyn crate::component::RenderFrame, area: Rect, theme: &Theme) {
        self.ensure_rendered(area.width);

        let mut lines: Vec<Line<'static>> = Vec::new();
        for entry in &self.entries[self.flushed..] {
            // `ensure_rendered` just computed this at `area.width`; reading
            // it back here is guaranteed to hit and never recomputes.
            let rendered = entry
                .cache
                .peek()
                .expect("ensure_rendered just populated this entry's cache");

            for rl in rendered {
                if let Some((role, bold)) = rl.theme_role {
                    let mut style = Style::default().fg(theme.color(role));
                    if bold {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    let mut line = rl.line.line().clone();
                    for span in &mut line.spans {
                        span.style = style;
                    }
                    lines.push(line);
                } else {
                    lines.push(rl.line.line().clone());
                }
            }
        }

        let height = area.height as usize;
        let start = lines.len().saturating_sub(height);
        let visible = lines.split_off(start);

        frame.render_widget(Paragraph::new(visible), area);
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        match event {
            // Resize invalidates nothing here directly -- `ensure_rendered`
            // keys on width already, so the next render naturally re-wraps
            // whatever changed and reuses whatever didn't.
            Event::Resize { .. } => Outcome::Redraw,
            _ => Outcome::Ignored,
        }
    }

    fn desired_height(&self, width: u16, available: u16) -> u16 {
        let theme = self.message_theme();
        let total: usize = self.entries[self.flushed..]
            .iter()
            .map(|e| e.render(width, &theme).len())
            .sum();
        (total as u16).min(available)
    }
}

#[cfg(test)]
mod transcript_streaming {
    use super::*;

    #[test]
    fn completion_after_cache_notice_reconciles_and_drains_stream_once() {
        let mut t = Transcript::new();
        t.push(Message::user("first prompt"));
        t.push(Message::assistant_streaming());
        t.append_stream("streamed reply **unfinished");
        t.push(Message::system("Prompt cache miss"));
        t.complete_assistant("streamed reply **finished**");
        t.finalize_last();

        let lines = t.drain_finalized(80);
        let text = lines
            .iter()
            .map(|line| {
                line.line()
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text.matches("streamed reply").count(), 1, "{text}");
        assert!(
            text.contains("finished"),
            "canonical completed source wins: {text}"
        );
        assert!(
            text.find("streamed reply") < text.find("Prompt cache miss"),
            "{text}"
        );
        assert_eq!(t.live_messages().count(), 0);
        assert!(t.drain_finalized(80).is_empty());

        t.push(Message::user("next prompt"));
        t.push(Message::assistant("next reply"));
        assert!(
            !t.drain_finalized(80).is_empty(),
            "next turn must not be blocked"
        );
        assert_eq!(t.live_messages().count(), 0);
    }

    #[test]
    fn settlement_after_notice_and_cell_does_not_strand_assistant() {
        let mut t = Transcript::new();
        let theme = t.message_theme();
        t.push(Message::user("prompt"));
        t.push(Message::assistant_streaming());
        t.append_stream("reply **unfinished");
        t.push(Message::system("accounting notice"));
        t.push_cell(
            Box::new(crate::render::cell::PlainCell::new(vec![Line::raw(
                "expanded output",
            )])),
            &theme,
        );
        t.finalize_last();
        assert!(!t.drain_finalized(80).is_empty());
        assert_eq!(t.live_messages().count(), 0);
        assert_eq!(
            t.flushed,
            t.entries.len(),
            "expanded cell also reaches history"
        );
    }

    #[test]
    fn unstable_markdown_tail_stays_hidden_until_settlement() {
        let mut t = Transcript::new();
        t.push(Message::assistant_streaming());

        t.append_stream("stable prefix **unfinished");
        let live = t.live_messages().next().unwrap();
        assert_eq!(live.content, "stable prefix ");
        assert!(!live.finalized);

        t.finalize_last();
        let settled = t.live_messages().next().unwrap();
        assert_eq!(settled.content, "stable prefix **unfinished");
        assert!(settled.finalized);
    }

    #[test]
    fn stable_prefixes_release_incrementally_without_duplication() {
        let mut t = Transcript::new();
        t.push(Message::assistant_streaming());

        t.append_stream("before [a link");
        assert_eq!(t.live_messages().next().unwrap().content, "before ");
        t.append_stream("](https://example.test) after");
        assert_eq!(
            t.live_messages().next().unwrap().content,
            "before [a link](https://example.test) after"
        );
    }

    #[test]
    fn settlement_reassembles_all_markdown_chunks_exactly_once() {
        let chunks = [
            "intro\n```rust\nfn main() {\n",
            "    println!(\"hi\");\n```\nthen **bo",
            "ld** and [link",
            "](https://example.test",
        ];
        let expected: String = chunks.concat();
        let mut t = Transcript::new();
        t.push(Message::assistant_streaming());
        for chunk in chunks {
            t.append_stream(chunk);
        }

        assert_ne!(
            t.live_messages().next().unwrap().content,
            expected,
            "an unstable tail must remain hidden before settlement"
        );
        t.finalize_last();
        assert_eq!(t.live_messages().next().unwrap().content, expected);

        let first_drain = t.drain_finalized(80);
        assert!(!first_drain.is_empty());
        assert!(
            t.drain_finalized(80).is_empty(),
            "settled output drains once"
        );
    }

    #[test]
    fn appending_a_token_only_recomputes_the_streaming_message() {
        let mut t = Transcript::new();
        t.push(Message::user("hello"));
        t.push(Message::assistant("settled reply one"));
        t.push(Message::assistant("settled reply two"));
        t.push(Message::assistant_streaming());

        t.ensure_rendered(40);
        let (hits, misses) = t.cache_stats();
        assert_eq!(hits, 0);
        assert_eq!(misses, 4, "first render is a miss for every entry");

        for _ in 0..5 {
            t.append_stream("more words ");
            t.ensure_rendered(40);
        }

        let (hits, misses) = t.cache_stats();
        // Each of the 5 appends: the 3 settled entries hit cache, the
        // streaming one misses (its content changed).
        assert_eq!(hits, 15, "settled entries must be reused, not recomputed");
        assert_eq!(
            misses, 9,
            "4 initial + 5 recomputes of the streaming entry only"
        );
    }

    #[test]
    fn rendering_at_an_unchanged_width_with_no_new_content_is_all_hits() {
        let mut t = Transcript::new();
        t.push(Message::user("hello"));
        t.push(Message::assistant("a settled reply"));
        t.ensure_rendered(40);
        t.ensure_rendered(40);
        t.ensure_rendered(40);
        let (hits, misses) = t.cache_stats();
        assert_eq!(misses, 2);
        assert_eq!(hits, 4);
    }
}

#[cfg(test)]
mod resize_and_flush {
    use super::*;

    #[test]
    fn resizing_re_wraps_without_losing_content() {
        let mut t = Transcript::new();
        t.push(Message::assistant(
            "the quick brown fox jumps over the lazy dog again and again and again",
        ));

        t.ensure_rendered(80);
        let (_, misses_wide) = t.cache_stats();

        t.ensure_rendered(20);
        let (_, misses_narrow) = t.cache_stats();
        assert!(
            misses_narrow > misses_wide,
            "a width change must invalidate the cache"
        );

        // Reconstruct the words from the narrow render and confirm nothing
        // was dropped by the rewrap.
        let rendered = t.entries[0].render(20, &t.message_theme());
        let joined: String = rendered
            .iter()
            .flat_map(|l| l.line.line().spans.iter().map(|s| s.content.as_ref()))
            .collect::<Vec<_>>()
            .join(" ");
        for word in "the quick brown fox jumps over the lazy dog again and again and again"
            .split_whitespace()
        {
            assert!(joined.contains(word), "lost {word:?} on resize");
        }
    }

    #[test]
    fn draining_stops_at_the_first_unfinalized_message_preserving_order() {
        let mut t = Transcript::new();
        t.push(Message::user("first"));
        t.push(Message::assistant("second"));
        t.push(Message::assistant_streaming());
        t.append_stream("still going");

        let lines = t.drain_finalized(80);
        assert!(!lines.is_empty());
        assert_eq!(t.flushed, 2);
        assert_eq!(t.live_messages().count(), 1);
        assert!(!t.live_messages().next().unwrap().finalized);
    }

    #[test]
    fn a_wide_character_message_produces_lines_no_wider_than_the_wrap_width() {
        let mut t = Transcript::new();
        t.push(Message::assistant(
            "文字文字文字文字 😀😀😀😀😀 plain text too",
        ));
        t.ensure_rendered(10);
        for entry in &t.entries {
            let rendered = entry.render(
                10,
                &Theme::new(
                    crate::terminal::Background::Dark,
                    crate::terminal::ColorDepth::TrueColor,
                ),
            );
            for line in rendered.iter().skip(1) {
                assert!(
                    line.line.width() <= 10,
                    "line {:?} exceeds width 10",
                    line.line.line()
                );
            }
        }
    }
}

#[cfg(test)]
mod bounded_window {
    use super::*;

    /// P2-5: the live bound of 150 is STRICT — it holds even when nothing
    /// has been flushed (the old eviction was capped by `flushed` and could
    /// retain up to 300 total, so this exact scenario could never pass).
    #[test]
    fn strict_live_bound_holds_regardless_of_flush_state() {
        let mut t = Transcript::new();
        for i in 0..500 {
            t.push(Message::user(format!("msg {i}")));
        }
        // Nothing was drained into scrollback, so every retained entry is
        // unflushed — and there are still exactly 150 of them.
        assert_eq!(t.len(), super::BOUNDED_LIVE_MAX);
        assert_eq!(t.live_messages().count(), super::BOUNDED_LIVE_MAX);
        assert_eq!(t.evicted_count(), 500 - super::BOUNDED_LIVE_MAX);
        // Paging window is available once history has been evicted.
        assert!(t.paging_window().is_some());
        let (before, limit) = t.paging_window().unwrap();
        assert_eq!(before, 500 - super::BOUNDED_LIVE_MAX);
        assert_eq!(limit, super::BOUNDED_LIVE_MAX);
    }

    /// The bound also holds with a mixed flushed/unflushed window: flushing
    /// never lets total memory grow past the same cap.
    #[test]
    fn total_window_stays_at_the_live_bound_even_after_flushing() {
        let mut t = Transcript::new();
        for i in 0..400 {
            t.push(Message::user(format!("msg {i}")));
            if i % 2 == 0 {
                t.drain_finalized(80);
            }
        }
        assert!(
            t.len() <= super::BOUNDED_LIVE_MAX,
            "total must stay bounded by the strict live cap"
        );
        assert!(t.evicted_count() > 0);
    }

    /// Evicting entries that were never flushed must not destroy unseen
    /// content: they are stashed for the host to insert into terminal
    /// scrollback before they leave memory.
    #[test]
    fn unflushed_evictions_are_stashed_for_scrollback_salvage() {
        let mut t = Transcript::new();
        for i in 0..160 {
            t.push(Message::user(format!("msg {i}")));
        }
        assert_eq!(t.eviction_overflow_len(), 160 - super::BOUNDED_LIVE_MAX);

        let lines = t.drain_eviction_overflow_lines(60);
        assert!(!lines.is_empty());
        let joined = lines
            .iter()
            .map(|l| {
                l.line()
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("msg 0"), "oldest evictee must be salvaged");
        assert!(
            joined.contains("msg 9"),
            "every unflushed evictee is stashed"
        );
        assert!(
            !joined.contains("msg 10"),
            "retained entries are not in overflow"
        );
        assert_eq!(t.eviction_overflow_len(), 0, "drain empties the stash");

        // The oldest RETAINED entry is exactly the first one not evicted.
        let first = t.entries[0].message().expect("message entry");
        assert!(first.content.contains("msg 10"));
    }

    /// Flushed evictees need no salvage — they were already inserted into
    /// terminal scrollback when they were flushed.
    #[test]
    fn flushed_evictions_leave_no_overflow() {
        let mut t = Transcript::new();
        // Fill, flush everything into "scrollback", then push past the cap.
        for i in 0..40 {
            t.push(Message::assistant(format!("early {i}")));
        }
        assert!(!t.drain_finalized(80).is_empty());
        for i in 0..150 {
            t.push(Message::user(format!("late {i}")));
        }
        assert!(t.evicted_count() > 0, "old flushed entries were evicted");
        assert_eq!(
            t.eviction_overflow_len(),
            0,
            "evicted flushed entries were already shown; nothing to salvage"
        );
    }

    #[test]
    fn session_switch_clears_and_resets_evicted() {
        let mut t = Transcript::new();
        for i in 0..400 {
            t.push(Message::assistant(format!("msg {i}")));
        }
        assert!(t.evicted_count() > 0);
        t.clear();
        assert_eq!(t.len(), 0);
        assert_eq!(t.evicted_count(), 0);
        assert_eq!(t.eviction_overflow_len(), 0);
        assert!(t.paging_window().is_none());
    }

    #[test]
    fn oldest_ranges_page_on_demand_via_before_cursor() {
        // Simulate paging: after eviction, the caller would fetch older ranges
        // with `before = evicted_count`. Ensure cursor advances correctly.
        let mut t = Transcript::new();
        for i in 0..350 {
            t.push(Message::user(format!("msg {i}")));
        }
        let (before, limit) = t.paging_window().expect("paging window");
        assert!(before > 0);
        assert_eq!(limit, super::BOUNDED_LIVE_MAX);
        // The cursor is exclusive: fetching `before` would return seq < before.
    }
}

#[cfg(test)]
mod styled_cells {
    use super::*;
    use crate::render::cell::{PlainCell, PrefixedWrappedCell, UserPromptCell};
    use crate::terminal::{Background, ColorDepth};
    use ratatui::style::Color;

    fn theme() -> Theme {
        Theme::new(Background::Dark, ColorDepth::TrueColor)
    }

    fn text_of(line: &HyperlinkLine) -> String {
        line.line()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    /// The decisive gap: a settled typed cell pushed into the transcript must
    /// reach scrollback with its donor styling intact — not flattened through
    /// `Message::system` plain text or a markdown reparse.
    #[test]
    fn pushed_cell_keeps_styled_spans_through_final_flush() {
        let style = Style::default()
            .fg(Color::Indexed(2))
            .add_modifier(Modifier::BOLD);
        let cell = PlainCell::new(vec![Line::from(vec![
            Span::raw("✔ ".to_string()),
            Span::styled("settled work".to_string(), style),
        ])]);

        let mut t = Transcript::new();
        t.push_cell(Box::new(cell), &theme());
        // A cell entry is not a Message; it must not leak into the message
        // projection used by paging/backfill.
        assert_eq!(t.live_messages().count(), 0);

        let lines = t.drain_finalized(80);
        assert_eq!(lines.len(), 1);
        let styled = lines[0]
            .line()
            .spans
            .iter()
            .find(|s| s.content.contains("settled work"))
            .expect("cell text present");
        assert_eq!(styled.style, style, "donor styling lost at flush");
        assert!(
            t.drain_finalized(80).is_empty(),
            "a settled cell flushes exactly once"
        );
    }

    /// Width-aware reflow: the same cell re-wraps through its own
    /// `display_lines` when the drain width changes, instead of reusing a
    /// stale wrap or re-parsing text.
    #[test]
    fn pushed_cell_reflows_at_a_new_width() {
        let cell = PrefixedWrappedCell::new(
            Line::from("word ".repeat(30).trim_end().to_string()),
            Span::raw("✔ ".to_string()),
            Span::raw("  ".to_string()),
        );
        let mut t = Transcript::new();
        t.push_cell(Box::new(cell), &theme());

        // Live render at wide, then the final flush happens narrower.
        t.ensure_rendered(80);
        let narrow = t.drain_finalized(20);
        assert!(
            narrow.len() > 2,
            "a narrow flush must re-wrap the cell, got {} lines",
            narrow.len()
        );
        for line in &narrow {
            assert!(
                line.line().width() <= 20,
                "reflowed line exceeds width: {:?}",
                line.line()
            );
        }
    }

    /// Unflushed eviction must salvage the styled cell itself, not a
    /// plain-text copy of it.
    #[test]
    fn unflushed_eviction_stashes_the_styled_cell() {
        let mut t = Transcript::new();
        for i in 0..160 {
            let style = Style::default().fg(Color::Indexed(1));
            t.push_cell(
                Box::new(PlainCell::new(vec![Line::from(Span::styled(
                    format!("cell {i}"),
                    style,
                ))])),
                &theme(),
            );
        }
        assert_eq!(t.len(), super::BOUNDED_LIVE_MAX);
        assert_eq!(
            t.eviction_overflow_len(),
            160 - super::BOUNDED_LIVE_MAX,
            "unflushed cells must be stashed, not dropped"
        );

        let salvaged = t.drain_eviction_overflow_lines(80);
        let first = &salvaged[0];
        assert!(text_of(first).contains("cell 0"));
        assert_eq!(
            first.line().spans[0].style.fg,
            Some(Color::Indexed(1)),
            "evicted cell must keep its styling through salvage"
        );
        assert_eq!(t.eviction_overflow_len(), 0, "drain empties the stash");
    }

    /// The cell trust boundary already sanitizes untrusted text at
    /// construction; `push_cell` adds no second pass but also never bypasses
    /// the first one — cells must be built by their sanitizing constructors.
    #[test]
    fn cell_construction_strips_control_bytes_before_push() {
        let cell = UserPromptCell::new("run it\x1b[2J\x07 now");
        let mut t = Transcript::new();
        t.push_cell(Box::new(cell), &theme());
        let lines = t.drain_finalized(80);
        let joined: String = lines.iter().map(text_of).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("run it"));
        assert!(!joined.contains('\x1b'), "ESC survived into scrollback");
        assert!(!joined.contains('\x07'), "BEL survived into scrollback");
    }
}

#[cfg(test)]
mod message_panel_tests {
    use super::*;
    use crate::terminal::{Background, ColorDepth};

    fn theme() -> Theme {
        Theme::new(Background::Dark, ColorDepth::TrueColor)
    }

    fn text_of(line: &HyperlinkLine) -> String {
        line.line()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    fn drained(message: Message, width: u16) -> Vec<HyperlinkLine> {
        let mut t = Transcript::new();
        t.set_theme(&theme());
        t.push(message);
        t.drain_finalized(width)
    }

    #[test]
    fn a_user_message_is_a_red_rail_panel_with_no_you_header() {
        let lines = drained(Message::user("fix the flaky retry test"), 40);
        let texts: Vec<String> = lines.iter().map(text_of).collect();
        assert_eq!(
            texts[0].trim_end(),
            "▌ fix the flaky retry test",
            "{texts:?}"
        );
        assert!(!texts.iter().any(|t| t.contains("You")), "{texts:?}");
        // Rail in the brand accent, text on the raised panel, row padded full width.
        let first = lines[0].line();
        assert_eq!(first.spans[0].content.as_ref(), "▌");
        assert_eq!(first.spans[0].style.fg, Some(theme().accent()));
        assert_eq!(first.spans[2].style.bg, theme().panel_bg());
        assert!(theme().panel_bg().is_some());
        assert_eq!(crate::transcript::wrap::display_width(&texts[0]), 40);
        // A blank row separates the turn.
        assert_eq!(texts.last().unwrap(), "");
    }

    #[test]
    fn a_long_user_message_wraps_under_the_rail_and_keeps_its_lines() {
        let lines = drained(Message::user("alpha beta gamma delta\nsecond line"), 14);
        let texts: Vec<String> = lines.iter().map(text_of).collect();
        let rows: Vec<&String> = texts.iter().filter(|t| t.starts_with('▌')).collect();
        assert!(rows.len() >= 3, "{texts:?}");
        assert!(rows.iter().all(|r| r.starts_with("▌ ")), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("second line")), "{texts:?}");
        for t in &texts {
            assert!(crate::transcript::wrap::display_width(t) <= 14, "{t:?}");
        }
    }

    #[test]
    fn user_text_is_literal_not_markdown() {
        let lines = drained(Message::user("# not a heading *or* emphasis"), 60);
        assert_eq!(
            text_of(&lines[0]).trim_end(),
            "▌ # not a heading *or* emphasis"
        );
    }

    #[test]
    fn notices_keep_the_backslashes_of_windows_paths() {
        let path = "(BBBB?BD:BClaudeBmyrepoB.auditBrepo)"
            .to_string()
            .replace('B', "\\");
        let lines = drained(Message::system(format!("session in {path}")), 120);
        let text = text_of(&lines[0]);
        assert!(text.contains(&path), "{text}");
        // Code spans and fences already keep backslashes literal.
        let lines = drained(Message::system("see `C:BtmpB.x`".replace('B', "\\")), 80);
        assert!(
            text_of(&lines[0]).contains(&"C:BtmpB.x".replace('B', "\\")),
            "{}",
            text_of(&lines[0])
        );
    }

    #[test]
    fn a_system_notice_is_one_dim_line_with_no_system_header() {
        let lines = drained(Message::system("Turn interrupted: interrupted by user"), 80);
        let texts: Vec<String> = lines.iter().map(text_of).collect();
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert_eq!(texts[0], "  Turn interrupted: interrupted by user");
        let span = &lines[0].line().spans[0];
        let dim = theme().color(Role::Dim);
        assert!(
            span.style.fg == Some(dim) || lines[0].line().style.fg == Some(dim),
            "{:?}",
            lines[0].line()
        );
    }

    #[test]
    fn assistant_prose_is_plain_text_with_no_header() {
        let lines = drained(Message::assistant("done"), 40);
        assert_eq!(text_of(&lines[0]), "  done");
        assert!(
            lines.iter().all(|l| !text_of(l).contains("Assistant")),
            "{lines:?}"
        );
    }

    #[test]
    fn without_256_colours_the_rail_alone_marks_the_user() {
        let mut t = Transcript::new();
        t.set_theme(&Theme::new(Background::Dark, ColorDepth::Ansi16));
        t.push(Message::user("hello"));
        let lines = t.drain_finalized(30);
        assert_eq!(lines[0].line().spans[2].style.bg, None);
        assert_eq!(text_of(&lines[0]).trim_end(), "▌ hello");
    }
}
