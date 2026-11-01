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

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_terminal_ui::crossterm::event::{KeyEvent, KeyModifiers};
    use rightkit_terminal_ui::transcript::{Message, Transcript};

    fn press(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn reader_with(lines: usize, oldest: Option<i64>) -> InspectionState {
        let mut t = Transcript::new();
        for i in 0..lines {
            t.push(Message::system(format!("line {i}")));
        }
        InspectionState::transcript(&t, 80, oldest, false)
    }

    fn texts(state: &InspectionState) -> Vec<String> {
        state
            .reader
            .as_ref()
            .unwrap()
            .visible_rows(None)
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn ctrl_f_opens_find_and_esc_closes_find_not_reader() {
        let mut s = reader_with(30, None);
        s.handle(&press(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(s.reader.as_ref().unwrap().find.is_open());
        for c in "line 7".chars() {
            s.handle(&press(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert!(!s.reader.as_ref().unwrap().find.matches().is_empty());
        assert!(texts(&s).iter().any(|l| l.trim() == "line 7"));
        s.handle(&press(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!s.reader.as_ref().unwrap().find.is_open());
        assert!(!s.is_closed());
        s.handle(&press(KeyCode::Esc, KeyModifiers::NONE));
        assert!(s.is_closed());
    }

    #[test]
    fn page_keys_move_the_view_and_home_end_bound_it() {
        let mut s = reader_with(100, None);
        let bottom = s.reader.as_ref().unwrap().view.top();
        s.handle(&press(KeyCode::PageUp, KeyModifiers::NONE));
        assert!(s.reader.as_ref().unwrap().view.top() < bottom);
        s.handle(&press(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(s.reader.as_ref().unwrap().view.top(), 0);
        s.handle(&press(KeyCode::End, KeyModifiers::NONE));
        assert_eq!(s.reader.as_ref().unwrap().view.top(), bottom);
    }

    #[test]
    fn older_request_only_at_top_and_prepend_keeps_anchor() {
        let mut s = reader_with(100, Some(500));
        assert!(s.take_older_request().is_none(), "not at top yet");
        s.handle(&press(KeyCode::Home, KeyModifiers::NONE));
        let req = s.take_older_request().expect("at top asks for older page");
        assert_eq!(req.before_seq, 500);
        assert!(s.take_older_request().is_none(), "one in flight");
        let first = texts(&s)[0].clone();
        s.older_page_loaded(vec!["old a".into(), "old b".into()], Some(498));
        assert_eq!(texts(&s)[0], first, "anchor preserved");
        s.handle(&press(KeyCode::Home, KeyModifiers::NONE));
        assert_eq!(texts(&s)[0], "old a");
        assert_eq!(s.take_older_request().unwrap().before_seq, 498);
    }

    #[test]
    fn failed_or_empty_page_stops_further_requests() {
        let mut s = reader_with(100, Some(500));
        s.handle(&press(KeyCode::Home, KeyModifiers::NONE));
        s.take_older_request().unwrap();
        s.older_page_failed();
        assert!(s.take_older_request().is_none());
    }

    #[test]
    fn unknown_cursor_never_requests() {
        let mut s = reader_with(100, None);
        s.handle(&press(KeyCode::Home, KeyModifiers::NONE));
        assert!(s.take_older_request().is_none());
    }

    #[test]
    fn found_match_in_prepended_history_is_searchable() {
        let mut s = reader_with(50, Some(10));
        s.handle(&press(KeyCode::Char('f'), KeyModifiers::CONTROL));
        for c in "needle".chars() {
            s.handle(&press(KeyCode::Char(c), KeyModifiers::NONE));
        }
        assert!(s.reader.as_ref().unwrap().find.matches().is_empty());
        s.handle(&press(KeyCode::Home, KeyModifiers::NONE));
        s.older_page_loaded(vec!["a needle here".into()], Some(9));
        assert_eq!(s.reader.as_ref().unwrap().find.matches().len(), 1);
    }

    #[test]
    fn header_snapshot() {
        let mut s = reader_with(3, None);
        assert_eq!(s.reader.as_ref().unwrap().header(), "Transcript");
        s.handle(&press(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert_eq!(
            s.reader.as_ref().unwrap().header(),
            "Find:   Enter next  Shift+Enter prev  Esc close"
        );
    }
}
