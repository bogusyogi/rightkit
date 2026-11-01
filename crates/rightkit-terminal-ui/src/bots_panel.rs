//! Read-only projection of daemon-stored Bot profiles.
//!
//! Bots remain Role Pack profiles plus scheduled tasks in daemon storage; this
//! panel renders their shared state without creating a terminal-local store.

use crate::component::RenderFrameExt as _;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem};

use crate::component::{Component, Event, Outcome, Role, Theme};
use crate::nav::{AgentId, AgentNode};

/// A live subagent row shown beneath the composer. Unlike [`BotProjection`],
/// this is a view of work attached to the current conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAgentProjection {
    pub id: AgentId,
    pub name: String,
    pub status: String,
    pub role: String,
    pub model: String,
    pub elapsed: String,
    pub activity: String,
}

impl From<&AgentNode> for LiveAgentProjection {
    fn from(agent: &AgentNode) -> Self {
        Self {
            id: agent.id.clone(),
            name: agent.name.clone(),
            status: agent.state.label().to_string(),
            // Engine read models currently use each child name as its role.
            role: agent.name.clone(),
            model: agent.model.clone(),
            elapsed: agent
                .elapsed_cost
                .split('·')
                .next()
                .unwrap_or_default()
                .trim()
                .to_string(),
            activity: agent.state.label().to_string(),
        }
    }
}

/// One row in the unified live-work panel. Payloads remain projections of
/// engine-owned work; this widget never creates task or process state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunningProjection {
    Shell {
        id: String,
        command: String,
        elapsed: String,
        last_output: String,
        port: Option<u16>,
        status: String,
    },
    Task {
        id: String,
        name: String,
        step: String,
        elapsed: String,
        status: String,
    },
}

/// Actions emitted by [`LiveSubagentsPanel`]; the host owns navigation and
/// changes which agent receives composer submissions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveSubagentsIntent {
    Open(AgentId),
    ReturnToParent,
    StopAgent(AgentId),
    ShowShellOutput(String),
    /// #126: attach the keyboard to a background shell's PTY (no id needed).
    AttachShell(String),
    StopShell(String),
    StopTask(String),
    ConfirmStopAgent(AgentId),
}

/// Compact keyboard-driven list of agents attached to the current turn.
#[derive(Debug, Clone, Default)]
pub struct LiveSubagentsPanel {
    agents: Vec<LiveAgentProjection>,
    running: Vec<RunningProjection>,
    selected: usize,
    focused: bool,
    intents: Vec<LiveSubagentsIntent>,
    pending_agent_stop: Option<AgentId>,
}

impl LiveSubagentsPanel {
    pub fn set_agents(&mut self, agents: Vec<LiveAgentProjection>) {
        self.agents = agents;
        self.selected = self.selected.min(self.row_count().saturating_sub(1));
    }

    pub fn set_running(&mut self, running: Vec<RunningProjection>) {
        self.running = running;
        self.selected = self.selected.min(self.row_count().saturating_sub(1));
    }

    pub fn set_tree(&mut self, root: &AgentNode) {
        fn collect(node: &AgentNode, rows: &mut Vec<LiveAgentProjection>) {
            for child in &node.children {
                if matches!(
                    child.state,
                    crate::nav::AgentState::Running | crate::nav::AgentState::WaitingApproval
                ) {
                    rows.push(LiveAgentProjection::from(child));
                }
                collect(child, rows);
            }
        }
        let mut agents = Vec::new();
        collect(root, &mut agents);
        self.set_agents(agents);
    }

    /// Rows the panel needs under the composer: none without subagents,
    /// else a titled rule plus up to three agent rows.
    pub fn rows(&self) -> u16 {
        if self.is_empty() {
            0
        } else {
            self.row_count().min(8) as u16 + 2
        }
    }

    fn row_count(&self) -> usize {
        self.agents.len() + self.running.len()
    }

    pub fn is_empty(&self) -> bool {
        self.row_count() == 0
    }

    fn stop_selected(&mut self) {
        if self.selected < self.agents.len() {
            self.pending_agent_stop = Some(self.agents[self.selected].id.clone());
            return;
        }
        match self
            .running
            .get(self.selected.saturating_sub(self.agents.len()))
        {
            Some(RunningProjection::Shell { id, .. }) => self
                .intents
                .push(LiveSubagentsIntent::StopShell(id.clone())),
            Some(RunningProjection::Task { id, .. }) => {
                self.intents.push(LiveSubagentsIntent::StopTask(id.clone()))
            }
            None => {}
        }
    }

