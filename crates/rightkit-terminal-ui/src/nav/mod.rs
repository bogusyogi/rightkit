//! Session and agent navigation.
//!
//! `daemon → root sessions → agent trees`. This module is a *lens* over that
//! shape: descending into a running subagent's transcript, watching it live,
//! stopping *that* agent without touching its parent or siblings, returning,
//! and moving sideways between root sessions.
//!
//! Three rules make that safe, and each has a test below:
//!
//! 1. **Navigation is not execution ownership.** Viewing an agent never takes
//!    control of it; leaving it never stops it. [`NavView`] has no method
//!    that stops or switches anything -- only [`NavIntent`] variants that ask
//!    the host to.
//! 2. **Detach is not cancel.** Switching to a sibling, the parent, another
//!    root session, or out entirely leaves work running.
//! 3. **Stop is scoped and explicit.** Stopping a child terminates that
//!    subtree only, and is never implied by any navigation action -- it is
//!    the one intent variant that names an agent to stop, gated behind an
//!    explicit confirmation that names exactly what will stop.

pub mod breadcrumb;
pub mod inspection;
pub mod intent;
pub mod model;
pub mod model_picker;
pub mod picker;
pub mod providers_picker;
pub mod scroll_state;
pub mod worktree_menu;

pub use breadcrumb::breadcrumb as render_breadcrumb;
pub use intent::{NavIntent, ProviderAction, SecretText};
pub use model::{AgentId, AgentNode, AgentState, SessionDetail, SessionId, SessionSummary};
pub use model_picker::{
    CycleDirection, EffortChoice, ModelChoice, ModelPicker, ProviderChoice, ProviderKind,
    ProviderModel, cycle_effort_intent, cycle_model_intents,
};
pub use picker::{ListPicker, PickerAction, PickerParams, SelectionItem};
pub use providers_picker::ProvidersPicker;
pub use scroll_state::ScrollState;

use crate::component::RenderFrameExt as _;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};

use crate::component::{Component, Event, Outcome, Role, Theme};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind};

/// Which picker is currently taking the alt-screen, if any.
///
/// A closed set on purpose: `/agents` navigates *within* a tree, `/sessions`
/// moves *between* trees, and the two are never the same list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Sessions,
    Agents,
}

/// The session/agent navigation view.
///
/// Holds a read model (refreshed by whoever owns the daemon connection, out
/// of scope here) plus the cursor and confirmation state needed to render one
/// frame and turn key presses into [`NavIntent`]s. It performs no effect.
#[derive(Debug, Clone, PartialEq)]
pub struct NavView {
    sessions: Vec<SessionSummary>,
    current_session: Option<SessionSummary>,
    tree: Option<AgentNode>,
    /// Ids descended into, root-first. Empty means "viewing the root agent
    /// itself, not yet descended."
    path: Vec<AgentId>,
    picker: Option<PickerKind>,
    cursor: usize,
    session_query: String,
    session_searching: bool,
    session_alpha_sort: bool,
    /// Row metadata (age, workspace, first-message preview) by session id.
    session_details: std::collections::HashMap<String, SessionDetail>,
    /// A session delete awaiting `[y/N]` confirmation.
    pending_session_delete: Option<SessionId>,
    /// A stop awaiting explicit confirmation. Set only by an explicit stop
    /// key; cleared by confirming, cancelling, or navigating away.
    pending_stop: Option<AgentId>,
    intents: Vec<NavIntent>,
}

impl Default for NavView {
    fn default() -> Self {
        Self::new()
    }
}

