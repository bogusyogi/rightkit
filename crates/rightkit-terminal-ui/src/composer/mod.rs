//! The composer: multi-line editing, paste classification, history recall,
//! and `/`-command/`@`-file completion.
//!
//! # Donor absorption
//!
//! The editing core is the licensed Codex donor implementation @ `444da310`
//! (`bottom_pane/textarea.rs`, `chat_composer.rs`, and their helper modules),
//! adapted behind the existing the host app [`Composer`] host API and
//! [`Intent`] boundary:
//!
//! - [`textarea`] is the donor `TextArea` verbatim: grapheme-accurate editing,
//!   atomic text elements, wrapped-line navigation, Vim state machines (off
//!   by default — `TextArea::set_vim_enabled` is parent-owned policy), and
//!   mouse selection.
//! - `paste_burst` + `paste_input` carry the donor's non-bracketed paste
//!   detection verbatim: 60 ms burst timeout, 120 ms Enter suppression,
//!   retroactive capture, `> ` blockquote continuation, and large-paste
//!   placeholder elements expanded on submit.
//! - `command_popup`, `file_search_popup`, `popup_state`,
//!   `completion_target`, `history`, `history_search`, and
//!   `vim_history` keep the donor state machines; item/query sources are
//!   the host app's shared command catalog and parent-wired file search.
//!
//! # What this module does *not* do
//!
//! It never cancels a turn, aborts a shell, resolves commands, stores durable
//! history, or exits the process. Those effects are [`Intent`]s emitted for
//! the host; drain them with [`Composer::drain_intents`] after every
//! `handle` call. File-search requests are likewise emitted through
//! [`Composer::drain_file_search_requests`] for the parent's canonical file
//! source; results come back via [`Composer::on_file_search_result`].

pub(crate) mod command_popup;
pub mod completion;
pub(crate) mod completion_target;
pub(crate) mod draft_state;
pub mod draft_store;
pub(crate) mod file_match;
pub(crate) mod file_search_popup;
pub(crate) mod fuzzy_match;
pub(crate) mod history;
pub(crate) mod history_search;
pub(crate) mod history_store;
pub mod intent;
pub(crate) mod key_hint;
pub(crate) mod keymap;
pub(crate) mod mention_codec;
pub(crate) mod mention_syntax;
pub(crate) mod paste_burst;
pub(crate) mod paste_input;
pub(crate) mod popup_rows;
pub(crate) mod popup_state;
pub(crate) mod sanitize;
pub(crate) mod scroll_state;
pub(crate) mod slash_input;
pub(crate) mod task_mentions;
pub(crate) mod terminal_hyperlinks;
pub(crate) mod text_element;
pub(crate) mod text_formatting;
pub(crate) mod text_selection;
pub mod textarea;
pub(crate) mod vim_history;
pub(crate) mod vim_search;
pub mod width;
// R17: one copy of each shared helper module. The composer reads the same
// wrapping, truncation and line helpers the transcript and the terminal
// substrate own.
pub(crate) use crate::transcript::{line_truncation, wrapping};

pub use completion::CompletionCandidate;
pub use file_match::{FileMatch, MatchType};
pub use history_store::default_history_path;
pub use intent::Intent;
pub use textarea::TextAreaState;

// Sibling modules reach each other through `crate::composer::<module>::<T>`
// directly; these private imports are only for this file's own use.
use command_popup::CompletionPopup;
pub(crate) use draft_state::ComposerDraft;
use draft_state::DraftState;
use draft_store::DraftStore;
use history::ChatComposerHistory;
use history::HistoryEntry;
use history_search::HistorySearchSession;
use popup_state::ActivePopup;
use popup_state::DismissedToken;
use popup_state::PopupState;
use textarea::TextArea;
use vim_history::VimHistory;

use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::event::{MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::WidgetRef;

use crate::command_effects::CommandEffect;
use crate::component::{Component, Event, Outcome, RenderFrameExt, Role, Theme};
use crate::composer::key_hint::KeyBindingListExt;
use crate::composer::text_element::{ByteRange, TextElement};
use crate::layout_mode::LayoutMode;

/// Double-tap window shared by Ctrl+C-quits and Escape-opens-picker.
const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(500);

/// Never let the composer's height cap drop below this, even on a tiny
/// terminal.
const MIN_HEIGHT: u16 = 5;

/// Prompt marker on the composer's first visual row and the gutter width it
/// reserves on every row. Continuation rows carry a blank gutter of the same
/// width, so wrapped and multiline content stays aligned and the cursor
/// column offset is uniform (`gutter + text column`).
const PROMPT_MARKER: &str = "▌";
const PROMPT_GUTTER: u16 = 2;

/// Placeholder for an empty buffer — the affordance a fresh session shows
/// instead of a bare blank line with the cursor at column zero.
const PLACEHOLDER: &str = "Ask anything · @ to attach files · / for commands";
const PLACEHOLDER_RUNNING: &str = "Type to queue · ⇥ pushes now";
const RUNNING_HINT: &str = "⏎ queue · ⇥ push now";

/// Pastes larger than this collapse to a placeholder element that expands on
/// submit. Verbatim donor constant (`chat_composer.rs`).
pub(crate) const LARGE_PASTE_CHAR_THRESHOLD: usize = 1000;

/// Outcome of a composer key dispatch — internal analogue of the donor
/// `InputResult`, reduced to what the host app's intent boundary produces.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum InputResult {
    None,
    /// A submission was emitted as an [`Intent`].
    Submitted,
}

/// Canonical interactive catalog used before a host supplies turn-aware
/// disabled reasons. Hosts replace it with their own richer command catalog;
/// the default lists `help`, the interactive command-effect verbs and `quit`.
fn default_completion_catalog() -> Vec<CompletionCandidate> {
    let help = CompletionCandidate::new("help", "Show available commands").with_aliases(["?"]);
    let quit = CompletionCandidate::new("quit", "Quit").with_aliases(["exit", "q"]);
    let effects = crate::command_effects::INTERACTIVE_EFFECT_SPECS
        .iter()
        .map(|entry| {
            CompletionCandidate::new(entry.name, entry.summary).with_aliases(
                entry
                    .aliases
                    .iter()
                    .filter(|alias| **alias != entry.name)
                    .copied(),
            )
        });
    std::iter::once(help)
        .chain(effects)
        .chain(std::iter::once(quit))
        .collect()
}

pub struct Composer {
    /// Editable draft: donor `DraftState` (textarea, pending pastes, burst
    /// state, `!` shell-mode flag).
    draft: DraftState,
    /// Shell-style history navigation + Ctrl+R search traversal (local
    /// entries; persistent store is parent-wired).
    history: ChatComposerHistory,
    /// Active Ctrl+R session, if any.
    history_search: Option<HistorySearchSession>,
    /// Bounded draft snapshots backing Vim undo/redo (active only when the
    /// parent enables Vim editing).
    vim_history: VimHistory,
    /// `/`-command and `@`-file popup lifecycle.
    popups: PopupState,
    /// Stable popup instance so `completion()` can always return a reference
    /// even while the popup is hidden.
    idle_popup: CompletionPopup,
    /// Shared command catalog (CG-A01 grammar projection from the host).
    completion_catalog: Vec<CompletionCandidate>,

    intents: Vec<Intent>,
    /// Pending `@` file-search queries for the parent's canonical file
    /// source (drained by the host after `handle`).
    file_search_requests: Vec<String>,

    /// Composer-scope keymap defaults (donor `built_in_defaults`, reduced).
    composer_keymap: keymap::ComposerKeymap,
    editor_keymap: std::sync::Arc<keymap::EditorKeymap>,
    vim_normal_keymap: keymap::VimNormalKeymap,

    last_ctrl_c: Option<Instant>,
    last_escape: Option<Instant>,

    /// Host-reported state driving the Escape priority chain. The composer
    /// only reads these; the host sets them.
    streaming: bool,
    placeholder_override: Option<String>,
    shell_running: bool,
    shell_input_mode: bool,
    /// Messages the host is holding in the mid-turn queue. Drives the
    /// empty-prompt `↑` (edit last queued) and `ctrl+x` (remove it) keys.
    queued: usize,
    /// Host input dispatch is draining a queued burst; defer idle-only flushes
    /// until queue drain completes while preserving explicit control flushes.
    defer_paste_burst_idle_flush: bool,

    terminal_rows: u16,
    last_width: u16,

    /// S:I7 — per-view unsent text, host-driven save/restore.
    drafts: DraftStore,
    /// S:I7 — resolved render mode.
    layout: LayoutMode,
    /// Codex empty-prompt shortcut overlay; never submitted to the provider.
    shortcut_overlay: bool,
    /// Cross-session prompt history file, once the host enables it.
    history_store: Option<history_store::HistoryStore>,
    /// Newest entry in the history file, for duplicate suppression.
    history_store_last: Option<String>,
}

impl Default for Composer {
    fn default() -> Self {
        let runtime_keymap = keymap::RuntimeKeymap::defaults();
        Self {
            draft: DraftState::new(),
            history: ChatComposerHistory::new(),
            history_search: None,
            vim_history: VimHistory::default(),
            popups: PopupState::default(),
            idle_popup: CompletionPopup::new(),
            completion_catalog: default_completion_catalog(),
            intents: Vec::new(),
            file_search_requests: Vec::new(),
            composer_keymap: runtime_keymap.composer.clone(),
            editor_keymap: std::sync::Arc::clone(&runtime_keymap.editor),
            vim_normal_keymap: runtime_keymap.vim_normal.clone(),
            last_ctrl_c: None,
            last_escape: None,
            streaming: false,
            placeholder_override: None,
            shell_running: false,
            shell_input_mode: false,
            queued: 0,
            defer_paste_burst_idle_flush: false,
            terminal_rows: 24,
            last_width: 80,
            drafts: DraftStore::new(),
            layout: LayoutMode::Normal,
            shortcut_overlay: false,
            history_store: None,
            history_store_last: None,
        }
    }
}

