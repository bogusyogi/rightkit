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
