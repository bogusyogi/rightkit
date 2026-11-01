//! WP-07 — Durable timer / event waits for the autonomous workflow control
//! plane.
//!
//! A *durable wait* is the canonical, journalable representation of "this node
//! is now blocked and will resume when `X` happens". Two flavours are in V1:
//!
//! - [`WaitKind::Timer`] — wakes when `now_unix_ms >= wake_at_unix_ms`. The
//!   scheduler is responsible for re-issuing the resume command; the wait
//!   itself never matches inbound events.
//! - [`WaitKind::Event`] — wakes when an inbound observation matches the
//!   wait's `topic` *and* `filter_hash`. Deadline-bounded; once
//!   `now_unix_ms >= deadline_unix_ms` the wait is forced to a [`WaitResume::TimedOut`]
//!   resume.
//!
//! ADR-0021 constraint: wait matching is **pure**. The scheduler may call
//! [`matches_event`] from any number of contexts without observable side
//! effects. [`matches_event`] returns `false` for `Timer` waits so callers
//! never accidentally wake a timer on an inbound observation.
//!
//! ## Why a separate module
//!
//! Waits are not part of [`crate::control_plane::domain::NodeKind`] — they are the *result*
//! of a node entering a blocked phase, journaled alongside the lifecycle
//! transition. Keeping them in their own module makes it impossible for the
//! reducer to confuse "this node is an event wait" with "this node has
//! scheduled an event wait".

#![allow(clippy::module_name_repetitions)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Fresh per-task state fence. Runtime adapters use these ids as namespaces;
/// no transcript/provider/browser/tool/budget/evidence state is shared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskIsolationV1 {
    pub task_id: String,
    pub transcript_id: String,
    pub provider_id: String,
    pub browser_id: String,
    pub tool_state_id: String,
    pub budget_id: String,
    pub evidence_id: String,
}

impl TaskIsolationV1 {
    pub fn fresh(task_id: &str) -> Result<Self, String> {
        let task_id = task_id.trim();
        if task_id.is_empty() {
            return Err("task id is empty".into());
        }
        let scoped = |kind: &str| format!("{task_id}:{kind}");
        Ok(Self {
            task_id: task_id.to_string(),
            transcript_id: scoped("transcript"),
            provider_id: scoped("provider"),
            browser_id: scoped("browser"),
            tool_state_id: scoped("tools"),
            budget_id: scoped("budget"),
            evidence_id: scoped("evidence"),
        })
    }
}

// ─── WaitKind ────────────────────────────────────────────────────────────────

/// The two V1 wait flavours. The set is closed: adding a third flavour is a
/// breaking change because waits are journaled verbatim and the replay path
/// must agree on the deserialized shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitKind {
    /// Wake at an absolute wall-clock instant. `wake_at_unix_ms` is the
    /// instant at which the scheduler MUST issue a resume; it is recorded
    /// in the journal so replay is exact.
    Timer {
        /// Absolute Unix epoch millisecond at which this wait fires.
        wake_at_unix_ms: i64,
    },
    /// Wake when an inbound observation matches `topic` *and* `filter_hash`,
    /// or when the deadline elapses (whichever comes first).
    Event {
        /// Inbound observation topic. Examples: `wf.event.input`,
        /// `wf.event.review`, `wf.event.command`.
        topic: String,
        /// `sha256:`-prefixed content hash of the filter predicate (or of
        /// the empty filter when no predicate is supplied). The payload hash
        /// of the inbound observation is compared against this value so the
        /// wait cannot accidentally match a structurally different event.
        filter_hash: String,
        /// Absolute Unix epoch millisecond after which the wait is forced
        /// to a [`WaitResume::TimedOut`] resume. Equal to the node's
        /// declared `wait_deadline_ms` offset from admission.
        deadline_unix_ms: i64,
    },
}

// ─── DurableWait ─────────────────────────────────────────────────────────────