impl Composer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Back Up / Ctrl+R with the cross-session history file at `path`: its
    /// newest entries load now (before this session's own), and every later
    /// submission is appended. Pass [`default_history_path`] (under the host state root) for the
    /// shared per-user file; failures degrade to session-only history.
    pub fn enable_persistent_history(&mut self, path: std::path::PathBuf) {
        let store = history_store::HistoryStore::new(path);
        let entries = store.load();
        self.history_store_last = entries.last().cloned();
        self.history
            .set_persistent_entries(entries.into_iter().map(HistoryEntry::new).collect());
        self.history_store = Some(store);
    }

    // -- host-driven state, for the Escape priority chain -----------------

    pub fn set_streaming(&mut self, streaming: bool) {
        self.streaming = streaming;
        if streaming {
            self.placeholder_override = None;
        }
    }

    /// One-shot empty-buffer placeholder (e.g. "Tell `<model>` what to do
    /// instead" after an approval Esc). Cleared when the next turn starts.
    pub fn set_placeholder_override(&mut self, text: Option<String>) {
        self.placeholder_override = text;
    }

    pub fn placeholder_override(&self) -> Option<&str> {
        self.placeholder_override.as_deref()
    }

    pub fn set_shell_running(&mut self, running: bool) {
        self.shell_running = running;
    }

    /// How many messages the host currently holds queued behind the turn.
    pub fn set_queued_count(&mut self, queued: usize) {
        self.queued = queued;
    }

    pub fn queued_count(&self) -> usize {
        self.queued
    }

    pub fn set_shell_input_mode(&mut self, active: bool) {
        self.shell_input_mode = active;
    }

    pub fn set_defer_paste_burst_idle_flush(&mut self, defer: bool) {
        self.defer_paste_burst_idle_flush = defer;
    }

    /// Drain everything the composer wants the host to do since the last
    /// call. Call after every `handle`.
    pub fn drain_intents(&mut self) -> Vec<Intent> {
        std::mem::take(&mut self.intents)
    }

    /// Drain pending `@` file-search queries for the parent's file source.
    /// Each drained query should eventually be answered with
    /// [`Composer::on_file_search_result`].
    pub fn drain_file_search_requests(&mut self) -> Vec<String> {
        std::mem::take(&mut self.file_search_requests)
    }

    /// Integrate results from an asynchronous file search (donor
    /// `on_file_search_result`).
    pub fn on_file_search_result(&mut self, query: String, matches: Vec<file_match::FileMatch>) {
        let Some((_, token)) = Self::current_prefixed_token(&self.draft.textarea, '@', false)
        else {
            return;
        };
        if token != query {
            return;
        }
        if let ActivePopup::File(popup) = &mut self.popups.active {
            popup.set_matches(&query, matches);
        }
    }

    /// Flush a due non-bracketed paste burst. The host should call this on
    /// its frame/tick cadence so a held first char renders; `handle` also
    /// flushes when an event arrives.
    pub fn flush_paste_burst_if_due(&mut self) -> bool {
        self.handle_paste_burst_flush(Instant::now())
    }

    /// Whether the composer is in any paste-burst transient state.
    pub fn is_in_paste_burst(&self) -> bool {
        self.draft.paste_burst.is_active()
    }

    pub fn shortcut_overlay_visible(&self) -> bool {
        self.shortcut_overlay
    }

    /// A delay that reliably exceeds the paste-burst timing threshold (test
    /// and host scheduling aid — donor `recommended_paste_flush_delay`).
    pub fn recommended_paste_flush_delay() -> Duration {
        paste_burst::PasteBurst::recommended_flush_delay()
    }

    /// Whether a popup or query owns input (donor `popup_active`).
    pub fn popup_active(&self) -> bool {
        self.history_search.is_some()
            || self.draft.textarea.vim_query().is_some()
            || self.popups.active()
    }

    // -- S:I10 completion popup --------------------------------------------

    /// Replace the popup's candidate catalog. The host calls this with the
    /// shared command registry's projection plus the six command effects.
    pub fn set_completion_catalog(&mut self, catalog: Vec<CompletionCandidate>) {
        self.completion_catalog = catalog.clone();
        if let ActivePopup::Command(popup) = &mut self.popups.active {
            popup.set_catalog(catalog.clone());
        }
        self.idle_popup.set_catalog(catalog);
    }

    /// Re-evaluate popup open/filter state from the current text. Kept for
    /// host compatibility; internally `sync_popups` runs after every key.
    pub fn sync_completion(&mut self) {
        self.sync_popups();
    }

    /// Popup state for the host's render pass.
    pub fn completion(&self) -> &CompletionPopup {
        match &self.popups.active {
            ActivePopup::Command(popup) => popup,
            _ => &self.idle_popup,
        }
    }

    /// Whether the command-completion popup is currently open.
    pub fn completion_open(&self) -> bool {
        matches!(self.popups.active, ActivePopup::Command(_))
    }

    /// Accept the highlighted command row into the buffer: complete the
    /// `/name` token to `/name ` and close the popup. Disabled rows cannot
    /// be accepted.
    pub fn accept_completion(&mut self) -> bool {
        let ActivePopup::Command(popup) = &mut self.popups.active else {
            return false;
        };
        let Some(candidate) = popup.selected_candidate().cloned() else {
            return false;
        };
        if candidate.disabled_reason.is_some() {
            return false;
        }
        self.insert_selected_command_name(&candidate.name);
        self.popups.active = ActivePopup::None;
        true
    }

    /// Complete the active `/name` token to `/{name} `, preserving the rest
    /// of the draft (donor completion semantics reduced to the catalog).
    fn insert_selected_command_name(&mut self, name: &str) {
        let text = self.draft.textarea.text().to_string();
        let first_line_end = text.find('\n').unwrap_or(text.len());
        if !text.starts_with('/') {
            return;
        }
        // Replace the `/name` token on the first line, keeping any tail.
        let token_end = text[..first_line_end]
            .find(char::is_whitespace)
            .unwrap_or(first_line_end);
        self.draft
            .textarea
            .replace_range(0..token_end, &format!("/{name}"));
        self.advance_past_completion_separator();
        self.history.reset_navigation();
    }

    // -- S:I7 draft preservation -------------------------------------------

    /// Save the current (expanded) buffer text as `view`'s draft. An empty
    /// buffer clears the draft.
    pub fn save_draft(&mut self, view: &str) {
        let text = self.current_text_expanded();
        self.drafts.save(view, text);
    }

    /// Restore `view`'s draft into the buffer (replacing its contents) and
    /// remove it from the store. Returns whether a draft was restored.
    pub fn restore_draft(&mut self, view: &str) -> bool {
        let Some(text) = self.drafts.take(view) else {
            return false;
        };
        self.set_text(&text);
        true
    }

    /// Whether `view` has an unsaved draft (for "draft pending" affordances).
    pub fn has_draft(&self, view: &str) -> bool {
        self.drafts.has_draft(view)
    }

    // -- S:I7 layout mode ---------------------------------------------------

    pub fn set_layout_mode(&mut self, layout: LayoutMode) {
        self.layout = layout;
    }

    pub fn layout_mode(&self) -> LayoutMode {
        self.layout
    }

    // -- draft snapshot helpers (shared by Vim history + history search) ---

    /// Donor `snapshot_draft`: snapshots the *canonical* draft — the shell
    /// `!` prefix and its element/cursor shift are part of the snapshot so a
    /// Vim undo round-trips bash-mode drafts.
    pub(crate) fn snapshot_draft(&self) -> ComposerDraft {
        ComposerDraft {
            text: self.current_text(),
            cursor: self.current_cursor(),
            text_elements: self.current_text_elements(),
            pending_pastes: self.draft.pending_pastes.clone(),
        }
    }

    /// Donor `restore_draft` reduced to composer-owned fields: re-imports the
    /// canonical text (absorbing a leading `!` back into bash-mode prompt
    /// state) and rebuilds elements, pastes, and cursor.
    pub(crate) fn restore_draft_snapshot(&mut self, draft: ComposerDraft) {
        let ComposerDraft {
            text,
            cursor,
            text_elements,
            pending_pastes,
        } = draft;
        let (text, text_elements) = self.imported_text_for_textarea(text, text_elements);
        self.draft
            .textarea
            .set_text_with_elements(&text, &text_elements);
        self.draft.pending_pastes = pending_pastes;
        self.set_current_cursor(cursor);
        self.sync_popups();
    }

    /// Donor `imported_text_for_textarea`: shell mode stores the leading `!`
    /// as prompt state instead of editable text, so full-buffer imports must
    /// absorb that prefix before rebuilding the textarea.
    fn imported_text_for_textarea(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
    ) -> (String, Vec<TextElement>) {
        if let Some(stripped) = text.strip_prefix('!') {
            self.draft.is_bash_mode = true;
            (
                stripped.to_string(),
                text_elements
                    .into_iter()
                    .filter_map(|element| Self::shift_text_element(element, -1))
                    .collect(),
            )
        } else {
            self.draft.is_bash_mode = false;
            (text, text_elements)
        }
    }

    /// Donor `set_current_cursor`: canonical cursors include the virtual `!`
    /// column, so translate back to the textarea's visible cursor.
    fn set_current_cursor(&mut self, cursor: usize) {
        let visible_cursor = if self.draft.is_bash_mode {
            cursor.saturating_sub(1)
        } else {
            cursor
        };
        self.draft
            .textarea
            .set_cursor(visible_cursor.min(self.draft.textarea.text().len()));
    }

    /// Canonical draft text — donor `current_text`: shell mode stores its
    /// leading `!` as prompt state outside the textarea, so it is re-prefixed
    /// here for submission, history, and bang-command detection.
    fn current_text(&self) -> String {
        if self.draft.is_bash_mode {
            format!("!{}", self.draft.textarea.text())
        } else {
            self.draft.textarea.text().to_string()
        }
    }

    /// Donor `current_text_elements`: element byte ranges are expressed
    /// against canonical text, so bash mode shifts them one byte right.
    fn current_text_elements(&self) -> Vec<TextElement> {
        let shift = isize::from(self.draft.is_bash_mode);
        self.draft
            .textarea
            .text_elements()
            .into_iter()
            .filter_map(|element| Self::shift_text_element(element, shift))
            .collect()
    }

    /// Donor `shift_text_element`: rebase one element range by `shift`,
    /// dropping it when the range would invert or underflow.
    fn shift_text_element(element: TextElement, shift: isize) -> Option<TextElement> {
        let start = element.byte_range.start.checked_add_signed(shift)?;
        let end = element.byte_range.end.checked_add_signed(shift)?;
        (start < end).then(|| element.map_range(|_| (start..end).into()))
    }

    /// Text with pending large-paste placeholders expanded (what submission
    /// and draft save hand to the host).
    fn current_text_expanded(&self) -> String {
        Self::expand_pending_pastes(
            self.draft.textarea.text(),
            self.draft.textarea.text_elements(),
            &self.draft.pending_pastes,
        )
        .0
    }

    /// Capped composer height: `max(5, 30% of terminal rows)`.
    fn max_height(&self) -> u16 {
        let thirty_pct = (self.terminal_rows as u32 * 3 / 10) as u16;
        thirty_pct.max(MIN_HEIGHT)
    }

    // -- submission --------------------------------------------------------

    /// Donor `is_bang_shell_command`: leading `!` marks shell-mode text
    /// (composer `!` notation is always supported; the host decides routing).
    fn is_bang_shell_command(&self) -> bool {
        self.current_text().trim_start().starts_with('!')
    }

    fn slash_commands_enabled(&self) -> bool {
        !self.completion_catalog.is_empty()
    }

    /// Donor `sync_bash_mode_from_text`: a leading `!` typed into the
    /// textarea converts to shell-mode prompt state (the `!` is stored
    /// outside the editable text).
    fn sync_bash_mode_from_text(&mut self) {
        if !self.draft.is_bash_mode && self.draft.textarea.text().starts_with('!') {
            self.draft.textarea.replace_range(0..1, "");
            self.draft.is_bash_mode = true;
        }
    }

    fn reconcile_deleted_elements(&mut self, elements_before: Vec<String>) {
        let elements_after: HashSet<String> =
            self.draft.textarea.element_payloads().into_iter().collect();
        let removed_payloads = elements_before
            .into_iter()
            .filter(|payload| !elements_after.contains(payload))
            .collect::<Vec<_>>();
        for removed in &removed_payloads {
            self.draft.pending_pastes.retain(|(ph, _)| ph != removed);
        }
    }

    fn next_large_paste_placeholder(&self, char_count: usize) -> String {
        let base = format!("[Pasted Content {char_count} chars]");
        let prefix = format!("{base} #");
        let mut max_suffix = 0usize;

        for (placeholder, _) in &self.draft.pending_pastes {
            if placeholder == &base {
                max_suffix = max_suffix.max(1);
                continue;
            }
            if let Some(suffix) = placeholder.strip_prefix(&prefix)
                && let Ok(value) = suffix.parse::<usize>()
            {
                max_suffix = max_suffix.max(value);
            }
        }

        if max_suffix == 0 {
            base
        } else {
            format!("{base} #{}", max_suffix + 1)
        }
    }

    /// Expand large-paste placeholders using element ranges and rebuild other
    /// element spans — verbatim donor `expand_pending_pastes`.
    pub(crate) fn expand_pending_pastes(
        text: &str,
        mut elements: Vec<TextElement>,
        pending_pastes: &[(String, String)],
    ) -> (String, Vec<TextElement>) {
        if pending_pastes.is_empty() || elements.is_empty() {
            return (text.to_string(), elements);
        }

        // Stage 1: index pending paste payloads by placeholder for
        // deterministic replacements.
        let mut pending_by_placeholder: HashMap<&str, VecDeque<&str>> = HashMap::new();
        for (placeholder, actual) in pending_pastes {
            pending_by_placeholder
                .entry(placeholder.as_str())
                .or_default()
                .push_back(actual.as_str());
        }

        // Stage 2: walk elements in order and rebuild text/spans in a single pass.
        elements.sort_by_key(|elem| elem.byte_range.start);

        let mut rebuilt = String::with_capacity(text.len());
        let mut rebuilt_elements = Vec::with_capacity(elements.len());
        let mut cursor = 0usize;

        for elem in elements {
            let start = elem.byte_range.start.min(text.len());
            let end = elem.byte_range.end.min(text.len());
            if start > end {
                continue;
            }
            if start > cursor {
                rebuilt.push_str(&text[cursor..start]);
            }
            let elem_text = &text[start..end];
            let placeholder = elem.placeholder(text).map(str::to_string);
            let replacement = placeholder
                .as_deref()
                .and_then(|ph| pending_by_placeholder.get_mut(ph))
                .and_then(VecDeque::pop_front);
            if let Some(actual) = replacement {
                // Stage 3: inline actual paste payloads and drop their
                // placeholder elements.
                rebuilt.push_str(actual);
            } else {
                // Stage 4: keep non-paste elements, updating their byte
                // ranges for the new text.
                let new_start = rebuilt.len();
                rebuilt.push_str(elem_text);
                let new_end = rebuilt.len();
                let placeholder = placeholder.or_else(|| Some(elem_text.to_string()));
                rebuilt_elements.push(TextElement::new(
                    ByteRange {
                        start: new_start,
                        end: new_end,
                    },
                    placeholder,
                ));
            }
            cursor = end;
        }

        // Stage 5: append any trailing text that followed the last element.
        if cursor < text.len() {
            rebuilt.push_str(&text[cursor..]);
        }

        (rebuilt, rebuilt_elements)
    }

    /// Rebuild element byte ranges after trimming (verbatim donor
    /// `trim_text_elements`).
    fn trim_text_elements(
        original: &str,
        trimmed: &str,
        elements: Vec<TextElement>,
    ) -> Vec<TextElement> {
        if trimmed.is_empty() || elements.is_empty() {
            return Vec::new();
        }
        let trimmed_start = original.len().saturating_sub(original.trim_start().len());
        let trimmed_end = trimmed_start.saturating_add(trimmed.len());

        elements
            .into_iter()
            .filter_map(|elem| {
                let start = elem.byte_range.start;
                let end = elem.byte_range.end;
                if end <= trimmed_start || start >= trimmed_end {
                    return None;
                }
                let new_start = start.saturating_sub(trimmed_start);
                let new_end = end.saturating_sub(trimmed_start).min(trimmed.len());
                if new_start >= new_end {
                    return None;
                }
                let placeholder = trimmed.get(new_start..new_end).map(str::to_string);
                Some(TextElement::new(
                    ByteRange {
                        start: new_start,
                        end: new_end,
                    },
                    placeholder,
                ))
            })
            .collect()
    }

    /// Record a submission into local history (verbatim donor call shape).
    fn record_submission_history(
        &mut self,
        text: String,
        text_elements: Vec<TextElement>,
        pending_pastes: Vec<(String, String)>,
    ) {
        if text.is_empty() {
            return;
        }
        if let Some(store) = &self.history_store {
            store.append(&text, self.history_store_last.as_deref());
            self.history_store_last = Some(text.clone());
        }
        self.history.record_local_submission(HistoryEntry {
            text,
            text_elements,
            pending_pastes,
        });
    }

    /// Enter on a non-popup composer: donor `handle_submission` reduced to
    /// the host app's intent boundary — pending pastes expand, the first line's
    /// `/name` resolves through `CommandEffect` (the six truthful effects),
    /// and everything else becomes `Intent::Submit` for the host to route.
    ///
    /// `should_queue` mirrors the donor flag: queued submissions skip the
    /// paste-burst Enter suppression so a modified queue binding still
    /// dispatches while a burst is active.
    fn handle_submission(&mut self, should_queue: bool, now: Instant) -> (InputResult, bool) {
        self.handle_paste_burst_flush(now);
        if !should_queue && self.handle_paste_enter(now) {
            return (InputResult::None, true);
        }
        if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
            self.apply_paste(pasted);
        }

        let original_input = self.current_text();
        let original_text_elements = self.current_text_elements();
        let original_pending_pastes = self.draft.pending_pastes.clone();

        // S:I5 — the six truthful effects resolve before submission, against
        // the shared grammar.
        if let Some((name, rest, _)) = slash_input::parse_slash_name(&original_input)
            && let Some(effect) = CommandEffect::resolve(name)
            && rest.trim().is_empty()
        {
            self.record_submission_history(
                original_input.trim().to_string(),
                Vec::new(),
                Vec::new(),
            );
            self.clear_draft();
            self.intents.push(Intent::CommandEffect(effect));
            return (InputResult::Submitted, true);
        }

        // Expand placeholders so element byte ranges stay aligned.
        let (mut text, mut text_elements) = Self::expand_pending_pastes(
            &original_input,
            original_text_elements.clone(),
            &self.draft.pending_pastes,
        );
        let expanded = text.clone();
        text = text.trim().to_string();
        text_elements = Self::trim_text_elements(&expanded, &text, text_elements);

        if text.is_empty() && self.draft.pending_pastes.is_empty() {
            return (InputResult::None, true);
        }

        // A literal `!`-command inside host shell-input mode stays literal.
        if self.shell_input_mode
            && text.starts_with('!')
            && !original_input.trim_start().starts_with('!')
        {
            text = original_input.trim().to_string();
        }

        self.record_submission_history(
            text.clone(),
            text_elements.clone(),
            original_pending_pastes,
        );
        self.clear_draft();
        self.intents.push(Intent::Submit(text));
        (InputResult::Submitted, true)
    }

    fn clear_draft(&mut self) {
        self.draft.textarea.set_text_clearing_elements("");
        self.draft.pending_pastes.clear();
        self.draft.is_bash_mode = false;
        self.popups.active = ActivePopup::None;
        self.history.reset_navigation();
        self.vim_history = VimHistory::default();
    }

    fn apply_history_entry(&mut self, entry: HistoryEntry) {
        self.flush_pending_input();
        self.draft
            .textarea
            .set_text_with_elements(&entry.text, &entry.text_elements);
        self.draft.pending_pastes = entry.pending_pastes;
        self.draft.is_bash_mode = false;
        let recalled = self.current_text();
        self.history.record_recalled_text(recalled);
        self.move_cursor_to_history_entry_end();
    }

    /// Donor `move_cursor_to_history_entry_end`: recalled/previewed entries
    /// land at the end of the text — the Vim normal end cursor when normal
    /// mode owns the cursor.
    fn move_cursor_to_history_entry_end(&mut self) {
        let cursor = if self.draft.textarea.is_vim_normal_mode() {
            self.draft.textarea.vim_normal_end_cursor()
        } else {
            self.draft.textarea.text().len()
        };
        self.draft.textarea.set_cursor(cursor);
        self.sync_popups();
    }

    /// Integrate already-classified buffered typing before replacing content.
    pub(crate) fn flush_pending_input(&mut self) {
        if let Some(text) = self.draft.paste_burst.flush_before_modified_input() {
            self.apply_paste(text);
        }
        self.draft.paste_burst.clear_after_explicit_paste();
    }

    // -- key dispatch -------------------------------------------------------

    fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        if key.kind == KeyEventKind::Release {
            return Outcome::Ignored;
        }
        // the host app intents that must preempt the donor pipeline.
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') | KeyCode::Char('C') if ctrl => return self.handle_ctrl_c(),
            // ctrl+x removes the last queued message, but only from an empty
            // prompt: with text it keeps its editor meaning.
            KeyCode::Char('x') | KeyCode::Char('X')
                if ctrl && self.queued > 0 && self.is_empty() =>
            {
                self.intents.push(Intent::RemoveLastQueued);
                return Outcome::Redraw;
            }
            KeyCode::Char('d') | KeyCode::Char('D') if ctrl => {
                if self.is_empty() {
                    self.intents.push(Intent::Quit);
                    return Outcome::Consumed;
                }
            }
            // Shift+Tab cycles the permission mode (Ask → Accept edits →
            // Auto → Yolo). Nothing else binds BackTab; plain Tab keeps its
            // completion meaning.
            KeyCode::Char('g') | KeyCode::Char('G')
                if ctrl && !key.modifiers.contains(KeyModifiers::ALT) && !self.popup_active() =>
            {
                let text = self.current_text_expanded();
                self.intents.push(Intent::OpenExternalEditor(text));
                return Outcome::Consumed;
            }
            KeyCode::BackTab => {
                self.intents.push(Intent::CyclePermissionMode);
                return Outcome::Redraw;
            }
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.intents.push(Intent::CyclePermissionMode);
                return Outcome::Redraw;
            }
            _ => {}
        }
        let (_result, redraw) = self.handle_key_event_inner(*key);
        // Keep `is_in_paste_burst` Enter suppression visible to the host.
        if _result == InputResult::Submitted {
            self.reset_vim_mode();
        }
        if redraw {
            Outcome::Redraw
        } else {
            Outcome::Consumed
        }
    }

    /// Donor `handle_key_event_inner` dispatch order, preserved:
    /// vim-search keys → history search → vim history → Ctrl+R entry →
    /// paste-tab capture → popup handlers → no-popup handler → popup sync.
    fn handle_key_event_inner(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        // Donor chat_composer::handle_empty_prompt_shortcut: Press only,
        // empty draft, no paste burst. Questions inside text remain literal.
        if key_event.kind == KeyEventKind::Press && !self.popup_active() {
            if self.shortcut_overlay && key_event.code == KeyCode::Esc {
                self.shortcut_overlay = false;
                return (InputResult::None, true);
            }
            if self.composer_keymap.toggle_shortcuts.is_pressed(key_event)
                && self.is_empty()
                && !self.is_in_paste_burst()
            {
                self.shortcut_overlay = !self.shortcut_overlay;
                return (InputResult::None, true);
            }
            self.shortcut_overlay = false;
        }
        if self.history_search.is_none()
            && !self.popups.active()
            && self.draft.textarea.wants_vim_search_key(key_event)
        {
            return self.handle_input_basic(key_event);
        }

        if self.history_search.is_some() {
            return self.handle_history_search_key(key_event);
        }

        if self.handle_vim_history_key(key_event) {
            return (InputResult::None, true);
        }

        if self
            .composer_keymap
            .history_search_previous
            .is_pressed(key_event)
        {
            return self.begin_history_search();
        }

        // Release a slash command held in the paste burst before Enter/Tab
        // so Tab is not captured as pasted whitespace and Enter can submit.
        self.release_held_slash_command_before_enter(key_event);
        if self.handle_paste_tab(key_event, Instant::now()) {
            return (InputResult::None, true);
        }

        // Completion must never run on a stale draft: a `/` while the command
        // popup is open accepts the highlighted command from the textarea
        // text, but fast typing leaves the rest of the line (`orktree add ..`)
        // still held by the paste burst, so completing `/w` first and
        // appending the held text later yields `/worktree orktree add ..`.
        // Release the held text into the draft first; the popup then
        // re-evaluates against the full line.
        if matches!(self.popups.active, ActivePopup::Command(_))
            && key_event.kind != KeyEventKind::Release
            && key_event.code == KeyCode::Char('/')
            && !self.draft.disable_paste_burst
            && self.draft.paste_burst.is_active()
            && let Some(text) = self.draft.paste_burst.flush_before_modified_input()
        {
            self.apply_paste(text);
            self.draft.paste_burst.clear_after_explicit_paste();
            self.sync_popups();
        }

        let result = match &mut self.popups.active {
            ActivePopup::Command(_) => self.handle_key_event_with_slash_popup(key_event),
            ActivePopup::File(_) => self.handle_key_event_with_file_popup(key_event),
            ActivePopup::None => self.handle_key_event_without_popup(key_event),
        };
        if result.0 == InputResult::Submitted {
            self.reset_vim_mode();
        }
        // Update (or hide/show) popup after processing the key.
        self.sync_popups();
        result
    }

    /// `Esc` disambiguation + the host app's escape priority chain, evaluated
    /// when no popup owns the key (donor path preserved first: `!`-mode exit
    /// and Vim insert/operator escapes run before this in
    /// `handle_key_event_without_popup`).
    fn handle_escape_chain(&mut self, key_event: KeyEvent) -> Option<(InputResult, bool)> {
        if key_event.code != KeyCode::Esc {
            return None;
        }
        if self.streaming {
            // Esc cancels only from an empty prompt. With text it does
            // nothing at all: it never sends, queues or unqueues.
            if self.draft.textarea.is_empty() {
                self.intents.push(Intent::CancelStreamingTurn);
            }
            return Some((InputResult::None, true));
        }
        if self.shell_running {
            self.intents.push(Intent::AbortRunningShell);
            return Some((InputResult::None, true));
        }
        if self.shell_input_mode {
            self.intents.push(Intent::ExitShellInputMode);
            return Some((InputResult::None, true));
        }
        if self.draft.textarea.is_empty() {
            let now = Instant::now();
            if let Some(last) = self.last_escape
                && now.duration_since(last) <= DOUBLE_TAP_WINDOW
            {
                self.intents.push(Intent::OpenPicker);
                self.last_escape = None;
                return Some((InputResult::None, true));
            }
            self.last_escape = Some(now);
        }
        Some((InputResult::None, true))
    }

    /// Handle key events when the `/` command popup is visible — donor
    /// `handle_key_event_with_slash_popup` reduced to the catalog seam.
    fn handle_key_event_with_slash_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if key_event.code == KeyCode::Esc {
            // Always dismiss the popup without changing the draft.
            let first_line = self.draft.textarea.text().lines().next().unwrap_or("");
            self.popups.dismissed_command_token =
                slash_input::command_popup_filter_text(first_line, /*cursor*/ 0);
            self.popups.active = ActivePopup::None;
            return (InputResult::None, true);
        }
        let ActivePopup::Command(popup) = &mut self.popups.active else {
            unreachable!();
        };

        match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_up();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_down();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            } => {
                // Ensure popup filtering/selection reflects the latest
                // composer text before applying completion.
                let text = self.draft.textarea.text();
                let first_line = text.lines().next().unwrap_or("").to_owned();
                let cursor = self.draft.textarea.cursor();
                let filter_text = slash_input::command_popup_filter_text(&first_line, cursor)
                    .unwrap_or_else(|| first_line.clone());
                popup.on_composer_text_change(&filter_text);
                let selected = popup.selected_candidate().map(|c| c.name.clone());
                if let Some(name) = selected
                    && self
                        .completion_catalog
                        .iter()
                        .find(|c| c.name == name)
                        .and_then(|c| c.disabled_reason.as_ref())
                        .is_none()
                {
                    self.insert_selected_command_name(&name);
                    self.popups.active = ActivePopup::None;
                    return (InputResult::None, true);
                }
                if self.streaming {
                    return self.handle_submission(true, Instant::now());
                }
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Char('/'),
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                // Treat "/" as accepting the highlighted command as text
                // completion while the slash-command popup is active.
                let text = self.draft.textarea.text();
                let first_line = text.lines().next().unwrap_or("").to_owned();
                let cursor = self.draft.textarea.cursor();
                let filter_text = slash_input::command_popup_filter_text(&first_line, cursor)
                    .unwrap_or_else(|| first_line.clone());
                popup.on_composer_text_change(&filter_text);
                let selected = popup.selected_candidate().map(|c| c.name.clone());
                if let Some(name) = selected
                    && self
                        .completion_catalog
                        .iter()
                        .find(|c| c.name == name)
                        .and_then(|c| c.disabled_reason.as_ref())
                        .is_none()
                {
                    self.insert_selected_command_name(&name);
                }
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                // Enter dispatches the highlighted command — the donor
                // returns `InputResult::Command`; this seam completes the
                // `/name` token then submits so the shared grammar resolves
                // `CommandEffect` vs `Intent::Submit`. Disabled rows are
                // visible but not acceptable.
                let selected = popup.selected_candidate().map(|c| c.name.clone());
                if let Some(name) = selected
                    && self
                        .completion_catalog
                        .iter()
                        .find(|c| c.name == name)
                        .and_then(|c| c.disabled_reason.as_ref())
                        .is_none()
                {
                    self.insert_selected_command_name(&name);
                    return self.handle_submission(false, Instant::now());
                }
                // Fallback to default newline handling if no command selected.
                self.handle_key_event_without_popup(key_event)
            }
            input => self.handle_input_basic(input),
        }
    }

    /// Handle key events when the `@` file-search popup is visible — donor
    /// `handle_key_event_with_file_popup` verbatim minus footer plumbing.
    fn handle_key_event_with_file_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        let ActivePopup::File(popup) = &mut self.popups.active else {
            unreachable!();
        };

        match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_up();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            } => {
                popup.move_down();
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Esc, ..
            } => {
                if let Some((range, query)) = completion_target::current_prefixed_token_range(
                    &self.draft.textarea,
                    '@',
                    /*allow_empty*/ false,
                ) {
                    self.popups.dismissed_file_token =
                        Some(DismissedToken::new(&self.draft.textarea, range, query));
                }
                self.popups.active = ActivePopup::None;
                (InputResult::None, true)
            }
            KeyEvent {
                code: KeyCode::Tab, ..
            }
            | KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => {
                let Some(sel) = popup.selected_match() else {
                    self.popups.active = ActivePopup::None;
                    return if key_event.code == KeyCode::Enter {
                        self.handle_key_event_without_popup(key_event)
                    } else {
                        (InputResult::None, true)
                    };
                };

                let sel_path = sel.to_string_lossy().replace('\\', "/");
                if let Some((token_range, _)) =
                    self.current_editable_at_token_range_with_options(/*allow_empty*/ false)
                {
                    self.insert_selected_path(token_range, &sel_path);
                }
                self.popups.active = ActivePopup::None;
                (InputResult::None, true)
            }
            input => self.handle_input_basic(input),
        }
    }

    /// Handle key events when no popup is visible — donor
    /// `handle_key_event_without_popup` reduced at the the host app seams:
    /// submit keys emit intents, Up/Down drive local history recall, and the
    /// escape priority chain is the host app's.
    fn handle_key_event_without_popup(&mut self, key_event: KeyEvent) -> (InputResult, bool) {
        if self.draft.is_bash_mode && key_event.code == KeyCode::Esc {
            if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
                self.apply_paste(pasted);
            }
            if self.draft.textarea.is_empty() {
                self.draft.is_bash_mode = false;
                return (InputResult::None, true);
            }
        }
        if self
            .draft
            .textarea
            .should_handle_vim_insert_escape(key_event)
            || (self.draft.textarea.is_vim_normal_mode()
                && self.draft.textarea.is_vim_operator_pending())
        {
            return self.handle_input_basic(key_event);
        }
        // Donor `!` shell-mode entry: `!` on an empty composer in Vim normal
        // mode switches to shell prompt state and drops into insert mode.
        if self.draft.textarea.is_vim_normal_mode()
            && self.is_empty()
            && matches!(
                key_event,
                KeyEvent {
                    code: KeyCode::Char('!'),
                    modifiers: KeyModifiers::NONE,
                    kind: KeyEventKind::Press | KeyEventKind::Repeat,
                    ..
                }
            )
        {
            self.draft.is_bash_mode = true;
            self.draft.textarea.enter_vim_insert_mode();
            return (InputResult::None, true);
        }
        if let Some(result) = self.handle_escape_chain(key_event) {
            return result;
        }

        // Donor `queue` binding: dispatches while a task is running or when
        // the draft is not a `!` shell command. The donor's
        // `Queued`/`Submitted` distinction collapses to `Intent::Submit`;
        // whether a running turn queues it is host-owned. `should_queue`
        // is `streaming` so burst Enter suppression is skipped exactly when
        // the result would queue rather than submit.
        // Tab pushes the text into the running turn (`Intent::Push`); while
        // idle it does nothing. Enter (below) is the queue/send key.
        if self.composer_keymap.queue.is_pressed(key_event) {
            if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
                self.apply_paste(pasted);
            }
            if !self.streaming || self.is_empty() {
                return (InputResult::None, true);
            }
            let result = self.handle_submission_with_time(true, Instant::now());
            if let Some(Intent::Submit(text)) = self.intents.last().cloned() {
                self.intents.pop();
                self.intents.push(Intent::Push(text));
            }
            return result;
        }
        // Empty prompt with queued messages: up edits the last queued one.
        if self.queued > 0
            && self.is_empty()
            && matches!(
                key_event,
                KeyEvent {
                    code: KeyCode::Up,
                    modifiers: KeyModifiers::NONE,
                    kind: KeyEventKind::Press | KeyEventKind::Repeat,
                    ..
                }
            )
        {
            self.intents.push(Intent::EditLastQueued);
            return (InputResult::None, true);
        }
        // Donor default `queue_submissions = false`: Enter keeps paste-burst
        // suppression even while streaming (the host decides queueing).
        if self.composer_keymap.submit.is_pressed(key_event) {
            return self.handle_submission_with_time(false, Instant::now());
        }

        let (history_up_pressed, history_down_pressed) = if self.draft.textarea.is_vim_normal_mode()
        {
            if self.draft.textarea.is_vim_operator_pending() {
                (false, false)
            } else {
                (
                    self.vim_normal_keymap.move_up.is_pressed(key_event),
                    self.vim_normal_keymap.move_down.is_pressed(key_event),
                )
            }
        } else {
            (
                self.editor_keymap.move_up.is_pressed(key_event),
                self.editor_keymap.move_down.is_pressed(key_event),
            )
        };
        if history_up_pressed || history_down_pressed {
            if self.draft.textarea.mouse_selection_range().is_none()
                && self
                    .history
                    .should_handle_navigation(&self.current_text(), self.draft.textarea.cursor())
            {
                let replace_entry = if history_up_pressed {
                    self.history.navigate_up()
                } else {
                    self.history.navigate_down()
                };
                if let Some(entry) = replace_entry {
                    self.apply_history_entry(entry);
                    return (InputResult::None, true);
                }
            }
            return self.handle_input_basic(key_event);
        }

        self.handle_input_basic(key_event)
    }

    /// Handles keys that mutate the textarea, including paste-burst
    /// detection — verbatim donor `handle_input_basic` reduced at the
    /// attachment/mention seams.
    ///
    /// Ordering is important:
    ///
    /// - Always flush any *due* paste burst first so buffered text does not
    ///   lag behind unrelated edits.
    /// - Then handle the incoming key, intercepting only text-producing
    ///   character input.
    /// - For non-text keys, flush via `flush_before_modified_input()` before
    ///   applying the key; otherwise `clear_window_after_non_char()` can
    ///   leave buffered text waiting without a timestamp to time out against.
    fn handle_input_basic(&mut self, input: KeyEvent) -> (InputResult, bool) {
        // Ignore key releases here to avoid treating them as additional input
        // (e.g., appending the same character twice via paste-burst logic).
        if !matches!(input.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
            return (InputResult::None, false);
        }

        self.handle_input_basic_with_time(input, Instant::now())
    }

    fn handle_input_basic_with_time(
        &mut self,
        input: KeyEvent,
        now: Instant,
    ) -> (InputResult, bool) {
        if !self.draft.textarea.is_vim_normal_mode() {
            self.begin_vim_edit(input);
        }
        // If we have a buffered non-bracketed paste burst and enough time has
        // elapsed since the last char, flush it before handling a new input.
        self.handle_paste_burst_flush(now);

        // If we're capturing a burst and receive Enter, accumulate it instead
        // of inserting.
        if matches!(input.code, KeyCode::Enter)
            && !self.draft.disable_paste_burst
            && self.draft.paste_burst.is_active()
            && self
                .draft
                .paste_burst
                .append_control_char_if_active('\n', now)
        {
            return (InputResult::None, true);
        }

        let has_non_text_modifier = key_hint::has_ctrl_or_alt(input.modifiers)
            || input
                .modifiers
                .intersects(KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META);

        // Intercept text-producing Char inputs to optionally accumulate into
        // a burst buffer.
        //
        // This preserves plain, Shift, and Windows AltGr input while keeping
        // shortcut modifiers out of the burst detector and flushing any
        // in-flight text before a non-text key.
        if let KeyEvent {
            code: KeyCode::Char(ch),
            ..
        } = input
        {
            if !has_non_text_modifier
                && !self.draft.disable_paste_burst
                && self.draft.textarea.allows_paste_burst()
            {
                // Non-ASCII characters (e.g., from IMEs) can arrive in quick
                // bursts, so avoid holding the first char while still
                // allowing burst detection for paste input.
                if !ch.is_ascii() {
                    return self.handle_non_ascii_char(input, now);
                }

                match self.draft.paste_burst.on_plain_char(ch, now) {
                    paste_burst::CharDecision::BufferAppend => {
                        self.draft.paste_burst.append_char_to_buffer(ch, now);
                        return (InputResult::None, true);
                    }
                    paste_burst::CharDecision::BeginBuffer { retro_chars } => {
                        let cur = self.draft.textarea.cursor();
                        let txt = self.draft.textarea.text();
                        let safe_cur = Self::clamp_to_char_boundary(txt, cur);
                        let before = &txt[..safe_cur];
                        if let Some(grab) = self.draft.paste_burst.decide_begin_buffer(
                            now,
                            before,
                            retro_chars as usize,
                        ) {
                            if grab.grabbed.is_empty()
                                || self.draft.textarea.retract_paste_burst(grab.start_byte)
                            {
                                self.draft.paste_burst.append_char_to_buffer(ch, now);
                                return (InputResult::None, true);
                            }
                            self.draft.paste_burst.clear_after_explicit_paste();
                        }
                        // If decide_begin_buffer opted not to start
                        // buffering, fall through to normal insertion below.
                    }
                    paste_burst::CharDecision::BeginBufferFromPending => {
                        // First char was held; now append the current one.
                        self.draft.paste_burst.append_char_to_buffer(ch, now);
                        return (InputResult::None, true);
                    }
                    paste_burst::CharDecision::RetainFirstChar => {
                        // Keep the first fast char pending momentarily.
                        return (InputResult::None, true);
                    }
                }
            }
            if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
                self.apply_paste(pasted);
            }
        }

        // Flush any buffered burst before applying a non-char input (arrow
        // keys, etc).
        //
        // `clear_window_after_non_char()` clears `last_plain_char_time`. If
        // we cleared that while `PasteBurst.buffer` is non-empty,
        // `flush_if_due()` would no longer have a timestamp to time out
        // against, and the buffered paste could remain stuck until another
        // plain char arrives.
        if !matches!(input.code, KeyCode::Char(_) | KeyCode::Enter)
            && let Some(pasted) = self.draft.paste_burst.flush_before_modified_input()
        {
            self.apply_paste(pasted);
        }
        // For non-char inputs (or after flushing), handle normally.
        // Track element removals so we can drop any corresponding
        // placeholders without scanning the full text. (Placeholders are
        // atomic elements; when deleted, the element disappears.)
        let elements_before = if self.draft.pending_pastes.is_empty() {
            None
        } else {
            Some(self.draft.textarea.element_payloads())
        };

        if self.draft.is_bash_mode
            && self.draft.textarea.vim_query().is_none()
            && matches!(input.code, KeyCode::Backspace)
            && self.draft.textarea.cursor() == 0
        {
            let started_vim_edit = self.begin_direct_vim_edit();
            self.draft.is_bash_mode = false;
            if started_vim_edit {
                self.finish_vim_edit();
            }
            return (InputResult::None, true);
        }

        self.begin_vim_edit(input);
        self.draft.textarea.input(input);
        self.sync_bash_mode_from_text();

        if let Some(elements_before) = elements_before {
            self.reconcile_deleted_elements(elements_before);
        }
        self.finish_vim_edit();

        // Update the paste-burst heuristic for text, shortcut, and non-char
        // events.
        match input.code {
            KeyCode::Char(_) => {
                if has_non_text_modifier {
                    self.draft.paste_burst.clear_window_after_non_char();
                }
            }
            KeyCode::Enter => {
                // Keep burst window alive (supports blank lines in paste).
            }
            _ => {
                // Other keys: clear burst window (buffer should have been
                // flushed above if needed).
                self.draft.paste_burst.clear_window_after_non_char();
            }
        }

        (InputResult::None, true)
    }

    /// Handle non-ASCII character input (often IME) while still supporting
    /// paste-burst detection — verbatim donor `handle_non_ascii_char`.
    fn handle_non_ascii_char(&mut self, input: KeyEvent, now: Instant) -> (InputResult, bool) {
        if self.draft.disable_paste_burst {
            // When burst detection is disabled, treat IME/non-ASCII input as
            // normal typing. In particular, do not retro-capture or buffer
            // already-inserted prefix text.
            self.draft.textarea.input(input);
            let text_after = self.draft.textarea.text();
            self.draft
                .pending_pastes
                .retain(|(placeholder, _)| text_after.contains(placeholder));
            return (InputResult::None, true);
        }
        if let KeyEvent {
            code: KeyCode::Char(ch),
            ..
        } = input
        {
            if self.draft.paste_burst.try_append_char_if_active(ch, now) {
                return (InputResult::None, true);
            }
            if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
                self.apply_paste(pasted);
            }
            if let Some(decision) = self.draft.paste_burst.on_plain_char_no_hold(now) {
                match decision {
                    paste_burst::CharDecision::BufferAppend => {
                        self.draft.paste_burst.append_char_to_buffer(ch, now);
                        return (InputResult::None, true);
                    }
                    paste_burst::CharDecision::BeginBuffer { retro_chars } => {
                        let cur = self.draft.textarea.cursor();
                        let txt = self.draft.textarea.text();
                        let safe_cur = Self::clamp_to_char_boundary(txt, cur);
                        let before = &txt[..safe_cur];
                        if let Some(grab) = self.draft.paste_burst.decide_begin_buffer(
                            now,
                            before,
                            retro_chars as usize,
                        ) {
                            if grab.grabbed.is_empty()
                                || self.draft.textarea.retract_paste_burst(grab.start_byte)
                            {
                                self.draft.paste_burst.append_char_to_buffer(ch, now);
                                return (InputResult::None, true);
                            }
                            self.draft.paste_burst.clear_after_explicit_paste();
                        }
                    }
                    _ => unreachable!("on_plain_char_no_hold returned unexpected variant"),
                }
            }
        }
        if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
            self.apply_paste(pasted);
        }
        let elements_before = self.draft.textarea.element_payloads();
        self.draft.textarea.input(input);
        self.reconcile_deleted_elements(elements_before);
        (InputResult::None, true)
    }

    #[inline]
    fn clamp_to_char_boundary(text: &str, pos: usize) -> usize {
        let mut p = pos.min(text.len());
        if p < text.len() && !text.is_char_boundary(p) {
            p = text
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= p)
                .last()
                .unwrap_or(0);
        }
        p
    }

    fn reset_vim_mode(&mut self) {
        self.vim_history = VimHistory::default();
        self.draft.textarea.enter_vim_insert_mode();
    }

    // -- popup synchronization ----------------------------------------------

    /// Synchronize `/` and `@` popups with the current text — donor
    /// `sync_popups` reduced to the two the host app popups.
    pub(crate) fn sync_popups(&mut self) {
        self.sync_slash_command_elements();
        if self.history_search.is_some() || self.draft.textarea.vim_query().is_some() {
            if self.popups.current_file_query.is_some() {
                self.file_search_requests.push(String::new());
                self.popups.current_file_query = None;
            }
            self.popups.active = ActivePopup::None;
            self.popups.dismissed_file_token = None;
            return;
        }
        if self.draft.textarea.mouse_selection_range().is_some() {
            self.popups.active = ActivePopup::None;
            return;
        }
        let file_token = self.current_editable_at_token_range_with_options(false);
        let browsing_history = self
            .history
            .should_handle_navigation(&self.current_text(), self.draft.textarea.cursor());
        // When browsing input history (shell-style Up/Down recall), skip all
        // popup synchronization so nothing steals focus from continued
        // history navigation.
        if browsing_history {
            if self.popups.current_file_query.is_some() {
                self.file_search_requests.push(String::new());
                self.popups.current_file_query = None;
            }
            self.popups.active = ActivePopup::None;
            return;
        }
        let at_token_start = file_token.as_ref().map(|(range, _)| range.start);

        let allow_command_popup =
            self.slash_commands_enabled() && !self.draft.is_bash_mode && at_token_start.is_none();
        self.sync_command_popup(allow_command_popup);

        if matches!(self.popups.active, ActivePopup::Command(_)) {
            if self.popups.current_file_query.is_some() {
                self.file_search_requests.push(String::new());
                self.popups.current_file_query = None;
            }
            self.popups.dismissed_file_token = None;
            return;
        }

        if let Some((range, token)) = file_token {
            self.sync_file_search_popup(range, token);
            return;
        }

        if self.popups.current_file_query.is_some() {
            self.file_search_requests.push(String::new());
            self.popups.current_file_query = None;
        }
        self.popups.dismissed_file_token = None;
        if matches!(self.popups.active, ActivePopup::File(_)) {
            self.popups.active = ActivePopup::None;
        }
    }

    /// Keep slash-command atomic elements aligned with the first line —
    /// donor `sync_slash_command_elements` reduced to the shared catalog.
    ///
    /// Slash commands are only valid at byte 0 of the first line. A complete
    /// `/name` token followed by whitespace becomes one atomic element once
    /// the cursor leaves the name; any slash-shaped element that no longer
    /// matches the desired prefix is stale and removed (so Backspace edits
    /// the name normally instead of deleting the whole token).
    fn sync_slash_command_elements(&mut self) {
        if !self.slash_commands_enabled() || self.draft.is_bash_mode {
            return;
        }
        let text = self.draft.textarea.text().to_string();
        let first_line_end = text.find('\n').unwrap_or(text.len());
        let first_line = &text[..first_line_end];
        let cursor = self.draft.textarea.cursor();
        let desired_range = self.slash_command_element_range(first_line, cursor);
        let mut has_desired = false;
        let mut stale_ranges = Vec::new();
        for elem in self.draft.textarea.text_elements() {
            let Some(payload) = elem.placeholder(&text) else {
                continue;
            };
            if payload.strip_prefix('/').is_none() {
                continue;
            }
            let range = elem.byte_range.start..elem.byte_range.end;
            if desired_range.as_ref() == Some(&range) {
                has_desired = true;
            } else {
                stale_ranges.push(range);
            }
        }
        for range in stale_ranges {
            self.draft.textarea.remove_element_range(range);
        }
        if let Some(range) = desired_range
            && !has_desired
        {
            self.draft.textarea.add_element_range(range);
        }
    }

    /// Donor `SlashInput::command_element_range` reduced to the shared
    /// catalog: the `0..1+name.len()` range when the first line holds a
    /// complete, catalog-known command token followed by whitespace and the
    /// cursor is outside the name.
    fn slash_command_element_range(
        &self,
        first_line: &str,
        cursor: usize,
    ) -> Option<std::ops::Range<usize>> {
        let (name, _rest, _rest_offset) = slash_input::parse_slash_name(first_line)?;
        if name.contains('/') {
            return None;
        }
        let element_end = 1 + name.len();
        // A draft tail can make an in-progress prefix look complete
        // ("/re" + "view"). Keep it editable until the cursor leaves the
        // command name.
        if cursor <= first_line.len() && (1..element_end).contains(&cursor) {
            return None;
        }
        let has_space_after = first_line
            .get(element_end..)
            .and_then(|tail| tail.chars().next())
            .is_some_and(char::is_whitespace);
        if !has_space_after {
            return None;
        }
        slash_input::find_command(&self.completion_catalog, name)
            .is_some()
            .then_some(0..element_end)
    }

    /// Synchronize the `/` popup with the current text — donor
    /// `sync_command_popup` over the shared catalog.
    fn sync_command_popup(&mut self, allow: bool) {
        let text = self.draft.textarea.text();
        let first_line_end = text.find('\n').unwrap_or(text.len());
        let first_line = &text[..first_line_end];
        // Keep an explicitly dismissed popup closed until the command token
        // changes.
        let command_token = slash_input::command_popup_filter_text(first_line, /*cursor*/ 0);
        if let Some(command_token) = command_token.as_deref()
            && self.popups.dismissed_command_token.as_deref() == Some(command_token)
        {
            return;
        }
        self.popups.dismissed_command_token = None;

        if !allow {
            if matches!(self.popups.active, ActivePopup::Command(_)) {
                self.popups.active = ActivePopup::None;
            }
            return;
        }
        // Determine whether the caret is inside the initial '/name' token on
        // the first line.
        let cursor = self.draft.textarea.cursor();
        let caret_on_first_line = cursor <= first_line_end;

        let is_editing_slash_command_name = caret_on_first_line
            && slash_input::is_editing_command_name(first_line, cursor, &self.completion_catalog);
        let command_filter_text = caret_on_first_line
            .then(|| slash_input::command_popup_filter_text(first_line, cursor))
            .flatten();

        // If the cursor is positioned within an `@token`, prefer the
        // file-search popup so users can insert a file path as an argument.
        if Self::current_prefixed_token(&self.draft.textarea, '@', false).is_some() {
            if matches!(self.popups.active, ActivePopup::Command(_)) {
                self.popups.active = ActivePopup::None;
            }
            return;
        }
        match &mut self.popups.active {
            ActivePopup::Command(popup) => {
                if is_editing_slash_command_name {
                    if let Some(command_filter_text) = command_filter_text.as_deref() {
                        popup.on_composer_text_change(command_filter_text);
                    }
                } else {
                    self.popups.active = ActivePopup::None;
                }
            }
            _ => {
                if is_editing_slash_command_name
                    && let Some(command_filter_text) = command_filter_text.as_deref()
                {
                    let mut popup = CompletionPopup::new();
                    popup.set_catalog(self.completion_catalog.clone());
                    popup.on_composer_text_change(command_filter_text);
                    self.popups.active = ActivePopup::Command(popup);
                }
            }
        }
    }

    /// Synchronize the `@` file-search popup — donor `sync_file_search_popup`
    /// with `AppEvent::StartFileSearch` replaced by the host-drained request
    /// queue.
    fn sync_file_search_popup(&mut self, range: std::ops::Range<usize>, query: String) {
        if self
            .popups
            .dismissed_file_token
            .as_ref()
            .is_some_and(|dismissed| dismissed.matches(&self.draft.textarea, &range, &query))
        {
            return;
        }

        self.file_search_requests.push(query.clone());

        match &mut self.popups.active {
            ActivePopup::File(popup) => {
                if query.is_empty() {
                    popup.set_empty_prompt();
                } else {
                    popup.set_query(&query);
                }
            }
            _ => {
                let mut popup = file_search_popup::FileSearchPopup::new();
                if query.is_empty() {
                    popup.set_empty_prompt();
                } else {
                    popup.set_query(&query);
                }
                self.popups.active = ActivePopup::File(popup);
            }
        }

        if query.is_empty() {
            self.popups.current_file_query = None;
        } else {
            self.popups.current_file_query = Some(query);
        }
        self.popups.dismissed_file_token = None;
    }

    // -- prefixed-token and completion helpers (verbatim donor algorithms) --

    fn current_prefixed_token(
        textarea: &TextArea,
        prefix: char,
        allow_empty: bool,
    ) -> Option<(std::ops::Range<usize>, String)> {
        completion_target::current_prefixed_token_range(textarea, prefix, allow_empty)
    }

    fn current_editable_at_token_range_with_options(
        &self,
        allow_empty: bool,
    ) -> Option<(std::ops::Range<usize>, String)> {
        let (range, token) = completion_target::current_prefixed_token_range(
            &self.draft.textarea,
            '@',
            allow_empty,
        )?;
        completion_target::prefixed_token_range_is_editable(
            &self.draft.textarea,
            '@',
            &range,
            &token,
        )
        .then_some((range, token))
    }

    /// Replace the active `@token` (the one under the cursor) with `path` —
    /// verbatim donor `insert_selected_path` (image-path attachment branch
    /// removed; paths insert as text).
    fn insert_selected_path(&mut self, token_range: std::ops::Range<usize>, path: &str) {
        let started_vim_edit = self.begin_direct_vim_edit();
        let token_range = self.separate_completion_from_adjacent_element(token_range);
        // If the path contains whitespace, wrap it in double quotes so the
        // local prompt arg parser treats it as a single argument. Avoid
        // adding quotes when the path already contains one.
        let needs_quotes = path.chars().any(char::is_whitespace);
        let inserted = if needs_quotes && !path.contains('"') {
            format!("\"{path}\"")
        } else {
            path.to_string()
        };

        // Replace just the active `@token` so unrelated text elements, such
        // as large-paste placeholders, remain atomic and can still expand on
        // submit.
        let start_idx = token_range.start;
        self.draft.textarea.replace_range(token_range, &inserted);
        let inserted_range = start_idx..start_idx.saturating_add(inserted.len());
        self.draft.textarea.set_cursor(inserted_range.end);
        self.advance_past_completion_separator();
        self.dismiss_completed_prefixed_token('@', inserted_range, &inserted);
        if started_vim_edit {
            self.finish_vim_edit();
        }
    }

    /// Leaves the cursor after one horizontal separator following a
    /// completion — verbatim donor `advance_past_completion_separator`.
    fn advance_past_completion_separator(&mut self) {
        let cursor = self.draft.textarea.cursor();
        let existing_separator_len = self.draft.textarea.text()[cursor..]
            .chars()
            .next()
            .filter(|c| {
                c.is_whitespace()
                    && !matches!(
                        *c,
                        '\n' | '\r'
                            | '\u{000B}'
                            | '\u{000C}'
                            | '\u{0085}'
                            | '\u{2028}'
                            | '\u{2029}'
                    )
            })
            .map(char::len_utf8);
        if let Some(separator_len) = existing_separator_len {
            let after_separator = cursor + separator_len;
            let separator_precedes_suffix = self.draft.textarea.text()[after_separator..]
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace());
            if separator_precedes_suffix {
                self.draft.textarea.insert_str_at(cursor, " ");
            } else {
                self.draft.textarea.set_cursor(after_separator);
            }
        } else {
            self.draft.textarea.insert_str_at(cursor, " ");
        }
    }

    /// Dismisses popup synchronization only for the exact token occurrence
    /// just inserted — verbatim donor `dismiss_completed_prefixed_token`.
    fn dismiss_completed_prefixed_token(
        &mut self,
        prefix: char,
        inserted_range: std::ops::Range<usize>,
        inserted_text: &str,
    ) {
        let Some(completed_token) = inserted_text.strip_prefix(prefix) else {
            return;
        };
        // Completion leaves the cursor on separator whitespace, where normal
        // token affinity would otherwise immediately reopen the popup for
        // sigil-prefixed inserted text.
        let Some((current_range, current_token)) = completion_target::current_prefixed_token_range(
            &self.draft.textarea,
            prefix,
            /*allow_empty*/ true,
        ) else {
            return;
        };
        if current_range != inserted_range || current_token != completed_token {
            return;
        }

        if prefix == '@' {
            self.popups.dismissed_file_token = Some(DismissedToken::new(
                &self.draft.textarea,
                current_range,
                current_token,
            ));
        }
    }

    /// Inserts a leading separator when a completion starts directly after an
    /// atomic element — verbatim donor
    /// `separate_completion_from_adjacent_element`.
    fn separate_completion_from_adjacent_element(
        &mut self,
        mut token_range: std::ops::Range<usize>,
    ) -> std::ops::Range<usize> {
        let starts_after_element = self
            .draft
            .textarea
            .text_element_ranges()
            .any(|range| range.end == token_range.start);
        if starts_after_element {
            self.draft
                .textarea
                .replace_range(token_range.start..token_range.start, " ");
            token_range.start += 1;
            token_range.end += 1;
        }
        token_range
    }

    // -- quit/copy entry points ----------------------------------------------

    fn handle_ctrl_c(&mut self) -> Outcome {
        let now = Instant::now();
        if let Some(last) = self.last_ctrl_c
            && now.duration_since(last) <= DOUBLE_TAP_WINDOW
        {
            self.intents.push(Intent::Quit);
            self.last_ctrl_c = None;
            return Outcome::Consumed;
        }
        // A running turn is cancelled first, text or no text; the draft is
        // kept so nothing the user typed is lost with the turn.
        if self.streaming {
            self.intents.push(Intent::CancelStreamingTurn);
            self.last_ctrl_c = Some(now);
            return Outcome::Redraw;
        }
        if self.shell_running {
            self.intents.push(Intent::AbortRunningShell);
            self.last_ctrl_c = Some(now);
            return Outcome::Redraw;
        }
        self.flush_pending_input();
        self.clear_draft();
        self.last_ctrl_c = Some(now);
        Outcome::Redraw
    }

    /// S:I4 — request a clipboard copy of the current buffer via OSC52.
    /// Returns `true` if a `Copy` intent was emitted.
    pub fn request_copy(&mut self) -> bool {
        let text = self.current_text_expanded();
        if text.is_empty() {
            return false;
        }
        self.intents.push(Intent::Copy(text));
        true
    }

    /// Copy the logical line under the cursor (detail-row copy). Falls back
    /// to the whole buffer when the buffer is single-line.
    pub fn request_copy_line(&mut self, _line: usize) -> bool {
        self.request_copy()
    }

    /// The composer's current cursor-line text — the same string
    /// `sync_completion` reads.
    pub fn cursor_line_text(&self) -> String {
        let text = self.draft.textarea.text();
        let cursor = self.draft.textarea.cursor().min(text.len());
        let start = text[..cursor].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let end = text[cursor..]
            .find('\n')
            .map(|i| cursor + i)
            .unwrap_or(text.len());
        text.get(start..end).unwrap_or_default().to_string()
    }

    /// Replace the whole buffer with `text` and put the cursor at its end.
    /// Used by the host for completion acceptance and draft restore paths
    /// that decide content outside the composer's own edit handlers.
    pub fn set_text(&mut self, text: &str) {
        self.history.reset_navigation();
        let (text, elements) = self.imported_text_for_textarea(text.to_string(), Vec::new());
        self.draft.textarea.set_text_with_elements(&text, &elements);
        self.draft.pending_pastes.clear();
        self.draft
            .textarea
            .set_cursor(self.draft.textarea.text().len());
        self.sync_popups();
    }

    /// Submit the buffer through the command-effect registry — kept for host
    /// compatibility; equivalent to pressing Enter.
    pub fn submit_with_command_effects(&mut self) {
        self.handle_submission(false, Instant::now());
    }

    /// Undo the last composer edit. Active only while the parent's Vim edit
    /// mode is enabled (the donor's undo ledger is Vim-transaction-based;
    /// insert-mode linear undo is not part of the donor design).
    pub fn undo_edit(&mut self) -> bool {
        self.vim_history.undo.pop_back().is_some_and(|snapshot| {
            let current = self.snapshot_draft();
            self.vim_history.redo.push_back(current);
            self.restore_draft_snapshot(snapshot);
            true
        })
    }

    /// Redo the most recently undone composer edit.
    pub fn redo_edit(&mut self) -> bool {
        self.vim_history.redo.pop_back().is_some_and(|snapshot| {
            let current = self.snapshot_draft();
            self.vim_history.undo.push_back(current);
            self.restore_draft_snapshot(snapshot);
            true
        })
    }

    pub fn is_empty(&self) -> bool {
        // Host shortcuts must not steal characters while the textarea's
        // pending first character or paste buffer owns an unfinished draft.
        self.draft.textarea.is_empty() && !self.draft.paste_burst.is_active()
    }

    // -- donor/parent seams used by absorbed behavior and tests --------------

    /// Enable the donor Vim edit mode. Off by default; enabling is
    /// parent-owned policy, so nothing here turns it on implicitly.
    ///
    /// Donor `set_vim_enabled`: buffered paste text is integrated first, the
    /// Vim `/`/`?` search layer is enabled with the mode, and the undo ledger
    /// restarts so pre-toggle edits cannot be undone as Vim transactions.
    pub fn set_vim_enabled(&mut self, enabled: bool) {
        if let Some(pasted) = self.draft.paste_burst.flush_before_modified_input() {
            self.apply_paste(pasted);
        }
        self.draft.textarea.enable_vim_search();
        self.draft.textarea.set_vim_enabled(enabled);
        self.vim_history = VimHistory::default();
        self.draft.paste_burst.clear_after_explicit_paste();
    }

    /// Cursor position including the virtual `!` bash-mode column (donor
    /// `current_cursor`).
    pub(crate) fn current_cursor(&self) -> usize {
        self.draft.textarea.cursor() + usize::from(self.draft.is_bash_mode)
    }

    /// Donor `handle_submission_with_time` (test-time `Instant` injection).
    /// `should_queue` is host-owned: submissions always emit
    /// [`Intent::Submit`]; the host queues while streaming.
    pub(crate) fn handle_submission_with_time(
        &mut self,
        should_queue: bool,
        now: Instant,
    ) -> (InputResult, bool) {
        self.handle_submission(should_queue, now)
    }

    /// Donor `handle_mouse`: route one mouse event through the textarea.
    /// Returns whether the draft/selection changed.
    pub(crate) fn handle_mouse(&mut self, event: crossterm::event::MouseEvent) -> bool {
        let state = *self.draft.textarea_state.borrow();
        let changed = self.draft.textarea.handle_mouse(event, state);
        if changed
            && matches!(event.kind, MouseEventKind::Up(MouseButton::Left))
            && let Some(range) = self.draft.textarea.mouse_selection_range()
        {
            let text = self.draft.textarea.text();
            if let Some(selected) = text.get(range) {
                let selected = selected.to_string();
                if !selected.is_empty() {
                    self.intents.push(Intent::Copy(selected));
                }
            }
        }
        changed
    }

    fn move_cursor_to_end(&mut self) {
        let end = self.current_text().len();
        self.draft.textarea.set_cursor(end);
    }
}

