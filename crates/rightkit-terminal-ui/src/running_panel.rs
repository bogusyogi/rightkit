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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_formats() {
        assert_eq!(format_elapsed(Duration::from_secs(5)), "5s");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "2m05s");
        assert_eq!(format_elapsed(Duration::from_secs(3720)), "1h02m");
    }

    #[test]
    fn chip_counts_by_kind_and_is_none_when_idle() {
        let t = Instant::now();
        let mut r = RunningRegistry::default();
        assert_eq!(r.chip(), None);
        r.start(WorkKind::Agent, "a1", "explore", t);
        r.start(WorkKind::Agent, "a2", "review", t);
        r.start(WorkKind::Shell, "s1", "cargo watch", t);
        assert_eq!(r.chip().as_deref(), Some("2 agents \u{b7} 1 shell"));
    }

    #[test]
    fn restart_with_same_id_does_not_duplicate() {
        let t = Instant::now();
        let mut r = RunningRegistry::default();
        r.start(WorkKind::Tool, "t", "grep", t);
        r.start(WorkKind::Tool, "t", "grep", t);
        assert_eq!(r.count(WorkKind::Tool), 1);
    }

    #[test]
    fn rows_show_elapsed() {
        let t = Instant::now();
        let mut r = RunningRegistry::default();
        r.start(WorkKind::Shell, "s", "sleep 90", t);
        let rows = r.rows(t + Duration::from_secs(61));
        assert_eq!(rows, vec!["\u{2301} sleep 90 \u{b7} 1m01s".to_string()]);
    }

    #[test]
    fn shell_exit_removes_and_records_status() {
        let t = Instant::now();
        let mut r = RunningRegistry::default();
        r.start(WorkKind::Shell, "s", "build", t);
        r.shell_exited("s", "exited (code 0)", t + Duration::from_secs(10));
        assert!(r.is_empty());
        let f = r.take_finished();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].status, "exited (code 0)");
        assert_eq!(f[0].ran_for, Duration::from_secs(10));
        assert!(r.take_finished().is_empty());
    }

    #[test]
    fn shell_exit_for_unknown_id_is_ignored() {
        let mut r = RunningRegistry::default();
        r.shell_exited("nope", "exited (code 1)", Instant::now());
        assert!(r.take_finished().is_empty());
    }

    #[test]
    fn finish_reports_presence() {
        let mut r = RunningRegistry::default();
        r.start(WorkKind::Tool, "t", "x", Instant::now());
        assert!(r.finish(WorkKind::Tool, "t"));
        assert!(!r.finish(WorkKind::Tool, "t"));
    }
}
