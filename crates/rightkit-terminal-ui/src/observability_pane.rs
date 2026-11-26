//! A193 observability pane model: context fill, tokens, cost, cache hit
//! and tool latency. Presentation only; the host feeds it usage snapshots
//! and tool durations, and renders [`ObservabilityPane::lines`].

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use std::collections::BTreeMap;
use std::time::Instant;

/// The rate-limit budget the provider reported on the headers of its latest response (A193-1).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderRateLimit {
    pub remaining_requests: Option<u64>,
    pub remaining_tokens: Option<u64>,
    /// Seconds from the response until the request window resets.
    pub reset_requests_secs: Option<u64>,
    /// Seconds from the response until the token window resets.
    pub reset_tokens_secs: Option<u64>,
}

impl ProviderRateLimit {
    /// The nearer of the two resets, in seconds from the response.
    pub fn nearest_reset_secs(&self) -> Option<u64> {
        match (self.reset_requests_secs, self.reset_tokens_secs) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

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
    /// The latest provider-reported budget and when it was observed.
    rate_limit: Option<(ProviderRateLimit, Instant)>,
    /// Milliseconds the prompt cache lifetime had left when it was last observed, and when.
    cache_ttl: Option<(u64, Instant)>,
    /// The cause and size of the latest unexpected prompt-cache miss.
    last_cache_miss: Option<(String, u64)>,
}

/// `(reason, tokens)` from the transcript notice the host prints for an unexpected cache miss,
/// `Prompt cache miss (serverSide): 4000 tokens re-billed, ...`.
pub fn parse_cache_miss_notice(text: &str) -> Option<(String, u64)> {
    let rest = text.strip_prefix("Prompt cache miss (")?;
    let (reason, rest) = rest.split_once("): ")?;
    let tokens = rest
        .split_once(" tokens re-billed")?
        .0
        .trim()
        .parse()
        .ok()?;
    Some((reason.to_string(), tokens))
}

impl ObservabilityPane {
    /// Replace the session totals with the latest snapshot.
    pub fn set_usage(&mut self, usage: UsageSnapshot) {
        self.usage = usage;
    }

    pub fn usage(&self) -> UsageSnapshot {
        self.usage
    }

    /// Keep the budget the provider reported on its latest response.
    pub fn set_rate_limit(&mut self, limit: ProviderRateLimit) {
        self.rate_limit = Some((limit, Instant::now()));
    }

    /// Keep the prompt cache lifetime left (ms) as of now; it counts down from here.
    pub fn set_cache_ttl_ms(&mut self, ms: u64) {
        self.cache_ttl = Some((ms, Instant::now()));
    }

    /// `rate limit: 59 requests, 9000 tokens left, resets in 12 s`, or None before any response reported one.
    pub fn rate_limit_line(&self) -> Option<String> {
        let (limit, at) = self.rate_limit.as_ref()?;
        let plural =
            |n: u64, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut parts = match (limit.remaining_requests, limit.remaining_tokens) {
            (Some(r), Some(t)) => format!(
                "{}, {} left",
                plural(r, "request", "requests"),
                plural(t, "token", "tokens")
            ),
            (Some(r), None) => format!("{} left", plural(r, "request", "requests")),
            (None, Some(t)) => format!("{} left", plural(t, "token", "tokens")),
            (None, None) => "reported".to_string(),
        };
        if let Some(reset) = limit.nearest_reset_secs() {
            let left = reset.saturating_sub(at.elapsed().as_secs());
            parts.push_str(&format!(", resets in {left} s"));
        }
        Some(format!("rate limit: {parts}"))
    }

    /// Keep the cause of the latest unexpected cache miss for the `last cache miss` row.
    pub fn set_last_cache_miss(&mut self, reason: String, tokens: u64) {
        self.last_cache_miss = Some((reason, tokens));
    }

    /// `cache expires in 287 s` (counting down) or `cache expired`; None when no lifetime applies.
    pub fn cache_ttl_line(&self) -> Option<String> {
        let (ms, at) = self.cache_ttl.as_ref()?;
        let left = ms.saturating_sub(at.elapsed().as_millis() as u64);
        Some(if left == 0 {
            "cache expired".to_string()
        } else {
            format!("cache expires in {} s", left.div_ceil(1000))
        })
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
        if let Some(line) = self.cache_ttl_line() {
            out.push(Line::from(line));
        }
        if let Some(line) = self.rate_limit_line() {
            out.push(Line::from(line));
        }
        if let Some((reason, tokens)) = &self.last_cache_miss {
            out.push(Line::from(format!(
                "last cache miss: {reason}, {tokens} tokens re-billed"
            )));
        }
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
