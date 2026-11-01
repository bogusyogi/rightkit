//! WP-07 — Transactional outbox for the autonomous workflow control plane.
//!
//! The *outbox* is the canonical place where the workflow plane records
//! "this run needs the world to know about X" requests. Each request is
//! journaled atomically with the lifecycle transition that produced it
//! (so a crash between the transition and the outbox append is
//! impossible), and is later drained by a separate dispatcher that
//! delivers the request to the matching channel (UI, dead-letter,
//! delivery service, etc.).
//!
//! ## Why a transactional outbox
//!
//! Without an outbox, the workflow plane would have to choose between:
//!
//! - **At-least-once delivery** (call the channel *after* journaling the
//!   transition) — which loses messages on crash between the two steps.
//! - **At-most-once delivery** (call the channel *before* journaling) —
//!   which can deliver messages that the journal later rolls back.
//!
//! The transactional outbox pattern is the standard fix: the journal and
//! the outbox row are written in the **same transaction**. The dispatcher
//! is a separate process that reads `Pending` outbox rows, delivers them,
//! and marks them `Delivered`. A crashed dispatcher simply re-reads the
//! same rows; idempotency on the receiver side is the caller's job.
//!
//! ## Outbox kinds
//!
//! [`OutboxKind`] is closed. Adding a new variant is a journal-shape
//! change that requires a migration — do not extend it without the
//! storage team's review.

#![allow(clippy::module_name_repetitions)]

use serde::{Deserialize, Serialize};

// ─── OutboxKind ──────────────────────────────────────────────────────────────

/// The closed set of outbox request kinds. Each kind maps to exactly one
/// delivery channel and one projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxKind {
    /// Approval request — surfaces in the user's "needs review" inbox.
    Approval,
    /// Input request — surfaces when a node is blocked waiting on user
    /// input.
    Input,
    /// Policy block — surfaces when a node attempted an action that was
    /// refused by the policy layer. Distinct from
    /// [`OutboxKind::Failure`] because policy blocks are *expected* and
    /// are not considered run failures.
    PolicyBlock,
    /// Dead letter — surfaces when a run has reached a terminal failure
    /// state that the workflow plane cannot recover from automatically.
    DeadLetter,
    /// Generic failure — surfaces when a node attempt failed with a
    /// failure class in the retryable set.
    Failure,
    /// Uncertain effect — surfaces when an effect landed in
    /// [`crate::control_plane::effect::EffectOutcome::OutcomeUnknown`] or
    /// [`crate::control_plane::effect::EffectOutcome::RequiresReconciliation`].
    UncertainEffect,
    /// User-facing delivery — surfaces when a run produced a deliverable
    /// artifact (e.g. a drafted document).
    Deliverable,
}

impl OutboxKind {
    /// Stable lowercase discriminator suitable for logging, error
    /// envelopes, and SQL filtering.
    pub const fn as_str(&self) -> &'static str {
        match self {
            OutboxKind::Approval => "approval",
            OutboxKind::Input => "input",
            OutboxKind::PolicyBlock => "policy_block",
            OutboxKind::DeadLetter => "dead_letter",
            OutboxKind::Failure => "failure",
            OutboxKind::UncertainEffect => "uncertain_effect",
            OutboxKind::Deliverable => "deliverable",
        }
    }
}

// ─── OutboxStatus ────────────────────────────────────────────────────────────

/// Lifecycle status of one outbox row. A row moves `Pending` → `Delivered`
/// (or `Pending` → `Failed`) exactly once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxStatus {
    /// Row has been journaled but not yet delivered.
    Pending,
    /// Dispatcher has successfully delivered the row.
    Delivered,
    /// Dispatcher has tried and given up on the row. The run is not
    /// affected — the outbox is purely a delivery side-channel — but the
    /// failure is recorded so operators can investigate.
    Failed,
}

// ─── OutboxItem ──────────────────────────────────────────────────────────────

/// One journaled outbox row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxItem {
    /// `wf_outbox_*` — namespaced id assigned at journal time.
    pub id: String,
    /// `wfr_*` — namespaced run id this row belongs to.
    pub run_id: String,
    /// Delivery kind.
    pub kind: OutboxKind,
    /// Canonical-JSON-encoded payload. The dispatcher MUST treat this as
    /// opaque — the workflow plane controls the schema.
    pub payload_json: String,
    /// Current status.
    pub status: OutboxStatus,
    /// Absolute Unix epoch millisecond at which this row was journaled.
    pub created_at_unix_ms: i64,
    /// Absolute Unix epoch millisecond at which the dispatcher marked the
    /// row delivered, or `None` if it has not been delivered yet.
    pub delivered_at_unix_ms: Option<i64>,
}