impl NavView {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            current_session: None,
            tree: None,
            path: Vec::new(),
            picker: None,
            cursor: 0,
            session_query: String::new(),
            session_searching: false,
            session_alpha_sort: false,
            session_details: std::collections::HashMap::new(),
            pending_session_delete: None,
            pending_stop: None,
            intents: Vec::new(),
        }
    }

    /// Feed in the sibling root sessions and which one is active. Pure data
    /// refresh -- never itself a switch.
    pub fn set_sessions(&mut self, sessions: Vec<SessionSummary>, current: Option<SessionSummary>) {
        self.sessions = sessions;
        self.current_session = current;
        self.cursor = self
            .cursor
            .min(self.visible_sessions().len().saturating_sub(1));
    }

    /// Feed per-session row metadata (age, workspace, preview). Pure data
    /// refresh; sessions without an entry render title-only.
    pub fn set_session_details(
        &mut self,
        details: std::collections::HashMap<String, SessionDetail>,
    ) {
        self.session_details = details;
    }

    /// The session delete awaiting confirmation, for rendering and tests.
    pub fn pending_session_delete(&self) -> Option<&SessionId> {
        self.pending_session_delete.as_ref()
    }

    /// Whether a session filter is typed or being typed.
    pub fn session_filter_active(&self) -> bool {
        self.session_searching || !self.session_query.is_empty()
    }

    /// Feed in the active root's agent tree. Pure data refresh.
    pub fn set_tree(&mut self, tree: AgentNode) {
        self.tree = Some(tree);
    }

    /// Open `/agents`: the current root's tree, cursor at the top.
    pub fn open_agents(&mut self) {
        self.picker = Some(PickerKind::Agents);
        self.cursor = 0;
        self.intents.push(NavIntent::OpenAgents);
    }

    /// Open `/sessions`: sibling root sessions, cursor at the top.
    pub fn open_sessions(&mut self) {
        self.picker = Some(PickerKind::Sessions);
        self.cursor = 0;
        self.session_query.clear();
        self.session_searching = false;
        self.intents.push(NavIntent::OpenSessions);
    }

    pub fn is_open(&self) -> bool {
        self.picker.is_some()
    }

    /// Release picker input when a host action returns to the composer.
    /// Closing this surface does not navigate or stop any agent.
    pub fn close_picker(&mut self) {
        self.picker = None;
        self.session_searching = false;
        self.pending_stop = None;
        self.pending_session_delete = None;
    }

    /// Whether the subagent (`/agents`) picker is currently visible.
    pub fn agents_picker_open(&self) -> bool {
        self.picker == Some(PickerKind::Agents)
    }

    /// Whether the root-session picker is currently visible.
    pub fn sessions_picker_open(&self) -> bool {
        self.picker == Some(PickerKind::Sessions)
    }

    /// The highlighted root-session metadata, if the session picker is open.
    pub fn selected_session(&self) -> Option<&SessionSummary> {
        self.sessions_picker_open()
            .then(|| self.visible_sessions().get(self.cursor).copied())
            .flatten()
    }

    fn visible_sessions(&self) -> Vec<&SessionSummary> {
        let query = self.session_query.to_lowercase();
        let mut rows: Vec<_> = self
            .sessions
            .iter()
            .filter(|row| {
                row.name.to_lowercase().contains(&query)
                    || row.id.0.to_lowercase().contains(&query)
                    || self.session_details.get(&row.id.0).is_some_and(|d| {
                        d.workspace.to_lowercase().contains(&query)
                            || d.preview.to_lowercase().contains(&query)
                    })
            })
            .collect();
        if self.session_alpha_sort {
            rows.sort_by_key(|row| row.name.to_lowercase());
        }
        rows
    }

    /// Drain queued intents for the host to act on. Mirrors
    /// `composer::Composer::drain_intents` -- nav emits, the host performs.
    pub fn drain_intents(&mut self) -> Vec<NavIntent> {
        std::mem::take(&mut self.intents)
    }

    /// The agent currently "viewed": the tree's root if `path` is empty,
    /// otherwise the deepest descended node. `None` only when no tree has
    /// been loaded yet.
    pub fn viewed_agent(&self) -> Option<&AgentNode> {
        let tree = self.tree.as_ref()?;
        match self.path.last() {
            None => Some(tree),
            Some(id) => tree.find(id),
        }
    }

    /// Children selectable from the current position in `/agents`: the root's
    /// top-level children before any descent, otherwise the viewed node's own
    /// children.
    fn current_children(&self) -> &[AgentNode] {
        match self.viewed_agent() {
            Some(node) => &node.children,
            None => &[],
        }
    }

    /// A stop request awaiting confirmation, for rendering and tests.
    pub fn pending_stop(&self) -> Option<&AgentId> {
        self.pending_stop.as_ref()
    }

    /// The exact agents a confirmed stop of `id` covers: the subtree rooted
    /// at `id`, resolved against the current tree. Empty when the tree does
    /// not know the id (stale model) -- an unknown scope must never be
    /// widened to "everything".
    pub fn stop_scope(&self, id: &AgentId) -> Vec<AgentId> {
        self.tree
            .as_ref()
            .and_then(|t| t.find(id))
            .map(|node| node.subtree_ids())
            .unwrap_or_default()
    }

    /// The breadcrumb for the current position: session name, one segment per
    /// descended agent, and the viewed agent's model.
    /// True while viewing the session's root agent (nothing descended into).
    pub fn viewing_root(&self) -> bool {
        self.path.is_empty()
    }

    pub fn breadcrumb(&self) -> String {
        let session_name = self
            .current_session
            .as_ref()
            .map(|s| s.name.as_str())
            .unwrap_or("(no session)");
        let names: Vec<String> = match &self.tree {
            None => Vec::new(),
            Some(tree) => {
                let mut names = vec![tree.name.clone()];
                let mut node = tree;
                for id in &self.path {
                    if let Some(child) = node.children.iter().find(|c| &c.id == id) {
                        names.push(child.name.clone());
                        node = child;
                    }
                }
                names
            }
        };
        // Model/provider attribution is owned by the identity strip
        // (S:I6); the breadcrumb names position only.
        breadcrumb::breadcrumb(session_name, &names)
    }

    fn handle_confirm_key(&mut self, key: &KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(target) = self.pending_stop.take() {
                    self.intents.push(NavIntent::Stop(target));
                }
                Outcome::Redraw
            }
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                // Cancel only the confirmation. No intent: declining a stop
                // is not itself navigation, so it must not ascend or close.
                self.pending_stop = None;
                Outcome::Redraw
            }
            _ => Outcome::Consumed,
        }
    }

    fn handle_delete_confirm_key(&mut self, key: &KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                if let Some(id) = self.pending_session_delete.take() {
                    self.intents.push(NavIntent::DeleteSession(id));
                }
                Outcome::Redraw
            }
            // Enter is deliberately not a confirm: destroy needs an explicit y.
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Enter => {
                self.pending_session_delete = None;
                Outcome::Redraw
            }
            _ => Outcome::Consumed,
        }
    }

    fn handle_agents_key(&mut self, key: &KeyEvent) -> Outcome {
        match key.code {
            KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                Outcome::Redraw
            }
            KeyCode::Down => {
                let len = self.current_children().len();
                if len > 0 {
                    self.cursor = (self.cursor + 1).min(len - 1);
                }
                Outcome::Redraw
            }
            KeyCode::Enter => {
                if let Some(child) = self.current_children().get(self.cursor) {
                    let id = child.id.clone();
                    self.path.push(id.clone());
                    self.cursor = 0;
                    self.intents.push(NavIntent::Descend(id));
                }
                Outcome::Redraw
            }
            KeyCode::Esc => {
                // Ascend one level; never a stop. Only a *second* Escape --
                // one pressed when the path was already empty, i.e. already
                // at the root -- closes the picker and returns to the
                // composer. The first Escape from any depth only pops one
                // level and stays open.
                let already_at_root = self.path.is_empty();
                if already_at_root {
                    self.picker = None;
                } else {
                    self.path.pop();
                }
                self.cursor = 0;
                self.intents.push(NavIntent::Ascend);
                Outcome::Redraw
            }
            KeyCode::Char('s') => {
                if let Some(target) = self.viewed_agent() {
                    self.pending_stop = Some(target.id.clone());
                }
                Outcome::Redraw
            }
            _ => Outcome::Consumed,
        }
    }

    fn handle_sessions_key(&mut self, key: &KeyEvent) -> Outcome {
        if self.session_searching {
            match key.code {
                KeyCode::Esc => {
                    if self.session_query.is_empty() {
                        self.session_searching = false;
                    } else {
                        self.session_query.clear();
                    }
                    self.cursor = 0;
                    return Outcome::Redraw;
                }
                KeyCode::Enter => {
                    self.session_searching = false;
                    return Outcome::Redraw;
                }
                KeyCode::Backspace => {
                    self.session_query.pop();
                    self.cursor = 0;
                    return Outcome::Redraw;
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.session_query.push(ch);
                    self.cursor = 0;
                    return Outcome::Redraw;
                }
                _ => return Outcome::Consumed,
            }
        }
        match key.code {
            KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                Outcome::Redraw
            }
            KeyCode::Down => {
                let len = self.visible_sessions().len();
                if len > 0 {
                    self.cursor = (self.cursor + 1).min(len - 1);
                }
                Outcome::Redraw
            }
            KeyCode::Enter => {
                if let Some(session) = self.visible_sessions().get(self.cursor) {
                    let id = session.id.clone();
                    self.picker = None;
                    self.intents.push(NavIntent::SwitchSession(id));
                }
                Outcome::Redraw
            }
            KeyCode::Esc => {
                self.picker = None;
                self.intents.push(NavIntent::Ascend);
                Outcome::Redraw
            }
            KeyCode::Char('/') => {
                self.session_searching = true;
                Outcome::Redraw
            }
            KeyCode::Char('s') if self.session_query.is_empty() => {
                self.session_alpha_sort = !self.session_alpha_sort;
                self.cursor = 0;
                Outcome::Redraw
            }
            KeyCode::Char('r') if self.session_query.is_empty() => {
                if let Some(session) = self.visible_sessions().get(self.cursor) {
                    let id = session.id.clone();
                    self.intents.push(NavIntent::RenameSession(id));
                }
                Outcome::Redraw
            }
            KeyCode::Char('d') | KeyCode::Delete if self.session_query.is_empty() => {
                if let Some(session) = self.visible_sessions().get(self.cursor) {
                    self.pending_session_delete = Some(session.id.clone());
                }
                Outcome::Redraw
            }
            // Type-to-filter: any other printable key starts the filter.
            KeyCode::Char(ch)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.session_searching = true;
                self.session_query.push(ch);
                self.cursor = 0;
                Outcome::Redraw
            }
            _ => Outcome::Consumed,
        }
    }

    fn render_agents(
        &self,
        frame: &mut dyn crate::component::RenderFrame,
        area: Rect,
        theme: &Theme,
    ) {
        let items: Vec<ListItem> = self
            .current_children()
            .iter()
            .enumerate()
            .map(|(i, node)| {
                let role = node.state.role();
                let marker = if i == self.cursor { "> " } else { "  " };
                let line = Line::from(vec![
                    Span::raw(marker),
                    Span::styled(theme.glyph(role), Style::default().fg(theme.color(role))),
                    Span::raw(format!(
                        " {} [{}] {} ({})",
                        node.name,
                        node.model,
                        node.elapsed_cost,
                        node.state.label()
                    )),
                ]);
                ListItem::new(line)
            })
            .collect();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(self.breadcrumb());
        frame.render_widget(List::new(items).block(block), area);
    }

    fn render_sessions(
        &self,
        frame: &mut dyn crate::component::RenderFrame,
        area: Rect,
        theme: &Theme,
    ) {
        let items: Vec<ListItem> = self
            .visible_sessions()
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let marker = if i == self.cursor { "> " } else { "  " };
                let detail = self.session_details.get(&s.id.0);
                let mut head = format!("{marker}{}", s.name);
                if let Some(d) = detail {
                    for part in [&d.age, &d.workspace] {
                        if !part.is_empty() {
                            head.push_str(" \u{b7} ");
                            head.push_str(part);
                        }
                    }
                }
                let mut lines = vec![Line::from(Span::raw(head))];
                if let Some(d) = detail.filter(|d| !d.preview.is_empty()) {
                    lines.push(Line::from(Span::styled(
                        format!("    {}", d.preview),
                        Style::default().fg(theme.color(Role::Border)),
                    )));
                }
                ListItem::new(lines)
            })
            .collect();
        let query = if self.session_searching {
            format!("search: {}_", self.session_query)
        } else if !self.session_query.is_empty() {
            format!("filter: {}", self.session_query)
        } else {
            String::new()
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(
                "sessions · {} · {} · type to filter · Enter resume · r rename · d delete · s sort · m actions",
                if self.session_alpha_sort {
                    "name"
                } else {
                    "recent"
                },
                query
            ))
            .border_style(Style::default().fg(theme.color(Role::Border)));
        frame.render_widget(List::new(items).block(block), area);
    }
}

