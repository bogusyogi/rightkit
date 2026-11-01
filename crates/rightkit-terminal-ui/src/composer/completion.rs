//! S:I10 — composer completion popup wiring.
//!
//! The prior lane found the completion API disconnected from the composer
//! entirely (`composer/mod.rs` has no completion machinery -- confirmed by
//! its search, reported in the A6 wiring note). This module is the missing
//! piece: a [`CompletionPopup`] that opens when the buffer looks like a
//! command (`/` at the start of a single logical line), filters a supplied
//! candidate list by prefix, and tracks a selection index for up/down.
//!
//! It does not fetch candidates itself -- [`crate::component::CommandSource`]
//! (already defined in `component.rs`) is the seam onto the shared command
//! registry (CG-A01's grammar), and whoever owns that registry supplies
//! `CompletionCandidate`s through `CompletionPopup::refresh`. This module
//! only owns the popup's open/closed state, its filtering, and its
//! selection -- the part docs/canon/daemon-master-canon-atoms.md's acceptance criterion actually asks for:
//! "typing `/` shows a popup with descriptions/aliases/disabled reasons".

/// One row the popup can show. `disabled_reason` is `Some` when
/// [`crate::component::CommandSource::valid_mid_turn`] says this command
/// cannot run right now (e.g. mid-turn-only or turn-only restrictions) --
/// shown, not hidden, per docs/canon/daemon-master-canon-atoms.md's "disabled reasons" requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionCandidate {
    pub name: String,
    pub description: String,
    pub aliases: Vec<String>,
    pub disabled_reason: Option<String>,
}

impl CompletionCandidate {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            aliases: Vec::new(),
            disabled_reason: None,
        }
    }

    pub fn with_aliases(mut self, aliases: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.aliases = aliases.into_iter().map(Into::into).collect();
        self
    }

    pub fn disabled(mut self, reason: impl Into<String>) -> Self {
        self.disabled_reason = Some(reason.into());
        self
    }

    /// Whether `prefix` (already stripped of its leading `/`) matches this
    /// candidate's name or any alias.
    fn matches(&self, prefix: &str) -> bool {
        if prefix.is_empty() {
            return true;
        }
        let prefix = prefix.to_ascii_lowercase();
        self.name.to_ascii_lowercase().starts_with(&prefix)
            || self
                .aliases
                .iter()
                .any(|alias| alias.to_ascii_lowercase().starts_with(&prefix))
    }
}

/// Popup open/closed state, its full candidate catalog, the current
/// filtered view, and the selected row.
#[derive(Debug, Default)]
pub struct CompletionPopup {
    catalog: Vec<CompletionCandidate>,
    open: bool,
    prefix: String,
    filtered: Vec<usize>,
    selected: usize,
}

