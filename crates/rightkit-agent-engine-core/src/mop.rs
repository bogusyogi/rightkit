//! Meltdown-Onset-Point detection — spot behavioral collapse on long tasks.
//! An agent that loops — repeating the
//! same action over and over with no progress — has hit a MOP and should have
//! its context reset rather than be allowed to burn the budget.
//!
//! Detection is a sliding window over action signatures: if any signature
//! recurs at least `repeat_threshold` times within the last `window_size`
//! actions, or the window's distinct-signature ratio collapses, a MOP fires.

use crate::cache::fnv1a;
use rightkit_agent_engine_protocol::ToolCall;
use std::collections::{HashMap, VecDeque};

/// Sliding-window meltdown detector.
#[derive(Debug, Clone)]
pub struct MopDetector {
    window: VecDeque<u64>,
    counts: HashMap<u64, usize>,
    window_size: usize,
    repeat_threshold: usize,
    min_distinct_ratio: f64,
}

impl MopDetector {
    pub fn new(window_size: usize, repeat_threshold: usize) -> Self {
        Self {
            window: VecDeque::with_capacity(window_size),
            counts: HashMap::new(),
            window_size: window_size.max(1),
            repeat_threshold: repeat_threshold.max(2),
            min_distinct_ratio: 0.34,
        }
    }

    /// Record one action signature; returns `true` if a MOP is now detected.
    pub fn record(&mut self, signature: u64) -> bool {
        self.window.push_back(signature);
        *self.counts.entry(signature).or_insert(0) += 1;
        if self.window.len() > self.window_size {
            if let Some(old) = self.window.pop_front() {
                if let Some(c) = self.counts.get_mut(&old) {
                    *c -= 1;
                    if *c == 0 {
                        self.counts.remove(&old);
                    }
                }
            }
        }
        self.is_melted()
    }

    fn is_melted(&self) -> bool {
        // Any single action recurring enough is a meltdown regardless of window
        // fill — a tight loop should be caught early.
        if self.counts.values().any(|&c| c >= self.repeat_threshold) {
            return true;
        }
        // The distinct-ratio collapse rule only applies once the window is full.
        if self.window.len() < self.window_size {
            return false;
        }
        let distinct = self.counts.len() as f64;
        distinct / self.window.len() as f64 <= self.min_distinct_ratio
    }

    /// Clear the window after a context reset so detection starts fresh.
    pub fn reset(&mut self) {
        self.window.clear();
        self.counts.clear();
    }
}