/// A durable wait registered against one node attempt. The journal writes
/// one row per wait; replays reconstruct the blocked set from these rows
/// alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableWait {
    /// `wfw_*` — namespaced id assigned at admission time.
    pub id: String,
    /// `wfr_*` — namespaced run id this wait belongs to.
    pub run_id: String,
    /// `wfn_*` — namespaced node id this wait is registered against.
    pub node_id: String,
    /// Timer or event.
    pub kind: WaitKind,
    /// Caller-supplied correlation key. Used to fan-in multiple waits on the
    /// same topic / payload (e.g. one inbound observation resumes every wait
    /// whose `correlation_key` matches).
    pub correlation_key: String,
    /// Generation fence. Bumped every time the wait's owning attempt is
    /// re-leased; the matcher MUST ignore observations whose own fence is
    /// less than or equal to this value (no zombie resumes).
    pub lease_fence: i64,
    /// Absolute Unix epoch millisecond when this wait row was journaled.
    pub created_at_unix_ms: i64,
}

// ─── WaitResume ──────────────────────────────────────────────────────────────

/// How a durable wait was resumed. The reducer accepts exactly one of these
/// variants per wait; anything else is a journal-integrity error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitResume {
    /// The wait matched an inbound observation. `observation_id` is the
    /// `wf_obs_*` row id for replay traceability.
    Resumed {
        /// Stable id of the inbound observation that satisfied the wait.
        observation_id: String,
    },
    /// The wait's deadline elapsed with no matching observation.
    TimedOut,
    /// The wait was cancelled (run cancellation, node re-admission, etc.).
    Cancelled,
}

// ─── matches_event ───────────────────────────────────────────────────────────

/// Pure matcher: returns `true` when `wait` (an `Event` variant) matches an
/// inbound observation with `topic` and `payload_hash`.
///
/// Timer waits never match events — the matcher returns `false` for the
/// [`WaitKind::Timer`] variant unconditionally. The scheduler uses this
/// asymmetry to safely fan-in inbound observations to all live event waits
/// without ever waking a timer prematurely.
pub fn matches_event(wait: &DurableWait, topic: &str, payload_hash: &str) -> bool {
    match &wait.kind {
        WaitKind::Event {
            topic: wait_topic,
            filter_hash,
            ..
        } => wait_topic == topic && filter_hash == payload_hash,
        WaitKind::Timer { .. } => false,
    }
}

