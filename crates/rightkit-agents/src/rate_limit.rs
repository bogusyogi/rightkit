//! Rate-limit state for one vendor agent: what the host last knew, and why it is not known
//! now.
//!
//! Pure state with no I/O. Feed it [`AgentEvent::RateLimit`](crate::AgentEvent::RateLimit)
//! payloads through [`RateLimitState::observe`], a failed probe through
//! [`RateLimitState::probe_failed`], and a vendor that reports no limits through
//! [`RateLimitState::mark_unsupported`].
//!
//! Rules (aligned with the CodeRight `RateLimitState`, issue #346, whose tests pin them):
//! - observed data replaces every other state;
//! - a failed probe keeps the last known snapshot, but does not replace `Unsupported`;
//! - `mark_unsupported` replaces every state, including `Known`.
//!
//! The type serialises as camelCase JSON tagged by `state`, for the host API.

use serde::{Deserialize, Serialize};

use crate::event::RateLimitInfo;

/// The facts one rate-limit report carried.
pub type RateLimitSnapshot = RateLimitInfo;

/// What the host knows about a vendor's rate limits.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
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

    /// Record a failed probe, keeping the last known snapshot. An `Unsupported` vendor stays
    /// unsupported: a failed probe says nothing about whether limits exist.
    pub fn probe_failed(&mut self, message: impl Into<String>) {
        if matches!(self, RateLimitState::Unsupported { .. }) {
            return;
        }
        let last_known = self.snapshot().cloned();
        *self = RateLimitState::ProbeFailed {
            message: message.into(),
            last_known,
        };
    }

    /// Record that the vendor reports no limits. Replaces any state, a known snapshot too.
    pub fn mark_unsupported(&mut self, reason: impl Into<String>) {
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
