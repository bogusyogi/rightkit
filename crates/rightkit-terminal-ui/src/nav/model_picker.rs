//! Model and reasoning-effort pickers.
//!
//! Donor lineage: codex `chatwidget/model_popups.rs` and
//! `chatwidget/model_popup_state.rs` (model + effort lists, `(current)` /
//! `(default)` markers, highlight-preserved refresh) plus pi's
//! `model-selector.ts`/`thinking-selector.ts` (current-selection badge,
//! provider tag, centered-window list) — all at the pinned revisions.
//!
//! Two boundaries hold here:
//!
//! - **Presentation only.** The picker is handed a *snapshot* of
//!   `ModelChoice`s assembled by the host from the host app's shared model
//!   catalog; it owns no catalog, no provider registry, no persistence.
//!   What the user picked is emitted as [`NavIntent::SelectModel`] /
//!   [`NavIntent::SelectEffort`] and applied through the existing
//!   route authority — the picker's visible state always derives from the
//!   committed selection it was given, never a draft it wrote.
//! - **Two-phase, not two pickers.** Model selection opens the model list;
//!   if the chosen model exposes more than one effort level the same modal
//!   swaps to an effort list (codex's two-popup flow, composed inline).
//!   Esc on the effort list returns to the model list; Esc on the model
//!   list cancels.

use crossterm::event::{KeyCode, KeyEventKind};
use ratatui::layout::Rect;

use crate::component::{Component, Event, Outcome, Theme};
use crate::render::sanitize;

use super::intent::NavIntent;
use super::picker::{ListPicker, PickerAction, PickerParams, SelectionItem};

/// One effort level a model supports. `id` is the value the route
/// authority recognizes (e.g. "low", "high"); `label` is what renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffortChoice {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub is_current: bool,
    pub is_default: bool,
}

impl EffortChoice {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: sanitize(&id.into()),
            label: sanitize(&label.into()),
            description: None,
            is_current: false,
            is_default: false,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(sanitize(&description.into()));
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
}

/// One selectable model. The host builds these from its catalog snapshot —
/// `id` is the model identifier the route authority knows, `display_name`
/// what renders, `provider` an optional dim badge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    pub id: String,
    pub provider: Option<String>,
    pub display_name: String,
    pub description: Option<String>,
    pub is_current: bool,
    pub is_default: bool,
    /// Effort levels this model exposes. Empty means the model is committed
    /// on selection with no second step.
    pub efforts: Vec<EffortChoice>,
}

/// Cycle direction for the chat-scoped hotkeys. The host supplies the
/// already-scoped selectable set; this helper cannot see catalog entries
/// outside that set or mutate committed routing state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CycleDirection {
    Next,
    Previous,
}

/// Return model + its current/default effort intents for the next eligible
/// model in the supplied chat-scoped set. Unknown current ids start at the
/// first/last item according to direction.
pub fn cycle_model_intents(
    models: &[ModelChoice],
    current_model: &str,
    direction: CycleDirection,
) -> Vec<NavIntent> {
    if models.is_empty() {
        return Vec::new();
    }
    let current = models.iter().position(|model| model.id == current_model);
    let index = match (current, direction) {
        (Some(index), CycleDirection::Next) => (index + 1) % models.len(),
        (Some(index), CycleDirection::Previous) => (index + models.len() - 1) % models.len(),
        (None, CycleDirection::Next) => 0,
        (None, CycleDirection::Previous) => models.len() - 1,
    };
    let model = &models[index];
    let mut intents = vec![NavIntent::SelectModel(model.id.clone())];
    if let Some(effort) = model
        .efforts
        .iter()
        .find(|effort| effort.is_current)
        .or_else(|| model.efforts.iter().find(|effort| effort.is_default))
        .or_else(|| model.efforts.first())
    {
        intents.push(NavIntent::SelectEffort(effort.id.clone()));
    }
    intents
}

/// Return the next effort intent for the current model. The caller supplies
/// the model's chat-scoped effort set.
pub fn cycle_effort_intent(
    efforts: &[EffortChoice],
    current_effort: &str,
    direction: CycleDirection,
) -> Option<NavIntent> {
    if efforts.is_empty() {
        return None;
    }
    let current = efforts
        .iter()
        .position(|effort| effort.id == current_effort);
    let index = match (current, direction) {
        (Some(index), CycleDirection::Next) => (index + 1) % efforts.len(),
        (Some(index), CycleDirection::Previous) => (index + efforts.len() - 1) % efforts.len(),
        (None, CycleDirection::Next) => 0,
        (None, CycleDirection::Previous) => efforts.len() - 1,
    };
    Some(NavIntent::SelectEffort(efforts[index].id.clone()))
}

