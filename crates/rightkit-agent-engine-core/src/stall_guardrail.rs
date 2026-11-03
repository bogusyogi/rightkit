// (no //! header — this file is included via `#[path] mod stall_guardrail;` in engine.rs.)

use rightkit_agent_engine_protocol::ToolCall;
use std::collections::HashMap;

/// Per-tool-call-signature graduated stall guardrail.
///
/// Mirrors Hermes's ToolCallGuardrailController (FEATURE-MATRIX par §#120,
/// "No-progress / stall bound", flips row from `worse` to `done` once shipped).
/// Two repeat-count stores are tracked independently for diagnostics:
///
/// * signature: `(tool_name, normalized_args_hash)` — same tool + same args.
/// * tool: `tool_name` alone — same tool, ANY args (observability only).
///
/// Only identical signatures drive enforcement. Different arguments are normal
/// for coding tools such as shell, search, and read_file; the broader runaway
/// detector handles pathological same-name sequences at much higher bounds.
/// Severity ladder: `Allow < Warn < Skip < Block`.
/// Thresholds are `const` so they are easy to tune without editing logic.
#[derive(Debug, Clone, Default)]
pub struct StallGuardrail {
    signature_counts: HashMap<(String, u64), u32>,
    tool_counts: HashMap<String, u32>,
    /// Last result hash per signature: a repeated call whose result changed
    /// (a re-read after the page or file moved on) made progress.
    last_results: HashMap<(String, u64), u64>,
}

/// Outcome emitted by `StallGuardrail::record`. `count` is the post-increment
/// consecutive-call count for whichever tier produced the tier (signature or
/// tool-name). Reading `StallSeverity::count()` is the canonical way to surface
/// it in messages, regardless of which tier fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallSeverity {
    Allow,
    Warn { count: u32 },
    Skip { count: u32 },
    Block { count: u32 },
}

impl StallSeverity {
    pub fn count(self) -> u32 {
        match self {
            StallSeverity::Allow => 0,
            StallSeverity::Warn { count }
            | StallSeverity::Skip { count }
            | StallSeverity::Block { count } => count,
        }
    }

    pub fn is_skip(self) -> bool {
        matches!(self, StallSeverity::Skip { .. })
    }

    pub fn is_block(self) -> bool {
        matches!(self, StallSeverity::Block { .. })
    }
}

impl StallGuardrail {
    /// WARN fires on the second consecutive identical call.
    pub const WARN_AT: u32 = 2;
    /// SKIP fires on the fourth consecutive identical call.
    pub const SKIP_AT: u32 = 4;
    /// BLOCK fires on the sixth consecutive identical call — the same terminal
    /// behavior as the legacy `GoalState::MAX_NO_PROGRESS=3` global cap.
    pub const BLOCK_AT: u32 = 6;

    pub fn new() -> Self {
        Self::default()
    }

    /// Clear BOTH tier counts. Invoke when a turn made progress (the engine
    /// calls this just before re-entering the loop after `ContinueAfterGoal`).
    pub fn reset(&mut self) {
        self.signature_counts.clear();
        self.tool_counts.clear();
        self.last_results.clear();
    }

    /// Feed a dispatched call's result back. When the same signature returns
    /// a different result than last time, the repeat observed progress, so
    /// its count restarts at this one call.
    pub fn observe_result(&mut self, call: &ToolCall, output: &str) {
        use std::hash::{Hash, Hasher};
        let key = (call.name.clone(), normalize_args_hash(&call.arguments_json));
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        output.hash(&mut hasher);
        let result = hasher.finish();
        if let Some(previous) = self.last_results.insert(key.clone(), result) {
            if previous != result {
                self.signature_counts.insert(key, 1);
            }
        }
    }

    /// Record one tool call and return the resulting severity. Increments BOTH
    /// tier counters before evaluating (so count == post-increment total).
    pub fn record(&mut self, name: &str, args_hash: u64) -> StallSeverity {
        let sig_entry = self
            .signature_counts
            .entry((name.to_string(), args_hash))
            .or_insert(0);
        *sig_entry += 1;
        let tool_entry = self.tool_counts.entry(name.to_string()).or_insert(0);
        *tool_entry += 1;

        severity_for_count(*sig_entry)
    }

    /// Convenience wrapper that hashes the call's `arguments_json` canonically
    /// (object keys sorted recursively) so re-ordered fields hash identically.
    pub fn record_call(&mut self, call: &ToolCall) -> StallSeverity {
        let hash = normalize_args_hash(&call.arguments_json);
        self.record(&call.name, hash)
    }

    /// Introspect the per-signature count directly — used by tests to prove
    /// that two `(tool_name, args_hash)` buckets are independent. Returns 0
    /// for unseen signatures.
    pub fn signature_count(&self, name: &str, args_hash: u64) -> u32 {
        self.signature_counts
            .get(&(name.to_string(), args_hash))
            .copied()
            .unwrap_or(0)
    }

    /// Introspect the per-tool-name count directly — used by tests to prove
    /// the same-tool-any-args tier accumulates independently of the per-
    /// signature tier. Returns 0 for unseen tools.
    pub fn tool_count(&self, name: &str) -> u32 {
        self.tool_counts.get(name).copied().unwrap_or(0)
    }
}

fn severity_for_count(count: u32) -> StallSeverity {
    if count >= StallGuardrail::BLOCK_AT {
        StallSeverity::Block { count }
    } else if count >= StallGuardrail::SKIP_AT {
        StallSeverity::Skip { count }
    } else if count >= StallGuardrail::WARN_AT {
        StallSeverity::Warn { count }
    } else {
        StallSeverity::Allow
    }
}

include!("stall_guardrail_args.rs");
include!("stall_guardrail_messages.rs");
