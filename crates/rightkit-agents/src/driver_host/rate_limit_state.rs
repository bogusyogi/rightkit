//! Rate-limit availability state (issue #346).
//!
//! A single "no usage info" value conflates three different situations that need different UI and
//! different recovery:
//!
//! - `NoData`: nothing has been observed yet. The next rate-limit event moves to `Known`.
//! - `ProbeFailed`: asking the vendor for usage failed (network, auth, parse). Transient: the next
//!   successful observation recovers to `Known`. The last known snapshot is kept so a UI can show
//!   it as stale rather than blank.
//! - `Unsupported`: the vendor or plan does not report usage. Data that does arrive still wins,
//!   because arrival proves the vendor reports usage.
//!
//! The transition rules are `crate::rate_limit::RateLimitState`'s: observed data replaces
//! every other state, a failed probe keeps the last known snapshot (and replaces `Unsupported`),
//! and `mark_unsupported` never overwrites a `Known` snapshot. This module is the serde
//! projection the API serialises, plus the `NormalizedEventKind` entry point.

use crate::driver_host::NormalizedEventKind;
use crate::event::RateLimitInfo;
use crate::rate_limit as rk;
use serde::{Deserialize, Serialize};

/// Last observed rate-limit window, taken from a [`NormalizedEventKind::RateLimit`] event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitSnapshot {
    pub status: String,
    pub rate_limit_type: Option<String>,
    pub resets_at: Option<u64>,
    pub utilization: Option<f64>,
    pub overage_status: Option<String>,
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
#[derive(Default)]
pub enum RateLimitState {
    #[default]
    NoData,
    ProbeFailed {
        message: String,
        /// Snapshot held before the failure, if any.
        last_known: Option<Box<RateLimitSnapshot>>,
    },
    Unsupported {
        reason: String,
    },
    Known {
        snapshot: Box<RateLimitSnapshot>,
    },
}

impl From<&RateLimitSnapshot> for RateLimitInfo {
    fn from(snapshot: &RateLimitSnapshot) -> Self {
        RateLimitInfo {
            status: snapshot.status.clone(),
            rate_limit_type: snapshot.rate_limit_type.clone(),
            resets_at: snapshot.resets_at,
            utilization: snapshot.utilization,
            overage_status: snapshot.overage_status.clone(),
            error_code: snapshot.error_code.clone(),
        }
    }
}

impl From<RateLimitInfo> for RateLimitSnapshot {
    fn from(info: RateLimitInfo) -> Self {
        RateLimitSnapshot {
            status: info.status,
            rate_limit_type: info.rate_limit_type,
            resets_at: info.resets_at,
            utilization: info.utilization,
            overage_status: info.overage_status,
            error_code: info.error_code,
        }
    }
}

impl RateLimitState {
    fn to_rightkit(&self) -> rk::RateLimitState {
        match self {
            Self::NoData => rk::RateLimitState::NoData,
            Self::ProbeFailed {
                message,
                last_known,
            } => rk::RateLimitState::ProbeFailed {
                message: message.clone(),
                last_known: last_known.as_deref().map(RateLimitInfo::from),
            },
            Self::Unsupported { reason } => rk::RateLimitState::Unsupported {
                reason: reason.clone(),
            },
            Self::Known { snapshot } => rk::RateLimitState::Known {
                snapshot: RateLimitInfo::from(snapshot.as_ref()),
            },
        }
    }

    fn from_rightkit(state: rk::RateLimitState) -> Self {
        match state {
            rk::RateLimitState::NoData => Self::NoData,
            rk::RateLimitState::ProbeFailed {
                message,
                last_known,
            } => Self::ProbeFailed {
                message,
                last_known: last_known.map(|info| Box::new(RateLimitSnapshot::from(info))),
            },
            rk::RateLimitState::Unsupported { reason } => Self::Unsupported { reason },
            rk::RateLimitState::Known { snapshot } => Self::Known {
                snapshot: Box::new(RateLimitSnapshot::from(snapshot)),
            },
        }
    }

    /// Move to `Known` when `kind` is a rate-limit event. Returns whether it was one.
    pub fn observe(&mut self, kind: &NormalizedEventKind) -> bool {
        let NormalizedEventKind::RateLimit {
            status,
            rate_limit_type,
            resets_at,
            utilization,
            overage_status,
            error_code,
            ..
        } = kind
        else {
            return false;
        };
        let mut state = self.to_rightkit();
        state.observe(RateLimitInfo {
            status: status.clone(),
            rate_limit_type: rate_limit_type.clone(),
            resets_at: *resets_at,
            utilization: *utilization,
            overage_status: overage_status.clone(),
            error_code: error_code.clone(),
        });
        *self = Self::from_rightkit(state);
        true
    }

