//! The paste route's view of the evidence gate.
//!
//! `paste_dispatch_gate` is the pure decision for the paste keystroke: the fresh focused control
//! must be the bound one, the field must be CONFIRMED not secure, and the sampling must be in
//! time. Unknown secure state, an unobservable control and a timeout all refuse (they used to
//! pass when a ready target had no secure evidence; that was the defect fixed here). The write
//! itself is only possible through `evidence_gate::authorize_with_input`, which applies this same decision
//! and hands out the `WritePermit` the keystroke needs.

use std::time::Duration;

use rightkit_transcript_library::delivery::CopyFallbackReason;

use crate::evidence_gate::{decide, ControlMatch};

/// Final gate for a synthetic paste: `Ok` only for (same control, not secure, in time). `Err`
/// carries the copy-fallback reason the route takes.
pub fn paste_dispatch_gate(
    control: ControlMatch,
    secure_state: Option<bool>,
    elapsed: Duration,
    budget: Duration,
) -> Result<(), CopyFallbackReason> {
    decide(control, secure_state, elapsed, budget).map_err(|refusal| refusal.fallback_reason())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUDGET: Duration = Duration::from_millis(100);
    const FAST: Duration = Duration::from_millis(1);

    #[test]
    fn positive_secure_evidence_vetoes_before_focus() {
        assert_eq!(
            paste_dispatch_gate(ControlMatch::Changed, Some(true), FAST, BUDGET),
            Err(CopyFallbackReason::Other("secure_target".to_string()))
        );
    }

    #[test]
    fn a_moved_or_unobservable_control_is_a_focus_change() {
        assert_eq!(
            paste_dispatch_gate(ControlMatch::Changed, Some(false), FAST, BUDGET),
            Err(CopyFallbackReason::FocusChanged)
        );
        assert_eq!(
            paste_dispatch_gate(ControlMatch::Unknown, Some(false), FAST, BUDGET),
            Err(CopyFallbackReason::FocusChanged)
        );
    }

    #[test]
    fn unknown_security_with_a_ready_target_now_refuses() {
        assert_eq!(
            paste_dispatch_gate(ControlMatch::Same, None, FAST, BUDGET),
            Err(CopyFallbackReason::Other("secure_unknown".to_string()))
        );
    }

    #[test]
    fn confirmed_non_secure_same_control_in_time_passes() {
        assert_eq!(
            paste_dispatch_gate(ControlMatch::Same, Some(false), FAST, BUDGET),
            Ok(())
        );
    }

    #[test]
    fn a_probe_that_timed_out_refuses() {
        assert_eq!(
            paste_dispatch_gate(ControlMatch::Same, Some(false), BUDGET * 2, BUDGET),
            Err(CopyFallbackReason::Other("evidence_timeout".to_string()))
        );
    }
}