impl Component for Composer {
    fn render(&mut self, frame: &mut dyn crate::component::RenderFrame, area: Rect, theme: &Theme) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let text_width = area.width.saturating_sub(PROMPT_GUTTER).max(1);
        self.last_width = text_width;

        // Popup rows draw above the draft, sharing the composer's own area —
        // the donor's "suggestions above the composer" layout, reduced to a
        // top overlay in the inline surface.
        let popup_height = match &self.popups.active {
            ActivePopup::Command(popup) => popup
                .calculate_required_height(area.width)
                .min(area.height.saturating_sub(1)),
            ActivePopup::File(popup) => popup
                .calculate_required_height()
                .min(area.height.saturating_sub(1)),
            ActivePopup::None => 0,
        };
        let text_area = Rect {
            x: area.x + PROMPT_GUTTER,
            y: area.y + popup_height,
            width: text_width,
            height: area.height.saturating_sub(popup_height),
        };
        let query_row =
            u16::from(self.history_search.is_some() || self.draft.textarea.vim_query().is_some())
                .min(text_area.height);
        let textarea_area = Rect {
            height: text_area.height.saturating_sub(query_row),
            ..text_area
        };

        // Gutter column: prompt marker on the first row, blank continuation.
        let dim_style = Style::default().fg(theme.color(Role::Dim));
        for row in 0..textarea_area.height.max(1) {
            let y = textarea_area.y + row;
            let span = if row == 0 {
                Span::styled(
                    format!("{PROMPT_MARKER} "),
                    Style::default().fg(theme.accent()),
                )
            } else {
                Span::styled(" ".repeat(PROMPT_GUTTER as usize), dim_style)
            };
            ratatui::widgets::Widget::render(
                Line::from(vec![span]),
                Rect {
                    x: area.x,
                    y,
                    width: PROMPT_GUTTER,
                    height: 1,
                },
                frame.buffer_mut(),
            );
        }

