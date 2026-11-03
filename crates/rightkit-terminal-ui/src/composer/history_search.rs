//! Composer-side Ctrl+R reverse history search session.
//!
//! Reduced from the Codex donor `bottom_pane/chat_composer/history_search.rs`
//! @ 444da310. The session lifecycle is the donor's: an idle session opens
//! with an empty query (no implicit preview), query edits restart traversal
//! from the newest unique match, Older/Newer keys move between unique
//! matches, `Enter` accepts only an actual match as an editable draft, and
//! `Esc`/Ctrl+C restores the saved draft — including the Vim edit ledger and
//! persistent Vim state parked when the preview took over the textarea. The
//! donor's async persistent-history plumbing (Pending lookups, sparkle
//! integration) is parent-owned; this history is synchronous, so traversal
//! resolves immediately through [`ChatComposerHistory::search`].

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};

use super::Composer;
use super::ComposerDraft;
use super::InputResult;
use super::history::{HistorySearchDirection, HistorySearchResult};
use super::popup_state::ActivePopup;
use super::vim_history::VimHistory;
use crate::composer::key_hint::{self, KeyBindingListExt};

/// Active composer-owned state for one Ctrl+R search interaction.
///
/// The session is created only by [`Composer::begin_history_search`] and is
/// cleared only by accepting, canceling, or replacing the search mode. It
/// stores the cancellation draft and Vim edit state separately from the
/// footer query and preview so background edits stay hidden.
#[derive(Debug)]
pub(super) struct HistorySearchSession {
    /// Saved draft to restore on cancellation, including background updates.
    pub(super) original_draft: ComposerDraft,
    /// Freeze the fallback only when background edits diverge from the saved draft.
    pub(super) preview_draft: Option<ComposerDraft>,
    /// Same-draft Vim edits to restore when a temporary preview is canceled.
    pub(super) original_vim_history: VimHistory,
    /// Active and completed Vim commands suspended during temporary draft replacement.
    pub(super) original_vim_state: super::textarea::VimPersistentState,
    /// Footer-owned query text typed or pasted while Ctrl+R search is active.
    pub(super) query: String,
    /// User-visible search status used to choose footer hints and preview behavior.
    status: HistorySearchStatus,
}

impl HistorySearchSession {
    fn preview_draft(&self) -> &ComposerDraft {
        self.preview_draft.as_ref().unwrap_or(&self.original_draft)
    }

    /// Renders newlines and tabs as visible markers for the footer and cursor placement.
    /// Matching continues to use the original query.
    fn display_query(&self) -> String {
        self.query.replace('\n', "␤").replace('\t', "␉")
    }
}

/// User-visible phase of the active Ctrl+R search session.
///
/// Search keeps the footer query and composer preview separate: `Idle` shows
/// the frozen fallback draft, `Searching` covers an in-flight edit,
/// `Match` previews a found entry, and `NoMatch` restores the frozen fallback
/// while leaving the search input open for more typing.
#[derive(Clone, Debug)]
enum HistorySearchStatus {
    Idle,
    Searching,
    Match,
    NoMatch,
}

impl Composer {
    /// Opens footer-owned reverse history search without previewing history yet.
    ///
    /// Entering search mode first integrates already-classified paste-burst
    /// text, then snapshots the full composer draft, clears any file/search
    /// popup state, and resets history traversal. The first visible match is
    /// produced only after the footer query becomes non-empty, which keeps
    /// Ctrl+R from replacing an empty composer with the latest prompt before
    /// the user has searched for anything.
    pub(super) fn begin_history_search(&mut self) -> (InputResult, bool) {
        if self.history_search.is_none() {
            self.flush_pending_input();
            self.draft.paste_burst.clear_window_after_non_char();

            if self.popups.current_file_query.is_some() {
                self.file_search_requests.push(String::new());
                self.popups.current_file_query = None;
            }
            self.popups.active = ActivePopup::None;

            let original_draft = self.snapshot_draft();
            let original_vim_history = std::mem::take(&mut self.vim_history);
            let mut original_vim_state = Default::default();
            self.draft
                .textarea
                .swap_vim_persistent_state(&mut original_vim_state);
            self.history_search = Some(HistorySearchSession {
                preview_draft: None,
                original_draft,
                original_vim_history,
                original_vim_state,
                query: String::new(),
                status: HistorySearchStatus::Idle,
            });
            self.history.reset_search();
        }
        (InputResult::None, true)
    }

