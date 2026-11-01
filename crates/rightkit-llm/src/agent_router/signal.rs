//! `TaskSignal` — the observable state a triage decision is computed from.
//!
//! Populated at the two R1a insertion seams (ADR
//! `docs/canon/daemon-master-canon-atoms.md` §D2 seam 1): the
//! per-sub-agent spawn pick. Every field is optional/defaulted via
//! [`TaskSignalBuilder`] so a caller that only has partial state (e.g. a
//! fresh session with no failure history yet) can still populate a valid
//! signal without inventing values.

use serde::{Deserialize, Serialize};

/// Coarse shape of the current turn, mirroring the states the engine loop
/// already distinguishes internally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnType {
    ToolResultFollowUp,
    FreshReasoning,
    PatchApply,
    TestRun,
    GoalContinuation,
}

/// The full observable-state input to [`crate::agent_router::triage::triage`].
///
/// `prompt_shape` fields are deliberately named to mirror
/// `orchestration::dispatch`'s private text extractors (word/path/sentence
/// counts) per ADR §D1 — this crate does not import `dispatch.rs` (it has no
/// public surface for these helpers and the lane fence forbids editing it),
/// so [`crate::agent_router::prompt_shape`] re-derives the same signals from raw prompt
/// text as a small shared helper, not a second divergent implementation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSignal {
    pub turn_type: TurnType,
    /// Tool calls observed in the last 4 turns.
    pub tool_call_density: u32,
    /// Consecutive failed-outcome turns (tool error, failing test run,
    /// rejected patch), windowed at 3 with hysteresis applied by the caller
    /// tracking it — this field only carries the current count.
    pub failure_streak: u32,
    /// Session budget ceiling minus accumulated cost, in USD.
    pub remaining_budget_usd: f64,
    /// Keyword floor per prior art: auth/money/migrate/delete/secret/token/
    /// pii/ssh. True means the sensitive-surface floor (D1) applies
    /// regardless of every other signal.
    pub touches_sensitive: bool,
    /// Number of distinct file/path references detected in the prompt.
    pub scoped_file_count: usize,
    /// Prompt-shape signals reused from `DispatchRouter`'s extractor
    /// approach (word count, question shape, broad-scope phrasing).
    pub prompt_shape: PromptShape,
    /// Current accumulated context size in tokens, used against a
    /// candidate model's `context_window_tokens` (ADR risk: context-window
    /// truncation on down-route).
    pub current_context_tokens: u64,
}

/// Prompt-text-derived signals, computed by [`crate::agent_router::prompt_shape`] from raw
/// task text so callers do not have to hand-roll these themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PromptShape {
    pub word_count: usize,
    pub sentence_count: usize,
    pub is_question: bool,
    pub broad_scope: bool,
}

/// Builder so callers populate only the fields they have; everything else
/// takes a conservative default (no failure history, no budget pressure
/// signal, not sensitive, empty prompt shape).
#[derive(Debug, Clone, Default)]
pub struct TaskSignalBuilder {
    turn_type: Option<TurnType>,
    tool_call_density: u32,
    failure_streak: u32,
    remaining_budget_usd: f64,
    touches_sensitive: bool,
    scoped_file_count: usize,
    prompt_shape: PromptShape,
    current_context_tokens: u64,
}

impl TaskSignalBuilder {
    pub fn new() -> Self {
        Self {
            remaining_budget_usd: f64::MAX,
            ..Default::default()
        }
    }

    pub fn turn_type(mut self, turn_type: TurnType) -> Self {
        self.turn_type = Some(turn_type);
        self
    }

    pub fn tool_call_density(mut self, density: u32) -> Self {
        self.tool_call_density = density;
        self
    }

    pub fn failure_streak(mut self, streak: u32) -> Self {
        self.failure_streak = streak;
        self
    }

    pub fn remaining_budget_usd(mut self, budget: f64) -> Self {
        self.remaining_budget_usd = budget;
        self
    }

    pub fn touches_sensitive(mut self, sensitive: bool) -> Self {
        self.touches_sensitive = sensitive;
        self
    }

    pub fn scoped_file_count(mut self, count: usize) -> Self {
        self.scoped_file_count = count;
        self
    }

    pub fn prompt_shape(mut self, shape: PromptShape) -> Self {
        self.prompt_shape = shape;
        self
    }

    /// Convenience: derive `prompt_shape`, `scoped_file_count`, and
    /// `touches_sensitive` from raw prompt text in one call.
    pub fn from_prompt_text(mut self, text: &str) -> Self {
        self.prompt_shape = crate::agent_router::prompt_shape::analyze(text);
        self.scoped_file_count = crate::agent_router::prompt_shape::path_reference_count(text);
        if crate::agent_router::prompt_shape::contains_sensitive_keyword(text) {
            self.touches_sensitive = true;
        }
        self
    }

    pub fn current_context_tokens(mut self, tokens: u64) -> Self {
        self.current_context_tokens = tokens;
        self
    }

    pub fn build(self) -> TaskSignal {
        TaskSignal {
            turn_type: self.turn_type.unwrap_or(TurnType::FreshReasoning),
            tool_call_density: self.tool_call_density,
            failure_streak: self.failure_streak,
            remaining_budget_usd: self.remaining_budget_usd,
            touches_sensitive: self.touches_sensitive,
            scoped_file_count: self.scoped_file_count,
            prompt_shape: self.prompt_shape,
            current_context_tokens: self.current_context_tokens,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_defaults_are_conservative() {
        let sig = TaskSignalBuilder::new().build();
        assert_eq!(sig.turn_type, TurnType::FreshReasoning);
        assert_eq!(sig.failure_streak, 0);
        assert!(!sig.touches_sensitive);
        assert_eq!(sig.tool_call_density, 0);
    }

    #[test]
    fn builder_populates_only_set_fields() {
        let sig = TaskSignalBuilder::new()
            .failure_streak(2)
            .touches_sensitive(true)
            .build();
        assert_eq!(sig.failure_streak, 2);
        assert!(sig.touches_sensitive);
        assert_eq!(sig.turn_type, TurnType::FreshReasoning);
    }

    #[test]
    fn from_prompt_text_derives_shape_and_sensitivity() {
        let sig = TaskSignalBuilder::new()
            .from_prompt_text("rotate the auth secret in src/keystore.rs")
            .build();
        assert!(sig.touches_sensitive);
        assert!(sig.scoped_file_count >= 1);
    }
}
