//! `/` command completion popup.
//!
//! Lifted from the Codex donor `bottom_pane/command_popup.rs` @ 444da310,
//! with the item source generalized: the donor filters its built-in
//! `SlashCommand` registry, while the host app filters the shared command
//! catalog supplied by the host through [`Composer::set_completion_catalog`]
//! (`CompletionCandidate`s — CG-A01: one grammar drives every surface). The
//! filtering, ordering, selection, windowing, and row-construction algorithm
//! below is the donor's, unchanged.
//!
//! `Composer::completion()` returns this type under the pre-absorption name
//! `CompletionPopup` so the existing host API is preserved.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::WidgetRef;

use super::popup_rows::GenericDisplayRow;
use super::popup_rows::MAX_POPUP_ROWS;
use super::popup_rows::render_rows_single_line;
use super::popup_rows::selection_style;
use super::scroll_state::ScrollState;
use crate::composer::completion::CompletionCandidate;

/// Popup open/closed state, its full candidate catalog, the current filtered
/// view, and the selected row. Internally this is the donor `CommandPopup`:
/// `command_filter` drives `filtered()` and `state` is the donor `ScrollState`.
#[derive(Default)]
pub struct CompletionPopup {
    command_filter: String,
    catalog: Vec<CompletionCandidate>,
    state: ScrollState,
}

