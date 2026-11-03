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
}

pub(crate) struct InspectionState {
    viewer: InspectionViewer,
    reader: Option<TranscriptReader>,
}

impl TranscriptReader {
    fn new(lines: Vec<String>, oldest_seq: Option<i64>, exhausted: bool) -> Self {
        let body_rows = 20;
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
        }
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        let Event::Key(key) = event else {
            return Outcome::Ignored;
        };
        if key.kind != KeyEventKind::Press {
            return Outcome::Ignored;
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
        (start..end)
            .map(|index| self.find.highlight_line(index, &self.lines[index], base))
            .collect()
    }

    fn header(&self) -> String {
        if self.find.is_open() {
            self.find.bar_text()
        } else if self.cursor.is_exhausted() {
            "Transcript · start of history".to_string()
        } else {
            "Transcript".to_string()
        }
    }

    fn footer(&self) -> &'static str {
        "↑↓/PgUp/PgDn/Home/End scroll · Ctrl+F find · Esc/q close"
    }

    fn render(
        &mut self,
        frame: &mut dyn rightkit_terminal_ui::component::RenderFrame,
        area: Rect,
        theme: &Theme,
    ) {
        let body = usize::from(area.height.saturating_sub(3)).max(1);
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
        let lines = transcript.inspection_lines(width);
        Self {
            viewer: InspectionViewer::transcript(lines.clone()),
            reader: Some(TranscriptReader::new(lines, oldest_seq, exhausted)),
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
