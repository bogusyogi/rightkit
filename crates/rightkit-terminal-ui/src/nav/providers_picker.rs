//! The `/providers` screen: the one provider setup surface.
//!
//! Levels, same modal discipline as [`super::model_picker::ModelPicker`]:
//!
//! 1. **Providers** — one row per provider, `name · status`.
//! 2. **Actions** — what that provider supports: OpenAI sign in/out,
//!    API-key add/replace/remove/test, vendor-CLI status and the command to
//!    run in the vendor's own terminal, and always "Show/hide models".
//! 3. **Models** — checkbox list; Space (or Enter) flips a row and emits
//!    [`NavIntent::ToggleModelVisibility`].
//!    **Key entry** — masked single-line input for add/replace key; the key
//!    leaves this widget only inside a [`SecretText`] on the intent.
//!
//! The picker holds no persistence and performs no effect: every action is a
//! [`NavIntent`] for the host, which answers with
//! [`ProvidersPicker::update_providers`] (fresh status) and a message line.
//! Esc steps one level back; Esc on level 1 closes.

use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;

use crate::component::{Component, Event, Outcome, Theme};

use super::intent::{NavIntent, ProviderAction, SecretText};
use super::model_picker::{ProviderChoice, ProviderKind};
use super::picker::{ListPicker, PickerAction, PickerParams, SelectionItem};

/// Which list the modal currently shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Providers,
    Actions,
    Models,
    KeyEntry,
}

/// One level-2 row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionRow {
    Run(ProviderAction),
    ShowHideModels,
}

impl ActionRow {
    pub fn label(self, kind: ProviderKind) -> &'static str {
        match self {
            ActionRow::ShowHideModels => "Show / hide models",
            ActionRow::Run(action) => match action {
                ProviderAction::SignIn if kind == ProviderKind::VendorCli => {
                    "Sign in (shows the command to run)"
                }
                ProviderAction::SignIn => "Sign in",
                ProviderAction::SignOut => "Sign out",
                ProviderAction::AddKey => "Add key",
                ProviderAction::ReplaceKey => "Replace key",
                ProviderAction::RemoveKey => "Remove key",
                ProviderAction::TestConnection => "Test connection",
                ProviderAction::Status => "Status",
            },
        }
    }
}

/// The level-2 action list for a provider — pure, so the screen's contract
/// is unit-testable. `ready` = key stored / signed in.
pub fn action_rows(kind: ProviderKind, ready: bool) -> Vec<ActionRow> {
    use ProviderAction::*;
    let mut rows: Vec<ActionRow> = match kind {
        ProviderKind::OAuth => vec![ActionRow::Run(SignIn), ActionRow::Run(SignOut)],
        ProviderKind::ApiKey if ready => vec![
            ActionRow::Run(ReplaceKey),
            ActionRow::Run(RemoveKey),
            ActionRow::Run(TestConnection),
        ],
        ProviderKind::ApiKey => vec![ActionRow::Run(AddKey)],
        ProviderKind::VendorCli => vec![ActionRow::Run(Status), ActionRow::Run(SignIn)],
    };
    rows.push(ActionRow::ShowHideModels);
    rows
}

/// The `/providers` modal. Wraps a [`ListPicker`]; swaps its contents as the
/// user descends and turns actions into intents.
pub struct ProvidersPicker {
    view_id: u32,
    picker: ListPicker,
    /// Every provider row plus its full model list. `hidden` state is kept
    /// in sync as the user toggles so re-entering a provider shows it.
    providers: Vec<ProviderChoice>,
    /// Index of the provider on screen at levels 2+.
    selected: Option<usize>,
    phase: Phase,
    actions: Vec<ActionRow>,
    intents: Vec<NavIntent>,
    /// Result line from the host's last action, shown under the level-2
    /// title.
    message: Option<String>,
    /// Masked key buffer for [`Phase::KeyEntry`]; never rendered or logged.
    key_buf: String,
    key_action: ProviderAction,
}

