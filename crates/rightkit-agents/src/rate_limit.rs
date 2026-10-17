//! Rate-limit state for one vendor agent: what the host last knew, and why it is not known
//! now.
//!
//! Pure state with no I/O. Feed it [`AgentEvent::RateLimit`](crate::AgentEvent::RateLimit)
//! payloads through [`RateLimitState::observe`], a failed probe through
//! [`RateLimitState::probe_failed`], and a vendor that reports no limits through
//! [`RateLimitState::mark_unsupported`].
//!
//! Rules: observed data replaces `NoData`, `ProbeFailed` and `Unsupported`; a failed probe
//! keeps the last known snapshot; `mark_unsupported` never overwrites a known snapshot.

use crate::event::RateLimitInfo;

/// The facts one rate-limit report carried.
pub type RateLimitSnapshot = RateLimitInfo;

/// What the host knows about a vendor's rate limits.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum RateLimitState {
    /// Nothing has been observed or probed yet.
    #[default]
    NoData,
    /// The last probe failed. `last_known` is the most recent snapshot seen before it.
    ProbeFailed {
        message: String,
        last_known: Option<RateLimitSnapshot>,
    },
    /// The vendor does not report rate limits here.
    Unsupported { reason: String },
    /// A snapshot was observed.
    Known { snapshot: RateLimitSnapshot },
}

impl RateLimitState {
    /// Record a report. Always wins over the earlier state.
    pub fn observe(&mut self, snapshot: RateLimitSnapshot) {
        *self = RateLimitState::Known { snapshot };
    }

    /// Record a failed probe, keeping the last known snapshot.
    pub fn probe_failed(&mut self, message: impl Into<String>) {
        let last_known = self.snapshot().cloned();
        *self = RateLimitState::ProbeFailed {
            message: message.into(),
            last_known,
        };
    }

    /// Record that the vendor reports no limits. A known snapshot is kept.
    pub fn mark_unsupported(&mut self, reason: impl Into<String>) {
        if matches!(self, RateLimitState::Known { .. }) {
            return;
        }
        *self = RateLimitState::Unsupported {
            reason: reason.into(),
        };
    }

    /// The snapshot the host can show now: the observed one, or after a failed probe the last
    /// known one. `None` otherwise.
    pub fn snapshot(&self) -> Option<&RateLimitSnapshot> {
        match self {
            RateLimitState::Known { snapshot } => Some(snapshot),
            RateLimitState::ProbeFailed { last_known, .. } => last_known.as_ref(),
            RateLimitState::NoData | RateLimitState::Unsupported { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(status: &str) -> RateLimitSnapshot {
        RateLimitSnapshot {
            status: status.to_string(),
            ..RateLimitSnapshot::default()
        }
    }

    #[test]
    fn data_beats_unsupported_and_probe_failure() {
        let mut state = RateLimitState::default();
        assert_eq!(state.snapshot(), None);
        state.mark_unsupported("no limits");
        state.observe(snapshot("allowed"));
        assert_eq!(state.snapshot(), Some(&snapshot("allowed")));
    }

    #[test]
    fn unsupported_does_not_hide_known_data() {
        let mut state = RateLimitState::default();
        state.observe(snapshot("allowed"));
        state.mark_unsupported("no limits");
        assert_eq!(state.snapshot(), Some(&snapshot("allowed")));
    }

    #[test]
    fn failed_probe_keeps_last_known_snapshot() {
        let mut state = RateLimitState::default();
        state.probe_failed("timeout");
        assert_eq!(state.snapshot(), None);
        state.observe(snapshot("allowed_warning"));
        state.probe_failed("timeout");
        assert_eq!(state.snapshot(), Some(&snapshot("allowed_warning")));
        assert!(matches!(
            state,
            RateLimitState::ProbeFailed { ref message, .. } if message == "timeout"
        ));
    }
}
