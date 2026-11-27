//! Meltdown-Onset-Point detection — spot behavioral collapse on long tasks.
//! An agent that loops — repeating the
//! same action over and over with no progress — has hit a MOP and should have
//! its context reset rather than be allowed to burn the budget.
//!
//! Detection is a sliding window over action signatures: if any signature
//! recurs at least `repeat_threshold` times within the last `window_size`
//! actions, or the window's distinct-signature ratio collapses, a MOP fires.

use rightkit_agent_engine_protocol::ToolCall;
use rightkit_cache::prefix::fnv1a_64;
use std::collections::{HashMap, VecDeque};

/// The closing of the line the shell tool puts before the diff of the files a command changed
/// (`[<host label>: changed by this command]`, the label is the host's shell notice label).
const CHANGE_REPORT_HEADER_END: &str = ": changed by this command]";

/// Most calls and result fingerprints [`ToolLoopDetector`] remembers between a call and its
/// result; past it that bookkeeping is cleared so a caller that never reports results
/// cannot grow it without bound.
const RESULT_TRACKING_MAX: usize = 4096;

/// A command's own output, without the diff of what it changed: a command that writes a counter file
/// prints the same line every run, and the changing diff must not make a true repeat look like progress.
pub(crate) fn strip_change_report(result: &str) -> &str {
    let mut offset = 0;
    for line in result.split_inclusive('\n') {
        let text = line.trim_end_matches(['\n', '\r']);
        if text.starts_with('[') && text.ends_with(CHANGE_REPORT_HEADER_END) {
            return result[..offset].trim_end();
        }
        offset += line.len();
    }
    result
}

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
/// Hashed with standard FNV-1a 64 (`rightkit_cache::prefix::fnv1a_64`). Signatures exist only
/// in this process's sliding window, never on disk, so no persisted key depends on the basis.
pub fn signature(action: &str) -> u64 {
    fnv1a_64(action)
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
    /// Calls recorded but not yet answered: call id -> shape.
    pending: HashMap<String, u64>,
    /// Latest result fingerprint seen per shape.
    last_result: HashMap<u64, u64>,
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
            pending: HashMap::new(),
            last_result: HashMap::new(),
        }
    }
}

impl ToolLoopDetector {
    /// A detector with the window and the warn and stop thresholds of `config`; the defaults
    /// of the config reproduce [`Default`].
    pub fn from_config(config: &crate::engine::LoopDetectorConfig) -> Self {
        Self {
            window_size: config.tool_loop_window_size,
            exact_warn_threshold: config.exact_warn_threshold,
            exact_stop_threshold: config.exact_stop_threshold,
            kind_warn_threshold: config.kind_warn_threshold,
            kind_stop_threshold: config.kind_stop_threshold,
            ..Self::default()
        }
    }

    /// Feed the result of an earlier recorded call. The same call with a new
    /// result is progress (an edit-then-test cycle), so its repeats are
    /// forgotten; the same call with the same result keeps counting.
    ///
    /// A failed or refused call (`is_error`) is no progress: a refusal's text differs from the last
    /// answer without anything having run, and counting it would let a refused repeat go on for ever.
    /// Returns true when the repeat brought a new result (progress), so the caller can reset the meltdown window too.
    pub fn observe_result(&mut self, call_id: &str, result: &str, is_error: bool) -> bool {
        let Some(shape) = self.pending.remove(call_id) else {
            return false;
        };
        if is_error {
            return false;
        }
        let fingerprint = signature(strip_change_report(result));
        if let Some(previous) = self.last_result.insert(shape, fingerprint) {
            if previous != fingerprint {
                self.shapes.retain(|s| *s != shape);
                self.shape_counts.remove(&shape);
                return true;
            }
        }
        false
    }

    /// Read-only view of the per-shape sliding-window counter. Exposed
    /// so the sibling idempotency axis (parity P-3) can re-use the
    /// same shape counts without duplicating bookkeeping.
    pub fn shape_counts_view(&self) -> &HashMap<u64, usize> {
        &self.shape_counts
    }

    pub fn record(&mut self, calls: &[ToolCall]) -> Option<LoopSignal> {
        for call in calls {
            let shape = signature(&tool_call_shape(call));
            if self.pending.len() >= RESULT_TRACKING_MAX {
                self.pending.clear();
            }
            if self.last_result.len() >= RESULT_TRACKING_MAX {
                self.last_result.clear();
            }
            self.pending.insert(call.id.clone(), shape);
            self.record_shape(shape);
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