        let highlights: Vec<_> = self
            .history_search_highlight_ranges()
            .into_iter()
            .chain(self.draft.textarea.vim_search_highlights())
            .map(|range| (range, Style::new().reversed().bold()))
            .collect();
        let mut state = self.draft.textarea_state.borrow_mut();
        self.draft.textarea.render_ref_styled_with_highlights(
            textarea_area,
            frame.buffer_mut(),
            &mut state,
            Style::default().fg(theme.color(Role::Text)),
            &highlights,
        );
        drop(state);
        if self.draft.textarea.is_empty() {
            let hint = match &self.placeholder_override {
                Some(text) if !self.streaming => text.as_str(),
                _ if self.streaming => PLACEHOLDER_RUNNING,
                _ => PLACEHOLDER,
            };
            ratatui::widgets::Widget::render(
                Line::from(Span::styled(hint, dim_style)),
                Rect {
                    height: 1,
                    ..textarea_area
                },
                frame.buffer_mut(),
            );
        } else if self.streaming && !self.current_text().contains('\n') {
            // What the keys do right now, on the right of a one-line draft.
            let used = crate::transcript::wrap::display_width(&self.current_text());
            let hint_width = crate::transcript::wrap::display_width(RUNNING_HINT);
            if used + hint_width + 3 <= usize::from(textarea_area.width) {
                ratatui::widgets::Widget::render(
                    Line::from(Span::styled(RUNNING_HINT, dim_style)),
                    Rect {
                        x: textarea_area.right() - hint_width as u16 - 1,
                        width: hint_width as u16,
                        height: 1,
                        ..textarea_area
                    },
                    frame.buffer_mut(),
                );
            }
        }
        if let Some(query) = self.draft.textarea.vim_query() {
            query.render(
                Rect {
                    y: textarea_area.bottom(),
                    height: 1,
                    ..text_area
                },
                frame.buffer_mut(),
            );
        } else if let Some(line) = self.history_search_footer_line() {
            ratatui::widgets::Widget::render(
                line,
                Rect {
                    y: textarea_area.bottom(),
                    height: 1,
                    ..text_area
                },
                frame.buffer_mut(),
            );
        }
        if popup_height > 0 {
            let popup_area = Rect {
                x: area.x,
                y: area.y,
                width: area.width,
                height: popup_height,
            };
            match &self.popups.active {
                ActivePopup::Command(popup) => {
                    popup.render_ref(popup_area, frame.buffer_mut());
                }
                ActivePopup::File(popup) => {
                    popup.render_ref(popup_area, frame.buffer_mut());
                }
                ActivePopup::None => {}
            }
        }

