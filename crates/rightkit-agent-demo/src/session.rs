//! Session transitions & bundle ids. Session credentials belong to rightkit-credentials in host.rs.
//!
//! consent -> token -> policy -> begin -> record -> stop -> verify (design, section 2). Every
//! transition the host makes goes through [`next`], so an illegal order (acting before consent, a
//! second request while one is live) is refused in one place.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// No session. Also the state after a refused or unanswered consent.
    Idle,
    AwaitingConsent,
    /// Consent given and a token minted; the recorder has not started (OS grants or the target app missing).
    Armed,
    Recording,
    Stopping,
    /// Ended normally; the bundle is sealed and verified.
    Sealed,
    /// Ended by the kill control. Only a fresh consent starts another session.
    Killed,
}

impl Phase {
    /// A session exists that owns the policy, the hotkey and (maybe) the recorder.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Phase::AwaitingConsent | Phase::Armed | Phase::Recording | Phase::Stopping
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Request,
    Approve,
    Deny,
    Started,
    Stop,
    Sealed,
    Kill,
}

/// The phase after `step`, or `None` when `step` is not allowed in `phase`.
pub fn next(phase: Phase, step: Step) -> Option<Phase> {
    use Phase as P;
    use Step as S;
    Some(match (phase, step) {
        (P::Idle | P::Sealed | P::Killed, S::Request) => P::AwaitingConsent,
        (P::AwaitingConsent, S::Approve) => P::Armed,
        (P::AwaitingConsent, S::Deny) => P::Idle,
        (P::Armed, S::Started) => P::Recording,
        // Stopping before the recorder ever started leaves nothing to seal.
        (P::Armed, S::Stop) => P::Idle,
        (P::Recording, S::Stop) => P::Stopping,
        (P::Stopping, S::Sealed) => P::Sealed,
        (P::AwaitingConsent | P::Armed | P::Recording | P::Stopping, S::Kill) => P::Killed,
        // Kill is idempotent once a session is over; the process-wide switch is what matters.
        (P::Idle | P::Sealed | P::Killed, S::Kill) => phase,
        _ => return None,
    })
}

/// A session id: letters, digits and `-`, so it is also a valid bundle id.
pub fn new_session_id() -> Result<String, String> {
    let mut raw = [0u8; 6];
    getrandom::fill(&mut raw).map_err(|e| format!("the OS random source failed: {e}"))?;
    Ok(format!("demo-{}", hex(&raw)))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unit tests on purpose: the transition table is pure, and an end-to-end run
    // cannot drive the illegal orders (it would need a second agent racing the first).
    #[test]
    fn only_the_documented_path_reaches_recording() {
        let mut p = Phase::Idle;
        for step in [
            Step::Request,
            Step::Approve,
            Step::Started,
            Step::Stop,
            Step::Sealed,
        ] {
            p = next(p, step).expect("legal step");
        }
        assert_eq!(p, Phase::Sealed);
        assert_eq!(
            next(Phase::Idle, Step::Started),
            None,
            "no recording without consent"
        );
        assert_eq!(next(Phase::AwaitingConsent, Step::Started), None);
        assert_eq!(
            next(Phase::Recording, Step::Request),
            None,
            "one session at a time"
        );
        assert_eq!(
            next(Phase::Killed, Step::Started),
            None,
            "killed is final until a fresh consent"
        );
    }

    #[test]
    fn kill_ends_every_active_phase_and_is_idempotent() {
        for p in [
            Phase::AwaitingConsent,
            Phase::Armed,
            Phase::Recording,
            Phase::Stopping,
        ] {
            assert_eq!(next(p, Step::Kill), Some(Phase::Killed));
            assert!(p.is_active());
        }
        assert_eq!(next(Phase::Killed, Step::Kill), Some(Phase::Killed));
        assert_eq!(next(Phase::Idle, Step::Kill), Some(Phase::Idle));
    }
}
