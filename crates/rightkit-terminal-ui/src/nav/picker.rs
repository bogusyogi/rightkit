//! The generic modal list picker — the shared widget behind model/effort
//! selection and any host-defined menu.
//!
//! Donor lineage: codex `tui/src/bottom_pane/list_selection_view.rs` at
//! `444da310e108da16aaeb18fd790b0ac464f08aca`, adapted to this crate's
//! seams:
//!
//! - **Intents, not callbacks.** Codex's `SelectionAction` is a boxed
//!   closure over `AppEventSender`; here selection produces
//!   [`PickerAction`]s the host drains — same "nav emits, host performs"
//!   discipline as [`super::intent::NavIntent`]. No donor event bus.
//! - **Sanitized at the boundary.** Item names, descriptions, and the
//!   search buffer pass through `render::sanitize`.
//! - **Disabled rows are structural.** A disabled item renders dimmed with
//!   its reason and is skipped by cursor movement and digit shortcuts —
//!   the user cannot land on it.

use crate::component::RenderFrameExt as _;
use crossterm::event::{KeyCode, KeyEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::component::{Component, Event, Outcome, Role, Theme};
use crate::render::cell::clipped_line;
use crate::render::sanitize;
#[cfg(test)]
use crate::transcript::wrap::display_width;

use super::scroll_state::ScrollState;

/// Rows of the item list visible at once inside the modal.
const MAX_VISIBLE_ROWS: usize = 10;

/// One selectable row in the picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionItem {
    /// Primary display label.
    pub name: String,
    /// Optional longer description, shown dimmed under the name.
    pub description: Option<String>,
    /// Extra text the search filter matches against (e.g. provider name);
    /// never displayed.
    pub search_value: Option<String>,
    /// Marks the row as the committed/current value (`(current)` suffix).
    pub is_current: bool,
    /// Marks the row as the default (`(default)` suffix).
    pub is_default: bool,
    /// Dimmed unselectable row with the shown reason.
    pub is_disabled: bool,
    pub disabled_reason: Option<String>,
    /// Checkbox state for toggleable rows (`/providers` model visibility):
    /// `Some` renders `[x]`/`[ ]` and turns Enter/Space into a toggle
    /// instead of an accept. `None` rows behave exactly as before.
    pub checked: Option<bool>,
}

impl SelectionItem {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: sanitize(&name.into()),
            description: None,
            search_value: None,
            is_current: false,
            is_default: false,
            is_disabled: false,
            disabled_reason: None,
            checked: None,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(sanitize(&description.into()));
        self
    }

    pub fn search_value(mut self, value: impl Into<String>) -> Self {
        self.search_value = Some(sanitize(&value.into()));
        self
    }

    pub fn current(mut self, is_current: bool) -> Self {
        self.is_current = is_current;
        self
    }

    pub fn default_flag(mut self, is_default: bool) -> Self {
        self.is_default = is_default;
        self
    }

    pub fn disabled(mut self, reason: Option<String>) -> Self {
        self.is_disabled = true;
        self.disabled_reason = reason.map(|r| sanitize(&r));
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }
}

/// Construction parameters for a [`ListPicker`].
#[derive(Debug, Clone)]
pub struct PickerParams {
    /// Correlates emitted actions back to the view that produced them, so a
    /// host running several pickers can tell them apart.
    pub view_id: u32,
    /// Bold title at the top of the modal.
    pub title: String,
    /// Dimmed explanatory line under the title.
    pub subtitle: Option<String>,
    pub items: Vec<SelectionItem>,
    /// Cursor lands here initially (typically the committed value's row).
    pub initial_selected_idx: Option<usize>,
    /// When true, typing edits a fuzzy filter; digits jump-select only when
    /// the filter is empty. When false, `1..9` always jump-select.
    pub is_searchable: bool,
    /// Footer hint line (e.g. "↑/↓ select · Enter confirm · Esc cancel").
    pub footer_hint: Option<String>,
    /// Whether Esc may close the picker. False for surfaces where closing
    /// must not be expressible (the caller renders its own affordance).
    pub allow_cancel: bool,
}

impl PickerParams {
    /// Sensible defaults for a simple pick-one list.
    pub fn picker(view_id: u32, title: impl Into<String>, items: Vec<SelectionItem>) -> Self {
        Self {
            view_id,
            title: sanitize(&title.into()),
            subtitle: None,
            items,
            initial_selected_idx: None,
            is_searchable: true,
            footer_hint: None,
            allow_cancel: true,
        }
    }
}