impl ProvidersPicker {
    /// Open on the provider list.
    pub fn new(view_id: u32, providers: Vec<ProviderChoice>) -> Self {
        let mut this = Self {
            view_id,
            picker: ListPicker::new(PickerParams::picker(view_id, "Providers", Vec::new())),
            providers,
            selected: None,
            phase: Phase::Providers,
            actions: Vec::new(),
            intents: Vec::new(),
            message: None,
            key_buf: String::new(),
            key_action: ProviderAction::AddKey,
        };
        this.picker = ListPicker::new(this.providers_params(None));
        this
    }

    fn provider_items(providers: &[ProviderChoice]) -> Vec<SelectionItem> {
        providers
            .iter()
            .map(|p| {
                SelectionItem::new(p.label.clone())
                    .current(p.is_current)
                    .search_value(p.id.clone())
            })
            .collect()
    }

    fn providers_params(&self, cursor: Option<usize>) -> PickerParams {
        let mut params = PickerParams::picker(
            self.view_id,
            "Providers",
            Self::provider_items(&self.providers),
        );
        params.initial_selected_idx = cursor.or(Some(0));
        params.is_searchable = false;
        params.footer_hint = Some("↑/↓ select · Enter open · Esc close".to_string());
        params
    }

    fn actions_params(&self, provider: &ProviderChoice, cursor: Option<usize>) -> PickerParams {
        let items = self
            .actions
            .iter()
            .map(|row| SelectionItem::new(row.label(provider.kind)))
            .collect();
        let mut params = PickerParams::picker(self.view_id, provider.label.clone(), items);
        params.subtitle = self.message.clone().or_else(|| provider.note.clone());
        params.initial_selected_idx = cursor.or(Some(0));
        params.is_searchable = false;
        params.footer_hint = Some("↑/↓ select · Enter run · Esc back to providers".to_string());
        params
    }

    /// Checkbox rows for one provider's model list: `[x]` = shown in
    /// `/model`, `[ ]` = hidden. Unavailable rows stay toggleable — hiding
    /// a row the account cannot serve is still the user's choice.
    fn model_items(provider: &ProviderChoice) -> Vec<SelectionItem> {
        let mut items: Vec<SelectionItem> = provider
            .models
            .iter()
            .map(|m| {
                let mut item = SelectionItem::new(m.choice.display_name.clone())
                    .checked(!m.hidden)
                    .current(m.choice.is_current)
                    .default_flag(m.choice.is_default)
                    .search_value(format!("{} {}", provider.id, m.model_id));
                if let Some(description) = &m.choice.description {
                    item = item.description(description.clone());
                }
                item
            })
            .collect();
        if items.is_empty() {
            items.push(
                SelectionItem::new("No models available").disabled(
                    provider
                        .note
                        .clone()
                        .or(Some("nothing to show".to_string())),
                ),
            );
        }
        items
    }

    fn models_params(&self, provider: &ProviderChoice) -> PickerParams {
        let current = provider.models.iter().position(|m| m.choice.is_current);
        let mut params = PickerParams::picker(
            self.view_id,
            format!("Models · {}", provider.label),
            Self::model_items(provider),
        );
        params.initial_selected_idx = current;
        params.footer_hint = Some("↑/↓ select · Space show/hide · Esc back".to_string());
        params
    }

    fn key_entry_params(&self, provider: &ProviderChoice) -> PickerParams {
        let masked = if self.key_buf.is_empty() {
            "(type or paste the key — input is hidden)".to_string()
        } else {
            format!(
                "{} ({} chars)",
                "•".repeat(self.key_buf.chars().count().min(32)),
                self.key_buf.chars().count()
            )
        };
        let verb = if self.key_action == ProviderAction::ReplaceKey {
            "Replace"
        } else {
            "Add"
        };
        let mut params = PickerParams::picker(
            self.view_id,
            format!("{verb} API key · {}", provider.label),
            vec![SelectionItem::new(masked)],
        );
        params.subtitle = Some("Stored in the OS keychain; never shown or logged.".to_string());
        params.is_searchable = false;
        params.footer_hint = Some("Enter save · Backspace edit · Esc cancel".to_string());
        params
    }