impl ModelChoice {
    pub fn new(id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            id: sanitize(&id.into()),
            provider: None,
            display_name: sanitize(&display_name.into()),
            description: None,
            is_current: false,
            is_default: false,
            efforts: Vec::new(),
        }
    }

    pub fn provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(sanitize(&provider.into()));
        self
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(sanitize(&description.into()));
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

    pub fn efforts(mut self, efforts: Vec<EffortChoice>) -> Self {
        self.efforts = efforts;
        self
    }
}

/// One catalog model under a [`ProviderChoice`] — the presentation row plus
/// the visibility metadata `/providers` toggles and `/model` filters on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderModel {
    /// The real model id the route authority knows (`route.model`) — the
    /// visibility key. The row's `choice.id` may instead be the opaque
    /// `provider::model` selection key under a cross-provider collision.
    pub model_id: String,
    /// Hidden from `/model`'s model list; the `/providers` checkbox state.
    /// The committed model is never hidden from its own picker.
    pub hidden: bool,
    /// The row `/model` level 2 renders (selection id, markers, efforts).
    pub choice: ModelChoice,
}

/// One provider group for the provider-first `/model` flow and the
/// `/providers` visibility manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderChoice {
    /// Provider id the route authority knows (`openai`, `minimax`, ...).
    pub id: String,
    /// Human label — display name plus auth source
    /// ("OpenAI · ChatGPT sign-in").
    pub label: String,
    /// The committed route lives under this provider.
    pub is_current: bool,
    /// A credential is configured for this provider (or a
    /// configured/committed route names it). `/model` level 1 offers only
    /// configured providers; `/providers` lists every known provider.
    pub configured: bool,
    /// Every catalog model under this provider, visibility flags included.
    pub models: Vec<ProviderModel>,
    /// How this provider authenticates; drives the `/providers` level-2
    /// action list.
    pub kind: ProviderKind,
    /// A credential / sign-in is present (OAuth signed in, key stored, or
    /// vendor CLI signed in). Selects Add-vs-Replace key and the like.
    pub ready: bool,
    /// Extra line for the provider: install hint when a vendor CLI is
    /// missing, the exact login command otherwise.
    pub note: Option<String>,
}

/// Authentication family of a [`ProviderChoice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProviderKind {
    /// Native OAuth sign-in (OpenAI / ChatGPT).
    OAuth,
    /// Pasted API key held in the keychain.
    #[default]
    ApiKey,
    /// A vendor CLI that authenticates itself (Claude Code, Antigravity).
    VendorCli,
}

impl ProviderChoice {
    /// `/model` level 1 offers configured providers, plus vendor CLIs even
    /// when missing so they render `not installed` with the install hint
    /// instead of silently vanishing.
    pub fn offered_in_model_picker(&self) -> bool {
        self.configured || self.kind == ProviderKind::VendorCli
    }
}

/// Which list the modal currently shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Provider-first `/model` level 1: pick a configured provider.
    Providers,
    Models,
    Efforts,
}

/// The model/effort modal. Wraps a [`ListPicker`]; swaps its contents when
/// a model with effort options is chosen.
pub struct ModelPicker {
    /// Correlates actions back to this view; passed through to the inner
    /// list picker so both phases report the same view.
    view_id: u32,
    picker: ListPicker,
    models: Vec<ModelChoice>,
    /// Provider groups for the provider-first flow (phase `Providers` and
    /// the source each `Models` rebuild projects from). Empty for a plain
    /// model-first picker.
    providers: Vec<ProviderChoice>,
    /// Index into `providers` of the provider whose models are on screen.
    selected_provider: Option<usize>,
    /// Effort options for the model being effort-picked when
    /// `phase == Efforts` (keyed by model id), or the standalone effort
    /// list in `effort_only` mode.
    efforts_for: Option<String>,
    efforts: Vec<EffortChoice>,
    phase: Phase,
    intents: Vec<NavIntent>,
    /// Distinguishes a fully-committed selection from a bare cancel for
    /// hosts that want to know.
    committed: bool,
}

