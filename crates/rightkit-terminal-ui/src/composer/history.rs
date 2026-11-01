//! Composer input history: shell-style Up/Down recall plus Ctrl+R incremental
//! search traversal over in-session entries.
//!
//! Adapted from the Codex donor `bottom_pane/chat_composer_history.rs`
//! @ 444da310. The donor interleaves a persistent cross-session history file
//! (async `AppEvent` lookups and batch cursors) with in-session entries. That
//! durable store is parent-owned: this reducer keeps the donor's local-entry
//! navigation and unique-match search algorithms verbatim and exposes
//! [`ChatComposerHistory::persistent`] as the parent's seam for cross-session
//! entries (`None` until the host wires its canonical history source).
//!
//! Ordering invariants preserved from the donor:
//! - `local_history` is oldest-to-newest (newest at END).
//! - `history_cursor` is `None` when not browsing; Down past the newest entry
//!   restores the pre-navigation draft via an empty `HistoryEntry`.
//! - `should_handle_navigation` only recaptures Up/Down while the buffer still
//!   shows the last recalled text with the cursor at a line boundary.
//! - `search` deduplicates on exact prompt text within one session and never
//!   mutates stored history.

use std::collections::HashSet;

use crate::composer::text_element::TextElement;

/// A composer history entry that can rehydrate draft state.
///
/// Reduced donor shape: local in-session submissions keep the text elements
/// and pending-paste payloads needed to restore atomic placeholders.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct HistoryEntry {
    /// Raw text stored in history (may include placeholder strings).
    pub(crate) text: String,
    /// Text element ranges for placeholders inside `text`.
    pub(crate) text_elements: Vec<TextElement>,
    /// Placeholder-to-payload pairs used to restore large paste content.
    pub(crate) pending_pastes: Vec<(String, String)>,
}