    fn rebuild(&mut self, cursor: Option<usize>) {
        let params = match (
            self.phase,
            self.selected.and_then(|i| self.providers.get(i)),
        ) {
            (Phase::Providers, _) | (_, None) => self.providers_params(cursor),
            (Phase::Actions, Some(p)) => self.actions_params(p, cursor),
            (Phase::Models, Some(p)) => self.models_params(p),
            (Phase::KeyEntry, Some(p)) => self.key_entry_params(p),
        };
        self.picker = ListPicker::new(params);
    }

    fn enter_actions(&mut self, provider_idx: usize, cursor: Option<usize>) {
        let Some(provider) = self.providers.get(provider_idx) else {
            return;
        };
        self.actions = action_rows(provider.kind, provider.ready);
        self.selected = Some(provider_idx);
        self.phase = Phase::Actions;
        self.rebuild(cursor);
    }

    fn accept_provider(&mut self, filtered_pos: usize) {
        if let Some(idx) = self.picker.filtered_idx(filtered_pos) {
            self.message = None;
            self.enter_actions(idx, None);
        }
    }

    fn accept_action(&mut self, filtered_pos: usize) {
        let (Some(provider_idx), Some(row_idx)) =
            (self.selected, self.picker.filtered_idx(filtered_pos))
        else {
            return;
        };
        let Some(row) = self.actions.get(row_idx).copied() else {
            return;
        };
        match row {
            ActionRow::ShowHideModels => {
                self.phase = Phase::Models;
                self.rebuild(None);
            }
            ActionRow::Run(action @ (ProviderAction::AddKey | ProviderAction::ReplaceKey)) => {
                self.key_buf.clear();
                self.key_action = action;
                self.phase = Phase::KeyEntry;
                self.rebuild(None);
            }
            ActionRow::Run(action) => {
                if let Some(provider) = self.providers.get(provider_idx) {
                    self.intents.push(NavIntent::ProviderAction {
                        provider: provider.id.clone(),
                        action,
                        secret: None,
                    });
                }
                self.message = Some("Working…".to_string());
                self.rebuild(Some(row_idx));
            }
        }
    }

    /// One checkbox flipped: update the local snapshot and ask the host to
    /// persist the NEW hidden state.
    fn toggle_model(&mut self, filtered_pos: usize) {
        let (Some(provider_idx), Some(item_idx)) =
            (self.selected, self.picker.filtered_idx(filtered_pos))
        else {
            return;
        };
        let Some(checked) = self
            .picker
            .items()
            .get(item_idx)
            .and_then(|item| item.checked)
        else {
            return;
        };
        let hidden = !checked;
        let Some(provider) = self.providers.get_mut(provider_idx) else {
            return;
        };
        let provider_id = provider.id.clone();
        let Some(model) = provider.models.get_mut(item_idx) else {
            return;
        };
        model.hidden = hidden;
        self.intents.push(NavIntent::ToggleModelVisibility {
            provider: provider_id,
            model: model.model_id.clone(),
            hidden,
        });
    }

    fn submit_key(&mut self) {
        let key = self.key_buf.trim().to_string();
        self.key_buf.clear();
        let Some(provider_idx) = self.selected else {
            return;
        };
        let Some(provider) = self.providers.get(provider_idx) else {
            return;
        };
        if key.is_empty() {
            self.message = Some("No key entered; nothing saved.".to_string());
        } else {
            self.intents.push(NavIntent::ProviderAction {
                provider: provider.id.clone(),
                action: self.key_action,
                secret: Some(SecretText::new(key)),
            });
            self.message = Some("Saving key…".to_string());
        }
        self.phase = Phase::Actions;
        self.rebuild(None);
    }

