//! Stall escalation — the loop-engineering response when the agent starts looping.
//! CR already has two detectors (`ToolLoopDetector` warn/hard-stop, `MopDetector`
//! meltdown). This module supplies the shared text/format they escalate with:
//! a strong "change your strategy" directive injected at the warning points, and
//! a structured reason for the `stalled` signal emitted at the hard stop. Logic
//! lives here so it is pure + unit-tested; the engine only injects/emits it.

/// Injected when the agent is detected looping, to force a strategy change rather
/// than another retry of the same failing approach.
pub const STRATEGY_RESET_DIRECTIVE: &str =
    "You are looping — repeating an approach that is not working. Do NOT retry it. \
     In one line, state WHY the current approach is failing, then take a \
     fundamentally different approach: a different tool, a different decomposition, \
     or a different hypothesis. If no different approach exists, say so plainly and stop.";

/// Human-readable reason for a stalled hand-off, given the detector's message.
pub fn stalled_reason(detail: &str) -> String {
    format!("agent stalled (loop detector hard-stop): {detail}")
}
