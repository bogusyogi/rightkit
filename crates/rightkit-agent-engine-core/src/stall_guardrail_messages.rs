// Sibling fragment of stall_guardrail.rs — included via `include!()`. Pure
// helpers: the model-facing and operator-facing text of the `repeated_call` Ask.

/// Reason (and policy source) of the Ask raised for a repeated call.
pub const REPEATED_CALL_REASON: &str = "repeated_call";

/// The Ask's reason line: shown on the approval card and recorded in the span.
pub fn repeated_call_reason(tool_name: &str, count: u32) -> String {
    format!(
        "{REPEATED_CALL_REASON}: tool '{tool_name}' was called {count}x in a row with identical arguments"
    )
}

/// Tool-result text when the repeated call was denied (by the user, or at once
/// when nothing can answer). The model reads this AS the tool outcome.
pub fn repeated_call_denied_message(tool_name: &str) -> String {
    format!(
        "{REPEATED_CALL_REASON}: tool '{tool_name}' was called three times in a row with identical arguments and this fourth call was denied, so it was not run. \
Do NOT retry it. Take a materially different approach: a different tool, different arguments, a different hypothesis, or stop and say so plainly."
    )
}