/// What the picker reports to the host. Emitted through `drain_actions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    /// The user confirmed the row at `index` in the *current* (filtered)
    /// item order. The picker reports the index into `items()` — callers
    /// compare against [`ListPicker::items`] to resolve the row.
    Accepted { view_id: u32, index: usize },
    /// The user flipped a checkbox row (`checked.is_some()`) — the picker
    /// stays open, the row's `checked` already carries the new state, and
    /// `index` is the filtered position exactly as [`Self::Accepted`].
    Toggled { view_id: u32, index: usize },
    /// The user cancelled (Esc with `allow_cancel`).
    Cancelled { view_id: u32 },
}

/// A modal vertical list with optional fuzzy filtering.
///
/// The widget holds no authority: it never knows what an accepted row
/// *means*. Meaning lives in the host, which reads the chosen index and
/// applies it through whatever registry or route owns that data.
pub struct ListPicker {
    view_id: u32,
    title: String,
    subtitle: Option<String>,
    /// The full, unfiltered item list. Indices in `PickerAction` refer to
    /// *filtered* positions; `filtered_idx()` maps them back.
    items: Vec<SelectionItem>,
    /// Indices into `items` currently passing the filter.
    filtered: Vec<usize>,
    scroll: ScrollState,
    query: String,
    is_searchable: bool,
    footer_hint: Option<String>,
    allow_cancel: bool,
    actions: Vec<PickerAction>,
    done: bool,
}

impl ListPicker {
    pub fn new(params: PickerParams) -> Self {
        let items = params.items;
        let filtered: Vec<usize> = (0..items.len()).collect();
        let mut scroll = ScrollState::new();
        scroll.clamp_selection(items.len());
        if let Some(idx) = params.initial_selected_idx
            && idx < items.len()
        {
            scroll.selected_idx = Some(idx);
        }
        Self {
            view_id: params.view_id,
            title: params.title,
            subtitle: params.subtitle,
            items,
            filtered,
            scroll,
            query: String::new(),
            is_searchable: params.is_searchable,
            footer_hint: params.footer_hint.map(|h| sanitize(&h)),
            allow_cancel: params.allow_cancel,
            actions: Vec::new(),
            done: false,
        }
    }

    /// Rows currently passing the filter, in display order.
    pub fn items(&self) -> Vec<&SelectionItem> {
        self.filtered.iter().map(|&i| &self.items[i]).collect()
    }

    /// The index into [`Self::items`] of a filtered row position.
    pub fn filtered_idx(&self, filtered_pos: usize) -> Option<usize> {
        self.filtered.get(filtered_pos).copied()
    }

    /// Currently highlighted row, as an index into `items()`.
    pub fn selected(&self) -> Option<usize> {
        self.scroll.selected_idx
    }

    /// Programmatically highlight a filtered row position — used to
    /// preserve the cursor across a refreshed item list.
    pub fn select(&mut self, filtered_pos: usize) {
        if filtered_pos < self.filtered.len() {
            self.scroll.selected_idx = Some(filtered_pos);
            self.scroll
                .ensure_visible(self.filtered.len(), MAX_VISIBLE_ROWS);
        }
    }

    /// The search/filter buffer.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// True once the picker has emitted a terminal action
    /// (accept/cancel). The host should drop it.
    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn drain_actions(&mut self) -> Vec<PickerAction> {
        std::mem::take(&mut self.actions)
    }

    /// Replace the item list while preserving the highlighted row by
    /// position-clamped index — used when the underlying catalog refreshes
    /// underneath an open picker.
    pub fn set_items(&mut self, items: Vec<SelectionItem>) {
        self.items = items;
        self.apply_filter();
    }

    /// Subsequence match, case-insensitive — the permissive filter the
    /// donor uses for picker search (a row survives if every query char
    /// appears in order in `name + search_value`).
    fn matches(&self, item: &SelectionItem) -> bool {
        if self.query.is_empty() {
            return true;
        }
        let hay = format!(
            "{} {}",
            item.name,
            item.search_value.as_deref().unwrap_or("")
        )
        .to_lowercase();
        let needle = self.query.to_lowercase();
        let mut chars = hay.chars();
        needle.chars().all(|c| chars.by_ref().any(|h| h == c))
    }

