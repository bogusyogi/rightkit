//! v1 escalation backstop: a [`Path::DirectEdit`] run that turns out to be
//! bigger than it looked hands off **once** to [`Path::ArchitectPlan`].
//!
//! Two signals, neither needing shared/atomic state (the read-count observer
//! that would is deferred to v2 — see the plan's v1 minimal core):
//!  1. the worker calls its own `escalate` tool (the reliable "LLM judges itself
//!     out of depth" signal — primary), or
//!  2. the direct-edit lane fails its deterministic gate or thin review after
//!     its retry budget (a transient failure was retried; a real complexity
//!     mismatch persists — secondary).
//!
//! Hard-capped at one hop via the run's `already_escalated` flag.
//!
//! [`Path::DirectEdit`]: crate::coding::trigger::Path::DirectEdit
//! [`Path::ArchitectPlan`]: crate::coding::trigger::Path::ArchitectPlan

/// Decide whether a `DirectEdit` run should escalate to `ArchitectPlan`.
///
/// `already_escalated` is the run's single boolean flag; once a hop has
/// happened it blocks any second hop regardless of the other signals.
pub fn should_escalate(
    escalate_requested: bool,
    direct_edit_failed_after_retry: bool,
    already_escalated: bool,
) -> bool {
    !already_escalated && (escalate_requested || direct_edit_failed_after_retry)
}

/// Map the direct-edit gate and thin-review result to the one-hop decision.
///
/// A direct edit is eligible to escalate only when either required check
/// rejects it; a fully approved edit has no escalation signal.
pub fn should_escalate_direct_edit(
    gate_ok: bool,
    review_ok: bool,
    already_escalated: bool,
) -> bool {
    should_escalate(false, !gate_ok || !review_ok, already_escalated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_escalate_tool_triggers() {
        assert!(should_escalate(true, false, false));
    }

    #[test]
    fn direct_edit_failure_after_retry_triggers() {
        assert!(should_escalate(false, true, false));
    }

    #[test]
    fn capable_worker_can_handoff_to_reasoning_once() {
        // The conductor's capable worker may request the reasoning/planned arm
        // when the first attempt exposes a complexity mismatch, but the same
        // run must reject a second hop.
        assert!(should_escalate(true, false, false));
        assert!(!should_escalate(true, false, true));
    }

    #[test]
    fn no_signal_does_not_escalate() {
        assert!(!should_escalate(false, false, false));
    }

    #[test]
    fn direct_edit_mapping_escalates_each_failed_required_check_once() {
        assert!(should_escalate_direct_edit(false, false, false));
        assert!(should_escalate_direct_edit(false, true, false));
        assert!(should_escalate_direct_edit(true, false, false));
        assert!(!should_escalate_direct_edit(true, true, false));
        assert!(!should_escalate_direct_edit(false, false, true));
        assert!(!should_escalate_direct_edit(true, false, true));
    }

    #[test]
    fn already_escalated_blocks_a_second_hop() {
        // Even with both signals firing, a run that already hopped does not hop again.
        assert!(!should_escalate(true, true, true));
        assert!(!should_escalate(true, false, true));
        assert!(!should_escalate(false, true, true));
    }
}
