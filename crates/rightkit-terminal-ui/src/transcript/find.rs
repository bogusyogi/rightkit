//! Transcript Find overlay (#143): Ctrl+F opens a find bar over the
//! transcript reader. Matches are highlighted, Enter / Shift+Enter (or
//! `n` / `N` once the query is committed) step through them, Esc closes.
//!
//! The state is pure: it searches a slice of plain transcript lines (see
//! `Transcript::inspection_lines`) and never owns history, so the host can
//! re-run [`FindState::refresh`] after the transcript changes or pages in
//! older entries.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// One hit: `line` index plus a half-open `[start, end)` range in chars.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindMatch {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// What a key did to the overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindOutcome {
    /// Overlay is closed and the key is not ours.
    Ignored,
    /// Handled; redraw the bar and highlights.
    Redraw,
    /// Handled and the view should scroll so this line is visible.
    Jump(usize),
    /// Overlay closed.
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Typing the query; letters extend it.
    Editing,
    /// Query committed with Enter; `n` / `N` navigate.
    Navigating,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindState {
    open: bool,
    mode: Mode,
    query: String,
    matches: Vec<FindMatch>,
    current: Option<usize>,
}

impl Default for FindState {
    fn default() -> Self {
        Self {
            open: false,
            mode: Mode::Editing,
            query: String::new(),
            matches: Vec::new(),
            current: None,
        }
    }
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Find every non-overlapping occurrence of `query` in `line`.
fn find_in_line(
    line: &str,
    query: &[char],
    sensitive: bool,
    line_idx: usize,
    out: &mut Vec<FindMatch>,
) {
    if query.is_empty() {
        return;
    }
    let hay: Vec<char> = line.chars().collect();
    if hay.len() < query.len() {
        return;
    }
    let eq = |a: char, b: char| {
        if sensitive {
            a == b
        } else {
            fold(a) == fold(b)
        }
    };
    let mut i = 0;
    while i + query.len() <= hay.len() {
        if query.iter().enumerate().all(|(k, q)| eq(hay[i + k], *q)) {
            out.push(FindMatch {
                line: line_idx,
                start: i,
                end: i + query.len(),
            });
            i += query.len();
        } else {
            i += 1;
        }
    }
}

impl FindState {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn matches(&self) -> &[FindMatch] {
        &self.matches
    }

    pub fn current(&self) -> Option<&FindMatch> {
        self.current.and_then(|i| self.matches.get(i))
    }

    /// Open the bar, keeping the previous query so Ctrl+F twice re-finds.
    pub fn open(&mut self) {
        self.open = true;
        self.mode = Mode::Editing;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.matches.clear();
        self.current = None;
    }

    /// Recompute matches against `lines` (smart case: an all-lowercase query
    /// is case-insensitive, any uppercase letter makes it exact), keeping the
    /// current match when it still exists.
    pub fn refresh(&mut self, lines: &[String]) {
        let keep = self.current().map(|m| (m.line, m.start));
        self.matches.clear();
        let query: Vec<char> = self.query.chars().collect();
        let sensitive = query.iter().any(|c| c.is_uppercase());
        for (idx, line) in lines.iter().enumerate() {
            find_in_line(line, &query, sensitive, idx, &mut self.matches);
        }
        self.current = if self.matches.is_empty() {
            None
        } else if let Some((line, start)) = keep {
            self.matches
                .iter()
                .position(|m| m.line == line && m.start == start)
                .or(Some(0))
        } else {
            Some(0)
        };
    }

    /// Re-run the search after the transcript changed, only while open.
    pub fn refresh_if_open(&mut self, lines: &[String]) {
        if self.open {
            self.refresh(lines);
        }
    }

    pub fn next_match(&mut self) -> Option<usize> {
        let len = self.matches.len();
        if len == 0 {
            return None;
        }
        let idx = self.current.map_or(0, |i| (i + 1) % len);
        self.current = Some(idx);
        Some(self.matches[idx].line)
    }

