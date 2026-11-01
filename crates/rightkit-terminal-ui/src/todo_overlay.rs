//! Todo / plan overlay model.
//!
//! The list is rebuilt from durable sources, never from transcript rows that
//! compaction may have removed: (1) `todo_write` call arguments recorded as
//! durable events, or (2) the checklist text carried by a compaction summary
//! (`Todos (n/m done):` followed by `[x]`, `[~]`, `[ ]` lines). Presentation
//! only; no engine type is constructed here.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    pub content: String,
    /// Present-continuous label shown while in progress.
    pub active_form: Option<String>,
    pub status: TodoStatus,
}

impl TodoItem {
    fn label(&self) -> &str {
        match (self.status, &self.active_form) {
            (TodoStatus::InProgress, Some(a)) if !a.trim().is_empty() => a,
            _ => &self.content,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TodoOverlay {
    items: Vec<TodoItem>,
}

impl TodoOverlay {
    pub fn items(&self) -> &[TodoItem] {
        &self.items
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn done_count(&self) -> usize {
        self.items
            .iter()
            .filter(|i| i.status == TodoStatus::Completed)
            .count()
    }

    /// Replace the list from one `todo_write` argument object (full-replace
    /// semantics). Invalid or blank items are skipped; an empty result keeps
    /// the existing list.
    pub fn apply_todo_write(&mut self, args: &serde_json::Value) {
        let Some(todos) = args.get("todos").and_then(serde_json::Value::as_array) else {
            return;
        };
        let items: Vec<TodoItem> = todos
            .iter()
            .filter_map(|t| {
                let content = t.get("content")?.as_str()?.trim();
                if content.is_empty() {
                    return None;
                }
                let status = match t.get("status").and_then(serde_json::Value::as_str) {
                    Some("completed") => TodoStatus::Completed,
                    Some("in_progress") => TodoStatus::InProgress,
                    _ => TodoStatus::Pending,
                };
                let active_form = t
                    .get("activeForm")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                Some(TodoItem {
                    content: content.to_string(),
                    active_form,
                    status,
                })
            })
            .collect();
        if !items.is_empty() {
            self.items = items;
        }
    }

    /// Rebuild from durable `todo_write` arguments in order; newest wins.
    pub fn rebuild_from_durable<'a>(
        calls: impl IntoIterator<Item = &'a serde_json::Value>,
    ) -> Self {
        let mut overlay = Self::default();
        for args in calls {
            overlay.apply_todo_write(args);
        }
        overlay
    }

    /// Rebuild from compaction-summary text containing the rendered checklist.
    /// Returns an empty overlay when no checklist lines are present.
    pub fn from_summary_text(text: &str) -> Self {
        let items = text
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let (status, rest) = if let Some(r) = line.strip_prefix("[x]") {
                    (TodoStatus::Completed, r)
                } else if let Some(r) = line.strip_prefix("[~]") {
                    (TodoStatus::InProgress, r)
                } else {
                    let r = line.strip_prefix("[ ]")?;
                    (TodoStatus::Pending, r)
                };
                let content = rest.trim();
                (!content.is_empty()).then(|| TodoItem {
                    content: content.to_string(),
                    active_form: None,
                    status,
                })
            })
            .collect();
        Self { items }
    }

    /// Prefer durable events; fall back to the compaction summary.
    pub fn rebuild<'a>(
        durable_calls: impl IntoIterator<Item = &'a serde_json::Value>,
        summary: Option<&str>,
    ) -> Self {
        let from_events = Self::rebuild_from_durable(durable_calls);
        if !from_events.is_empty() {
            return from_events;
        }
        summary.map(Self::from_summary_text).unwrap_or_default()
    }

    /// Overlay rows: a `Plan n/m done` header then one row per step.
    pub fn lines(&self) -> Vec<Line<'static>> {
        if self.items.is_empty() {
            return Vec::new();
        }
        let mut out = vec![Line::from(Span::styled(
            format!("Plan {}/{} done", self.done_count(), self.items.len()),
            Style::default().add_modifier(Modifier::BOLD),
        ))];
        for item in &self.items {
            let (mark, style) = match item.status {
                TodoStatus::Completed => ("[x]", Style::default().add_modifier(Modifier::DIM)),
                TodoStatus::InProgress => ("[~]", Style::default().add_modifier(Modifier::BOLD)),
                TodoStatus::Pending => ("[ ]", Style::default()),
            };
            out.push(Line::from(Span::styled(
                format!("{mark} {}", item.label()),
                style,
            )));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn plan_steps_render_with_progress_header() {
        let call = json!({"todos": [
            {"content": "Read", "status": "completed"},
            {"content": "Edit", "status": "in_progress", "activeForm": "Editing"},
            {"content": "Test", "status": "pending"}
        ]});
        let o = TodoOverlay::rebuild_from_durable([&call]);
        assert_eq!(
            text(&o.lines()),
            vec!["Plan 1/3 done", "[x] Read", "[~] Editing", "[ ] Test"]
        );
    }

    #[test]
    fn newest_durable_call_wins_and_empty_call_keeps_list() {
        let a = json!({"todos": [{"content": "A", "status": "pending"}]});
        let b = json!({"todos": [{"content": "B", "status": "completed"}]});
        let bad = json!({"todos": []});
        let o = TodoOverlay::rebuild_from_durable([&a, &b, &bad]);
        assert_eq!(o.items().len(), 1);
        assert_eq!(o.items()[0].content, "B");
    }

    #[test]
    fn rebuilds_from_compaction_summary_when_no_durable_events() {
        let summary = "Summary...\nTodos (1/3 done):\n[x] Read\n[~] Editing\n[ ] Test\nmore";
        let o = TodoOverlay::rebuild(std::iter::empty(), Some(summary));
        assert_eq!(o.items().len(), 3);
        assert_eq!(o.items()[1].status, TodoStatus::InProgress);
        assert_eq!(o.done_count(), 1);
    }

    #[test]
    fn durable_events_beat_summary_and_nothing_is_empty() {
        let a = json!({"todos": [{"content": "Live", "status": "pending"}]});
        let o = TodoOverlay::rebuild([&a], Some("[x] Stale"));
        assert_eq!(o.items()[0].content, "Live");
        assert!(
            TodoOverlay::rebuild(std::iter::empty(), None)
                .lines()
                .is_empty()
        );
    }
}
