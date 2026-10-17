//! The eyes gate a producer applies when it delivers a brief's assets.
//!
//! `review.mode` on a content brief says which gate applies:
//!
//! * `manual`: a person must approve these exact assets in the app. Until
//!   they do, the producer refuses with [`EYES_APPROVAL_REQUIRED`].
//! * `auto_when_clean`: deliver without a person only when an automated
//!   review of these exact assets (the `rightkit-review` verdict) returned a
//!   clean verdict under the brief's `review.policyRevision`. A producer with
//!   no such verdict treats the mode exactly as `manual` and refuses. It never
//!   delivers on the strength of the mode alone (fail closed).
//! * `none`: deliver, and report [`EyesGate::NotRequired`].

use crate::types::content_brief::ReviewMode;

/// The producer refusal code for a delivery that still needs a person's
/// approval of the assets. A consumer treats it as "awaiting approval", not a
/// failure.
pub const EYES_APPROVAL_REQUIRED: &str = "eyes_approval_required";

/// Which gate allowed a delivery. Serialised as snake_case text by callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EyesGate {
    /// A person approved these exact assets.
    Approved,
    /// An automated review returned a clean verdict under the policy revision.
    AutoClean,
    /// The brief's `review.mode` is `none`.
    NotRequired,
}

impl EyesGate {
    /// The wire name: `approved`, `auto_clean` or `not_required`.
    pub fn name(self) -> &'static str {
        match self {
            EyesGate::Approved => "approved",
            EyesGate::AutoClean => "auto_clean",
            EyesGate::NotRequired => "not_required",
        }
    }
}

/// Decides the gate for a delivery. `person_approved` is true when a person
/// approved these exact assets; `clean_verdict` is true when an automated
/// review of these exact assets returned clean under the brief's
/// `policyRevision`. `Err` carries [`EYES_APPROVAL_REQUIRED`].
pub fn eyes_gate(
    mode: ReviewMode,
    person_approved: bool,
    clean_verdict: bool,
) -> Result<EyesGate, &'static str> {
    match mode {
        ReviewMode::None => Ok(EyesGate::NotRequired),
        ReviewMode::AutoWhenClean if clean_verdict => Ok(EyesGate::AutoClean),
        ReviewMode::Manual | ReviewMode::AutoWhenClean if person_approved => Ok(EyesGate::Approved),
        ReviewMode::Manual | ReviewMode::AutoWhenClean => Err(EYES_APPROVAL_REQUIRED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_needs_a_person() {
        assert_eq!(
            eyes_gate(ReviewMode::Manual, false, true),
            Err(EYES_APPROVAL_REQUIRED)
        );
        assert_eq!(
            eyes_gate(ReviewMode::Manual, true, false),
            Ok(EyesGate::Approved)
        );
    }

    #[test]
    fn auto_when_clean_fails_closed() {
        assert_eq!(
            eyes_gate(ReviewMode::AutoWhenClean, false, false),
            Err(EYES_APPROVAL_REQUIRED)
        );
        assert_eq!(
            eyes_gate(ReviewMode::AutoWhenClean, false, true),
            Ok(EyesGate::AutoClean)
        );
    }

    #[test]
    fn none_is_not_required() {
        assert_eq!(
            eyes_gate(ReviewMode::None, false, false),
            Ok(EyesGate::NotRequired)
        );
    }
}