    pub fn prev(&mut self) -> Option<usize> {
        let len = self.matches.len();
        if len == 0 {
            return None;
        }
        let idx = self.current.map_or(len - 1, |i| (i + len - 1) % len);
        self.current = Some(idx);
        Some(self.matches[idx].line)
    }

    /// Route one key. Ctrl+F opens (or re-focuses) the bar from any state.
    pub fn handle_key(&mut self, key: &KeyEvent, lines: &[String]) -> FindOutcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('f') | KeyCode::Char('F')) {
            self.open();
            self.refresh(lines);
            return self
                .current()
                .map_or(FindOutcome::Redraw, |m| FindOutcome::Jump(m.line));
        }
        if !self.open {
            return FindOutcome::Ignored;
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let jump = |line: Option<usize>| line.map_or(FindOutcome::Redraw, FindOutcome::Jump);
        match key.code {
            KeyCode::Esc => {
                self.close();
                FindOutcome::Closed
            }
            KeyCode::Enter => {
                self.mode = Mode::Navigating;
                jump(if shift {
                    self.prev()
                } else {
                    self.next_match()
                })
            }
            KeyCode::BackTab => jump(self.prev()),
            KeyCode::Char('N') if self.mode == Mode::Navigating => jump(self.prev()),
            KeyCode::Char('n') if self.mode == Mode::Navigating => jump(self.next_match()),
            KeyCode::Char('/') if self.mode == Mode::Navigating => {
                self.mode = Mode::Editing;
                FindOutcome::Redraw
            }
            KeyCode::Backspace => {
                self.mode = Mode::Editing;
                self.query.pop();
                self.current = None;
                self.refresh(lines);
                jump(self.current().map(|m| m.line))
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                self.mode = Mode::Editing;
                self.query.push(c);
                self.current = None;
                self.refresh(lines);
                jump(self.current().map(|m| m.line))
            }
            // Navigation keys (PageUp, Home, arrows...) fall through to the
            // scrollback view while the bar is open.
            _ => FindOutcome::Ignored,
        }
    }

    /// Style one transcript line: every match reversed, the current one also
    /// bold and underlined. `base` is the line's own style for the gaps.
    pub fn highlight_line(&self, line_idx: usize, text: &str, base: Style) -> Line<'static> {
        let current = self.current;
        let mut hits: Vec<(usize, FindMatch)> = self
            .matches
            .iter()
            .enumerate()
            .filter(|(_, m)| m.line == line_idx)
            .map(|(i, m)| (i, *m))
            .collect();
        if hits.is_empty() {
            return Line::from(Span::styled(text.to_string(), base));
        }
        hits.sort_by_key(|(_, m)| m.start);
        let chars: Vec<char> = text.chars().collect();
        let slice = |a: usize, b: usize| chars[a..b.min(chars.len())].iter().collect::<String>();
        let mut spans = Vec::new();
        let mut at = 0;
        for (idx, m) in hits {
            if m.start > at {
                spans.push(Span::styled(slice(at, m.start), base));
            }
            let mut style = base.add_modifier(Modifier::REVERSED);
            if Some(idx) == current {
                style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            }
            spans.push(Span::styled(slice(m.start, m.end), style));
            at = m.end;
        }
        if at < chars.len() {
            spans.push(Span::styled(slice(at, chars.len()), base));
        }
        Line::from(spans)
    }

    /// The one-row find bar text.
    pub fn bar_text(&self) -> String {
        let count = if self.query.is_empty() {
            String::new()
        } else if self.matches.is_empty() {
            "  no matches".to_string()
        } else {
            format!(
                "  {}/{}",
                self.current.map_or(0, |i| i + 1),
                self.matches.len()
            )
        };
        format!(
            "Find: {}{}  Enter next  Shift+Enter prev  Esc close",
            self.query, count
        )
    }

    pub fn bar_line(&self, width: u16, style: Style) -> Line<'static> {
        let mut text = self.bar_text();
        let w = usize::from(width);
        if text.chars().count() > w {
            text = text.chars().take(w).collect();
        }
        Line::from(Span::styled(text, style))
    }
}