        // Hardware cursor follows the textarea's wrap-aware position; the
        // query line takes the footer position while a search is active.
        if query_row == 1 {
            let query_area = Rect {
                y: textarea_area.bottom(),
                height: 1,
                ..text_area
            };
            if let Some(query) = self.draft.textarea.vim_query() {
                if let Some((x, y)) = query.cursor_pos(query_area) {
                    frame.set_cursor_position((x, y));
                }
                return;
            }
            if let Some((x, y)) = self.history_search_query_cursor_pos(query_area) {
                frame.set_cursor_position((x, y));
            }
            return;
        }
        let state = *self.draft.textarea_state.borrow();
        if let Some((x, y)) = self
            .draft
            .textarea
            .cursor_pos_with_state(textarea_area, state)
        {
            frame.set_cursor_position((x, y));
        }
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Paste(text) => {
                self.handle_paste(text.clone());
                Outcome::Redraw
            }
            Event::Resize { width, height } => {
                self.terminal_rows = *height;
                self.last_width = width.saturating_sub(PROMPT_GUTTER);
                Outcome::Redraw
            }
            Event::Mouse(mouse) => {
                if self.handle_mouse(*mouse) {
                    Outcome::Redraw
                } else {
                    Outcome::Ignored
                }
            }
            Event::Focus(_) => Outcome::Redraw,
            Event::Resume => Outcome::Redraw,
        }
    }

    fn desired_height(&self, width: u16, available: u16) -> u16 {
        // Donor `desired_height`: textarea rows + popup rows + footer query
        // row, capped by the host's bound and the composer maximum.
        let text_width = width.saturating_sub(PROMPT_GUTTER).max(1);
        let content = self.draft.textarea.desired_height(text_width).max(1);
        let popup = match &self.popups.active {
            ActivePopup::Command(popup) => popup.calculate_required_height(width),
            ActivePopup::File(popup) => popup.calculate_required_height(),
            ActivePopup::None => 0,
        };
        let query =
            u16::from(self.history_search.is_some() || self.draft.textarea.vim_query().is_some());
        (content + popup + query)
            .min(self.max_height())
            .min(available.max(1))
    }
}
