//! Read-only full-screen TUI inspection flows.
//!
//! This module adapts canonical transcript/diff text to the shared TUI reader;
//! it keeps no history and performs no workspace effects, other than holding
//! the paged-in older lines of the transcript reader (#112) and its Find
//! overlay state (#143).

use rightkit_terminal_ui::component::{Component, Event, Outcome, RenderFrameExt, Role, Theme};
use rightkit_terminal_ui::crossterm::event::{KeyCode, KeyEventKind};
use rightkit_terminal_ui::nav::inspection::{InspectionIntent, InspectionViewer};
use rightkit_terminal_ui::ratatui::layout::Rect;
use rightkit_terminal_ui::ratatui::style::{Modifier, Style};
use rightkit_terminal_ui::ratatui::text::{Line, Span, Text};
use rightkit_terminal_ui::ratatui::widgets::{Block, Borders, Paragraph};
use rightkit_terminal_ui::transcript::find::{FindOutcome, FindState};
use rightkit_terminal_ui::transcript::history_pager::{
    HistoryCursor, PageRequest, ScrollOutcome, ScrollbackView,
};

/// Rows requested per older-history page.
pub(crate) const OLDER_PAGE_LIMIT: usize = 200;

/// Transcript reader with Ctrl+F find, PageUp/PageDown/Home/End scrollback
/// and on-demand older history. Diff inspection keeps the shared viewer.
struct TranscriptReader {
    lines: Vec<String>,
    view: ScrollbackView,
    find: FindState,
    cursor: HistoryCursor,
    closed: bool,
    /// Rows the body showed on the last render (drives page size).
    body_rows: usize,
    /// Bookmarked line indexes (`m` toggles one at the top visible line, `'` jumps to the next).
    bookmarks: Vec<usize>,
    /// Copy mode (`v`): the anchor line and the cursor line of the selection.
    selection: Option<(usize, usize)>,
    /// What the last copy said, shown in the header until the next key.
    status: Option<String>,
    /// Entries left the terminal's memory and the stored chat has no older page to put above the
    /// first line held here (#536): the top of the reader is then not the start of the chat.
    unloaded_above: bool,
}

pub(crate) struct InspectionState {
    viewer: InspectionViewer,
    reader: Option<TranscriptReader>,
}

impl TranscriptReader {
    fn new(lines: Vec<String>, oldest_seq: Option<i64>, exhausted: bool, evicted: usize) -> Self {
        let body_rows = 20;
        // The stored chat keeps the prompt and the finished answer as events, not the entries the
        // screen was built from (thinking parts are not stored at all), so entries that left
        // memory cannot be put back above the first line; say so instead of "start of history".
        let unloaded_above = evicted > 0 && (exhausted || oldest_seq.is_none_or(|seq| seq <= 1));
        let mut cursor = HistoryCursor::new(oldest_seq);
        if exhausted {
            cursor.page_loaded(None);
        }
        Self {
            view: ScrollbackView::new(lines.len(), body_rows),
            lines,
            find: FindState::default(),
            cursor,
            closed: false,
            body_rows,
            bookmarks: Vec::new(),
            selection: None,
            status: None,
            unloaded_above,
        }
    }

    /// The selected line range in copy mode, ends included.
    fn selected_range(&self) -> Option<(usize, usize)> {
        self.selection
            .map(|(anchor, cursor)| (anchor.min(cursor), anchor.max(cursor)))
    }

    /// The next bookmark after the top visible line, wrapping to the first.
    fn next_bookmark(&self) -> Option<usize> {
        let top = self.view.visible().0;
        self.bookmarks
            .iter()
            .copied()
            .find(|line| *line > top)
            .or_else(|| self.bookmarks.first().copied())
    }