/// Match a persisted compiled filter against an event exposed as
/// `inputs.event`. Corrupt or unbounded expressions fail closed.
pub fn matches_event_filter(expression: &str, expected_hash: &str, event: &Value) -> bool {
    let Ok(expr) = crate::control_plane::expression::parse(expression) else {
        return false;
    };
    if !crate::control_plane::expression::is_bounded(&expr, 64, 16)
        || crate::control_plane::domain::ContentHash::sha256(
            crate::control_plane::expression::canonical_text(&expr).as_bytes(),
        )
        .as_str()
            != expected_hash
    {
        return false;
    }
    let mut context = crate::control_plane::expression::EvalContext::empty();
    context.inputs.insert("event".into(), event.clone());
    matches!(
        crate::control_plane::expression::evaluate(&expr, &context),
        Ok(Value::Bool(true))
    )
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn event_wait(topic: &str, filter_hash: &str, deadline_ms: i64) -> DurableWait {
        DurableWait {
            id: "wfw_test".into(),
            run_id: "wfr_run".into(),
            node_id: "wfn_node".into(),
            kind: WaitKind::Event {
                topic: topic.into(),
                filter_hash: filter_hash.into(),
                deadline_unix_ms: deadline_ms,
            },
            correlation_key: "corr".into(),
            lease_fence: 1,
            created_at_unix_ms: 0,
        }
    }

    fn timer_wait(wake_ms: i64) -> DurableWait {
        DurableWait {
            id: "wfw_test".into(),
            run_id: "wfr_run".into(),
            node_id: "wfn_node".into(),
            kind: WaitKind::Timer {
                wake_at_unix_ms: wake_ms,
            },
            correlation_key: "corr".into(),
            lease_fence: 1,
            created_at_unix_ms: 0,
        }
    }

    #[test]
    fn event_wait_matches_on_topic_and_payload_hash() {
        let w = event_wait("wf.event.input", "sha256:abc", 1_000);
        assert!(matches_event(&w, "wf.event.input", "sha256:abc"));
    }

    #[test]
    fn event_wait_does_not_match_on_topic_mismatch() {
        let w = event_wait("wf.event.input", "sha256:abc", 1_000);
        assert!(!matches_event(&w, "wf.event.review", "sha256:abc"));
    }

    #[test]
    fn event_wait_does_not_match_on_payload_hash_mismatch() {
        let w = event_wait("wf.event.input", "sha256:abc", 1_000);
        assert!(!matches_event(&w, "wf.event.input", "sha256:def"));
    }

    #[test]
    fn timer_wait_never_matches_events() {
        let w = timer_wait(1_000);
        assert!(!matches_event(&w, "wf.event.input", "sha256:abc"));
        assert!(!matches_event(&w, "", ""));
    }

    #[test]
    fn compiled_filter_matches_normalized_event_and_rejects_hash_drift() {
        let expression = "(inputs.event.state == \"ready\")";
        let hash = crate::control_plane::domain::ContentHash::sha256(expression.as_bytes());
        let event = serde_json::json!({"state":"ready"});
        assert!(matches_event_filter(expression, hash.as_str(), &event));
        assert!(!matches_event_filter(expression, "sha256:corrupt", &event));
    }

    #[test]
    fn event_wait_with_empty_filter_and_topic_does_not_match_populated_event() {
        // Defensive: an empty topic or filter hash must still require both
        // sides to be empty before matching.
        let w = event_wait("", "", 1_000);
        assert!(matches_event(&w, "", ""));
        assert!(!matches_event(&w, "wf.event.input", ""));
    }

    #[test]
    fn wait_resume_serde_round_trip() {
        let r = WaitResume::Resumed {
            observation_id: "wf_obs_1".into(),
        };
        let s = serde_json::to_string(&r).expect("serialize");
        let back: WaitResume = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(r, back);

        let t = WaitResume::TimedOut;
        let s = serde_json::to_string(&t).expect("serialize");
        let back: WaitResume = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(t, back);

        let c = WaitResume::Cancelled;
        let s = serde_json::to_string(&c).expect("serialize");
        let back: WaitResume = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(c, back);
    }

    #[test]
    fn fresh_task_isolation_namespaces_every_state_surface() {
        let state = TaskIsolationV1::fresh("task-1").expect("state");
        assert!(state.transcript_id.starts_with("task-1:"));
        assert_ne!(state.transcript_id, state.provider_id);
        assert!(TaskIsolationV1::fresh("").is_err());
    }

    #[test]
    fn fresh_task_isolation_is_injective_across_all_surfaces_and_tasks() {
        // S:N3a — enforced isolation requires that no two tasks share any
        // surface id and that no single task collides across its own
        // surfaces. Fresh namespaces must be injective in both directions.
        let a = TaskIsolationV1::fresh("task-a").expect("state");
        let b = TaskIsolationV1::fresh("task-b").expect("state");
        let surfaces_a = [
            a.transcript_id.as_str(),
            a.provider_id.as_str(),
            a.browser_id.as_str(),
            a.tool_state_id.as_str(),
            a.budget_id.as_str(),
            a.evidence_id.as_str(),
        ];
        let surfaces_b = [
            b.transcript_id.as_str(),
            b.provider_id.as_str(),
            b.browser_id.as_str(),
            b.tool_state_id.as_str(),
            b.budget_id.as_str(),
            b.evidence_id.as_str(),
        ];
        assert_eq!(
            std::collections::HashSet::<&str>::from_iter(surfaces_a).len(),
            6,
            "one task's six surface ids must be pairwise distinct"
        );
        for (own, other) in surfaces_a.iter().zip(surfaces_b.iter()) {
            assert_ne!(own, other, "two tasks must never share a surface id");
        }
        for surface in surfaces_a {
            assert!(surface.starts_with("task-a:"), "{surface}");
        }
        for surface in surfaces_b {
            assert!(surface.starts_with("task-b:"), "{surface}");
        }
    }

    #[test]
    fn fresh_task_isolation_rejects_blank_and_whitespace_task_ids() {
        assert!(TaskIsolationV1::fresh("").is_err());
        assert!(TaskIsolationV1::fresh("   ").is_err());
    }
}