impl CompletionPopup {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the full candidate catalog (called by the host whenever the
    /// shared command registry's contents could have changed).
    pub fn set_catalog(&mut self, catalog: Vec<CompletionCandidate>) {
        let selected_name = self
            .selected_candidate()
            .map(|candidate| candidate.name.clone());
        self.catalog = catalog;
        if self.open {
            self.refilter();
            // Host refreshes can alter candidate ordering while a popup is
            // open. Preserve selection by canonical command name, never by
            // its stale display-row index.
            if let Some(name) = selected_name
                && let Some(position) = self
                    .filtered
                    .iter()
                    .position(|&index| self.catalog[index].name == name)
            {
                self.selected = position;
            }
        }
    }

    /// Re-evaluate open/closed and filtering from the composer's current
    /// single-line buffer text. Call after every buffer edit. `line` is the
    /// full text of the logical line the cursor is on; `is_single_line`
    /// tells the popup whether the buffer has more than one logical line
    /// (a multi-line buffer never looks like a command).
    pub fn sync(&mut self, line: &str, is_single_line: bool) {
        if !is_single_line || !line.starts_with('/') {
            self.close();
            return;
        }
        let prefix = &line[1..];
        if prefix.contains(char::is_whitespace) {
            // Past the command name -- typing arguments, not selecting a
            // command anymore.
            self.close();
            return;
        }
        self.prefix = prefix.to_string();
        self.open = true;
        self.refilter();
    }

    fn refilter(&mut self) {
        self.filtered = self
            .catalog
            .iter()
            .enumerate()
            .filter(|(_, c)| c.matches(&self.prefix))
            .map(|(i, _)| i)
            .collect();
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
        if self.filtered.is_empty() {
            self.selected = 0;
        }
    }

    pub fn close(&mut self) {
        self.open = false;
        self.prefix.clear();
        self.filtered.clear();
        self.selected = 0;
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Rows currently visible, in display order.
    pub fn visible(&self) -> Vec<&CompletionCandidate> {
        self.filtered.iter().map(|&i| &self.catalog[i]).collect()
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn selected_candidate(&self) -> Option<&CompletionCandidate> {
        self.filtered.get(self.selected).map(|&i| &self.catalog[i])
    }

    pub fn select_next(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.filtered.len();
    }

    pub fn select_prev(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.filtered.len() - 1
        } else {
            self.selected - 1
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Vec<CompletionCandidate> {
        vec![
            CompletionCandidate::new("new", "Start a new session"),
            CompletionCandidate::new("clear", "Clear the transcript").with_aliases(["cls"]),
            CompletionCandidate::new("rewind", "Rewind to a prior turn")
                .disabled("no turns to rewind to"),
        ]
    }

    #[test]
    fn typing_a_slash_on_an_empty_line_opens_the_popup_with_everything() {
        let mut p = CompletionPopup::new();
        p.set_catalog(catalog());
        p.sync("/", true);
        assert!(p.is_open());
        assert_eq!(p.visible().len(), 3);
    }

    #[test]
    fn a_non_slash_line_keeps_the_popup_closed() {
        let mut p = CompletionPopup::new();
        p.set_catalog(catalog());
        p.sync("hello", true);
        assert!(!p.is_open());
    }

    #[test]
    fn a_multi_line_buffer_never_opens_the_popup() {
        let mut p = CompletionPopup::new();
        p.set_catalog(catalog());
        p.sync("/new", false);
        assert!(!p.is_open());
    }

    #[test]
    fn typing_past_the_command_name_closes_the_popup() {
        let mut p = CompletionPopup::new();
        p.set_catalog(catalog());
        p.sync("/new ", true);
        assert!(!p.is_open());
    }

    #[test]
    fn filtering_narrows_by_prefix_matching_name_or_alias() {
        let mut p = CompletionPopup::new();
        p.set_catalog(catalog());
        p.sync("/cl", true);
        assert!(p.is_open());
        let names: Vec<&str> = p.visible().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["clear"]);
    }

    #[test]
    fn alias_prefix_matches_its_owning_candidate() {
        let mut p = CompletionPopup::new();
        p.set_catalog(catalog());
        p.sync("/cls", true);
        let names: Vec<&str> = p.visible().iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["clear"]);
    }

    #[test]
    fn filtering_matches_command_case_insensitively_like_resolution() {
        let mut p = CompletionPopup::new();
        p.set_catalog(catalog());
        p.sync("/NE", true);
        assert_eq!(
            p.visible()
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["new"]
        );
    }

    #[test]
    fn disabled_candidates_are_shown_not_hidden_with_their_reason() {
        let mut p = CompletionPopup::new();
        p.set_catalog(catalog());
        p.sync("/rew", true);
        let visible = p.visible();
        assert_eq!(visible.len(), 1);
        assert_eq!(
            visible[0].disabled_reason.as_deref(),
            Some("no turns to rewind to")
        );
    }

    #[test]
    fn select_next_and_prev_wrap_around() {
        let mut p = CompletionPopup::new();
        p.set_catalog(catalog());
        p.sync("/", true);
        assert_eq!(p.selected_index(), 0);
        p.select_prev();
        assert_eq!(p.selected_index(), 2);
        p.select_next();
        p.select_next();
        assert_eq!(p.selected_index(), 1);
    }

    #[test]
    fn narrowing_the_filter_clamps_the_selection_into_range() {
        let mut p = CompletionPopup::new();
        p.set_catalog(catalog());
        p.sync("/", true);
        p.select_prev(); // now at index 2 ("rewind")
        p.sync("/cl", true); // filtered down to 1 row
        assert_eq!(p.selected_index(), 0);
        assert_eq!(p.selected_candidate().unwrap().name, "clear");
    }
}
