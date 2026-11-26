//! The cast tree: who is doing the work, and on which model.
//!
//! ```text
//!   ◆   main     gpt-6.1-sol    working
//!   ├ ✓ reader   haiku-5.5      done                              9s
//!   └ · coder    sonnet-5.5     working                          42s
//! ```
//!
//! Drawn only when the session has sub-agents on a model other than the main
//! one, or more than one sub-agent running; a single-model session keeps the
//! flat ledger. Everything shown comes from the agent tree the engine already
//! reports (role, model, state, elapsed): there is no per-child cost or
//! current-deed field in that tree, so those columns are not drawn.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::component::{Role, Theme};
use crate::nav::{AgentNode, AgentState};
use crate::transcript::wrap::display_width;

const INDENT: &str = "  ";

fn model_known(model: &str) -> bool {
    !model.trim().is_empty() && model != "-"
}

fn descendants<'a>(node: &'a AgentNode, out: &mut Vec<&'a AgentNode>) {
    for child in &node.children {
        out.push(child);
        descendants(child, out);
    }
}

/// How many distinct models the main agent and its sub-agents use.
pub fn distinct_models(root: &AgentNode) -> usize {
    let mut all = Vec::new();
    descendants(root, &mut all);
    let mut models: Vec<&str> = Vec::new();
    for node in std::iter::once(root).chain(all) {
        if model_known(&node.model) && !models.contains(&node.model.as_str()) {
            models.push(node.model.as_str());
        }
    }
    models.len()
}

/// Whether the tree earns a screen: a sub-agent on another model than the main
/// one, or more than one sub-agent running at once.
pub fn cast_wanted(root: &AgentNode) -> bool {
    if root.children.is_empty() {
        return false;
    }
    let mut all = Vec::new();
    descendants(root, &mut all);
    let other_model = all
        .iter()
        .any(|n| model_known(&n.model) && n.model != root.model);
    let running = all
        .iter()
        .filter(|n| matches!(n.state, AgentState::Running))
        .count();
    other_model || running > 1
}

/// The sub-agent that is acting now (the first one running), when the tree is
/// drawn: the working line names its role and model.
pub fn acting_child(root: &AgentNode) -> Option<&AgentNode> {
    if !cast_wanted(root) {
        return None;
    }
    let mut all = Vec::new();
    descendants(root, &mut all);
    all.into_iter()
        .find(|n| matches!(n.state, AgentState::Running))
}

fn clip(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = display_width(&ch.to_string());
        if used + w + 1 > width {
            break;
        }
        out.push(ch);
        used += w;
    }
    out.push('…');
    out
}

fn pad(text: &str, width: usize) -> String {
    let used = display_width(text);
    format!("{text}{}", " ".repeat(width.saturating_sub(used)))
}

/// The cast tree as transcript rows: a trunk line for the main agent, then one
/// line per direct sub-agent with `├`/`└` connectors. Empty unless
/// [`cast_wanted`].
pub fn cast_tree_lines(root: &AgentNode, width: u16, theme: &Theme) -> Vec<Line<'static>> {
    if !cast_wanted(root) {
        return Vec::new();
    }
    let width = usize::from(width).max(24);
    let dim = Style::default().fg(theme.color(Role::Dim));
    let text = Style::default().fg(theme.color(Role::Text));
    let main_role = if root.name.trim().is_empty() || root.name == "root agent" {
        "main".to_string()
    } else {
        root.name.clone()
    };
    let role_w = std::iter::once(main_role.as_str())
        .chain(root.children.iter().map(|c| c.name.as_str()))
        .map(display_width)
        .max()
        .unwrap_or(4)
        .clamp(6, 12);
    let model_w = std::iter::once(root.model.as_str())
        .chain(root.children.iter().map(|c| c.model.as_str()))
        .map(display_width)
        .max()
        .unwrap_or(8)
        .clamp(8, 20);
    // `  ├ ✓ ` is the widest prefix; the trunk pads to the same column.
    let prefix_w = display_width(INDENT) + 4;

    let mut lines = Vec::new();
    let trunk_doing = match root.state {
        AgentState::Running | AgentState::WaitingApproval => "working",
        _ => "idle",
    };
    lines.push(Line::from(vec![
        Span::raw(INDENT),
        Span::styled(
            "◆".to_string(),
            Style::default()
                .fg(theme.accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(pad(&clip(&main_role, role_w), role_w), text),
        Span::raw(" "),
        Span::styled(pad(&clip(&root.model, model_w), model_w), dim),
        Span::raw(" "),
        Span::styled(trunk_doing.to_string(), dim),
    ]));

    let last = root.children.len().saturating_sub(1);
    for (index, child) in root.children.iter().enumerate() {
        let connector = if index == last { "└" } else { "├" };
        let (glyph, role, doing) = match child.state {
            AgentState::Running => ("·", Role::Running, "working"),
            AgentState::WaitingApproval => ("!", Role::Attention, "needs you"),
            AgentState::Done => ("✓", Role::Success, "done"),
            AgentState::Stopped => ("✗", Role::Failure, "stopped"),
        };
        let elapsed = child
            .elapsed_cost
            .split('·')
            .next()
            .unwrap_or_default()
            .trim()
            .to_string();
        let model = if model_known(&child.model) {
            child.model.as_str()
        } else {
            ""
        };
        let left_w = prefix_w + role_w + 1 + model_w + 1 + display_width(doing);
        let gap = width
            .saturating_sub(left_w + display_width(&elapsed) + 1)
            .max(2);
        let mut spans = vec![
            Span::styled(format!("{INDENT}{connector} "), dim),
            Span::styled(glyph.to_string(), Style::default().fg(theme.color(role))),
            Span::raw(" "),
            Span::styled(pad(&clip(&child.name, role_w), role_w), text),
            Span::raw(" "),
            Span::styled(pad(&clip(model, model_w), model_w), dim),
            Span::raw(" "),
            Span::styled(doing.to_string(), Style::default().fg(theme.color(role))),
        ];
        if !elapsed.is_empty() {
            spans.push(Span::raw(" ".repeat(gap)));
            spans.push(Span::styled(elapsed, dim));
        }
        lines.push(Line::from(spans));
    }
    lines
}