impl Component for NavView {
    fn render(&mut self, frame: &mut dyn crate::component::RenderFrame, area: Rect, theme: &Theme) {
        match self.picker {
            None => {}
            Some(PickerKind::Agents) => self.render_agents(frame, area, theme),
            Some(PickerKind::Sessions) => self.render_sessions(frame, area, theme),
        }
        if let Some(id) = &self.pending_session_delete {
            let label = self
                .sessions
                .iter()
                .find(|row| &row.id == id)
                .map_or(id.0.as_str(), |row| row.name.as_str());
            let warn = Line::from(Span::styled(
                format!("Permanently delete \"{label}\"? [y/N]"),
                Style::default().fg(theme.color(Role::Attention)),
            ));
            frame.render_widget(ratatui::widgets::Paragraph::new(warn), area);
        }
        if let Some(target) = &self.pending_stop {
            let warn = Line::from(Span::styled(
                format!("Stop {target}? This ends only that agent's subtree. [y/N]"),
                Style::default().fg(theme.color(Role::Attention)),
            ));
            frame.render_widget(ratatui::widgets::Paragraph::new(warn), area);
        }
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        if self.picker.is_none() {
            return Outcome::Ignored;
        }
        if let Event::Mouse(mouse) = event {
            // Adapted to Rust from Waveloom 97cbbec `cmd/waveloom/tui.go:handleMouse`
            // (Apache-2.0): shift-modified pointer input remains terminal-owned
            // selection; only unmodified wheel motion changes this picker.
            if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                return Outcome::Ignored;
            }
            // Click hit-testing stays with surface geometry, which this read
            // model deliberately does not own.
            return match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.cursor = self.cursor.saturating_sub(1);
                    Outcome::Redraw
                }
                MouseEventKind::ScrollDown => {
                    let len = match self.picker {
                        Some(PickerKind::Agents) => self.current_children().len(),
                        Some(PickerKind::Sessions) => self.visible_sessions().len(),
                        None => 0,
                    };
                    if len > 0 {
                        self.cursor = (self.cursor + 1).min(len - 1);
                    }
                    Outcome::Redraw
                }
                _ => Outcome::Ignored,
            };
        }
        let Event::Key(key) = event else {
            return Outcome::Ignored;
        };
        // Crossterm can emit a release when keyboard enhancement is active.
        // Navigation is an action surface, so only press/repeat may consume
        // it; otherwise one physical key can move or activate twice.
        if key.kind == KeyEventKind::Release {
            return Outcome::Ignored;
        }
        if self.pending_session_delete.is_some() {
            return self.handle_delete_confirm_key(key);
        }
        if self.pending_stop.is_some() {
            return self.handle_confirm_key(key);
        }
        match self.picker {
            Some(PickerKind::Agents) => self.handle_agents_key(key),
            Some(PickerKind::Sessions) => self.handle_sessions_key(key),
            None => Outcome::Ignored,
        }
    }
}
