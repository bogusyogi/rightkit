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