/// Hash an action (e.g. a tool name + argument digest) into a signature.
/// Shares the one FNV-1a implementation (`crate::cache::fnv1a`); the constants are
/// identical, so meltdown signatures are unchanged.
pub fn signature(action: &str) -> u64 {
    fnv1a(action)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopSignal {
    Warn(String),
    HardStop(String),
}

#[derive(Debug, Clone)]
pub struct ToolLoopDetector {
    shapes: VecDeque<u64>,
    shape_counts: HashMap<u64, usize>,
    tool_names: VecDeque<String>,
    tool_name_counts: HashMap<String, usize>,
    window_size: usize,
    exact_warn_threshold: usize,
    exact_stop_threshold: usize,
    kind_warn_threshold: usize,
    kind_stop_threshold: usize,
    warned_exact: bool,
    warned_kind: bool,
}

impl Default for ToolLoopDetector {
    fn default() -> Self {
        Self {
            shapes: VecDeque::with_capacity(40),
            shape_counts: HashMap::new(),
            tool_names: VecDeque::with_capacity(40),
            tool_name_counts: HashMap::new(),
            window_size: 40,
            exact_warn_threshold: 3,
            exact_stop_threshold: 5,
            kind_warn_threshold: 30,
            kind_stop_threshold: 40,
            warned_exact: false,
            warned_kind: false,
        }
    }
}

impl ToolLoopDetector {
    /// Read-only view of the per-shape sliding-window counter. Exposed
    /// so the sibling idempotency axis (parity P-3) can re-use the
    /// same shape counts without duplicating bookkeeping.
    pub fn shape_counts_view(&self) -> &HashMap<u64, usize> {
        &self.shape_counts
    }

    pub fn record(&mut self, calls: &[ToolCall]) -> Option<LoopSignal> {
        for call in calls {
            self.record_shape(signature(&tool_call_shape(call)));
            self.record_tool_name(call.name.clone());
        }

        let max_shape = self.shape_counts.values().copied().max().unwrap_or(0);
        let max_kind = self.tool_name_counts.values().copied().max().unwrap_or(0);

        if max_shape >= self.exact_stop_threshold {
            return Some(LoopSignal::HardStop(format!(
                "exact repeated tool-call shape reached {max_shape} occurrences in the recent window"
            )));
        }
        if max_kind >= self.kind_stop_threshold {
            let tool_name = self
                .tool_name_counts
                .iter()
                .max_by_key(|(_, count)| *count)
                .map(|(name, _)| name.as_str())
                .unwrap_or("tool");
            return Some(LoopSignal::HardStop(format!(
                "tool type '{tool_name}' reached {max_kind} calls in the recent window"
            )));
        }
        if max_shape >= self.exact_warn_threshold && !self.warned_exact {
            self.warned_exact = true;
            return Some(LoopSignal::Warn(format!(
                "exact repeated tool-call shape reached {max_shape} occurrences; vary approach or summarize progress before continuing"
            )));
        }
        if max_kind >= self.kind_warn_threshold && !self.warned_kind {
            self.warned_kind = true;
            let tool_name = self
                .tool_name_counts
                .iter()
                .max_by_key(|(_, count)| *count)
                .map(|(name, _)| name.as_str())
                .unwrap_or("tool");
            return Some(LoopSignal::Warn(format!(
                "tool type '{tool_name}' is dominating the recent window with {max_kind} calls; pause and decide whether the repeated reads/actions are still useful"
            )));
        }
        None
    }

    fn record_shape(&mut self, shape: u64) {
        self.shapes.push_back(shape);
        *self.shape_counts.entry(shape).or_insert(0) += 1;
        while self.shapes.len() > self.window_size {
            if let Some(old) = self.shapes.pop_front() {
                decrement_count(&mut self.shape_counts, &old);
            }
        }
    }

    fn record_tool_name(&mut self, name: String) {
        self.tool_names.push_back(name.clone());
        *self.tool_name_counts.entry(name).or_insert(0) += 1;
        while self.tool_names.len() > self.window_size {
            if let Some(old) = self.tool_names.pop_front() {
                decrement_count(&mut self.tool_name_counts, &old);
            }
        }
    }
}

fn decrement_count<K: Eq + std::hash::Hash + Clone>(counts: &mut HashMap<K, usize>, key: &K) {
    if let Some(count) = counts.get_mut(key) {
        *count -= 1;
        if *count == 0 {
            counts.remove(key);
        }
    }
}

pub(crate) fn tool_call_shape(call: &ToolCall) -> String {
    format!(
        "{}:{}",
        call.name,
        canonical_json_shape(&call.arguments_json)
    )
}

fn canonical_json_shape(raw: &str) -> String {
    serde_json::from_str::<serde_json::Value>(raw)
        .map(|value| canonical_value_shape(&value))
        .unwrap_or_else(|_| raw.trim().to_string())
}

fn canonical_value_shape(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries = map
                .iter()
                .map(|(key, value)| format!("{key}:{}", canonical_value_shape(value)))
                .collect::<Vec<_>>();
            entries.sort();
            format!("{{{}}}", entries.join(","))
        }
        serde_json::Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(canonical_value_shape)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_action_triggers_mop() {
        let mut d = MopDetector::new(5, 3);
        let s = signature("write_file:foo.rs");
        assert!(!d.record(s));
        assert!(!d.record(signature("read_file:bar")));
        assert!(!d.record(s));
        // Third occurrence of `s` within the window → meltdown.
        assert!(d.record(s));
    }

    #[test]
    fn varied_actions_do_not_trigger() {
        let mut d = MopDetector::new(6, 3);
        let mut melted = false;
        for i in 0..12 {
            melted |= d.record(signature(&format!("action-{i}")));
        }
        assert!(!melted, "all-distinct actions should never meltdown");
    }

    #[test]
    fn low_distinct_ratio_triggers() {
        // Window of 6 alternating between two actions → distinct ratio 2/6 ≈ 0.33.
        let mut d = MopDetector::new(6, 100); // disable the count rule
        let a = signature("a");
        let b = signature("b");
        let seq = [a, b, a, b, a, b];
        let mut melted = false;
        for s in seq {
            melted |= d.record(s);
        }
        assert!(melted, "two actions cycling should look like a meltdown");
    }

    #[test]
    fn reset_clears_state() {
        let mut d = MopDetector::new(3, 2);
        let s = signature("loop");
        d.record(s);
        d.record(s);
        d.reset();
        assert!(!d.record(s), "after reset the window is empty");
    }

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: format!("tc-{name}"),
            name: name.to_string(),
            arguments_json: args.to_string(),
        }
    }

    #[test]
    fn tool_loop_warns_then_stops_exact_repeated_shape() {
        let mut detector = ToolLoopDetector::default();
        let repeated = call("read_file", serde_json::json!({"path":"Cargo.toml"}));

        assert_eq!(detector.record(std::slice::from_ref(&repeated)), None);
        assert_eq!(detector.record(std::slice::from_ref(&repeated)), None);
        assert!(matches!(
            detector.record(std::slice::from_ref(&repeated)),
            Some(LoopSignal::Warn(message)) if message.contains("exact repeated")
        ));
        assert_eq!(detector.record(std::slice::from_ref(&repeated)), None);
        assert!(matches!(
            detector.record(&[repeated]),
            Some(LoopSignal::HardStop(message)) if message.contains("exact repeated")
        ));
    }

    #[test]
    fn tool_loop_stops_broad_tool_type_runaway() {
        let mut detector = ToolLoopDetector::default();
        let mut signal = None;
        for index in 0..40 {
            signal = detector.record(&[call(
                "read_file",
                serde_json::json!({"path": format!("file-{index}.rs")}),
            )]);
        }

        assert!(matches!(
            signal,
            Some(LoopSignal::HardStop(message)) if message.contains("read_file")
        ));
    }
}
