//! Why a supervisor's health probe failed, and the run of failures since the last success
//! (Pulse #567, ES-1, ES-3, #481).
//!
//! Every [`ProbeFailure`] still counts as one failed probe for the supervisor
//! ([`crate::supervisor`]); the type only keeps the reason that a `bool` throws away. Keys are
//! typed and short ([`ProbeFailure::key`]): a daemon error's message can carry an OS error with a
//! path, so only its code is kept (privacy).
//!
//! A probe holds one connection; it never goes through accept again, so without a canary a full
//! connection table or a stuck accept loop would be invisible while the held connection keeps
//! answering. A host polls with [`CANARY_EVERY`] polls between fresh-connect canaries and counts a
//! canary failure as a failed probe only after [`CANARY_TRIP`] in a row, so one full connection
//! table for a few seconds is logged and not acted on, while a table or accept loop that stays
//! closed for minutes is a wedge, and is.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::{ConnectError, ERROR_TIMEOUT};

/// A probe polls this many times between fresh-connect canaries.
pub const CANARY_EVERY: u32 = 15;
/// Consecutive failed canaries before a canary failure counts as a failed probe.
pub const CANARY_TRIP: u32 = 6;
/// Most distinct reasons one streak keeps (a flapping reason, such as a changing pid, cannot grow
/// the map without limit).
const MAX_REASONS: usize = 16;

/// Why one probe failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeFailure {
    /// No record, unreachable, hello refused or similar: the daemon's error code only.
    Connect { code: String },
    /// The host behind the socket is not the expected executable.
    HostIdentity,
    /// The record names a process other than the child the supervisor launched.
    ForeignPid { record: u32, child: u32 },
    /// The stage that ran out of its bound: `connect` (connect and hello) or a call name.
    Timeout(&'static str),
    /// The doctor call answered `ready: false`, with its `reason`.
    NotReady { reason: String },
    /// The doctor call returned an error (its code only, as for `Connect`).
    Call { code: String },
    /// The periodic fresh connect failed while the held connection was fine.
    Canary(Box<ProbeFailure>),
}

impl ProbeFailure {
    /// A short stable key: what the streak counts by and what a log line names.
    pub fn key(&self) -> String {
        match self {
            ProbeFailure::Connect { code } => format!("connect:{code}"),
            ProbeFailure::HostIdentity => "host_identity".into(),
            ProbeFailure::ForeignPid { record, child } => {
                format!("foreign_pid:record={record},child={child}")
            }
            ProbeFailure::Timeout(stage) => format!("timeout:{stage}"),
            ProbeFailure::NotReady { reason } => format!("not_ready:{reason}"),
            ProbeFailure::Call { code } => format!("call:{code}"),
            ProbeFailure::Canary(inner) => format!("canary:{}", inner.key()),
        }
    }

    /// The failure a connect error is: a timeout stage, a host identity refusal or the daemon's
    /// error code.
    pub fn from_connect(error: ConnectError) -> Self {
        match error {
            ConnectError::Service(e) if e.code == ERROR_TIMEOUT => ProbeFailure::Timeout("connect"),
            ConnectError::Service(e) => ProbeFailure::Connect { code: e.code },
            ConnectError::HostIdentity(_) => ProbeFailure::HostIdentity,
        }
    }
}

/// A text cut to 120 characters, for a reason taken from a peer.
pub fn bounded(text: &str) -> String {
    text.chars().take(120).collect()
}

/// The run of failed probes since the last success; shared by the probe (writer) and the
/// supervisor's event callback (a trip reads it).
#[derive(Default)]
pub struct ProbeStreak(Mutex<Streak>);

#[derive(Default)]
struct Streak {
    total: u32,
    reasons: BTreeMap<String, u32>,
}

impl ProbeStreak {
    /// Counts one failure; returns `(first of a streak, failures in the streak)`.
    pub fn failed(&self, failure: &ProbeFailure) -> (bool, u32) {
        let mut streak = self.0.lock().unwrap_or_else(|e| e.into_inner());
        streak.total = streak.total.saturating_add(1);
        let key = failure.key();
        if streak.reasons.len() < MAX_REASONS || streak.reasons.contains_key(&key) {
            *streak.reasons.entry(key).or_insert(0) += 1;
        }
        (streak.total == 1, streak.total)
    }

    /// A probe succeeded: the streak is over.
    pub fn ok(&self) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Streak::default();
    }

    /// `{ "streak": n, "reasons": { key: count } }` for a trip's log line.
    pub fn snapshot(&self) -> serde_json::Value {
        let streak = self.0.lock().unwrap_or_else(|e| e.into_inner());
        serde_json::json!({ "streak": streak.total, "reasons": streak.reasons })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streak_counts_and_resets() {
        let streak = ProbeStreak::default();
        let failure = ProbeFailure::HostIdentity;
        assert_eq!(streak.failed(&failure), (true, 1));
        assert_eq!(streak.failed(&failure), (false, 2));
        assert_eq!(streak.snapshot()["reasons"]["host_identity"], 2);
        streak.ok();
        assert_eq!(streak.failed(&failure), (true, 1));
    }

    #[test]
    fn keys_are_stable() {
        assert_eq!(
            ProbeFailure::Canary(Box::new(ProbeFailure::Timeout("connect"))).key(),
            "canary:timeout:connect"
        );
        assert_eq!(
            ProbeFailure::ForeignPid {
                record: 1,
                child: 2
            }
            .key(),
            "foreign_pid:record=1,child=2"
        );
    }
}
