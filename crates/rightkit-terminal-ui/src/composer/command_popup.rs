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
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Widget;
use ratatui::widgets::WidgetRef;

use super::popup_rows::MAX_POPUP_ROWS;
use super::scroll_state::ScrollState;
use super::width::char_width;
use super::width::display_width;
use crate::composer::completion::CompletionCandidate;

/// Group labels for the bare `/` list, in display order. The last is the
/// catch-all for commands the table below does not name.
const GROUP_LABELS: [&str; 6] = ["session", "review", "model", "policy", "info", "other"];
const OTHER_GROUP: usize = 5;
/// Commands the bare `/` list shows per group (12 in all); typing reaches the rest.
const GROUP_CAPS: [usize; 6] = [3, 2, 2, 2, 2, 1];
/// Display order within a group, and which group a command belongs to.
const KNOWN_COMMANDS: &[(&str, usize)] = &[
    ("new", 0),
    ("resume", 0),
    ("rewind", 0),
    ("compact", 0),
    ("undo", 0),
    ("redo", 0),
    ("reset", 0),
    ("fork", 0),
    ("clear", 0),
    ("tree", 0),
    ("session", 0),
    ("rename", 0),
    ("archive", 0),
    ("delete", 0),
    ("copy", 0),
    ("quit", 0),
    ("diff", 1),
    ("review", 1),
    ("audit", 1),
    ("commit", 1),
    ("summary", 1),
    ("architect", 1),
    ("blueprint", 1),
    ("model", 2),
    ("providers", 2),
    ("effort", 2),
    ("models", 2),
    ("mode", 3),
    ("trust", 3),
    ("permissions", 3),
    ("plan", 3),
    ("ask", 3),
    ("accept-edits", 3),
    ("auto", 3),
    ("yolo", 3),
    ("status", 4),
    ("help", 4),
    ("doctor", 4),
    ("usage", 4),
    ("context", 4),
    ("logs", 4),
    ("insights", 4),
    ("caps", 4),
];
/// Width of the group-label column in the grouped list.
const GROUP_LABEL_COLS: usize = 9;
/// Below this width the grouped list is one column.
const TWO_COLUMN_MIN_WIDTH: u16 = 72;
/// Longest description shown in a list row, in columns.
const DESCRIPTION_COLS: usize = 40;

fn group_of(name: &str) -> usize {
    KNOWN_COMMANDS
        .iter()
        .find(|(known, _)| *known == name)
        .map_or(OTHER_GROUP, |(_, group)| *group)
}

/// Position in the display order; unknown commands keep catalog order after
/// the known ones (the sort is stable).
fn rank_of(name: &str) -> usize {
    KNOWN_COMMANDS
        .iter()
        .position(|(known, _)| *known == name)
        .unwrap_or(usize::MAX)
}