    pub fn selected(&self) -> Option<&LiveAgentProjection> {
        self.agents.get(self.selected)
    }

    pub fn footer_summary(&self) -> Option<String> {
        let agents = self.agents.len();
        let shells = self
            .running
            .iter()
            .filter(|item| matches!(item, RunningProjection::Shell { .. }))
            .count();
        let tasks = self
            .running
            .iter()
            .filter(|item| matches!(item, RunningProjection::Task { .. }))
            .count();
        let mut parts = Vec::new();
        if agents > 0 {
            parts.push(format!(
                "{agents} agent{}",
                if agents == 1 { "" } else { "s" }
            ));
        }
        if shells > 0 {
            parts.push(format!(
                "{shells} shell{}",
                if shells == 1 { "" } else { "s" }
            ));
        }
        if tasks > 0 {
            parts.push(format!("{tasks} task{}", if tasks == 1 { "" } else { "s" }));
        }
        (!parts.is_empty()).then(|| format!("{} · ← / Ctrl+T", parts.join(" · ")))
    }

    pub fn drain_intents(&mut self) -> Vec<LiveSubagentsIntent> {
        std::mem::take(&mut self.intents)
    }
}

impl Component for LiveSubagentsPanel {
    fn render(&mut self, frame: &mut dyn crate::component::RenderFrame, area: Rect, theme: &Theme) {
        if self.is_empty() {
            return;
        }
        let mut items: Vec<ListItem> = self
            .agents
            .iter()
            .enumerate()
            .map(|(index, agent)| {
                let marker = if index == self.selected { "› " } else { "  " };
                let label = if agent.role.is_empty() {
                    &agent.name
                } else {
                    &agent.role
                };
                ListItem::new(format!(
                    "🤖 {marker}{label} · {} · {} · {} · {}",
                    agent.model, agent.activity, agent.elapsed, agent.status
                ))
            })
            .collect();
        for (index, row) in self.running.iter().enumerate() {
            let selected = self.agents.len() + index == self.selected;
            let marker = if selected { "› " } else { "  " };
            let label = match row {
                RunningProjection::Shell {
                    command,
                    elapsed,
                    last_output,
                    port,
                    status,
                    ..
                } => format!(
                    "⌁ {marker}{command} · {elapsed} · {status} · {}{}",
                    last_output,
                    port.map(|p| format!(" · :{p}")).unwrap_or_default()
                ),
                RunningProjection::Task {
                    name,
                    step,
                    elapsed,
                    status,
                    ..
                } => format!("✓ {marker}{name} · {step} · {elapsed} · {status}"),
            };
            items.push(ListItem::new(label));
        }
        let role = if self.focused {
            Role::Running
        } else {
            Role::Border
        };
        frame.render_widget(
            List::new(items).block(
                Block::default()
                    .borders(Borders::TOP)
                    .title(Span::styled(
                        if self.pending_agent_stop.is_some() {
                            "Stop agent? · y confirm · n/Esc cancel"
                        } else {
                            "Running · ↑↓ select · Enter open · a attach · k stop · Esc close"
                        },
                        Style::default().fg(theme.color(role)),
                    ))
                    .border_style(Style::default().fg(theme.color(role))),
            ),
            area,
        );
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        match event {
            Event::Focus(focused) => {
                self.focused = *focused;
                Outcome::Redraw
            }
            Event::Key(key)
                if self.focused && key.kind == crossterm::event::KeyEventKind::Press =>
            {
                if let Some(agent) = self.pending_agent_stop.take() {
                    if matches!(key.code, crossterm::event::KeyCode::Char('y' | 'Y')) {
                        self.intents
                            .push(LiveSubagentsIntent::ConfirmStopAgent(agent));
                    }
                    return Outcome::Redraw;
                }
                match key.code {
                    crossterm::event::KeyCode::Up => {
                        self.selected = self.selected.saturating_sub(1);
                        Outcome::Redraw
                    }
                    crossterm::event::KeyCode::Down => {
                        self.selected = (self.selected + 1).min(self.row_count().saturating_sub(1));
                        Outcome::Redraw
                    }
                    crossterm::event::KeyCode::Enter => {
                        if let Some(agent) = self
                            .selected()
                            .filter(|_| self.selected < self.agents.len())
                        {
                            self.intents
                                .push(LiveSubagentsIntent::Open(agent.id.clone()));
                            Outcome::Redraw
                        } else {
                            match self
                                .running
                                .get(self.selected.saturating_sub(self.agents.len()))
                            {
                                Some(RunningProjection::Shell { id, .. }) => {
                                    self.intents
                                        .push(LiveSubagentsIntent::ShowShellOutput(id.clone()));
                                    Outcome::Redraw
                                }
                                _ => Outcome::Ignored,
                            }
                        }
                    }
                    crossterm::event::KeyCode::Char('a') | crossterm::event::KeyCode::Char('A') => {
                        match self
                            .running
                            .get(self.selected.saturating_sub(self.agents.len()))
                        {
                            Some(RunningProjection::Shell { id, .. })
                                if self.selected >= self.agents.len() =>
                            {
                                self.intents
                                    .push(LiveSubagentsIntent::AttachShell(id.clone()));
                                Outcome::Redraw
                            }
                            _ => Outcome::Ignored,
                        }
                    }
                    crossterm::event::KeyCode::Char('k') | crossterm::event::KeyCode::Char('K') => {
                        self.stop_selected();
                        Outcome::Redraw
                    }
                    crossterm::event::KeyCode::Esc => {
                        self.intents.push(LiveSubagentsIntent::ReturnToParent);
                        Outcome::Redraw
                    }
                    _ => Outcome::Ignored,
                }
            }
            _ => Outcome::Ignored,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotProjection {
    pub id: String,
    pub name: String,
    pub model: String,
    pub state: String,
    pub next_run: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct BotsPanel {
    bots: Vec<BotProjection>,
    selected: usize,
    focused: bool,
}

impl BotsPanel {
    pub fn set_bots(&mut self, bots: Vec<BotProjection>) {
        self.bots = bots;
        self.selected = self.selected.min(self.bots.len().saturating_sub(1));
    }

    pub fn selected(&self) -> Option<&BotProjection> {
        self.bots.get(self.selected)
    }
}

impl Component for BotsPanel {
    fn render(&mut self, frame: &mut dyn crate::component::RenderFrame, area: Rect, theme: &Theme) {
        let items = if self.bots.is_empty() {
            vec![ListItem::new("No daemon Bot profiles.")]
        } else {
            self.bots
                .iter()
                .enumerate()
                .map(|(index, bot)| {
                    let marker = if index == self.selected { "> " } else { "  " };
                    let next = bot
                        .next_run
                        .as_deref()
                        .map(|value| format!(" · next {value}"))
                        .unwrap_or_default();
                    ListItem::new(Line::from(format!(
                        "{marker}{} [{}] {} · {}{next}",
                        bot.name, bot.model, bot.state, bot.id
                    )))
                })
                .collect()
        };
        let role = if self.focused {
            Role::Running
        } else {
            Role::Border
        };
        frame.render_widget(
            List::new(items).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled("bots", Style::default().fg(theme.color(role))))
                    .border_style(Style::default().fg(theme.color(role))),
            ),
            area,
        );
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        match event {
            Event::Focus(focused) => {
                self.focused = *focused;
                Outcome::Redraw
            }
            Event::Mouse(mouse) => match mouse.kind {
                crossterm::event::MouseEventKind::ScrollUp => {
                    self.selected = self.selected.saturating_sub(1);
                    Outcome::Redraw
                }
                crossterm::event::MouseEventKind::ScrollDown => {
                    if !self.bots.is_empty() {
                        self.selected = (self.selected + 1).min(self.bots.len() - 1);
                    }
                    Outcome::Redraw
                }
                _ => Outcome::Ignored,
            },
            _ => Outcome::Ignored,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_survives_a_daemon_profile_refresh() {
        let mut panel = BotsPanel::default();
        panel.set_bots(vec![BotProjection {
            id: "reviewer".into(),
            name: "Reviewer".into(),
            model: "openai/gpt-5.6".into(),
            state: "enabled".into(),
            next_run: Some("tomorrow".into()),
        }]);
        assert_eq!(panel.selected().unwrap().id, "reviewer");
    }
}