    /// Handles every key while the footer is acting as the history search input.
    ///
    /// The method consumes search-mode keys before normal composer editing sees
    /// them. It guarantees that `Esc` and Ctrl+C restore the original draft,
    /// `Enter` only accepts an actual match, plain characters edit the footer
    /// query, and navigation keys delegate traversal to `ChatComposerHistory`.
    pub(super) fn handle_history_search_key(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if key_event.kind == KeyEventKind::Release {
            return (InputResult::None, false);
        }

        if self
            .composer_keymap
            .history_search_previous
            .is_pressed(key_event)
            || matches!(key_event.code, KeyCode::Up)
        {
            let result = self.history_search_in_direction(HistorySearchDirection::Older);
            return (result, true);
        }

        if self
            .composer_keymap
            .history_search_next
            .is_pressed(key_event)
            || matches!(key_event.code, KeyCode::Down)
        {
            let result = self.history_search_in_direction(HistorySearchDirection::Newer);
            return (result, true);
        }

        match key_event {
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                self.cancel_history_search();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Char(c),
                modifiers,
                ..
            } if modifiers.contains(KeyModifiers::CONTROL) && c.eq_ignore_ascii_case(&'c') => {
                self.cancel_history_search();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Char('\u{0003}'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                self.cancel_history_search();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                if self
                    .history_search
                    .as_ref()
                    .is_some_and(|search| matches!(search.status, HistorySearchStatus::Match))
                {
                    self.history_search = None;
                    self.history.reset_search();
                    self.move_cursor_to_end();
                }
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Backspace,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('h'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.update_history_search_query(|query| {
                    query.pop();
                });
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Char('u'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                self.update_history_search_query(String::clear);
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Char(ch),
                modifiers,
                ..
            } if !key_hint::has_ctrl_or_alt(modifiers) => {
                self.update_history_search_query(|query| query.push(ch));
                (InputResult::None, true)
            }
            _ => (InputResult::None, true),
        }
    }

    fn history_search_in_direction(&mut self, direction: HistorySearchDirection) -> InputResult {
        let Some((query, original_draft)) = self
            .history_search
            .as_ref()
            .map(|search| (search.query.clone(), search.preview_draft().clone()))
        else {
            return InputResult::None;
        };
        if query.is_empty() {
            self.history.reset_search();
            if let Some(search) = self.history_search.as_mut() {
                search.status = HistorySearchStatus::Idle;
            }
            self.restore_draft_snapshot(original_draft);
            return InputResult::None;
        }
        let result = self.history.search(&query, direction, /*restart*/ false);
        self.apply_history_search_result(result);
        InputResult::None
    }

    /// Edits the footer query and restarts history traversal from the newest entry.
    /// An empty query restores the original draft and leaves search open.
    pub(super) fn update_history_search_query(&mut self, edit: impl FnOnce(&mut String)) {
        let Some(search) = self.history_search.as_mut() else {
            return;
        };
        edit(&mut search.query);
        search.status = HistorySearchStatus::Searching;
        let query = search.query.clone();
        let original_draft = search.preview_draft().clone();
        self.restore_draft_preserving_search(original_draft);
        if query.is_empty() {
            self.history.reset_search();
            if let Some(search) = self.history_search.as_mut() {
                search.status = HistorySearchStatus::Idle;
            }
            return;
        }
        let result =
            self.history
                .search(&query, HistorySearchDirection::Older, /*restart*/ true);
        self.apply_history_search_result(result);
    }

    /// Cancels active history search and restores the draft from before search mode opened.
    ///
    /// Restores the Vim edit ledger and persistent Vim state captured at search
    /// start so a temporary preview cannot corrupt the original undo history.
    pub(super) fn cancel_history_search(&mut self) -> bool {
        let Some(mut search) = self.history_search.take() else {
            return false;
        };
        self.history.reset_navigation();
        self.restore_draft_snapshot(search.original_draft);
        self.vim_history = search.original_vim_history;
        self.draft
            .textarea
            .swap_vim_persistent_state(&mut search.original_vim_state);
        true
    }

    /// Applies a traversal result to the composer preview and search status.
    ///
    /// `Found` previews the matching entry, `AtBoundary` preserves the current
    /// match, and `NotFound` restores the fallback draft while keeping the query
    /// available for further edits. Treating `AtBoundary` like `NotFound` would
    /// produce the visible "no match" flicker at the end of a one-result search.
    fn apply_history_search_result(&mut self, result: HistorySearchResult) {
        match result {
            HistorySearchResult::Found(entry) => {
                if let Some(search) = self.history_search.as_mut() {
                    search.status = HistorySearchStatus::Match;
                }
                self.apply_history_entry(entry);
            }
            HistorySearchResult::AtBoundary => {
                if let Some(search) = self.history_search.as_mut() {
                    search.status = HistorySearchStatus::Match;
                }
            }
            HistorySearchResult::NotFound => {
                let original_draft = self
                    .history_search
                    .as_ref()
                    .map(|search| search.preview_draft().clone());
                if let Some(search) = self.history_search.as_mut() {
                    search.status = HistorySearchStatus::NoMatch;
                }
                if let Some(original_draft) = original_draft {
                    self.restore_draft_preserving_search(original_draft);
                }
            }
        }
    }

    /// Restore `draft` into the textarea while `history_search` is active —
    /// the session itself (query + original_draft) is untouched.
    fn restore_draft_preserving_search(&mut self, draft: ComposerDraft) {
        self.restore_draft_snapshot(draft);
    }

    /// Footer line shown while search is active, e.g. `reverse-i-search: git`.
    ///
    /// The footer displays the query as the editable field and uses the status
    /// to decide whether to show searching, match actions, or no-match feedback.
    pub(super) fn history_search_footer_line(&self) -> Option<Line<'static>> {
        let search = self.history_search.as_ref()?;
        let mut line = Line::from(vec![
            "reverse-i-search: ".dim(),
            search.display_query().cyan(),
        ]);
        match search.status {
            HistorySearchStatus::Idle => {}
            HistorySearchStatus::Searching => line.push_span("  searching".dim()),
            HistorySearchStatus::Match => {
                line.push_span("  ".dim());
                line.push_span(Span::from(key_hint::plain(KeyCode::Enter)));
                line.push_span(" accept".dim());
                line.push_span(" · ".dim());
                line.push_span(Span::from(key_hint::plain(KeyCode::Esc)));
                line.push_span(" cancel".dim());
            }
            HistorySearchStatus::NoMatch => line.push_span("  no match".red()),
        }
        Some(line)
    }

    /// Returns byte ranges that should be highlighted in the current composer preview.
    ///
    /// Highlights are only exposed while a matched history entry is being
    /// previewed. Once the user accepts with `Enter`, the search session is
    /// cleared and this returns an empty set so the accepted text becomes an
    /// ordinary editable draft again.
    pub(super) fn history_search_highlight_ranges(&self) -> Vec<Range<usize>> {
        let Some(search) = self.history_search.as_ref() else {
            return Vec::new();
        };
        if !matches!(search.status, HistorySearchStatus::Match) || search.query.is_empty() {
            return Vec::new();
        }
        Self::case_insensitive_match_ranges(self.draft.textarea.text(), &search.query)
    }

    fn case_insensitive_match_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
        if query.is_empty() {
            return Vec::new();
        }

        let query_lower = query
            .chars()
            .flat_map(char::to_lowercase)
            .collect::<String>();
        if query_lower.is_empty() {
            return Vec::new();
        }

        let mut folded = String::new();
        let mut folded_spans: Vec<(Range<usize>, Range<usize>)> = Vec::new();
        for (original_start, ch) in text.char_indices() {
            let original_range = original_start..original_start + ch.len_utf8();
            for lower in ch.to_lowercase() {
                let folded_start = folded.len();
                folded.push(lower);
                folded_spans.push((folded_start..folded.len(), original_range.clone()));
            }
        }

        let mut ranges = Vec::new();
        let mut search_from = 0;
        // Use two-pointer method to find matches in linear time.
        let mut start_span = 0;
        let mut end_span = 0;
        while search_from <= folded.len() {
            let Some(relative_start) = folded[search_from..].find(&query_lower) else {
                break;
            };
            let folded_start = search_from + relative_start;
            let folded_end = folded_start + query_lower.len();
            while folded_spans[start_span].0.end <= folded_start {
                start_span += 1;
            }
            while folded_spans[end_span].0.end < folded_end {
                end_span += 1;
            }
            ranges.push(folded_spans[start_span].1.start..folded_spans[end_span].1.end);
            search_from = folded_end;
        }
        ranges
    }

    /// Cursor position for the footer query line (right after the query).
    pub(super) fn history_search_query_cursor_pos(
        &self,
        area: ratatui::layout::Rect,
    ) -> Option<(u16, u16)> {
        let session = self.history_search.as_ref()?;
        let col = "reverse-i-search: ".len() + session.display_query().len();
        (area.width > col as u16).then_some((area.x + col as u16, area.y))
    }
}
