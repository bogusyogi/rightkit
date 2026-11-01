//! Classify and integrate composer pastes and capture raw paste tabs before completion or submission.
//!
//! Lifted from the Codex donor `bottom_pane/chat_composer/paste_input.rs` (and
//! `reconnect.rs::handle_paste_enter`) @ 444da310, reduced at the the host app
//! seams: the donor's image-attachment branch is absent (this composer has no
//! attachment pipeline — pasting an image path inserts the path text), and the
//! donor's sparkle hook is absent. Everything else — CRLF/CR normalization,
//! sanitization, history-search/Vim-query routing, blockquote `> ` prefix
//! continuation, large-paste placeholder insertion into `pending_pastes`,
//! burst-buffer flush ordering, and Enter suppression — is the donor
//! algorithm unchanged.
//!
//! Multiline pastes on a line beginning with `> ` continue that prefix after
//! every newline, including blank and trailing lines. This happens before
//! large pastes are collapsed, so their expanded text retains the quoting.
//! Two unquoted newlines after the inserted text or placeholder leave the
//! cursor in the next Markdown block.
//! Paste classification and insertion share one textarea edit target,
//! including selection replacement.

use std::time::Instant;

use super::textarea::EditTarget;
use super::*;
use crate::composer::paste_burst::FlushResult;
use crate::composer::sanitize::sanitize_user_text;

impl Composer {
    pub(crate) fn insert_str(&mut self, text: &str) {
        self.insert_str_at_target(self.draft.textarea.edit_target(), text);
    }

    pub(super) fn insert_str_at_target(&mut self, target: EditTarget, text: &str) {
        let started_vim_edit = self.begin_direct_vim_edit();
        let elements_before = target
            .replaces_text()
            .then(|| self.draft.textarea.element_payloads());
        self.draft.textarea.insert_str_at_target(target, text);
        if let Some(elements_before) = elements_before {
            self.reconcile_deleted_elements(elements_before);
        }
        self.sync_bash_mode_from_text();
        self.sync_popups();
        if started_vim_edit {
            self.finish_vim_edit();
        }
    }

    /// Classify an explicit paste before integrating text shared with the
    /// buffered key path.
    pub(crate) fn handle_paste(&mut self, pasted: String) -> bool {
        self.apply_paste(pasted)
    }