// ─── mark_delivered ──────────────────────────────────────────────────────────

/// Mark `item` as delivered at `now_unix_ms`. Sets both
/// [`OutboxItem::status`] to [`OutboxStatus::Delivered`] and
/// [`OutboxItem::delivered_at_unix_ms`] to `Some(now_unix_ms)`.
///
/// This is the only mutation the dispatcher is allowed to perform on a
/// `Pending` row; once delivered, the row is read-only for replay.
pub fn mark_delivered(item: &mut OutboxItem, now_unix_ms: i64) {
    item.status = OutboxStatus::Delivered;
    item.delivered_at_unix_ms = Some(now_unix_ms);
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn item(status: OutboxStatus) -> OutboxItem {
        OutboxItem {
            id: "wf_outbox_1".into(),
            run_id: "wfr_run".into(),
            kind: OutboxKind::Approval,
            payload_json: "{}".into(),
            status,
            created_at_unix_ms: 1_000,
            delivered_at_unix_ms: None,
        }
    }

    #[test]
    fn mark_delivered_sets_status_and_timestamp() {
        let mut i = item(OutboxStatus::Pending);
        assert_eq!(i.status, OutboxStatus::Pending);
        assert!(i.delivered_at_unix_ms.is_none());

        mark_delivered(&mut i, 5_000);

        assert_eq!(i.status, OutboxStatus::Delivered);
        assert_eq!(i.delivered_at_unix_ms, Some(5_000));
    }

    #[test]
    fn mark_delivered_overwrites_a_previous_timestamp() {
        // If the dispatcher retries (e.g. after a crash) and the row was
        // re-marked, the latest timestamp wins. The row is in
        // [`OutboxStatus::Delivered`] state, so a future mark is a
        // dispatcher bug — but the function is total and just overwrites.
        let mut i = item(OutboxStatus::Pending);
        mark_delivered(&mut i, 5_000);
        mark_delivered(&mut i, 7_500);
        assert_eq!(i.delivered_at_unix_ms, Some(7_500));
    }

    #[test]
    fn outbox_kind_as_str_is_stable() {
        assert_eq!(OutboxKind::Approval.as_str(), "approval");
        assert_eq!(OutboxKind::Input.as_str(), "input");
        assert_eq!(OutboxKind::PolicyBlock.as_str(), "policy_block");
        assert_eq!(OutboxKind::DeadLetter.as_str(), "dead_letter");
        assert_eq!(OutboxKind::Failure.as_str(), "failure");
        assert_eq!(OutboxKind::UncertainEffect.as_str(), "uncertain_effect");
        assert_eq!(OutboxKind::Deliverable.as_str(), "deliverable");
    }

    #[test]
    fn outbox_kind_round_trips_via_serde() {
        for k in [
            OutboxKind::Approval,
            OutboxKind::Input,
            OutboxKind::PolicyBlock,
            OutboxKind::DeadLetter,
            OutboxKind::Failure,
            OutboxKind::UncertainEffect,
            OutboxKind::Deliverable,
        ] {
            let s = serde_json::to_string(&k).expect("serialize");
            let back: OutboxKind = serde_json::from_str(&s).expect("deserialize");
            assert_eq!(back, k);
        }
    }

    #[test]
    fn outbox_status_round_trips_via_serde() {
        for s in [
            OutboxStatus::Pending,
            OutboxStatus::Delivered,
            OutboxStatus::Failed,
        ] {
            let j = serde_json::to_string(&s).expect("serialize");
            let back: OutboxStatus = serde_json::from_str(&j).expect("deserialize");
            assert_eq!(back, s);
        }
    }

    #[test]
    fn outbox_item_round_trips_via_serde_with_delivered_timestamp() {
        let mut i = item(OutboxStatus::Pending);
        mark_delivered(&mut i, 9_999);
        let j = serde_json::to_string(&i).expect("serialize");
        let back: OutboxItem = serde_json::from_str(&j).expect("deserialize");
        assert_eq!(back, i);
        assert_eq!(back.delivered_at_unix_ms, Some(9_999));
    }
}
