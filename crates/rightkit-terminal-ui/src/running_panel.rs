//! A190 running-work registry: active tools, subagents and background shells
//! with elapsed time, plus the per-kind footer chip.
//!
//! Pure model. The host feeds lifecycle events (including the durable
//! `backgroundShellExited` event) and a monotonic clock; nothing here owns
//! engine state. Rendering stays with `bots_panel::LiveSubagentsPanel`; this
//! module produces the rows it displays.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkKind {
    Tool,
    Agent,
    Shell,
}

impl WorkKind {
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Tool => "\u{2699}",
            Self::Agent => "\u{25c6}",
            Self::Shell => "\u{2301}",
        }
    }

    fn noun(self, n: usize) -> &'static str {
        match (self, n == 1) {
            (Self::Tool, true) => "tool",
            (Self::Tool, false) => "tools",
            (Self::Agent, true) => "agent",
            (Self::Agent, false) => "agents",
            (Self::Shell, true) => "shell",
            (Self::Shell, false) => "shells",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkItem {
    pub kind: WorkKind,
    pub id: String,
    pub label: String,
    pub started: Instant,
}

/// Finished shell record kept briefly so the exit is visible, not silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishedShell {
    pub id: String,
    pub label: String,
    pub status: String,
    pub ran_for: Duration,
}

#[derive(Debug, Default)]
pub struct RunningRegistry {
    items: Vec<WorkItem>,
    finished: Vec<FinishedShell>,
}

impl RunningRegistry {
    pub fn start(&mut self, kind: WorkKind, id: &str, label: &str, now: Instant) {
        self.items.retain(|i| !(i.kind == kind && i.id == id));
        self.items.push(WorkItem {
            kind,
            id: id.into(),
            label: label.into(),
            started: now,
        });
    }

    /// Remove an item; returns whether it was present.
    pub fn finish(&mut self, kind: WorkKind, id: &str) -> bool {
        let before = self.items.len();
        self.items.retain(|i| !(i.kind == kind && i.id == id));
        before != self.items.len()
    }

    /// `backgroundShellExited`: drop the shell and remember its typed exit label.
    pub fn shell_exited(&mut self, id: &str, status_label: &str, now: Instant) {
        if let Some(pos) = self
            .items
            .iter()
            .position(|i| i.kind == WorkKind::Shell && i.id == id)
        {
            let item = self.items.remove(pos);
            self.finished.push(FinishedShell {
                id: item.id,
                label: item.label,
                status: status_label.into(),
                ran_for: now.saturating_duration_since(item.started),
            });
            if self.finished.len() > 8 {
                self.finished.remove(0);
            }
        }
    }

    pub fn take_finished(&mut self) -> Vec<FinishedShell> {
        std::mem::take(&mut self.finished)
    }

    pub fn count(&self, kind: WorkKind) -> usize {
        self.items.iter().filter(|i| i.kind == kind).count()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn items(&self) -> &[WorkItem] {
        &self.items
    }

    /// Footer chip by kind, e.g. `2 agents · 1 shell`. None when idle.
    pub fn chip(&self) -> Option<String> {
        let parts: Vec<String> = [WorkKind::Tool, WorkKind::Agent, WorkKind::Shell]
            .into_iter()
            .filter_map(|k| {
                let n = self.count(k);
                (n > 0).then(|| format!("{n} {}", k.noun(n)))
            })
            .collect();
        (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
    }

    /// One display row per item, oldest first.
    pub fn rows(&self, now: Instant) -> Vec<String> {
        self.items
            .iter()
            .map(|i| {
                format!(
                    "{} {} \u{b7} {}",
                    i.kind.glyph(),
                    i.label,
                    format_elapsed(now.saturating_duration_since(i.started))
                )
            })
            .collect()
    }
}

pub fn format_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}
