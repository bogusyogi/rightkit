//! A026 history paging and scrollback: PageUp/PageDown, Home/End and wheel
//! scrolling over a transcript longer than memory, loading older history from
//! the daemon on demand.
//!
//! [`ScrollbackView`] is the viewport arithmetic; [`HistoryCursor`] decides
//! when an older page is needed and builds the daemon request. The transport
//! is the existing `GET /sessions/{id}/events/history?before=SEQ&limit=N`
//! (backwards paging); `/events/replay?after=N` (fca58c7be) is the forward
//! durable catch-up path, exposed by [`replay_path`].

use crossterm::event::{KeyCode, KeyEvent};

/// Viewport over `total` lines showing `height` rows starting at `top`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScrollbackView {
    top: usize,
    total: usize,
    height: usize,
    /// True while pinned to the newest line; new output keeps following.
    follow: bool,
}

/// What a navigation key asks of the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollOutcome {
    Ignored,
    Moved,
    /// Already at the top of what is loaded; older history may be fetched.
    AtTop,
}

impl ScrollbackView {
    pub fn new(total: usize, height: usize) -> Self {
        let mut v = Self {
            top: 0,
            total,
            height,
            follow: true,
        };
        v.snap_to_bottom();
        v
    }

    pub fn top(&self) -> usize {
        self.top
    }

    pub fn is_following(&self) -> bool {
        self.follow
    }

    pub fn at_top(&self) -> bool {
        self.top == 0
    }

    fn max_top(&self) -> usize {
        self.total.saturating_sub(self.height)
    }

    fn snap_to_bottom(&mut self) {
        self.top = self.max_top();
        self.follow = true;
    }

    /// Visible `[start, end)` line range.
    pub fn visible(&self) -> (usize, usize) {
        (self.top, (self.top + self.height).min(self.total))
    }

    pub fn resize(&mut self, height: usize) {
        self.height = height;
        if self.follow {
            self.snap_to_bottom();
        } else {
            self.top = self.top.min(self.max_top());
        }
    }

    /// New lines arrived at the tail.
    pub fn append(&mut self, count: usize) {
        self.total += count;
        if self.follow {
            self.snap_to_bottom();
        }
    }

    /// `count` older lines were loaded above; keep the reader's anchor still.
    pub fn prepend(&mut self, count: usize) {
        self.total += count;
        self.top += count;
        self.follow = false;
        self.top = self.top.min(self.max_top());
    }

    pub fn scroll_up(&mut self, rows: usize) -> ScrollOutcome {
        if self.top == 0 {
            return ScrollOutcome::AtTop;
        }
        self.top = self.top.saturating_sub(rows);
        self.follow = false;
        ScrollOutcome::Moved
    }

    pub fn scroll_down(&mut self, rows: usize) -> ScrollOutcome {
        self.top = (self.top + rows).min(self.max_top());
        self.follow = self.top == self.max_top();
        ScrollOutcome::Moved
    }

    /// Make `line` visible (used by Find jumps), centring when it was off screen.
    pub fn reveal(&mut self, line: usize) {
        let (a, b) = self.visible();
        if line >= a && line < b {
            return;
        }
        self.top = line.saturating_sub(self.height / 2).min(self.max_top());
        self.follow = self.top == self.max_top();
    }

    /// PageUp/PageDown overlap by one row so context is never lost; Home/End
    /// jump to the oldest loaded line and back to the live tail.
    pub fn handle_key(&mut self, key: &KeyEvent) -> ScrollOutcome {
        let page = self.height.saturating_sub(1).max(1);
        match key.code {
            KeyCode::PageUp => self.scroll_up(page),
            KeyCode::PageDown => self.scroll_down(page),
            KeyCode::Home => {
                let was_top = self.top == 0;
                self.top = 0;
                self.follow = false;
                if was_top {
                    ScrollOutcome::AtTop
                } else {
                    ScrollOutcome::Moved
                }
            }
            KeyCode::End => {
                self.snap_to_bottom();
                ScrollOutcome::Moved
            }
            _ => ScrollOutcome::Ignored,
        }
    }
}

/// A request for the page of events immediately before `before_seq`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    pub before_seq: i64,
    pub limit: usize,
}

/// Tracks the oldest sequence in memory and whether older history remains.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HistoryCursor {
    oldest_seq: Option<i64>,
    exhausted: bool,
    in_flight: bool,
}

impl HistoryCursor {
    pub fn new(oldest_seq: Option<i64>) -> Self {
        Self {
            oldest_seq,
            ..Self::default()
        }
    }

    /// The reader is at the top of loaded lines: return a request if older
    /// durable history may exist and none is already in flight.
    pub fn request_older(&mut self, limit: usize) -> Option<PageRequest> {
        let before = self.oldest_seq?;
        if self.exhausted || self.in_flight || before <= 1 {
            return None;
        }
        self.in_flight = true;
        Some(PageRequest {
            before_seq: before,
            limit,
        })
    }