    /// Enable or disable paste-burst handling.
    ///
    /// `disable_paste_burst` is an escape hatch for terminals/platforms where
    /// the burst heuristic is unwanted or has already been handled elsewhere.
    ///
    /// When transitioning from enabled → disabled, we "defuse" any in-flight
    /// burst state so it cannot affect subsequent normal typing:
    ///
    /// - First, flush held/buffered text via
    ///   [`PasteBurst::flush_before_modified_input`] and integrate it with
    ///   [`Self::apply_paste`]. Its key events have already been classified;
    ///   [`Self::handle_paste`] classifies explicit terminal pastes before
    ///   using the same integration path (large-paste placeholders and popup
    ///   sync).
    /// - Then clear the burst timing and Enter-suppression window via
    ///   [`PasteBurst::clear_after_explicit_paste`].
    ///
    /// We intentionally do not use `clear_window_after_non_char()` here: it
    /// clears timing state without emitting any buffered text, which can leave
    /// a non-empty buffer unable to flush later (because `flush_if_due()`
    /// relies on `last_plain_char_time` to time out).
    #[cfg(test)]
    pub(crate) fn set_disable_paste_burst(&mut self, disabled: bool) {
        let was_disabled = self.draft.disable_paste_burst;
        self.draft.disable_paste_burst = disabled;
        if disabled && !was_disabled {
            if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
                self.apply_paste(pasted);
            }
            self.draft.paste_burst.clear_after_explicit_paste();
        }
    }

    /// Applies any due `PasteBurst` flush at time `now`.
    ///
    /// Converts [`PasteBurst::flush_if_due`] results into concrete textarea
    /// mutations. Buffered keys have already been classified; paste results
    /// use [`Self::apply_paste`].
    ///
    /// Callers:
    ///
    /// - UI ticks via [`Composer::flush_paste_burst_if_due`], so held
    ///   first-chars can render.
    /// - Input handling via [`Composer::handle_input_basic`], so a due burst
    ///   does not lag.
    /// - Submission, before deciding whether Enter belongs to an active paste
    ///   burst.
    pub(super) fn handle_paste_burst_flush(&mut self, now: Instant) -> bool {
        if self.defer_paste_burst_idle_flush && self.draft.paste_burst.is_buffering() {
            return false;
        }
        match self.draft.paste_burst.flush_if_due(now) {
            FlushResult::Paste(pasted) => {
                self.apply_paste(pasted);
                true
            }
            FlushResult::Typed(ch) => {
                self.insert_str(ch.to_string().as_str());
                true
            }
            FlushResult::None => false,
        }
    }

    /// Integrate pasted text into the composer.
    ///
    /// Both input paths integrate their text here:
    ///
    /// - Real/explicit paste events surfaced by the terminal, and
    /// - Non-bracketed "paste bursts" that
    ///   [`PasteBurst`](crate::composer::paste_burst::PasteBurst) buffers and
    ///   later flushes here.
    ///
    /// Behavior:
    ///
    /// - If history search is active, inserts nonempty text into its query and
    ///   ignores empty pastes.
    /// - If Vim search is active, inserts text into its query.
    /// - Otherwise, if the paste is larger than `LARGE_PASTE_CHAR_THRESHOLD`
    ///   chars, inserts a placeholder element (expanded on submit) and stores
    ///   the full text in `pending_pastes`.
    /// - Otherwise, inserts the pasted text directly into the textarea.
    ///
    /// Composer edits clear paste-burst Enter suppression and sync popups.
    pub(super) fn apply_paste(&mut self, pasted: String) -> bool {
        let pasted = pasted.replace("\r\n", "\n").replace('\r', "\n");
        let pasted = sanitize_user_text(pasted.into());
        if self.history_search.is_some() {
            if !pasted.is_empty() {
                self.update_history_search_query(|query| query.push_str(&pasted));
            }
            return true;
        }
        if let Some(query) = self.draft.textarea.vim_query_mut() {
            query.editor.insert_str(&pasted);
            return true;
        }
        let started_vim_edit = self.begin_direct_vim_edit();
        let elements_before = self.draft.textarea.element_payloads();
        let target = self.draft.textarea.edit_target();
        let before_cursor = &self.draft.textarea.text()[..target.start()];
        let current_line = before_cursor.rsplit('\n').next().unwrap_or_default();
        let is_blockquote = !self.is_bang_shell_command()
            && current_line.starts_with("> ")
            && pasted.contains('\n');
        let pasted = if is_blockquote {
            std::borrow::Cow::Owned(pasted.replace('\n', "\n> "))
        } else {
            pasted
        };
        let char_count = pasted.chars().count();
        if char_count > LARGE_PASTE_CHAR_THRESHOLD {
            let placeholder = self.next_large_paste_placeholder(char_count);
            self.draft
                .textarea
                .insert_element_at_target(target, &placeholder);
            self.draft
                .pending_pastes
                .push((placeholder, pasted.into_owned()));
        } else {
            self.insert_str_at_target(target, &pasted);
        }
        if is_blockquote {
            let textarea = &mut self.draft.textarea;
            textarea.insert_str_at(textarea.cursor(), "\n\n");
        }
        self.draft.paste_burst.clear_after_explicit_paste();
        self.reconcile_deleted_elements(elements_before);
        self.sync_popups();
        if started_vim_edit {
            self.finish_vim_edit();
        }
        true
    }

    /// Enter arriving while a fast-typed `/command` is still held by the burst
    /// detector (terminals that deliver keystrokes in one read, e.g. ConPTY):
    /// release the held text into the draft first so the slash popup exists
    /// and Enter accepts it, instead of being swallowed as a pasted newline.
    const NEWLINES: [char; 2] = ['\n', '\r'];

    pub(super) fn release_held_slash_command_before_enter(&mut self, key: KeyEvent) {
        // Enter submits and Tab completes the slash command held in the burst;
        // neither may be swallowed into the burst buffer as pasted whitespace.
        if !matches!(key.code, KeyCode::Enter | KeyCode::Tab)
            || !key.modifiers.is_empty()
            || self.draft.disable_paste_burst
            || self.draft.is_bash_mode
            || !self.slash_commands_enabled()
            || !self.draft.paste_burst.is_active()
        {
            return;
        }
        let held = self.draft.paste_burst.held_text();
        if held.is_empty() || held.contains(Self::NEWLINES) {
            return;
        }
        // Tab completes only a command name still being typed fast; held text
        // with whitespace is a real paste, where Tab is pasted literally.
        if key.code == KeyCode::Tab && held.contains(char::is_whitespace) {
            return;
        }
        let current = self.draft.textarea.text();
        let first_line = format!("{current}{held}");
        if !first_line.starts_with('/') || first_line.contains(Self::NEWLINES) {
            return;
        }
        if let Some(text) = self.draft.paste_burst.flush_before_modified_input() {
            self.apply_paste(text);
        }
        self.draft.paste_burst.clear_after_explicit_paste();
        self.sync_popups();
    }

    /// Preserve Enter inside a paste burst without attempting submission.
    ///
    /// Lifted verbatim from the donor `reconnect.rs`; slash context bypasses
    /// suppression so a submit key can still dispatch a command.
    pub(crate) fn handle_paste_enter(&mut self, now: Instant) -> bool {
        let in_slash_context = self.slash_commands_enabled()
            && !self.draft.is_bash_mode
            && (matches!(self.popups.active, ActivePopup::Command(_))
                || self
                    .draft
                    .textarea
                    .text()
                    .lines()
                    .next()
                    .unwrap_or("")
                    .starts_with('/'));
        if !self.draft.disable_paste_burst
            && self.draft.paste_burst.is_active()
            && !in_slash_context
            && self
                .draft
                .paste_burst
                .append_control_char_if_active('\n', now)
        {
            return true;
        }
        if !in_slash_context
            && !self.draft.disable_paste_burst
            && self
                .draft
                .paste_burst
                .newline_should_insert_instead_of_submit(now)
        {
            self.draft.textarea.insert_str("\n");
            self.draft.paste_burst.extend_window(now);
            return true;
        }

        false
    }

    /// Capture a raw Tab that arrives inside an active paste burst so the key
    /// never triggers completion or focus movement mid-paste.
    pub(super) fn handle_paste_tab(&mut self, key: KeyEvent, now: Instant) -> bool {
        if key.code != KeyCode::Tab
            || !key.modifiers.is_empty()
            || self.draft.disable_paste_burst
            || !self.draft.textarea.allows_paste_burst()
        {
            return false;
        }

        self.handle_paste_burst_flush(now);
        if self
            .draft
            .paste_burst
            .append_control_char_if_active('\t', now)
        {
            return true;
        }

        // Short non-ASCII prefixes are inserted directly, without a held first character.
        if self
            .draft
            .paste_burst
            .direct_insert_newline_should_insert(now)
        {
            self.draft
                .paste_burst
                .begin_with_retro_grabbed(String::new(), now);
            return self
                .draft
                .paste_burst
                .append_control_char_if_active('\t', now);
        }

        false
    }
}

/// Continue the current line's literal `> ` quote prefix across pasted
/// newlines (donor `chat_composer.rs::apply_paste` semantics). A newline
/// sandwiched between two other newlines opens a blank quoted line and keeps
/// only `>`; every other newline — including those entering the first or
/// trailing pasted line — keeps the full `> ` prefix.
#[cfg(test)]
#[path = "blockquote_paste_tests.rs"]
mod blockquote_paste_tests;
