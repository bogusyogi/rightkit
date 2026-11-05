// (no //! header — this file is included via `#[path] mod stall_guardrail;` in engine.rs.)

use rightkit_agent_engine_protocol::ToolCall;
use std::collections::HashMap;

/// Cross-iteration repeated-call bound (sessions rule 32).
///
/// The idle and no-progress limits bound one provider or tool call, not the
/// loop. A tool call that repeats the same tool with identical arguments three
/// times in a row is a loop: the fourth identical call is raised as an Ask with
/// reason `repeated_call` (OpenCode's `doom_loop`: "the same tool call repeats
/// 3 times with identical input", default `ask`). Unattended, the Ask is denied
/// at once under headless R8 and the model is told. The turn loop only counts;
/// the Ask itself goes through the permission path
/// (`resolve_single_tool_permission`), so the policy verdict stays with the
/// permission layer.
///
/// "In a row" is strict: any other call in between restarts the count. A repeat
/// whose result changed since the last identical call (a re-read after the
/// page or file moved on) made progress and restarts the count at that call.
#[derive(Debug, Clone, Default)]
pub struct StallGuardrail {
    /// Signature of the previous call and how many identical calls in a row
    /// ended with it.
    last: Option<(String, u64)>,
    run: u32,
    /// Last result hash per signature, for the progress check.
    last_results: HashMap<(String, u64), u64>,
}

/// Outcome of `StallGuardrail::record`. `count` is the number of identical
/// calls in a row including this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StallSeverity {
    Allow,
    /// The call repeats the same tool with identical arguments for the fourth
    /// (or later) time in a row: raise an Ask with reason `repeated_call`.
    Ask {
        count: u32,
    },
}

impl StallSeverity {
    pub fn is_ask(self) -> bool {
        matches!(self, StallSeverity::Ask { .. })
    }
}

impl StallGuardrail {
    /// The Ask fires on the fourth identical call in a row: three have run.
    pub const ASK_AT: u32 = 4;

    pub fn new() -> Self {
        Self::default()
    }

    /// Clear the run. Invoke when a turn made progress (the engine calls this
    /// just before re-entering the loop after `ContinueAfterGoal`).
    pub fn reset(&mut self) {
        self.last = None;
        self.run = 0;
        self.last_results.clear();
    }

    /// Feed a dispatched call's result back. When the same signature returns a
    /// different result than last time, the repeat observed progress, so the
    /// run restarts at this one call.
    pub fn observe_result(&mut self, call: &ToolCall, output: &str) {
        use std::hash::{Hash, Hasher};
        let key = (call.name.clone(), normalize_args_hash(&call.arguments_json));
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        output.hash(&mut hasher);
        let result = hasher.finish();
        if let Some(previous) = self.last_results.insert(key.clone(), result) {
            if previous != result && self.last.as_ref() == Some(&key) {
                self.run = 1;
            }
        }
    }

    /// Record one tool call and return the resulting severity.
    pub fn record(&mut self, name: &str, args_hash: u64) -> StallSeverity {
        let key = (name.to_string(), args_hash);
        if self.last.as_ref() == Some(&key) {
            self.run = self.run.saturating_add(1);
        } else {
            self.last = Some(key);
            self.run = 1;
        }
        severity_for_count(self.run)
    }

    /// Convenience wrapper that hashes the call's `arguments_json` canonically
    /// (object keys sorted recursively) so re-ordered fields hash identically.
    pub fn record_call(&mut self, call: &ToolCall) -> StallSeverity {
        let hash = normalize_args_hash(&call.arguments_json);
        self.record(&call.name, hash)
    }

    /// Identical calls in a row so far. Used by tests.
    pub fn run_length(&self) -> u32 {
        self.run
    }
}

fn severity_for_count(count: u32) -> StallSeverity {
    if count >= StallGuardrail::ASK_AT {
        StallSeverity::Ask { count }
    } else {
        StallSeverity::Allow
    }
}

include!("stall_guardrail_args.rs");
include!("stall_guardrail_messages.rs");