    /// Keys of copy mode: Up/Down extend the selection, `y` or Enter copies it, Esc or `v` leaves.
    fn handle_copy_mode(
        &mut self,
        key: &rightkit_terminal_ui::crossterm::event::KeyEvent,
    ) -> Outcome {
        let Some((anchor, cursor)) = self.selection else {
            return Outcome::Ignored;
        };
        let last = self.lines.len().saturating_sub(1);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                let cursor = (cursor + 1).min(last);
                self.selection = Some((anchor, cursor));
                self.view.reveal(cursor);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let cursor = cursor.saturating_sub(1);
                self.selection = Some((anchor, cursor));
                self.view.reveal(cursor);
            }
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some((from, to)) = self.selected_range() {
                    let text = self.lines[from..=to.min(last)].join("\n");
                    self.status = Some(crate::tui_host::clipboard_copy_notice(&text));
                }
                self.selection = None;
            }
            KeyCode::Esc | KeyCode::Char('v') => {
                self.selection = None;
            }
            _ => {}
        }
        Outcome::Redraw
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        let Event::Key(key) = event else {
            return Outcome::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return Outcome::Ignored;
        }
        self.status = None;
        if self.selection.is_some() {
            return self.handle_copy_mode(key);
        }
        match self.find.handle_key(key, &self.lines) {
            FindOutcome::Ignored => {}
            FindOutcome::Redraw => return Outcome::Redraw,
            FindOutcome::Jump(line) => {
                self.view.reveal(line);
                return Outcome::Redraw;
            }
            FindOutcome::Closed => return Outcome::Redraw,
        }
        match self.view.handle_key(key) {
            ScrollOutcome::Ignored => {}
            ScrollOutcome::Moved | ScrollOutcome::AtTop => return Outcome::Redraw,
        }
        match key.code {
            // Bookmarks: `m` marks (or unmarks) the top visible line, `'` jumps to the next mark.
            KeyCode::Char('m') if key.modifiers.is_empty() => {
                let line = self.view.visible().0;
                if let Some(at) = self.bookmarks.iter().position(|b| *b == line) {
                    self.bookmarks.remove(at);
                } else if line < self.lines.len() {
                    self.bookmarks.push(line);
                    self.bookmarks.sort_unstable();
                }
            }
            KeyCode::Char('\'') if key.modifiers.is_empty() => match self.next_bookmark() {
                Some(line) => {
                    self.view.reveal(line);
                }
                None => self.status = Some("No bookmarks: press m to mark the top line.".into()),
            },
            // Copy mode: `v` starts a line selection at the top visible line.
            KeyCode::Char('v') if key.modifiers.is_empty() => {
                let line = self.view.visible().0;
                if line < self.lines.len() {
                    self.selection = Some((line, line));
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.closed = true,
            KeyCode::Up => {
                self.view.scroll_up(1);
            }
            KeyCode::Down => {
                self.view.scroll_down(1);
            }
            _ => return Outcome::Ignored,
        }
        Outcome::Redraw
    }

    /// The styled rows currently visible (no frame, so it is testable).
    fn visible_rows(&self, theme: Option<&Theme>) -> Vec<Line<'static>> {
        let (start, end) = self.view.visible();
        let base = match theme {
            Some(theme) => Style::default().fg(theme.color(Role::Text)),
            None => Style::default(),
        };
        let selected = self.selected_range();
        (start..end)
            .map(|index| {
                let mark = if self.bookmarks.contains(&index) {
                    "▸ "
                } else {
                    "  "
                };
                let mut line = self.find.highlight_line(index, &self.lines[index], base);
                line.spans.insert(0, Span::styled(mark, base));
                if selected.is_some_and(|(from, to)| index >= from && index <= to) {
                    for span in &mut line.spans {
                        span.style = span.style.add_modifier(Modifier::REVERSED);
                    }
                }
                line
            })
            .collect()
    }

    fn header(&self) -> String {
        if let Some(status) = &self.status {
            status.clone()
        } else if self.selection.is_some() {
            "Copy mode · ↑↓ extend · y copy · Esc cancel".to_string()
        } else if self.find.is_open() {
            self.find.bar_text()
        } else if self.unloaded_above {
            "Transcript · earlier lines not loaded · Esc, then scroll the terminal".to_string()
        } else if self.cursor.is_exhausted() {
            "Transcript · start of history".to_string()
        } else {
            "Transcript".to_string()
        }
    }

    fn footer(&self) -> &'static str {
        "↑↓/PgUp/PgDn/Home/End scroll · Ctrl+F/F3 find · m mark · ' next mark · v copy mode · Esc/q close"
    }

    fn render(
        &mut self,
        frame: &mut dyn rightkit_terminal_ui::component::RenderFrame,
        area: Rect,
        theme: &Theme,
    ) {
        // Two border rows, the header and the key legend: with one row fewer reserved the legend
        // was drawn below the frame's last inner row and never seen in a chat longer than a screen.
        let body = usize::from(area.height.saturating_sub(4)).max(1);
        if body != self.body_rows {
            self.body_rows = body;
            self.view.resize(body);
        }
        let mut rows = vec![Line::from(Span::styled(
            self.header(),
            Style::default()
                .fg(theme.color(Role::Running))
                .add_modifier(Modifier::BOLD),
        ))];
        rows.extend(self.visible_rows(Some(theme)));
        rows.push(Line::from(Span::styled(
            self.footer(),
            Style::default().fg(theme.color(Role::Dim)),
        )));
        frame.render_widget(
            Paragraph::new(Text::from(rows)).block(Block::default().borders(Borders::ALL)),
            area,
        );
    }
}

