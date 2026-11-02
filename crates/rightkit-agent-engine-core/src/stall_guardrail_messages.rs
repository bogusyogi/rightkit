// Sibling fragment of stall_guardrail.rs — included by the parent module. Pure
// helpers: turn a stall-guardrail outcome into the model-facing string the
// engine injects at the right integration point.

/// Message injected on the synthetic tool result path when a call is SKIP'd.
/// The model sees this AS the tool outcome (no real tool runs).
pub fn skipped_message(tool_name: &str, count: u32) -> String {
    format!(
        "[stall-guardrail:skipped] Tool '{tool_name}' was called {count}x in a row with no progress and has been SKIPPED to break the loop. \
Do NOT retry it. Take a materially different approach: a different tool, a different decomposition, a different hypothesis, or stop and say so plainly."
    )
}

/// Message appended to the next-turn context when the call RAN with WARN
/// severity (executed, but the model is being told to vary the approach).
pub fn warn_message(tool_name: &str, count: u32) -> String {
    format!(
        "[stall-guardrail:warn] Tool '{tool_name}' has been called {count}x consecutively with no progress. \
          Take a different approach in your next turn instead of repeating the same argument.",
        count = count,
    )
}

/// Message emitted when BLOCK closes the goal as blocked (matches the
/// existing `stalled_reason(...)` shape so the orchestration observation
/// surface already in use is preserved).
pub fn block_message(tool_name: &str, count: u32) -> String {
    format!(
        "agent stalled (per-tool-signature guardrail hard-stop): tool '{tool_name}' repeated {count}x consecutively; closing goal as blocked"
    )
}