impl ModelPicker {
    /// Open on the model list. `models` should already mark `is_current` /
    /// `is_default` per the *committed* route state — the picker displays
    /// what it's given; it does not commit anything.
    pub fn new(view_id: u32, models: Vec<ModelChoice>) -> Self {
        let items = Self::model_items(&models);
        let initial = models.iter().position(|m| m.is_current);
        let mut params = PickerParams::picker(view_id, "Select model", items);
        params.initial_selected_idx = initial;
        params.footer_hint =
            Some("↑/↓ select · Enter choose · Esc cancel — current marked (current)".to_string());
        Self {
            view_id,
            picker: ListPicker::new(params),
            models,
            providers: Vec::new(),
            selected_provider: None,
            efforts_for: None,
            efforts: Vec::new(),
            phase: Phase::Models,
            intents: Vec::new(),
            committed: false,
        }
    }

    /// Open on the provider list — the provider-first `/model` flow.
    /// `providers` should contain only the providers a picker may offer
    /// (the host filters `configured`); `is_current` marks the committed
    /// route's provider. Enter descends into that provider's model list
    /// (hidden models filtered out — the committed row always survives),
    /// Esc on the model list returns here.
    pub fn provider_first(view_id: u32, providers: Vec<ProviderChoice>) -> Self {
        let items = Self::provider_items(&providers);
        let initial = providers.iter().position(|p| p.is_current);
        let mut params = PickerParams::picker(view_id, "Select provider", items);
        params.initial_selected_idx = initial;
        params.footer_hint =
            Some("↑/↓ select · Enter choose · Esc cancel — current marked (current)".to_string());
        Self {
            view_id,
            picker: ListPicker::new(params),
            models: Vec::new(),
            providers,
            selected_provider: None,
            efforts_for: None,
            efforts: Vec::new(),
            phase: Phase::Providers,
            intents: Vec::new(),
            committed: false,
        }
    }

    /// Open directly on the effort list for the current model — the
    /// `/effort`-style flow with no model step.
    pub fn effort_only(view_id: u32, model_label: &str, efforts: Vec<EffortChoice>) -> Self {
        let items = Self::effort_items(&efforts);
        let initial = efforts.iter().position(|e| e.is_current);
        let mut params = PickerParams::picker(
            view_id,
            format!("Select reasoning level for {}", sanitize(model_label)),
            items,
        );
        params.initial_selected_idx = initial;
        params.footer_hint = Some("↑/↓ select · Enter choose · Esc cancel".to_string());
        Self {
            view_id,
            picker: ListPicker::new(params),
            models: Vec::new(),
            providers: Vec::new(),
            selected_provider: None,
            efforts_for: None,
            efforts,
            phase: Phase::Efforts,
            intents: Vec::new(),
            committed: false,
        }
    }

    /// Refresh the model snapshot while open — e.g. the catalog finished
    /// loading. Preserves the highlighted row by model id, the codex
    /// `model_popup_state` behavior: a refresh must not yank the cursor
    /// away from what the user was reading.
    pub fn set_models(&mut self, models: Vec<ModelChoice>) {
        if self.phase != Phase::Models {
            self.models = models;
            return;
        }
        let highlighted_id = self
            .picker
            .selected()
            .and_then(|pos| self.picker.filtered_idx(pos))
            .and_then(|idx| self.models.get(idx))
            .map(|m| m.id.clone());
        let target = highlighted_id
            .as_ref()
            .and_then(|id| models.iter().position(|m| &m.id == id))
            .or_else(|| models.iter().position(|m| m.is_current));
        let items = Self::model_items(&models);
        self.models = models;
        self.picker.set_items(items);
        // `target` indexes the unfiltered list; `select` takes a filtered
        // position, so map it through the picker's filter first.
        let filtered_target = target.and_then(|m_idx| {
            (0..self.picker.items().len()).find(|&pos| self.picker.filtered_idx(pos) == Some(m_idx))
        });
        if let Some(pos) = filtered_target {
            self.picker.select(pos);
        }
    }

    /// Provider rows for level 1 of the provider-first flow.
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

    /// Rebuild the level-1 provider list — the Esc target of the model
    /// list in provider-first mode. Keeps the cursor on the provider the
    /// user came from.
    fn providers_phase_params(&self) -> PickerParams {
        let items = Self::provider_items(&self.providers);
        let mut params = PickerParams::picker(self.view_id, "Select provider", items);
        params.initial_selected_idx = self
            .selected_provider
            .or_else(|| self.providers.iter().position(|p| p.is_current));
        params.footer_hint =
            Some("↑/↓ select · Enter choose · Esc cancel — current marked (current)".to_string());
        params
    }