    fn apply_filter(&mut self) {
        self.filtered = (0..self.items.len())
            .filter(|&i| self.matches(&self.items[i]))
            .collect();
        self.scroll.clamp_selection(self.filtered.len());
        self.scroll
            .ensure_visible(self.filtered.len(), MAX_VISIBLE_ROWS);
    }

    /// Move the cursor `delta` rows, skipping disabled entries. Repeats the
    /// donor's behavior of never landing the cursor on a row the user can't
    /// choose.
    fn move_selection(&mut self, delta: isize) {
        let len = self.filtered.len();
        if len == 0 {
            return;
        }
        let mut idx = self.scroll.selected_idx.unwrap_or(0);
        for _ in 0..len {
            let next = (idx as isize + delta).rem_euclid(len as isize) as usize;
            if !self.items[self.filtered[next]].is_disabled {
                idx = next;
                break;
            }
            idx = next;
        }
        self.scroll.selected_idx = Some(idx);
        self.scroll.ensure_visible(len, MAX_VISIBLE_ROWS);
    }

    /// Move by one page or to an edge, skipping disabled rows the same way.
    fn move_to(&mut self, target: usize) {
        let len = self.filtered.len();
        if len == 0 {
            return;
        }
        let target = target.min(len - 1);
        // Walk toward the target from the current position; if the target
        // itself is disabled, prefer the nearest enabled row in the same
        // direction, else any enabled row.
        let cur = self.scroll.selected_idx.unwrap_or(0);
        let dir: isize = if target >= cur { 1 } else { -1 };
        let mut idx = target;
        loop {
            if !self.items[self.filtered[idx]].is_disabled {
                break;
            }
            let next = idx as isize + dir;
            if next < 0 || next >= len as isize {
                // Walk the other way.
                let mut back = target as isize - dir;
                let mut found = None;
                while back >= 0 && back < len as isize {
                    if !self.items[self.filtered[back as usize]].is_disabled {
                        found = Some(back as usize);
                        break;
                    }
                    back -= dir;
                }
                idx = found.unwrap_or(target);
                break;
            }
            idx = next as usize;
        }
        self.scroll.selected_idx = Some(idx);
        self.scroll.ensure_visible(len, MAX_VISIBLE_ROWS);
    }

    fn accept_selected(&mut self) {
        let Some(idx) = self.scroll.selected_idx else {
            return;
        };
        let Some(&item_idx) = self.filtered.get(idx) else {
            return;
        };
        if self.items[item_idx].is_disabled {
            return;
        }
        // A checkbox row toggles in place — the picker stays open and the
        // host reads the new state off the row.
        if let Some(checked) = self.items[item_idx].checked {
            self.items[item_idx].checked = Some(!checked);
            self.actions.push(PickerAction::Toggled {
                view_id: self.view_id,
                index: idx,
            });
            return;
        }
        self.actions.push(PickerAction::Accepted {
            view_id: self.view_id,
            index: idx,
        });
        self.done = true;
    }

    fn cancel(&mut self) {
        if !self.allow_cancel {
            return;
        }
        self.actions.push(PickerAction::Cancelled {
            view_id: self.view_id,
        });
        self.done = true;
    }

    fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::PageUp => {
                let cur = self.scroll.selected_idx.unwrap_or(0);
                self.move_to(cur.saturating_sub(MAX_VISIBLE_ROWS));
            }
            KeyCode::PageDown => {
                let cur = self.scroll.selected_idx.unwrap_or(0);
                self.move_to((cur + MAX_VISIBLE_ROWS).min(self.filtered.len().saturating_sub(1)));
            }
            KeyCode::Home => self.move_to(0),
            KeyCode::End => self.move_to(self.filtered.len().saturating_sub(1)),
            KeyCode::Enter => self.accept_selected(),
            KeyCode::Esc => self.cancel(),
            // Space flips a checkbox row when one is highlighted; on a
            // plain list it still feeds the search filter below.
            KeyCode::Char(' ')
                if self
                    .scroll
                    .selected_idx
                    .and_then(|pos| self.filtered.get(pos))
                    .is_some_and(|&item_idx| self.items[item_idx].checked.is_some()) =>
            {
                self.accept_selected();
            }
            KeyCode::Backspace if self.is_searchable => {
                self.query.pop();
                self.apply_filter();
            }
            KeyCode::Char(c) => {
                if !self.is_searchable || (self.query.is_empty() && c.is_ascii_digit()) {
                    // Digit jump when not searching or when the filter is
                    // empty — same affordance as the donor's shortcuts.
                    if c.is_ascii_digit() && c != '0' {
                        let target = (c as usize) - ('1' as usize);
                        if target < self.filtered.len() {
                            self.move_to(target);
                            self.accept_selected();
                        }
                        return;
                    }
                }
                if self.is_searchable {
                    self.query.push(c);
                    self.apply_filter();
                }
            }
            _ => {}
        }
    }

    fn rows(&self, width: u16, theme: &Theme) -> Vec<Line<'static>> {
        let dim = Style::default().fg(theme.color(Role::Dim));
        let mut lines: Vec<Line<'static>> = Vec::new();

        lines.push(Line::from(Span::styled(
            self.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )));
        if let Some(subtitle) = &self.subtitle {
            lines.push(Line::from(Span::styled(subtitle.clone(), dim)));
        }
        if self.is_searchable {
            lines.push(Line::from(vec![
                Span::styled("Search: ".to_string(), dim),
                Span::raw(self.query.clone()),
                Span::styled("█".to_string(), dim),
            ]));
        }
        lines.push(Line::from(""));

        let len = self.filtered.len();
        if len == 0 {
            lines.push(Line::from(Span::styled(
                "  No matching items".to_string(),
                dim,
            )));
        } else {
            let start = self.scroll.scroll_top.min(len.saturating_sub(1));
            let end = (start + MAX_VISIBLE_ROWS).min(len);
            for pos in start..end {
                let item = &self.items[self.filtered[pos]];
                let selected = self.scroll.selected_idx == Some(pos);
                let marker = if selected { "› " } else { "  " };
                let mut spans = vec![Span::styled(
                    marker.to_string(),
                    if selected {
                        Style::default().add_modifier(Modifier::BOLD)
                    } else {
                        dim
                    },
                )];
                let mut name = item.name.clone();
                if let Some(checked) = item.checked {
                    name = format!("{} {name}", if checked { "[x]" } else { "[ ]" });
                }
                if item.is_current {
                    name.push_str(" (current)");
                }
                if item.is_default {
                    name.push_str(" (default)");
                }
                let name_style = if item.is_disabled {
                    dim
                } else if selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                spans.push(Span::styled(name, name_style));
                if item.is_disabled
                    && let Some(reason) = &item.disabled_reason
                {
                    spans.push(Span::styled(format!(" ({reason})"), dim));
                }
                lines.push(clipped_line(Line::from(spans), width.saturating_sub(2)));
                if let Some(desc) = &item.description {
                    let desc_line = Line::from(vec![
                        Span::raw("    ".to_string()),
                        Span::styled(desc.clone(), dim),
                    ]);
                    lines.push(clipped_line(desc_line, width.saturating_sub(2)));
                }
            }
            if len > MAX_VISIBLE_ROWS {
                lines.push(Line::from(Span::styled(
                    format!(
                        "  ({}/{})",
                        self.scroll.selected_idx.map(|i| i + 1).unwrap_or(0),
                        len
                    ),
                    dim,
                )));
            }
        }

        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            self.footer_hint.clone().unwrap_or_else(|| {
                if self.is_searchable {
                    "type to filter · ↑/↓ select · Enter confirm · Esc cancel".to_string()
                } else {
                    "↑/↓ select · Enter confirm · Esc cancel".to_string()
                }
            }),
            dim,
        )));
        lines
            .into_iter()
            .map(|line| clipped_line(line, width))
            .collect()
    }
}