    /// A page arrived. `lowest_seq` is the smallest seq in it; `None` (an
    /// empty page) means the beginning of history was reached.
    pub fn page_loaded(&mut self, lowest_seq: Option<i64>) {
        self.in_flight = false;
        match lowest_seq {
            Some(seq) => self.oldest_seq = Some(seq),
            None => self.exhausted = true,
        }
    }

    /// The request failed; allow a retry on the next top-of-view event.
    pub fn page_failed(&mut self) {
        self.in_flight = false;
    }

    pub fn is_exhausted(&self) -> bool {
        self.exhausted
    }
}

/// Backwards paging path for older history.
pub fn history_path(session_id: &str, req: &PageRequest) -> String {
    format!(
        "/sessions/{session_id}/events/history?before={}&limit={}",
        req.before_seq, req.limit
    )
}

/// Forward durable catch-up path (fca58c7be).
pub fn replay_path(session_id: &str, after: i64, limit: usize) -> String {
    format!("/sessions/{session_id}/events/replay?after={after}&limit={limit}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn starts_pinned_to_bottom() {
        let v = ScrollbackView::new(100, 10);
        assert_eq!(v.visible(), (90, 100));
        assert!(v.is_following());
    }

    #[test]
    fn page_up_down_overlap_one_row_and_clamp() {
        let mut v = ScrollbackView::new(100, 10);
        assert_eq!(v.handle_key(&key(KeyCode::PageUp)), ScrollOutcome::Moved);
        assert_eq!(v.top(), 81);
        assert!(!v.is_following());
        v.handle_key(&key(KeyCode::PageDown));
        assert_eq!(v.top(), 90);
        assert!(v.is_following());
        v.handle_key(&key(KeyCode::PageDown));
        assert_eq!(v.top(), 90);
    }

    #[test]
    fn home_end_jump_and_report_top() {
        let mut v = ScrollbackView::new(100, 10);
        assert_eq!(v.handle_key(&key(KeyCode::Home)), ScrollOutcome::Moved);
        assert!(v.at_top());
        assert_eq!(v.handle_key(&key(KeyCode::Home)), ScrollOutcome::AtTop);
        assert_eq!(v.handle_key(&key(KeyCode::PageUp)), ScrollOutcome::AtTop);
        v.handle_key(&key(KeyCode::End));
        assert_eq!(v.visible(), (90, 100));
        assert!(v.is_following());
        assert_eq!(
            v.handle_key(&key(KeyCode::Char('x'))),
            ScrollOutcome::Ignored
        );
    }

    #[test]
    fn prepend_keeps_the_reader_anchor() {
        let mut v = ScrollbackView::new(100, 10);
        v.handle_key(&key(KeyCode::Home));
        v.prepend(50);
        assert_eq!(v.top(), 50);
        assert_eq!(v.visible(), (50, 60));
    }

    #[test]
    fn append_follows_only_when_pinned() {
        let mut v = ScrollbackView::new(100, 10);
        v.append(5);
        assert_eq!(v.visible(), (95, 105));
        v.handle_key(&key(KeyCode::PageUp));
        let top = v.top();
        v.append(5);
        assert_eq!(v.top(), top);
    }

    #[test]
    fn short_transcript_never_scrolls() {
        let mut v = ScrollbackView::new(4, 10);
        assert_eq!(v.visible(), (0, 4));
        v.scroll_down(3);
        assert_eq!(v.top(), 0);
    }

    #[test]
    fn reveal_centres_offscreen_line_only() {
        let mut v = ScrollbackView::new(100, 10);
        v.reveal(95);
        assert_eq!(v.top(), 90);
        v.reveal(20);
        assert_eq!(v.top(), 15);
        v.reveal(18);
        assert_eq!(v.top(), 15);
    }

    #[test]
    fn cursor_requests_older_pages_once_until_loaded() {
        let mut c = HistoryCursor::new(Some(301));
        let req = c.request_older(200).unwrap();
        assert_eq!(
            req,
            PageRequest {
                before_seq: 301,
                limit: 200
            }
        );
        assert!(c.request_older(200).is_none(), "in flight");
        c.page_loaded(Some(101));
        assert_eq!(c.request_older(200).unwrap().before_seq, 101);
        c.page_failed();
        assert!(c.request_older(200).is_some(), "retry after failure");
        c.page_loaded(None);
        assert!(c.is_exhausted());
        assert!(c.request_older(200).is_none());
    }

    #[test]
    fn cursor_stops_at_first_seq_or_unknown() {
        assert!(HistoryCursor::new(Some(1)).request_older(10).is_none());
        assert!(HistoryCursor::new(None).request_older(10).is_none());
    }

    #[test]
    fn paths_match_daemon_routes() {
        let req = PageRequest {
            before_seq: 42,
            limit: 200,
        };
        insta::assert_snapshot!(
            history_path("s1", &req),
            @"/sessions/s1/events/history?before=42&limit=200"
        );
        insta::assert_snapshot!(
            replay_path("s1", 7, 500),
            @"/sessions/s1/events/replay?after=7&limit=500"
        );
    }
}