    /// Rebuild the model list for the selected provider — used both when a
    /// provider is accepted and when Esc returns here from the effort
    /// list, so the title/footer always reflect the provider-first flow.
    fn models_phase_params(&self) -> PickerParams {
        let items = Self::model_items(&self.models);
        let current = self.models.iter().position(|m| m.is_current);
        let (title, hint) = match self
            .selected_provider
            .and_then(|idx| self.providers.get(idx))
        {
            Some(provider) => (
                format!("Select model · {}", provider.label),
                "↑/↓ select · Enter choose · Esc back to providers — current marked (current)",
            ),
            None => (
                "Select model".to_string(),
                "↑/↓ select · Enter choose · Esc cancel — current marked (current)",
            ),
        };
        let mut items = items;
        if items.is_empty() {
            // A provider with nothing to pick (vendor CLI not installed, or
            // every model hidden) explains itself rather than showing a
            // blank list.
            let note = self
                .selected_provider
                .and_then(|idx| self.providers.get(idx))
                .and_then(|p| p.note.clone());
            items.push(
                SelectionItem::new("No models available")
                    .disabled(Some(note.unwrap_or_else(|| "nothing to show".to_string()))),
            );
        }
        let mut params = PickerParams::picker(self.view_id, title, items);
        params.initial_selected_idx = current;
        params.footer_hint = Some(hint.to_string());
        params
    }

    /// Model rows: `name [provider]` with `(current)`/`(default)` markers
    /// and optional description. Provider rides in `search_value` so search
    /// matches it without crowding the row.
    fn model_items(models: &[ModelChoice]) -> Vec<SelectionItem> {
        models
            .iter()
            .map(|m| {
                let mut item = SelectionItem::new(if let Some(p) = &m.provider {
                    format!("{} [{}]", m.display_name, p)
                } else {
                    m.display_name.clone()
                })
                .current(m.is_current)
                .default_flag(m.is_default);
                if let Some(desc) = &m.description {
                    item = item.description(desc.clone());
                }
                if let Some(p) = &m.provider {
                    item = item.search_value(format!("{p} {}", m.id));
                } else {
                    item = item.search_value(m.id.clone());
                }
                item
            })
            .collect()
    }

    fn effort_items(efforts: &[EffortChoice]) -> Vec<SelectionItem> {
        efforts
            .iter()
            .map(|e| {
                let mut item = SelectionItem::new(e.label.clone())
                    .current(e.is_current)
                    .default_flag(e.is_default)
                    .search_value(e.id.clone());
                if let Some(desc) = &e.description {
                    item = item.description(desc.clone());
                }
                item
            })
            .collect()
    }

    /// Enter the selected provider's model list — the second step of the
    /// provider-first flow. `/model` hides models the user hid through
    /// `/providers`, except the committed row, which is always selectable.
    fn accept_provider(&mut self, filtered_pos: usize) {
        let Some(item_idx) = self.picker.filtered_idx(filtered_pos) else {
            return;
        };
        if self.providers.get(item_idx).is_none() {
            return;
        }
        self.selected_provider = Some(item_idx);
        self.models = self.providers[item_idx]
            .models
            .iter()
            .filter(|m| !m.hidden || m.choice.is_current)
            .map(|m| m.choice.clone())
            .collect();
        let params = self.models_phase_params();
        self.picker = ListPicker::new(params);
        self.phase = Phase::Models;
    }

    fn accept_model(&mut self, filtered_pos: usize) {
        let Some(item_idx) = self.picker.filtered_idx(filtered_pos) else {
            return;
        };
        let Some(model) = self.models.get(item_idx).cloned() else {
            return;
        };
        if model.efforts.len() > 1 {
            // Second step: pick effort for this model.
            let efforts = model.efforts.clone();
            let items = Self::effort_items(&efforts);
            let initial = efforts.iter().position(|e| e.is_current);
            let mut params = PickerParams::picker(
                self.view_id,
                format!("Select reasoning level for {}", model.display_name),
                items,
            );
            params.initial_selected_idx = initial;
            params.footer_hint = Some("↑/↓ select · Enter choose · Esc back to models".to_string());
            self.efforts_for = Some(model.id.clone());
            self.efforts = efforts;
            self.phase = Phase::Efforts;
            self.picker = ListPicker::new(params);
            return;
        }
        self.committed = true;
        self.intents.push(NavIntent::SelectModel(model.id));
        if let Some(only) = model.efforts.first() {
            self.intents.push(NavIntent::SelectEffort(only.id.clone()));
        }
    }

