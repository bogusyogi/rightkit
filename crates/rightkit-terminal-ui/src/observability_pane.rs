//! A193 observability pane model: context fill, tokens, cost, cache hit
//! and tool latency. Presentation only; the host feeds it usage snapshots
//! and tool durations, and renders [`ObservabilityPane::lines`].

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct UsageSnapshot {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub cost_usd: f64,
    /// Tokens currently occupying the context window.
    pub context_used: u64,
    pub context_window: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolStat {
    pub calls: u32,
    pub total_ms: u64,
    pub max_ms: u64,
}

impl ToolStat {
    pub fn avg_ms(&self) -> u64 {
        if self.calls == 0 {
            0
        } else {
            self.total_ms / u64::from(self.calls)
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ObservabilityPane {
    usage: UsageSnapshot,
    tools: BTreeMap<String, ToolStat>,
}

impl ObservabilityPane {
    /// Replace the session totals with the latest snapshot.
    pub fn set_usage(&mut self, usage: UsageSnapshot) {
        self.usage = usage;
    }

    pub fn usage(&self) -> UsageSnapshot {
        self.usage
    }

    pub fn record_tool(&mut self, name: &str, duration_ms: u64) {
        let s = self.tools.entry(name.to_string()).or_default();
        s.calls += 1;
        s.total_ms += duration_ms;
        s.max_ms = s.max_ms.max(duration_ms);
    }

    pub fn tool_stat(&self, name: &str) -> Option<ToolStat> {
        self.tools.get(name).copied()
    }

    /// Context fill in whole percent; 0 when the window is unknown.
    pub fn context_pct(&self) -> u8 {
        let u = &self.usage;
        if u.context_window == 0 {
            return 0;
        }
        (u.context_used.saturating_mul(100) / u.context_window).min(100) as u8
    }

    /// Cached share of input tokens in whole percent.
    pub fn cache_hit_pct(&self) -> u8 {
        let u = &self.usage;
        if u.input_tokens == 0 {
            return 0;
        }
        (u.cached_input_tokens.saturating_mul(100) / u.input_tokens).min(100) as u8
    }

    pub fn lines(&self) -> Vec<Line<'static>> {
        let u = &self.usage;
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let mut out = vec![Line::from(Span::styled("Observability", bold))];
        let ctx = if u.context_window == 0 {
            "context: unknown".to_string()
        } else {
            format!(
                "context: {}% ({}/{} tokens)",
                self.context_pct(),
                u.context_used,
                u.context_window
            )
        };
        out.push(Line::from(ctx));
        out.push(Line::from(format!(
            "tokens: {} in, {} out, cache hit {}%",
            u.input_tokens,
            u.output_tokens,
            self.cache_hit_pct()
        )));
        out.push(Line::from(format!("cost: ${:.4}", u.cost_usd)));
        if !self.tools.is_empty() {
            out.push(Line::from(Span::styled("tools (avg / max ms):", bold)));
            for (name, s) in &self.tools {
                out.push(Line::from(format!(
                    "  {name}: {} calls, {} / {}",
                    s.calls,
                    s.avg_ms(),
                    s.max_ms
                )));
            }
        }
        out
    }
}
