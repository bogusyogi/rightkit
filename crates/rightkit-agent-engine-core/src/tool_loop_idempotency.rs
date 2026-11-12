//! Tri-axis tool-loop guard — idempotency-aware third axis (parity P-3).
//!
//! The pre-P-3 `ToolLoopDetector` (in `mop.rs`) covers two axes:
//! exact-shape repeat (warn at 3 / stop at 5) and broad same-name
//! runaway (warn at 30 / stop at 40). This file adds a THIRD axis
//! driven by tool capability metadata:
//!
//! - **Idempotency-aware exact-repeat**: splits the exact-shape axis
//!   by whether the tool mutates state. Repeated identical MUTATING
//!   calls fire earlier (default 2) than repeated identical
//!   READ-ONLY calls (default 4). Idempotency is decided by the
//!   existing `Tool::is_read_only()` method — the same surface
//!   `iteration_budget::classify_programmatic` uses for the
//!   iteration-budget classification, so no parallel taxonomy is
//!   introduced.
//!
//! - **Same-name, varying-args**: a same-name run where at least one
//!   shape varies from the others — distinct from "same-name all
//!   identical" (handled by the existing exact-shape axis).
//!
//! Both new axes emit `LoopSignal::Warn` only — they raise the
//! signal earlier than the legacy axes, not later. Existing
//! hard-stop behavior is unchanged.
//!
//! The new detector state and `record_idempotency_axis` method are
//! added as a sibling helper that takes the existing
//! `ToolLoopDetector` shape-count state (read-only) plus its own
//! name-run bookkeeping. This keeps `mop.rs` under the 400-line
//! module cap.

use crate::mop::{signature, tool_call_shape, LoopSignal};
use rightkit_agent_coding_tools::ToolRegistry;
use rightkit_agent_engine_protocol::ToolCall;
use std::collections::{HashMap, VecDeque};

/// Idempotency classification of a single tool call. Drives the
/// third axis of the loop guard. Reuses the existing
/// capability/permission metadata so we never invent a parallel
/// taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdempotencyClass {
    /// Tool is registered AND `is_read_only()` returns `true`.
    ReadOnly,
    /// Anything else — mutating tools, un-registered tools, or tools
    /// without an `is_read_only()` method (conservative default =
    /// mutating, same fallback
    /// `iteration_budget::is_every_tool_read_only` uses).
    Mutating,
}

impl IdempotencyClass {
    /// Classify a tool call against the live tool registry. Tools
    /// missing from the registry are conservatively classified as
    /// mutating — matches the fallback policy in
    /// `iteration_budget::is_every_tool_read_only`.
    pub fn classify(call: &ToolCall, tools: &ToolRegistry) -> Self {
        match tools.get(&call.name) {
            Some(tool) if tool.is_read_only() => IdempotencyClass::ReadOnly,
            _ => IdempotencyClass::Mutating,
        }
    }
}

/// Bookkeeping for the new axes. Owned by the engine loop (not
/// embedded in `ToolLoopDetector`) so the existing detector stays
/// unchanged for callers that have not opted into the third axis.
#[derive(Debug, Clone)]
pub struct IdempotencyAxis {
    /// Sliding window of `(tool_name, shape_sig)` observations used
    /// to compute the longest same-name run with at least one shape
    /// variation.
    name_runs: VecDeque<(String, u64)>,
    /// Longest qualifying run currently in the window — updated by
    /// `record_step` after each batch.
    name_run_max: usize,
    /// One-way latch so each axis warns at most once per session.
    warned_mutating_repeat: bool,
    warned_read_only_repeat: bool,
    warned_same_name: bool,
    /// Thresholds (lifted from the unified `LoopDetectorConfig`).
    mutating_repeat_warn_threshold: usize,
    read_only_repeat_warn_threshold: usize,
    same_name_varying_warn_threshold: usize,
    /// Window bound (kept in sync with `ToolLoopDetector::window_size`).
    window_size: usize,
}

impl IdempotencyAxis {
    pub fn new(
        mutating_repeat_warn_threshold: usize,
        read_only_repeat_warn_threshold: usize,
        same_name_varying_warn_threshold: usize,
        window_size: usize,
    ) -> Self {
        Self {
            name_runs: VecDeque::with_capacity(window_size),
            name_run_max: 0,
            warned_mutating_repeat: false,
            warned_read_only_repeat: false,
            warned_same_name: false,
            mutating_repeat_warn_threshold,
            read_only_repeat_warn_threshold,
            same_name_varying_warn_threshold,
            window_size,
        }
    }