/// `text` cut to `cols` display columns, ending in `…` when cut.
fn clip_cols(text: &str, cols: usize) -> String {
    if display_width(text) <= cols {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let w = char_width(ch);
        if used + w + 1 > cols {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

/// Display-only shortening of a registry description: up to the first
/// sentence end, comma or `DESCRIPTION_COLS` columns, whichever is first.
/// The registry text itself is never changed (other surfaces read it).
/// Short phrases for the commands people reach for most. The registry keeps
/// its full sentences (help and other surfaces read those); the list shows a
/// phrase so a row is read at a glance instead of ending in an ellipsis.
const DISPLAY_PHRASES: &[(&str, &str)] = &[
    ("new", "start fresh"),
    ("resume", "pick up an earlier chat"),
    ("rewind", "back to an earlier turn"),
    ("compact", "shrink the context"),
    ("undo", "undo the last change"),
    ("redo", "redo what was undone"),
    ("reset", "stop, drop pending edits"),
    ("fork", "branch this chat"),
    ("clear", "start a new session"),
    ("diff", "what changed this turn"),
    ("review", "review the working tree"),
    ("commit", "commit the changes"),
    ("model", "switch model or effort"),
    ("providers", "sign in, add keys"),
    ("effort", "set reasoning effort"),
    ("mode", "ask, auto, plan"),
    ("trust", "trust this folder"),
    ("permissions", "rules and allowances"),
    ("status", "what is running"),
    ("help", "keys and commands"),
    ("doctor", "check the install"),
    ("quit", "leave"),
];

/// The description the list shows for `name`: its phrase when it has one,
/// else the registry text cut short.
fn display_description(name: &str, description: &str) -> String {
    match DISPLAY_PHRASES.iter().find(|(command, _)| *command == name) {
        Some((_, phrase)) => (*phrase).to_string(),
        None => short_description(description),
    }
}

fn short_description(description: &str) -> String {
    let text = description.trim();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut cut = text.len();
    for (k, (at, ch)) in chars.iter().enumerate() {
        let sentence_end = *ch == '.'
            && chars
                .get(k + 1)
                .is_none_or(|(_, next)| next.is_whitespace());
        if *ch == ',' || sentence_end {
            cut = *at;
            break;
        }
    }
    clip_cols(text[..cut].trim_end(), DESCRIPTION_COLS)
}

/// One row of the grouped list: the position in the filtered list, and the
/// group label when this row opens its group.
struct GroupedRow {
    position: usize,
    label: Option<&'static str>,
}

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
    /// previous behavior hosts rely on.
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

    /// Preferred height for the popup at `width`: the rows of the grouped or
    /// filtered list (capped), plus one row for the count.
    pub(crate) fn calculate_required_height(&self, width: u16) -> u16 {
        let list = self.filtered();
        let rows = if list.is_empty() {
            1
        } else if self.is_grouped() {
            self.grouped_columns(&list, width)
                .iter()
                .map(Vec::len)
                .max()
                .unwrap_or(1)
                .max(1)
        } else {
            list.len().min(MAX_POPUP_ROWS)
        };
        // One more row for the count.
        rows as u16 + 1
    }

    /// Compute exact/prefix matches over the catalog, paired with optional
    /// highlight indices. Preserves catalog presentation order; exact matches
    /// sort before prefix matches — verbatim from the donor `filtered()`.
    fn filtered(&self) -> Vec<(usize, Option<Vec<usize>>)> {
        let filter = self.command_filter.trim();
        let mut out: Vec<(usize, Option<Vec<usize>>)> = Vec::new();
        if filter.is_empty() {
            return self.curated();
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
                // Unavailable commands surface only by name, never through prose.
                if !seen.contains(&index)
                    && candidate.disabled_reason.is_none()
                    && description_matches(&candidate.description, &needles)
                {
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

    /// The bare `/` view: available commands grouped (session, review, model,
    /// policy, info, other), a few per group; unavailable ones stay hidden
    /// until their name is typed.
    fn curated(&self) -> Vec<(usize, Option<Vec<usize>>)> {
        let mut by_group: Vec<Vec<usize>> = vec![Vec::new(); GROUP_LABELS.len()];
        for (index, candidate) in self.catalog.iter().enumerate() {
            if candidate.disabled_reason.is_none() {
                by_group[group_of(&candidate.name)].push(index);
            }
        }
        let mut out = Vec::new();
        for (group, mut members) in by_group.into_iter().enumerate() {
            members.sort_by_key(|index| rank_of(&self.catalog[*index].name));
            out.extend(
                members
                    .into_iter()
                    .take(GROUP_CAPS[group])
                    .map(|i| (i, None)),
            );
        }
        out
    }

    /// Commands that can run now: the denominator of the count.
    fn available_count(&self) -> usize {
        self.catalog
            .iter()
            .filter(|candidate| candidate.disabled_reason.is_none())
            .count()
    }

    /// First row of a `win`-row window over `len` rows that keeps the
    /// selection visible (same rule as the shared popup rows).
    fn window_start(&self, len: usize, win: usize) -> usize {
        if len == 0 || win == 0 {
            return 0;
        }
        let mut start = self.state.scroll_top.min(len - 1);
        if let Some(selected) = self.state.selected_idx {
            if selected < start {
                start = selected;
            } else if selected >= start + win {
                start = selected + 1 - win;
            }
        }
        start.min(len.saturating_sub(win))
    }

    /// The grouped view as columns of rows: two columns split on a group
    /// boundary when the width allows (selection runs down the first column,
    /// then the second), else one column that scrolls.
    fn grouped_columns(
        &self,
        list: &[(usize, Option<Vec<usize>>)],
        width: u16,
    ) -> Vec<Vec<GroupedRow>> {
        let two = width >= TWO_COLUMN_MIN_WIDTH;
        let (start, end) = if two {
            (0, list.len())
        } else {
            let win = MAX_POPUP_ROWS.min(list.len());
            let start = self.window_start(list.len(), win);
            (start, start + win)
        };
        let mut rows: Vec<GroupedRow> = Vec::new();
        let mut previous = None;
        for (position, entry) in list.iter().enumerate().take(end).skip(start) {
            let group = group_of(&self.catalog[entry.0].name);
            let label = if previous == Some(group) {
                None
            } else {
                Some(GROUP_LABELS[group])
            };
            previous = Some(group);
            rows.push(GroupedRow { position, label });
        }
        if !two {
            return vec![rows];
        }
        let half = rows.len().div_ceil(2);
        let split = rows
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, row)| row.label.is_some())
            .map(|(i, _)| i)
            .find(|i| *i >= half)
            .unwrap_or(rows.len());
        let right = rows.split_off(split);
        vec![rows, right]
    }

    fn is_grouped(&self) -> bool {
        self.command_filter.trim().is_empty()
    }

    /// One grouped row: dim label, command name, dim short description.
    fn grouped_line(
        &self,
        row: &GroupedRow,
        candidate: &CompletionCandidate,
        name_cols: usize,
        cols: usize,
    ) -> Line<'static> {
        let dim = Style::default().add_modifier(Modifier::DIM);
        let label_cols = GROUP_LABEL_COLS;
        let selected = self.state.selected_idx == Some(row.position);
        let pill = if selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        let remaining = cols.saturating_sub(GROUP_LABEL_COLS + name_cols);
        let mut description = clip_cols(
            &display_description(&candidate.name, &candidate.description),
            remaining,
        );
        if selected {
            description = format!("{description:<remaining$}");
        }
        Line::from(vec![
            Span::styled(format!("{:<label_cols$}", row.label.unwrap_or("")), dim),
            Span::styled(
                format!("{:<name_cols$}", format!("/{}", candidate.name)),
                pill.add_modifier(Modifier::BOLD),
            ),
            Span::styled(description, if selected { pill } else { dim }),
        ])
    }

    fn render_grouped(
        &self,
        area: Rect,
        buf: &mut Buffer,
        list: &[(usize, Option<Vec<usize>>)],
        body_rows: u16,
    ) {
        let columns = self.grouped_columns(list, area.width);
        let gap = 2u16;
        let col_w = if columns.len() == 2 {
            area.width.saturating_sub(gap) / 2
        } else {
            area.width
        };
        let name_cols = list
            .iter()
            .map(|(index, _)| display_width(&self.catalog[*index].name) + 1)
            .max()
            .unwrap_or(0)
            + 2;
        for (c, rows) in columns.iter().enumerate() {
            let x = area.x + (c as u16) * (col_w + gap);
            for (r, row) in rows.iter().enumerate() {
                if r as u16 >= body_rows {
                    break;
                }
                let candidate = &self.catalog[list[row.position].0];
                self.grouped_line(row, candidate, name_cols, col_w as usize)
                    .render(
                        Rect {
                            x,
                            y: area.y + r as u16,
                            width: col_w,
                            height: 1,
                        },
                        buf,
                    );
            }
        }
    }

    /// The typed view: one column, best match first, the typed prefix bold;
    /// an unavailable command shows dim with its reason.
    fn render_filtered(
        &self,
        area: Rect,
        buf: &mut Buffer,
        list: &[(usize, Option<Vec<usize>>)],
        body_rows: u16,
    ) {
        let win = (body_rows as usize).min(MAX_POPUP_ROWS).min(list.len());
        let start = self.window_start(list.len(), win);
        let dim = Style::default().add_modifier(Modifier::DIM);
        let names: Vec<String> = list
            .iter()
            .map(|(index, _)| {
                let candidate = &self.catalog[*index];
                match self.matched_alias(candidate) {
                    Some(alias) => format!("/{alias}  ({})", candidate.name),
                    None => format!("/{}", candidate.name),
                }
            })
            .collect();
        let name_cols = names[start..start + win]
            .iter()
            .map(|name| display_width(name))
            .max()
            .unwrap_or(0)
            + 2;
        for (offset, position) in (start..start + win).enumerate() {
            let (index, indices) = &list[position];
            let candidate = &self.catalog[*index];
            let disabled = candidate.disabled_reason.is_some();
            let selected = self.state.selected_idx == Some(position) && !disabled;
            let base = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else if disabled {
                dim
            } else {
                Style::default()
            };
            // Match offsets count from the name after its `/`.
            let matched: Vec<usize> = indices
                .as_ref()
                .map(|v| v.iter().map(|i| i + 1).collect())
                .unwrap_or_default();
            let mut spans: Vec<Span<'static>> = vec![Span::raw(" ")];
            let mut run = String::new();
            let mut run_matched = false;
            for (at, ch) in names[position].chars().enumerate() {
                let is_match = matched.contains(&at);
                if is_match != run_matched && !run.is_empty() {
                    let style = if run_matched {
                        base.add_modifier(Modifier::BOLD)
                    } else {
                        base
                    };
                    spans.push(Span::styled(std::mem::take(&mut run), style));
                }
                run_matched = is_match;
                run.push(ch);
            }
            if !run.is_empty() {
                let style = if run_matched {
                    base.add_modifier(Modifier::BOLD)
                } else {
                    base
                };
                spans.push(Span::styled(run, style));
            }
            let used = display_width(&names[position]);
            let remaining = (area.width as usize).saturating_sub(1 + name_cols);
            let description = match &candidate.disabled_reason {
                Some(reason) => format!("unavailable: {reason}"),
                None => display_description(&candidate.name, &candidate.description),
            };
            let description = clip_cols(&description, remaining);
            let description = if selected {
                format!("{description:<remaining$}")
            } else {
                description
            };
            spans.push(Span::styled(
                " ".repeat(name_cols.saturating_sub(used)),
                base,
            ));
            spans.push(Span::styled(description, if selected { base } else { dim }));
            Line::from(spans).render(
                Rect {
                    x: area.x,
                    y: area.y + offset as u16,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
        }
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
        if area.width == 0 || area.height == 0 {
            return;
        }
        let list = self.filtered();
        let dim = Style::default().add_modifier(Modifier::DIM);
        // The last row carries the count; the rows above carry the list.
        let body_rows = if area.height >= 2 {
            area.height - 1
        } else {
            area.height
        };
        if list.is_empty() {
            Line::from(Span::styled(
                "no matches",
                dim.add_modifier(Modifier::ITALIC),
            ))
            .render(Rect { height: 1, ..area }, buf);
        } else if self.is_grouped() {
            self.render_grouped(area, buf, &list, body_rows);
        } else {
            self.render_filtered(area, buf, &list, body_rows);
        }
        if area.height >= 2 {
            let total = self.available_count();
            let text = if self.is_grouped() {
                format!("{} of {total} \u{b7} type to filter", list.len())
            } else if list.is_empty() {
                format!("0 of {total} \u{b7} esc closes")
            } else {
                format!(
                    "{} of {total} \u{b7} \u{23ce} runs \u{b7} esc closes",
                    list.len()
                )
            };
            let width = display_width(&text).min(area.width as usize) as u16;
            Line::from(Span::styled(text, dim)).render(
                Rect {
                    x: area.right() - width,
                    y: area.bottom() - 1,
                    width,
                    height: 1,
                },
                buf,
            );
        }
    }
}