impl CompletionPopup {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Replace the full candidate catalog (called by the host whenever the
    /// shared command registry's contents could have changed). Preserves the
    /// selection by canonical command name across the swap, matching the
    /// previous the host app behavior hosts rely on.
    pub fn set_catalog(&mut self, catalog: Vec<CompletionCandidate>) {
        let selected_name = self.selected_candidate().map(|c| c.name.clone());
        self.catalog = catalog;
        self.state.clamp_selection(self.filtered_items().len());
        if let Some(name) = selected_name
            && let Some(position) = self.filtered_items().iter().position(|c| c.name == name)
        {
            self.state.selected_idx = Some(position);
        }
    }

    /// Update the filter string based on the current composer text. The text
    /// passed in is expected to start with a leading '/'. Everything after the
    /// *first* '/' on the *first* line becomes the active filter that is used
    /// to narrow down the list of available commands.
    pub(crate) fn on_composer_text_change(&mut self, text: &str) {
        let first_line = text.lines().next().unwrap_or("");
        let previous_filter = std::mem::take(&mut self.command_filter);

        if let Some(stripped) = first_line.strip_prefix('/') {
            // Extract the *first* token (sequence of non-whitespace
            // characters) after the slash so that `/clear something` still
            // shows the help for `/clear`.
            let token = stripped.trim_start();
            let cmd_token = token.split_whitespace().next().unwrap_or("");
            self.command_filter = cmd_token.to_string();
        }

        if self.command_filter != previous_filter {
            self.state.reset();
        }

        // Reset or clamp selected index based on new filtered list.
        let matches_len = self.filtered_items().len();
        self.state.clamp_selection(matches_len);
        self.state
            .ensure_visible(matches_len, MAX_POPUP_ROWS.min(matches_len));
    }

    /// Preferred height for the popup at `width`: one line per visible row
    /// (the reduced single-line row path) plus the window cap.
    pub(crate) fn calculate_required_height(&self, _width: u16) -> u16 {
        self.filtered_items().len().clamp(1, MAX_POPUP_ROWS) as u16 + 1
    }

    /// Compute exact/prefix matches over the catalog, paired with optional
    /// highlight indices. Preserves catalog presentation order; exact matches
    /// sort before prefix matches — verbatim from the donor `filtered()`.
    fn filtered(&self) -> Vec<(usize, Option<Vec<usize>>)> {
        let filter = self.command_filter.trim();
        let mut out: Vec<(usize, Option<Vec<usize>>)> = Vec::new();
        if filter.is_empty() {
            return (0..self.catalog.len()).map(|i| (i, None)).collect();
        }

        let filter_lower = filter.to_lowercase();
        let filter_chars = filter.chars().count();
        let mut exact: Vec<(usize, Option<Vec<usize>>)> = Vec::new();
        let mut prefix: Vec<(usize, Option<Vec<usize>>)> = Vec::new();
        let indices_for = |offset: usize| Some((offset..offset + filter_chars).collect());

        // One catalog entry may match by name or by any alias; an alias hit
        // highlights the alias span inside the display text when it is shown.
        let mut push_match =
            |index: usize, display: &str, alias: Option<&str>, alias_offset: usize| {
                let display_lower = display.to_lowercase();
                let alias_lower = alias.map(str::to_lowercase);
                let display_exact = display_lower == filter_lower;
                let alias_exact = alias_lower.as_deref() == Some(filter_lower.as_str());
                if display_exact || alias_exact {
                    let offset = if display_exact { 0 } else { alias_offset };
                    exact.push((index, indices_for(offset)));
                    return;
                }
                let display_prefix = display_lower.starts_with(&filter_lower);
                let alias_prefix = alias_lower
                    .as_ref()
                    .is_some_and(|alias| alias.starts_with(&filter_lower));
                if display_prefix || alias_prefix {
                    let offset = if display_prefix { 0 } else { alias_offset };
                    prefix.push((index, indices_for(offset)));
                }
            };

        for (index, candidate) in self.catalog.iter().enumerate() {
            push_match(index, &candidate.name, None, 0);
            for alias in &candidate.aliases {
                push_match(index, alias, Some(alias), 0);
            }
        }

        out.extend(exact);
        // An entry that matched on both name and alias appears once.
        let mut seen = std::collections::HashSet::new();
        out.retain(|(index, _)| seen.insert(*index));
        prefix.retain(|(index, _)| seen.insert(*index));
        out.extend(prefix);

        // Last tier: description-word matches (never outranks a name/alias
        // hit). Filters under 3 characters are too noisy to match prose.
        if filter_chars >= 3 {
            let needles = description_needles(&filter_lower);
            for (index, candidate) in self.catalog.iter().enumerate() {
                if !seen.contains(&index) && description_matches(&candidate.description, &needles) {
                    seen.insert(index);
                    out.push((index, None));
                }
            }
        }
        out
    }

    fn filtered_items(&self) -> Vec<&CompletionCandidate> {
        self.filtered()
            .into_iter()
            .map(|(i, _)| &self.catalog[i])
            .collect()
    }

    /// Render-ready rows — donor `rows_from_matches` reduced to the
    /// single-line row model, keeping the `>` selection prefix, match
    /// highlighting, and disabled-reason treatment.
    fn rows_from_matches(
        &self,
        matches: Vec<(usize, Option<Vec<usize>>)>,
    ) -> Vec<GenericDisplayRow> {
        matches
            .into_iter()
            .enumerate()
            .map(|(index, (catalog_index, indices))| {
                let candidate = &self.catalog[catalog_index];
                // One command, several names: when the typed prefix matches
                // an alias rather than the canonical name, label the row with
                // that alias and bracket the canonical action.
                let alias_label = self.matched_alias(candidate);
                let (name, description) = match alias_label {
                    Some(alias) => (
                        format!("/{alias}  ({})", candidate.name),
                        format!("\u{b7} {}", candidate.description),
                    ),
                    None => (
                        format!("/{}", candidate.name),
                        candidate.description.clone(),
                    ),
                };
                GenericDisplayRow {
                    category_tag: None,
                    name,
                    name_prefix_spans: vec![if self.state.selected_idx == Some(index) {
                        "> ".into()
                    } else {
                        "  ".into()
                    }],
                    selection_style: Some(selection_style()),
                    match_indices: indices.map(|v| v.into_iter().map(|i| i + 1).collect()),
                    display_shortcut: None,
                    description: Some(description),
                    is_disabled: candidate.disabled_reason.is_some(),
                    disabled_reason: candidate.disabled_reason.clone(),
                }
            })
            .collect()
    }

    /// The alias the typed filter selects, when it matches an alias but not
    /// the canonical name (exact alias hits win over prefix hits).
    fn matched_alias<'a>(&self, candidate: &'a CompletionCandidate) -> Option<&'a str> {
        let filter = self.command_filter.trim().to_lowercase();
        if filter.is_empty() || candidate.name.to_lowercase().starts_with(&filter) {
            return None;
        }
        let aliases = || {
            candidate
                .aliases
                .iter()
                .filter(|alias| !alias.eq_ignore_ascii_case(&candidate.name))
        };
        aliases()
            .find(|alias| alias.to_lowercase() == filter)
            .or_else(|| aliases().find(|alias| alias.to_lowercase().starts_with(&filter)))
            .map(String::as_str)
    }

    /// Move the selection cursor one step up.
    pub(crate) fn move_up(&mut self) {
        let len = self.filtered_items().len();
        self.state.move_up_wrap(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    /// Move the selection cursor one step down.
    pub(crate) fn move_down(&mut self) {
        let len = self.filtered_items().len();
        self.state.move_down_wrap(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    /// Return the currently selected candidate, if any.
    pub fn selected_candidate(&self) -> Option<&CompletionCandidate> {
        let matches = self.filtered_items();
        self.state
            .selected_idx
            .and_then(|idx| matches.get(idx).copied())
    }

    // -- preserved pre-absorption host-facing surface ---------------------

    /// Rows currently visible, in display order (used by host render code).
    pub fn visible(&self) -> Vec<&CompletionCandidate> {
        self.filtered_items()
    }

    pub fn selected_index(&self) -> usize {
        self.state.selected_idx.unwrap_or(0)
    }

    pub fn select_next(&mut self) {
        self.move_down();
    }

    pub fn select_prev(&mut self) {
        self.move_up();
    }
}

/// Typed words that users reach for but descriptions phrase differently.
/// Deliberately tiny: `/login` has no command of its own, so it surfaces the
/// command whose description says "sign in".
const DESCRIPTION_SYNONYMS: &[(&str, &str)] = &[
    ("login", "sign in"),
    ("signin", "sign in"),
    ("logon", "sign in"),
];

/// Lowercase needles to look for as word prefixes in a description.
fn description_needles(filter_lower: &str) -> Vec<String> {
    let mut needles = vec![filter_lower.to_string()];
    for (typed, phrase) in DESCRIPTION_SYNONYMS {
        if *typed == filter_lower {
            needles.push((*phrase).to_string());
        }
    }
    needles
}

/// Whether `filter_lower` (typed `/name` fragment, already lowercase, no
/// `/`) matches `description` through the description tier. Used by the
/// popup's editing gate (`is_editing_command_name`) so a description-only
/// hit — `/login` surfacing `/providers` via "sign in" — still opens the
/// popup; the same <3-character noise floor applies there.
pub(crate) fn description_match_any(description: &str, filter_lower: &str) -> bool {
    if filter_lower.chars().count() < 3 {
        return false;
    }
    description_matches(description, &description_needles(filter_lower))
}

/// Whether any needle starts at a word boundary of `description`.
fn description_matches(description: &str, needles: &[String]) -> bool {
    let lower = description.to_lowercase();
    needles.iter().any(|needle| {
        lower.match_indices(needle.as_str()).any(|(at, _)| {
            lower[..at]
                .chars()
                .next_back()
                .is_none_or(|prev| !prev.is_alphanumeric())
        })
    })
}

impl WidgetRef for CompletionPopup {
    fn render_ref(&self, area: Rect, buf: &mut Buffer) {
        let rows = self.rows_from_matches(self.filtered());
        render_rows_single_line(area, buf, &rows, &self.state, MAX_POPUP_ROWS, "no matches");
    }
}