impl Component for ListPicker {
    fn render(&mut self, frame: &mut dyn crate::component::RenderFrame, area: Rect, theme: &Theme) {
        if self.done {
            return;
        }
        let lines = self.rows(area.width.saturating_sub(2), theme);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.color(Role::Border)));
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        if self.done {
            return Outcome::Ignored;
        }
        let Event::Key(key) = event else {
            return Outcome::Ignored;
        };
        if key.kind == KeyEventKind::Release {
            return Outcome::Ignored;
        }
        self.handle_key(key.code);
        Outcome::Redraw
    }

    fn desired_height(&self, _width: u16, available: u16) -> u16 {
        // Title + optional subtitle + optional search + blank + items +
        // blank + footer, plus the border.
        let mut h = 2u16; // border
        h += 1; // title
        if self.subtitle.is_some() {
            h += 1;
        }
        if self.is_searchable {
            h += 1;
        }
        h += 1; // blank
        let visible_rows: u16 = self
            .filtered
            .iter()
            .take(MAX_VISIBLE_ROWS)
            .map(|&i| 1 + u16::from(self.items[i].description.is_some()))
            .sum();
        h += visible_rows + u16::from(self.filtered.len() > MAX_VISIBLE_ROWS);
        h += 2; // blank + footer
        h.min(available)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Event {
        Event::Key(crossterm::event::KeyEvent::new(
            code,
            crossterm::event::KeyModifiers::NONE,
        ))
    }

    fn items() -> Vec<SelectionItem> {
        vec![
            SelectionItem::new("alpha"),
            SelectionItem::new("beta").current(true),
            SelectionItem::new("gamma"),
        ]
    }

    #[test]
    fn the_cursor_wraps_and_never_lands_on_a_disabled_row() {
        let mut list = items();
        list[1].is_disabled = true;
        let mut picker = ListPicker::new(PickerParams::picker(1, "pick", list));
        picker.handle(&key(KeyCode::Down));
        assert_eq!(picker.selected(), Some(2));
        picker.handle(&key(KeyCode::Down));
        assert_eq!(picker.selected(), Some(0));
        picker.handle(&key(KeyCode::Up));
        assert_eq!(picker.selected(), Some(2));
    }

    #[test]
    fn search_filters_and_the_cursor_clamps_into_bounds() {
        let mut picker = ListPicker::new(PickerParams::picker(1, "pick", items()));
        picker.handle(&key(KeyCode::Char('g')));
        assert_eq!(picker.items().len(), 1);
        assert_eq!(picker.selected(), Some(0));
        picker.handle(&key(KeyCode::Backspace));
        assert_eq!(picker.items().len(), 3);
    }

    #[test]
    fn accepting_emits_the_filtered_index_and_closes() {
        let mut picker = ListPicker::new(PickerParams::picker(7, "pick", items()));
        picker.handle(&key(KeyCode::Down));
        picker.handle(&key(KeyCode::Enter));
        assert_eq!(
            picker.drain_actions(),
            vec![PickerAction::Accepted {
                view_id: 7,
                index: 1
            }]
        );
        assert!(picker.is_done());
    }

    #[test]
    fn a_disabled_row_cannot_be_accepted_even_if_forced_selected() {
        let mut list = items();
        list[1].is_disabled = true;
        let mut picker = ListPicker::new(PickerParams::picker(1, "pick", list));
        // Navigation never lands on a disabled row (covered above); force
        // the cursor there anyway to prove Enter still emits nothing.
        picker.select(1);
        picker.handle(&key(KeyCode::Enter));
        assert!(picker.drain_actions().is_empty());
        assert!(!picker.is_done());
    }

    #[test]
    fn escape_cancels_only_when_allowed() {
        let mut picker = ListPicker::new(PickerParams::picker(3, "pick", items()));
        picker.handle(&key(KeyCode::Esc));
        assert_eq!(
            picker.drain_actions(),
            vec![PickerAction::Cancelled { view_id: 3 }]
        );

        let mut params = PickerParams::picker(3, "pick", items());
        params.allow_cancel = false;
        let mut picker = ListPicker::new(params);
        picker.handle(&key(KeyCode::Esc));
        assert!(picker.drain_actions().is_empty());
        assert!(!picker.is_done());
    }

    #[test]
    fn digits_jump_select_when_the_filter_is_empty() {
        let mut params = PickerParams::picker(1, "pick", items());
        params.is_searchable = false;
        let mut picker = ListPicker::new(params);
        picker.handle(&key(KeyCode::Char('2')));
        assert_eq!(
            picker.drain_actions(),
            vec![PickerAction::Accepted {
                view_id: 1,
                index: 1
            }]
        );
    }

    #[test]
    fn hostile_item_text_is_sanitized_at_construction() {
        let item = SelectionItem::new("evil \x1b[2J name").description("\x07desc");
        assert!(!item.name.contains('\u{1b}'));
        assert!(!item.description.unwrap().contains('\u{7}'));
    }

    #[test]
    fn rendered_rows_respect_the_width() {
        let mut list = items();
        list[0].name = "a very long item name that will overflow a narrow modal".into();
        let picker = ListPicker::new(PickerParams::picker(1, "pick", list));
        let theme = Theme::new(
            crate::terminal::Background::Dark,
            crate::terminal::ColorDepth::TrueColor,
        );
        for line in picker.rows(20, &theme) {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(display_width(&text) <= 20, "row exceeded width: {text:?}");
        }
    }
}