    /// Reset state (called on session restart or context reset).
    pub fn reset(&mut self) {
        self.name_runs.clear();
        self.name_run_max = 0;
        self.warned_mutating_repeat = false;
        self.warned_read_only_repeat = false;
        self.warned_same_name = false;
    }

    /// Record one batch of tool calls and emit the first warn signal
    /// from any of the three new-axis rules. Returns `Some(Warn)`
    /// only when a threshold trips AND the matching `warned_*`
    /// latch has not been set yet. Returns `None` otherwise.
    ///
    /// `shape_counts` is the per-shape sliding-window counter the
    /// legacy `ToolLoopDetector` maintains; we re-use it for the
    /// idempotency exact-repeat rule so the count is shared.
    pub fn record(
        &mut self,
        calls: &[ToolCall],
        tools: &ToolRegistry,
        shape_counts: &HashMap<u64, usize>,
    ) -> Option<LoopSignal> {
        // Update the sliding-window same-name run bookkeeping.
        for call in calls {
            let shape = signature(&tool_call_shape(call));
            self.record_step(call.name.clone(), shape);
        }

        // Idempotency exact-repeat axis: the LATEST call's shape
        // count, classified by capability. Mutating tools fire at
        // `mutating_repeat_warn_threshold` (default 2); read-only
        // tools fire at `read_only_repeat_warn_threshold` (default 4).
        if let Some(latest) = calls.last() {
            let latest_shape = signature(&tool_call_shape(latest));
            let count = shape_counts.get(&latest_shape).copied().unwrap_or(0);
            let class = IdempotencyClass::classify(latest, tools);
            match class {
                IdempotencyClass::Mutating
                    if count >= self.mutating_repeat_warn_threshold
                        && !self.warned_mutating_repeat =>
                {
                    self.warned_mutating_repeat = true;
                    return Some(LoopSignal::Warn(format!(
                        "mutating tool '{name}' called identically {count} times in the recent window; a redundant write is the most dangerous kind of loop — vary approach before continuing",
                        name = latest.name,
                    )));
                }
                IdempotencyClass::ReadOnly
                    if count >= self.read_only_repeat_warn_threshold
                        && !self.warned_read_only_repeat =>
                {
                    self.warned_read_only_repeat = true;
                    return Some(LoopSignal::Warn(format!(
                        "read-only tool '{name}' called identically {count} times in the recent window; vary approach or summarize progress before continuing",
                        name = latest.name,
                    )));
                }
                _ => {}
            }
        }

        // Same-name, varying-args axis. We track this with a parallel
        // queue of `(name, shape)` so we can detect a run of the same
        // name where at least one shape differs from the run's modal
        // shape. The longest same-name run inside the window that
        // includes at least one shape variation trips the warn.
        if self.name_run_max >= self.same_name_varying_warn_threshold && !self.warned_same_name {
            self.warned_same_name = true;
            // Find the dominant name in the window for the warning
            // message (matches the kind-axis UX).
            let dominant = dominant_name_in(&self.name_runs);
            return Some(LoopSignal::Warn(format!(
                "tool '{dominant}' called {n} times in the recent window with varying arguments; pause and decide whether the repeated calls are still useful",
                n = self.name_run_max,
            )));
        }
        None
    }

    fn record_step(&mut self, name: String, shape: u64) {
        self.name_runs.push_back((name, shape));
        while self.name_runs.len() > self.window_size {
            self.name_runs.pop_front();
        }
        self.name_run_max = compute_longest_varying_run(&self.name_runs);
    }
}

/// Find the dominant (most-frequent) tool name in the run window.
fn dominant_name_in(runs: &VecDeque<(String, u64)>) -> String {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (name, _) in runs {
        *counts.entry(name.as_str()).or_insert(0) += 1;
    }
    counts
        .into_iter()
        .max_by_key(|(_, c)| *c)
        .map(|(name, _)| name.to_string())
        .unwrap_or_else(|| "tool".to_string())
}

/// Compute the longest run of consecutive same-name entries in `runs`
/// that contains at least two distinct shapes. Returns 0 if no run
/// qualifies (e.g. all entries share one shape, or runs alternate
/// names).
fn compute_longest_varying_run(runs: &VecDeque<(String, u64)>) -> usize {
    let mut best = 0usize;
    let mut i = 0;
    while i < runs.len() {
        let name = &runs[i].0;
        let mut j = i;
        let mut shapes_seen: HashMap<u64, ()> = HashMap::new();
        while j < runs.len() && runs[j].0 == *name {
            shapes_seen.insert(runs[j].1, ());
            j += 1;
        }
        if shapes_seen.len() >= 2 {
            let run_len = j - i;
            if run_len > best {
                best = run_len;
            }
        }
        i = j;
    }
    best
}