    fn accept_effort(&mut self, filtered_pos: usize) {
        let Some(item_idx) = self.picker.filtered_idx(filtered_pos) else {
            return;
        };
        let Some(effort_id) = self.efforts.get(item_idx).map(|e| e.id.clone()) else {
            return;
        };
        self.committed = true;
        if let Some(model_id) = self.efforts_for.clone() {
            self.intents.push(NavIntent::SelectModel(model_id));
        }
        self.intents.push(NavIntent::SelectEffort(effort_id));
    }

    /// Whether the user committed a selection (vs cancelled).
    pub fn committed(&self) -> bool {
        self.committed
    }

    pub fn is_done(&self) -> bool {
        self.picker.is_done()
    }

    /// Drain `NavIntent`s for the host's route authority.
    pub fn drain_intents(&mut self) -> Vec<NavIntent> {
        std::mem::take(&mut self.intents)
    }
}

impl Component for ModelPicker {
    fn render(&mut self, frame: &mut dyn crate::component::RenderFrame, area: Rect, theme: &Theme) {
        self.picker.render(frame, area, theme);
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        let Event::Key(key) = event else {
            return self.picker.handle(event);
        };
        if key.kind == KeyEventKind::Release {
            return Outcome::Ignored;
        }
        if key.code == KeyCode::Esc {
            match self.phase {
                // Esc on the provider-first model list returns to the
                // provider list rather than cancelling outright.
                Phase::Models if !self.providers.is_empty() => {
                    self.picker = ListPicker::new(self.providers_phase_params());
                    self.phase = Phase::Providers;
                    self.selected_provider = None;
                    return Outcome::Redraw;
                }
                // Esc on the second step returns to the model list rather
                // than cancelling outright.
                Phase::Efforts if self.efforts_for.is_some() => {
                    self.picker = ListPicker::new(self.models_phase_params());
                    self.phase = Phase::Models;
                    self.efforts_for = None;
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
                    Phase::Models => self.accept_model(index),
                    Phase::Efforts => self.accept_effort(index),
                },
                PickerAction::Toggled { .. } | PickerAction::Cancelled { .. } => {
                    // Model-picker rows are never checkboxes, and ListPicker
                    // already marked itself done on cancel — nothing emits.
                }
            }
        }
        outcome
    }