    fn handle_key_entry(&mut self, event: &Event) -> Outcome {
        match event {
            Event::Paste(text) => {
                self.key_buf
                    .extend(text.chars().filter(|c| !c.is_control()));
            }
            Event::Key(key) => match key.code {
                KeyCode::Esc => {
                    self.key_buf.clear();
                    self.phase = Phase::Actions;
                    self.rebuild(None);
                    return Outcome::Redraw;
                }
                KeyCode::Enter => {
                    self.submit_key();
                    return Outcome::Redraw;
                }
                KeyCode::Backspace => {
                    self.key_buf.pop();
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    self.key_buf.push(c);
                }
                _ => return Outcome::Consumed,
            },
            _ => return Outcome::Ignored,
        }
        self.rebuild(None);
        Outcome::Redraw
    }

    /// Host callback: fresh provider data after an action, with the result
    /// line to show. Keeps the user on the same provider and level (key
    /// entry falls back to the action list).
    pub fn update_providers(&mut self, providers: Vec<ProviderChoice>, message: Option<String>) {
        let cursor = self.picker.selected();
        self.providers = providers;
        self.message = message;
        if self.phase == Phase::KeyEntry {
            self.phase = Phase::Actions;
        }
        match (self.phase, self.selected) {
            (Phase::Providers, _) => self.rebuild(cursor),
            (_, Some(idx)) if idx < self.providers.len() => {
                if self.phase == Phase::Actions {
                    let ready = self.providers[idx].ready;
                    self.actions = action_rows(self.providers[idx].kind, ready);
                    self.rebuild(cursor);
                } else {
                    self.rebuild(None);
                }
            }
            _ => {
                self.phase = Phase::Providers;
                self.selected = None;
                self.rebuild(None);
            }
        }
    }

    pub fn is_done(&self) -> bool {
        self.picker.is_done()
    }

    /// Drain `NavIntent`s for the host.
    pub fn drain_intents(&mut self) -> Vec<NavIntent> {
        std::mem::take(&mut self.intents)
    }
}

impl Component for ProvidersPicker {
    fn render(&mut self, frame: &mut dyn crate::component::RenderFrame, area: Rect, theme: &Theme) {
        self.picker.render(frame, area, theme);
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        if self.phase == Phase::KeyEntry {
            if let Event::Key(key) = event
                && key.kind == KeyEventKind::Release
            {
                return Outcome::Ignored;
            }
            return self.handle_key_entry(event);
        }
        let Event::Key(key) = event else {
            return self.picker.handle(event);
        };
        if key.kind == KeyEventKind::Release {
            return Outcome::Ignored;
        }
        // Esc steps one level back rather than closing outright.
        if key.code == KeyCode::Esc {
            match self.phase {
                Phase::Models => {
                    self.phase = Phase::Actions;
                    self.rebuild(None);
                    return Outcome::Redraw;
                }
                Phase::Actions => {
                    let cursor = self.selected;
                    self.phase = Phase::Providers;
                    self.selected = None;
                    self.message = None;
                    self.rebuild(cursor);
                    return Outcome::Redraw;
                }
                _ => {}
            }
        }
        let outcome = self.picker.handle(event);
        for action in self.picker.drain_actions() {
            match action {
                PickerAction::Accepted { index, .. } => match self.phase {
                    Phase::Providers => self.accept_provider(index),
                    Phase::Actions => self.accept_action(index),
                    // A plain row accept in the Models phase is the
                    // "No models" placeholder; checkbox rows emit Toggled.
                    Phase::Models | Phase::KeyEntry => {}
                },
                PickerAction::Toggled { index, .. } => {
                    if self.phase == Phase::Models {
                        self.toggle_model(index);
                    }
                }
                PickerAction::Cancelled { .. } => {}
            }
        }
        outcome
    }

    fn desired_height(&self, width: u16, available: u16) -> u16 {
        self.picker.desired_height(width, available)
    }
}