impl InspectionState {
    /// `oldest_seq` is the durable cursor of the oldest event already paged
    /// (`None` when unknown, which disables older-page requests) and
    /// `exhausted` whether the ledger has no older rows.
    pub(crate) fn transcript(
        transcript: &rightkit_terminal_ui::transcript::Transcript,
        width: u16,
        oldest_seq: Option<i64>,
        exhausted: bool,
    ) -> Self {
        // The frame takes two columns and the bookmark gutter two more: lines rendered at the full
        // terminal width lost their last four characters at the right border.
        let lines = transcript.inspection_lines(width.saturating_sub(4).max(1));
        let evicted = transcript.evicted_count();
        Self {
            viewer: InspectionViewer::transcript(lines.clone()),
            reader: Some(TranscriptReader::new(lines, oldest_seq, exhausted, evicted)),
        }
    }

    pub(crate) fn diff(diff: &str) -> Self {
        Self {
            viewer: InspectionViewer::diff(diff.lines().map(str::to_string)),
            reader: None,
        }
    }

    pub(crate) fn is_closed(&mut self) -> bool {
        if let Some(reader) = self.reader.as_ref() {
            return reader.closed;
        }
        self.viewer
            .drain_intents()
            .into_iter()
            .any(|intent| matches!(intent, InspectionIntent::Close))
    }

    /// When the reader sits at the top of what is loaded, the request for the
    /// next older page (at most one in flight).
    pub(crate) fn take_older_request(&mut self) -> Option<PageRequest> {
        let reader = self.reader.as_mut()?;
        if !reader.view.at_top() {
            return None;
        }
        reader.cursor.request_older(OLDER_PAGE_LIMIT)
    }

    /// An older page arrived: prepend its display lines (oldest first) above
    /// the reader's anchor and advance the cursor to `lowest_seq`.
    pub(crate) fn older_page_loaded(&mut self, older: Vec<String>, lowest_seq: Option<i64>) {
        let Some(reader) = self.reader.as_mut() else {
            return;
        };
        reader.cursor.page_loaded(lowest_seq);
        let added = older.len();
        reader.lines.splice(0..0, older);
        reader.view.prepend(added);
        for line in &mut reader.bookmarks {
            *line += added;
        }
        if let Some((anchor, cursor)) = reader.selection {
            reader.selection = Some((anchor + added, cursor + added));
        }
        reader.find.refresh_if_open(&reader.lines);
    }

    /// The fetch failed; the reader stops asking rather than spinning.
    pub(crate) fn older_page_failed(&mut self) {
        if let Some(reader) = self.reader.as_mut() {
            reader.cursor.page_failed();
            reader.cursor.page_loaded(None);
        }
    }
}

impl Component for InspectionState {
    fn render(
        &mut self,
        frame: &mut dyn rightkit_terminal_ui::component::RenderFrame,
        area: Rect,
        theme: &Theme,
    ) {
        if let Some(reader) = self.reader.as_mut() {
            reader.render(frame, area, theme);
            return;
        }
        self.viewer.render(frame, area, theme);
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        if let Some(reader) = self.reader.as_mut() {
            return reader.handle(event);
        }
        self.viewer.handle(event)
    }
}
