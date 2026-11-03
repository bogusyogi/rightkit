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