    /// Record a failed probe; `Known` keeps its snapshot as `last_known`.
    pub fn probe_failed(&mut self, message: impl Into<String>) {
        let mut state = self.to_rightkit();
        state.probe_failed(message);
        *self = Self::from_rightkit(state);
    }

    /// Record that the vendor does not report usage. A `Known` snapshot is kept.
    pub fn mark_unsupported(&mut self, reason: impl Into<String>) {
        let mut state = self.to_rightkit();
        state.mark_unsupported(reason);
        *self = Self::from_rightkit(state);
    }

    pub fn snapshot(&self) -> Option<&RateLimitSnapshot> {
        match self {
            Self::Known { snapshot } => Some(snapshot.as_ref()),
            Self::ProbeFailed { last_known, .. } => last_known.as_deref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limit(status: &str, utilization: f64) -> NormalizedEventKind {
        NormalizedEventKind::RateLimit {
            account: None,
            status: status.into(),
            rate_limit_type: Some("five_hour".into()),
            resets_at: Some(1_790_000_000),
            utilization: Some(utilization),
            overage_status: None,
            error_code: None,
        }
    }

    #[test]
    fn starts_as_no_data_and_ignores_unrelated_events() {
        let mut state = RateLimitState::default();
        assert_eq!(state, RateLimitState::NoData);
        assert!(!state.observe(&NormalizedEventKind::TurnCompleted));
        assert_eq!(state, RateLimitState::NoData);
    }

    #[test]
    fn the_three_states_are_distinct() {
        let no_data = RateLimitState::NoData;
        let mut failed = RateLimitState::NoData;
        failed.probe_failed("timeout");
        let mut unsupported = RateLimitState::NoData;
        unsupported.mark_unsupported("plan reports no usage");
        assert_ne!(no_data, failed);
        assert_ne!(no_data, unsupported);
        assert_ne!(failed, unsupported);
        assert!(matches!(failed, RateLimitState::ProbeFailed { .. }));
        assert!(matches!(unsupported, RateLimitState::Unsupported { .. }));
    }

    #[test]
    fn recovers_to_known_after_a_probe_failure() {
        let mut state = RateLimitState::NoData;
        state.probe_failed("network down");
        assert!(matches!(state, RateLimitState::ProbeFailed { .. }));
        assert!(state.observe(&limit("allowed", 0.2)));
        match &state {
            RateLimitState::Known { snapshot } => {
                assert_eq!(snapshot.status, "allowed");
                assert_eq!(snapshot.utilization, Some(0.2));
            }
            other => panic!("expected Known, got {other:?}"),
        }
    }

    #[test]
    fn failure_after_known_keeps_the_stale_snapshot_until_recovery() {
        let mut state = RateLimitState::NoData;
        state.observe(&limit("allowed_warning", 0.9));
        state.probe_failed("auth expired");
        state.probe_failed("auth expired again");
        match &state {
            RateLimitState::ProbeFailed {
                message,
                last_known,
            } => {
                assert_eq!(message, "auth expired again");
                assert_eq!(last_known.as_ref().unwrap().status, "allowed_warning");
            }
            other => panic!("expected ProbeFailed, got {other:?}"),
        }
        assert_eq!(state.snapshot().unwrap().utilization, Some(0.9));
        state.observe(&limit("allowed", 0.1));
        assert!(matches!(state, RateLimitState::Known { .. }));
    }

    #[test]
    fn unsupported_yields_to_real_data_and_never_hides_a_known_snapshot() {
        let mut state = RateLimitState::NoData;
        state.mark_unsupported("no usage endpoint");
        assert!(matches!(state, RateLimitState::Unsupported { .. }));
        assert!(state.observe(&limit("rejected", 1.0)));
        assert!(matches!(state, RateLimitState::Known { .. }));
        state.mark_unsupported("late");
        assert!(matches!(state, RateLimitState::Known { .. }));
        assert_eq!(state.snapshot().unwrap().status, "rejected");
    }

    #[test]
    fn state_serializes_with_a_camel_case_tag() {
        let mut state = RateLimitState::NoData;
        state.probe_failed("x");
        let value = serde_json::to_value(&state).unwrap();
        assert_eq!(value["state"], "probeFailed");
        assert_eq!(
            serde_json::to_value(RateLimitState::NoData).unwrap()["state"],
            "noData"
        );
    }
}