impl HistoryEntry {
    /// Creates a text-only history entry.
    pub(crate) fn new(text: String) -> Self {
        Self {
            text,
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HistorySearchDirection {
    /// Traverse toward older history offsets.
    Older,
    /// Traverse toward newer history offsets.
    Newer,
}

/// Result of a single incremental history search step.
///
/// `AtBoundary` means the current selected match is still valid but the
/// requested direction has no further unique match. The donor's `Pending`
/// variant exists only for its async persistent fetch path; this reduced
/// history is synchronous, so an exhausted scan reports `AtBoundary`/`NotFound`
/// directly.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HistorySearchResult {
    Found(HistoryEntry),
    AtBoundary,
    NotFound,
}

/// State for one active Ctrl+R search query.
///
/// `seen_texts` keys on exact prompt text because the UI previews and accepts
/// text, not the storage identity of each historical record.
#[derive(Clone, Debug)]
struct HistorySearchState {
    query: String,
    query_lower: String,
    selected_offset: Option<usize>,
    unique_matches: Vec<UniqueHistoryMatch>,
    selected_match_index: Option<usize>,
    seen_texts: HashSet<String>,
}

impl HistorySearchState {
    fn new(query: &str) -> Self {
        Self {
            query: query.to_string(),
            query_lower: query.to_lowercase(),
            selected_offset: None,
            unique_matches: Vec::new(),
            selected_match_index: None,
            seen_texts: HashSet::new(),
        }
    }
}

/// A unique search match cached with enough draft state to be selected again.
#[derive(Clone, Debug)]
struct UniqueHistoryMatch {
    offset: usize,
    entry: HistoryEntry,
}

/// State machine for shell-style history navigation (Up/Down) and Ctrl+R
/// search inside the composer. Intentionally decoupled from rendering.
#[derive(Clone, Default)]
pub(crate) struct ChatComposerHistory {
    /// Messages submitted during this session (newest at END). Local entries
    /// retain full draft state for placeholder/paste restoration.
    local_history: Vec<HistoryEntry>,
    /// Parent-owned persistent entries, oldest-to-newest. `None` until the
    /// host supplies its canonical durable history; treated as empty then.
    persistent: Option<Vec<HistoryEntry>>,
    /// Current cursor within the combined (persistent + local) history.
    history_cursor: Option<isize>,
    /// The text last inserted into the composer by history navigation; the
    /// "cursor at line boundary" gate in `should_handle_navigation` pairs
    /// with this to keep shell-like recall from stealing multiline movement.
    last_history_text: Option<String>,
    /// Active incremental history search state, if any.
    search: Option<HistorySearchState>,
}

impl ChatComposerHistory {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Supply (or clear) the parent-owned persistent history entries. The
    /// entries are oldest-to-newest and sit before `local_history` in the
    /// combined offset space, matching the donor's layout.
    pub(crate) fn set_persistent_entries(&mut self, entries: Vec<HistoryEntry>) {
        self.persistent = Some(entries);
        self.reset_navigation();
    }

    fn persistent_count(&self) -> usize {
        self.persistent.as_ref().map_or(0, Vec::len)
    }

    fn total_entries(&self) -> usize {
        self.persistent_count() + self.local_history.len()
    }

    /// Combined offset space: `[0, persistent)` are persistent entries,
    /// `[persistent, total)` index into `local_history`.
    fn entry_at(&self, offset: usize) -> Option<HistoryEntry> {
        let persistent = self.persistent_count();
        if offset < persistent {
            return self
                .persistent
                .as_ref()
                .and_then(|p| p.get(offset))
                .cloned();
        }
        self.local_history.get(offset - persistent).cloned()
    }

    /// Record a local in-session submission (newest at END).
    pub(crate) fn record_local_submission(&mut self, entry: HistoryEntry) {
        self.local_history.push(entry);
    }

    /// Whether the composer should hand Up/Down to history recall.
    ///
    /// Donor rules preserved: empty text always navigates; otherwise the
    /// current text must equal the last recalled entry and the cursor must be
    /// at a line boundary (start or end).
    pub(crate) fn should_handle_navigation(&self, text: &str, cursor: usize) -> bool {
        if self.total_entries() == 0 {
            return false;
        }
        if text.is_empty() {
            return true;
        }
        if cursor != 0 && cursor != text.len() {
            return false;
        }
        matches!(&self.last_history_text, Some(prev) if prev == text)
    }

    /// Up: move toward older entries. Returns the entry to display.
    pub(crate) fn navigate_up(&mut self) -> Option<HistoryEntry> {
        self.search = None;
        let total = self.total_entries();
        if total == 0 {
            return None;
        }
        let next_idx = match self.history_cursor {
            None => (total as isize) - 1,
            Some(0) => return None,
            Some(idx) => idx - 1,
        };
        self.history_cursor = Some(next_idx);
        let entry = self.entry_at(next_idx as usize)?;
        self.record_recalled_text(entry.text.clone());
        Some(entry)
    }

    /// Down: move toward newer entries; past the newest returns an empty
    /// entry meaning "restore the pre-navigation draft" and exits browsing.
    pub(crate) fn navigate_down(&mut self) -> Option<HistoryEntry> {
        self.search = None;
        let total = self.total_entries();
        if total == 0 {
            return None;
        }
        let next_idx_opt = match self.history_cursor {
            None => return None,
            Some(idx) if (idx as usize) + 1 >= total => None,
            Some(idx) => Some(idx + 1),
        };
        match next_idx_opt {
            Some(idx) => {
                self.history_cursor = Some(idx);
                let entry = self.entry_at(usize::try_from(idx).ok()?)?;
                self.record_recalled_text(entry.text.clone());
                Some(entry)
            }
            None => {
                self.history_cursor = None;
                self.last_history_text = None;
                Some(HistoryEntry::default())
            }
        }
    }

    /// Track the text currently shown so `should_handle_navigation` knows
    /// whether the next Up/Down continues browsing.
    pub(crate) fn record_recalled_text(&mut self, text: String) {
        self.last_history_text = Some(text);
    }

    pub(crate) fn is_navigating(&self) -> bool {
        self.history_cursor.is_some()
    }

    pub(crate) fn reset_navigation(&mut self) {
        self.history_cursor = None;
        self.last_history_text = None;
        self.search = None;
    }

    /// Clears only the active incremental search state — donor
    /// `reset_search`, used when a Ctrl+R session opens, accepts, or returns
    /// to an empty query.
    pub(crate) fn reset_search(&mut self) {
        self.search = None;
    }

    /// Advance the active Ctrl+R search and return the next visible result.
    ///
    /// `restart` is passed after opening search or editing the query; it
    /// clears the unique-match cache and starts from the end of combined
    /// history. Repeated calls with the same query and `restart == false`
    /// move relative to the current unique match, preserving the selection at
    /// boundaries — the donor's traversal contract, minus its async pending
    /// states (this history is synchronous).
    pub(crate) fn search(
        &mut self,
        query: &str,
        direction: HistorySearchDirection,
        restart: bool,
    ) -> HistorySearchResult {
        let total = self.total_entries();
        if total == 0 {
            self.search = Some(HistorySearchState::new(query));
            return HistorySearchResult::NotFound;
        }

        let query_changed = self
            .search
            .as_ref()
            .is_none_or(|search| search.query != query);
        if query_changed || restart || self.search.is_none() {
            self.search = Some(HistorySearchState::new(query));
        }

        let boundary_if_exhausted = !restart
            && self
                .search
                .as_ref()
                .and_then(|search| search.selected_offset)
                .is_some();
        if !restart && !query_changed {
            if let Some(result) = self.select_cached_unique_match(direction) {
                return result;
            }
        }

        let start_offset = self.search_start_offset(total, direction, query_changed || restart);
        let Some(start_offset) = start_offset else {
            return self.exhausted_search_result(direction, boundary_if_exhausted);
        };

        let result = self.advance_search_from(start_offset, direction);
        if matches!(result, HistorySearchResult::NotFound) {
            self.exhausted_search_result(direction, boundary_if_exhausted)
        } else {
            result
        }
    }

    fn search_start_offset(
        &self,
        total_entries: usize,
        direction: HistorySearchDirection,
        restart: bool,
    ) -> Option<usize> {
        let selected = self
            .search
            .as_ref()
            .and_then(|search| search.selected_offset);
        match direction {
            HistorySearchDirection::Older => {
                if restart {
                    total_entries.checked_sub(1)
                } else {
                    selected.and_then(|offset| offset.checked_sub(1))
                }
            }
            HistorySearchDirection::Newer => {
                if restart {
                    Some(0)
                } else {
                    selected
                        .and_then(|offset| offset.checked_add(1))
                        .filter(|offset| *offset < total_entries)
                }
            }
        }
    }

    /// Walk from `offset` in `direction` until a unique matching entry is
    /// found or the combined history is exhausted.
    fn advance_search_from(
        &mut self,
        mut offset: usize,
        direction: HistorySearchDirection,
    ) -> HistorySearchResult {
        let total = self.total_entries();
        while offset < total {
            if let Some(entry) = self.entry_at(offset) {
                if self.search_matches(&entry) && self.search_result_is_unique(&entry) {
                    return self.search_match(offset, entry);
                }
            }
            match direction {
                HistorySearchDirection::Older => match offset.checked_sub(1) {
                    Some(next) => offset = next,
                    None => break,
                },
                HistorySearchDirection::Newer => {
                    offset += 1;
                }
            }
        }
        HistorySearchResult::NotFound
    }

    fn search_matches(&self, entry: &HistoryEntry) -> bool {
        let Some(search) = &self.search else {
            return false;
        };
        search.query_lower.is_empty() || entry.text.to_lowercase().contains(&search.query_lower)
    }

    fn search_result_is_unique(&self, entry: &HistoryEntry) -> bool {
        let Some(search) = &self.search else {
            return false;
        };
        !search.seen_texts.contains(&entry.text)
    }

    fn search_match(&mut self, offset: usize, entry: HistoryEntry) -> HistorySearchResult {
        let Some(search) = self.search.as_mut() else {
            return HistorySearchResult::NotFound;
        };
        search.seen_texts.insert(entry.text.clone());
        search.selected_offset = Some(offset);
        search.unique_matches.push(UniqueHistoryMatch {
            offset,
            entry: entry.clone(),
        });
        search.selected_match_index = Some(search.unique_matches.len() - 1);
        HistorySearchResult::Found(entry)
    }

    /// Revisit an already-discovered unique match when the requested
    /// direction has one cached, without rescanning duplicate offsets.
    fn select_cached_unique_match(
        &mut self,
        direction: HistorySearchDirection,
    ) -> Option<HistorySearchResult> {
        let search = self.search.as_mut()?;
        let current = search.selected_match_index?;
        let next = match direction {
            HistorySearchDirection::Older => current.checked_add(1)?,
            HistorySearchDirection::Newer => current.checked_sub(1)?,
        };
        let m = search.unique_matches.get(next)?;
        search.selected_match_index = Some(next);
        search.selected_offset = Some(m.offset);
        Some(HistorySearchResult::Found(m.entry.clone()))
    }

    fn exhausted_search_result(
        &self,
        direction: HistorySearchDirection,
        boundary_if_exhausted: bool,
    ) -> HistorySearchResult {
        let _ = direction;
        if boundary_if_exhausted {
            HistorySearchResult::AtBoundary
        } else {
            HistorySearchResult::NotFound
        }
    }
}