    fn desired_height(&self, width: u16, available: u16) -> u16 {
        self.picker.desired_height(width, available)
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

    fn models() -> Vec<ModelChoice> {
        vec![
            ModelChoice::new("m-fast", "fast")
                .default_flag(true)
                .efforts(vec![
                    EffortChoice::new("low", "Low").default_flag(true),
                    EffortChoice::new("high", "High"),
                ]),
            ModelChoice::new("m-slow", "slow").current(true),
        ]
    }

    #[test]
    fn picking_a_model_with_no_effort_step_commits_immediately() {
        // The cursor opens on the committed row ("slow"), so Enter alone.
        let mut picker = ModelPicker::new(1, models());
        picker.handle(&key(KeyCode::Enter));
        assert_eq!(
            picker.drain_intents(),
            vec![NavIntent::SelectModel("m-slow".into())]
        );
        assert!(picker.committed());
        assert!(picker.is_done());
    }

    #[test]
    fn a_multi_effort_model_swaps_to_the_effort_list() {
        let mut picker = ModelPicker::new(1, models());
        picker.handle(&key(KeyCode::Up)); // cursor 1 -> 0, "fast"
        picker.handle(&key(KeyCode::Enter)); // choose "fast" (row 0)
        assert!(!picker.is_done(), "effort step should replace the list");
        picker.handle(&key(KeyCode::Down)); // "High"
        picker.handle(&key(KeyCode::Enter));
        assert_eq!(
            picker.drain_intents(),
            vec![
                NavIntent::SelectModel("m-fast".into()),
                NavIntent::SelectEffort("high".into()),
            ]
        );
        assert!(picker.committed());
    }

    #[test]
    fn escape_on_the_effort_step_returns_to_models() {
        let mut picker = ModelPicker::new(1, models());
        picker.handle(&key(KeyCode::Up)); // cursor 1 -> 0, "fast"
        picker.handle(&key(KeyCode::Enter)); // into effort list for "fast"
        picker.handle(&key(KeyCode::Esc)); // back
        assert!(!picker.is_done());
        assert!(!picker.committed());
        picker.handle(&key(KeyCode::Esc)); // cancel for real
        assert!(picker.is_done());
        assert!(picker.drain_intents().is_empty());
    }

    #[test]
    fn effort_only_mode_emits_just_the_effort() {
        let efforts = vec![
            EffortChoice::new("low", "Low"),
            EffortChoice::new("high", "High"),
        ];
        let mut picker = ModelPicker::effort_only(1, "current model", efforts);
        picker.handle(&key(KeyCode::Down));
        picker.handle(&key(KeyCode::Enter));
        assert_eq!(
            picker.drain_intents(),
            vec![NavIntent::SelectEffort("high".into())]
        );
    }

    #[test]
    fn a_refresh_preserves_the_highlighted_model_by_id() {
        let mut picker = ModelPicker::new(1, models());
        // The committed "slow" model is already highlighted at construction.
        let mut refreshed = models();
        // Insert a new model at the front; "slow" must stay highlighted.
        refreshed.insert(0, ModelChoice::new("m-new", "new"));
        picker.set_models(refreshed);
        picker.handle(&key(KeyCode::Enter));
        assert_eq!(
            picker.drain_intents(),
            vec![NavIntent::SelectModel("m-slow".into())]
        );
    }

    #[test]
    fn cancelling_commits_nothing() {
        let mut picker = ModelPicker::new(1, models());
        picker.handle(&key(KeyCode::Esc));
        assert!(!picker.committed());
        assert!(picker.drain_intents().is_empty());
        assert!(picker.is_done());
    }

    fn providers() -> Vec<ProviderChoice> {
        vec![
            ProviderChoice {
                id: "openai".to_string(),
                label: "OpenAI · ChatGPT sign-in".to_string(),
                is_current: true,
                configured: true,
                kind: ProviderKind::default(),
                ready: true,
                note: None,
                models: vec![
                    ProviderModel {
                        model_id: "m-shown".to_string(),
                        hidden: false,
                        choice: ModelChoice::new("m-shown", "shown"),
                    },
                    ProviderModel {
                        model_id: "m-hidden".to_string(),
                        hidden: true,
                        choice: ModelChoice::new("m-hidden", "hidden"),
                    },
                    // The committed row stays visible even when hidden.
                    ProviderModel {
                        model_id: "m-current".to_string(),
                        hidden: true,
                        choice: ModelChoice::new("m-current", "current").current(true),
                    },
                ],
            },
            ProviderChoice {
                id: "minimax".to_string(),
                label: "MiniMax · API key".to_string(),
                is_current: false,
                configured: true,
                kind: ProviderKind::default(),
                ready: true,
                note: None,
                models: vec![ProviderModel {
                    model_id: "MiniMax-M3".to_string(),
                    hidden: false,
                    choice: ModelChoice::new("MiniMax-M3", "MiniMax-M3"),
                }],
            },
        ]
    }

    #[test]
    fn provider_first_flow_descends_then_returns() {
        let mut picker = ModelPicker::provider_first(1, providers());
        // Cursor opens on the committed provider; Enter descends.
        picker.handle(&key(KeyCode::Enter));
        assert!(!picker.is_done());
        // The model list hides the hidden row but keeps the committed one.
        let items = picker.picker.items();
        let names: Vec<String> = items.iter().map(|item| item.name.clone()).collect();
        assert_eq!(names, vec!["shown", "current"]);
        assert!(items[1].is_current);
        // Esc returns to the provider list rather than cancelling.
        picker.handle(&key(KeyCode::Esc));
        assert!(!picker.is_done());
        let items = picker.picker.items();
        let names: Vec<String> = items.iter().map(|item| item.name.clone()).collect();
        assert_eq!(names, vec!["OpenAI · ChatGPT sign-in", "MiniMax · API key"]);
        assert!(items[0].is_current);
        picker.handle(&key(KeyCode::Esc));
        assert!(picker.is_done());
        assert!(picker.drain_intents().is_empty());
    }

    #[test]
    fn provider_first_selection_commits_through_the_same_intent() {
        let mut picker = ModelPicker::provider_first(1, providers());
        picker.handle(&key(KeyCode::Enter)); // openai's models
        picker.handle(&key(KeyCode::Enter)); // cursor is on the committed row
        assert_eq!(
            picker.drain_intents(),
            vec![NavIntent::SelectModel("m-current".into())]
        );
        assert!(picker.committed());
    }
}
