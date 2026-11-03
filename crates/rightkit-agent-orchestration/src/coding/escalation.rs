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
