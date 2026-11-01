//! Read-only projection of one daemon-owned browser workspace.
//!
//! This owns no CDP/browser state. A daemon client hydrates this small snapshot
//! from the existing workspace status contract (`get
//! /sessions/{id}/browser/workspace`), preserving one browser truth across
//! CLI, desktop, & terminal surfaces. The lease strip mirrors the desktop
//! strip: `controlLease.owner` distinguishes human control from agent control,
//! and the generation is the queued-action invalidation counter the agent act
//! path checks before every commit (A15f).

use crate::component::RenderFrameExt as _;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::component::{Component, Event, Outcome, Role, Theme};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BrowserWorkspaceProjection {
    pub session_id: String,
    pub mode: String,
    pub url: String,
    pub control_owner: String,
    /// `controlLease.generation` from the status contract; `0` means the
    /// hydrating client had no lease state yet.
    pub lease_generation: u64,
    pub title: String,
    pub console_error_count: usize,
    pub paused: bool,
    pub latest_action: String,
}

impl BrowserWorkspaceProjection {
    /// Human-controlled sessions carry the daemon's `workspace-user` lease
    /// owner; any other (or empty) owner means the runtime holds control.
    pub fn human_controlled(&self) -> bool {
        self.control_owner == "workspace-user"
    }
}

#[derive(Debug, Clone, Default)]
pub struct BrowserPanel {
    snapshot: Option<BrowserWorkspaceProjection>,
    focused: bool,
}

impl BrowserPanel {
    pub fn set_snapshot(&mut self, snapshot: BrowserWorkspaceProjection) {
        self.snapshot = Some(snapshot);
    }

    pub fn snapshot(&self) -> Option<&BrowserWorkspaceProjection> {
        self.snapshot.as_ref()
    }

    /// The lease strip lines — pure so tests can assert the operator-facing
    /// projection without a renderer.
    fn summary_lines(snapshot: &BrowserWorkspaceProjection) -> Vec<String> {
        let mut lines = vec![
            format!("{} · {}", snapshot.mode, snapshot.session_id),
            format!("url: {}", snapshot.url),
        ];
        if !snapshot.title.is_empty() {
            lines.push(format!("title: {}", snapshot.title));
        }
        let owner = if snapshot.human_controlled() {
            format!("{} (human)", snapshot.control_owner)
        } else {
            "agent".to_string()
        };
        if snapshot.lease_generation > 0 {
            lines.push(format!(
                "control: {} · lease gen {}",
                owner, snapshot.lease_generation
            ));
        } else {
            lines.push(format!("control: {owner}"));
        }
        if snapshot.console_error_count > 0 {
            lines.push(format!(
                "errors: {} console error{}",
                snapshot.console_error_count,
                if snapshot.console_error_count == 1 {
                    ""
                } else {
                    "s"
                }
            ));
        }
        lines.push(format!("latest: {}", snapshot.latest_action));
        lines
    }
}

impl Component for BrowserPanel {
    fn render(&mut self, frame: &mut dyn crate::component::RenderFrame, area: Rect, theme: &Theme) {
        let (title, lines) = match &self.snapshot {
            Some(snapshot) => {
                let state = if snapshot.paused { "paused" } else { "active" };
                (
                    format!("browser · {state}"),
                    BrowserPanel::summary_lines(snapshot),
                )
            }
            None => (
                "browser".to_string(),
                vec!["No daemon browser workspace is selected.".to_string()],
            ),
        };
        let role = if self.focused {
            Role::Running
        } else {
            Role::Border
        };
        let text = lines.into_iter().map(Line::from).collect::<Vec<Line>>();
        frame.render_widget(
            Paragraph::new(text).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(Span::styled(title, Style::default().fg(theme.color(role))))
                    .border_style(Style::default().fg(theme.color(role))),
            ),
            area,
        );
    }

    fn handle(&mut self, event: &Event) -> Outcome {
        if let Event::Focus(focused) = event {
            self.focused = *focused;
            return Outcome::Redraw;
        }
        Outcome::Ignored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn human_snapshot() -> BrowserWorkspaceProjection {
        BrowserWorkspaceProjection {
            session_id: "browser-1".into(),
            mode: "visible".into(),
            url: "https://example.test".into(),
            control_owner: "workspace-user".into(),
            lease_generation: 7,
            title: "Example".into(),
            console_error_count: 2,
            paused: true,
            latest_action: "navigate".into(),
        }
    }

    #[test]
    fn projection_keeps_daemon_session_and_control_owner() {
        let mut panel = BrowserPanel::default();
        panel.set_snapshot(BrowserWorkspaceProjection {
            session_id: "browser-1".into(),
            mode: "headless".into(),
            url: "https://example.test".into(),
            control_owner: "agent".into(),
            latest_action: "navigate".into(),
            paused: false,
            ..BrowserWorkspaceProjection::default()
        });
        let snapshot = panel.snapshot().unwrap();
        assert_eq!(snapshot.session_id, "browser-1");
        assert_eq!(snapshot.control_owner, "agent");
        assert!(!snapshot.human_controlled());
    }

    #[test]
    fn lease_strip_reports_human_control_generation_and_errors() {
        let lines = BrowserPanel::summary_lines(&human_snapshot());
        assert!(lines
            .iter()
            .any(|line| line.contains("workspace-user (human)") && line.contains("lease gen 7")));
        assert!(lines.iter().any(|line| line == "errors: 2 console errors"));
        assert!(lines.iter().any(|line| line.contains("title: Example")));
    }

    #[test]
    fn agent_control_without_lease_owner_renders_agent_label() {
        let mut snapshot = human_snapshot();
        snapshot.control_owner.clear();
        snapshot.lease_generation = 0;
        snapshot.console_error_count = 0;
        snapshot.title.clear();
        let lines = BrowserPanel::summary_lines(&snapshot);
        assert!(lines.iter().any(|line| line == "control: agent"));
        assert!(!lines.iter().any(|line| line.contains("lease gen")));
        assert!(!lines.iter().any(|line| line.starts_with("errors:")));
        assert!(!lines.iter().any(|line| line.starts_with("title:")));
    }
}
