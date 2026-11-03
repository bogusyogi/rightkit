//! WP-06 — Daemon-side normalized trigger-event inbox and occurrence
//! admission.
//!
//! This module owns three layers, in the order the data flows through
//! them:
//!
//! 1. [`event`] — the post-admission shape of a trigger event.
//!    `NormalizedTriggerEvent`, the trust envelope, and the persisted
//!    projections live here.
//! 2. [`dedupe`] — stable event-id / content-hash / dedupe-key derivation.
//!    The same helper is used by the live write path and the recovery
//!    worker so restarts and replays converge.
//! 3. [`inbox`] — the daemon-side facade that turns a
//!    `NormalizedTriggerEvent` into a row in `workflow_trigger_events`.
//!    Uses [`rightkit_agent_engine_storage::WorkflowTriggerWriter`] for the idempotent
//!    insert.
//! 4. [`occurrence`] — typed admission policy. Combines the trigger's
//!    `OccurrencePolicy` (dedupe / overlap / misfire / version pin) with
//!    the daemon's wall-clock view to produce one of
//!    `Admit` / `Coalesce` / `Skip` / `Queue` / `Replace` / `Reject`.
//!
//! The upstream `api_triggers` delivery route is responsible for
//! authentication (HMAC), replay suppression, body-size limits, workspace
//! validation, and trust classification; by the time a value reaches
//! [`inbox::TriggerInbox::admit_normalized`] it has already been
//! authenticated and shaped into a bounded [`NormalizedTriggerEvent`].
//!
//! ## Constraint summary
//!
//! - Every value here is built once. Re-deriving the event id from
//!   `(source, external_id, time)` is the only way to reproduce the id
//!   across restarts and replays; [`dedupe::derive_event_id`] exposes
//!   that derivation.
//! - The trigger id (when present) must use the `wft_` namespace prefix.
//! - The `(source, dedupe_key)` tuple is the storage layer's
//!   idempotency key. Two events with the same tuple but different
//!   content hashes will not collide because `dedupe_key` already folds
//!   in `content_hash`.

#![forbid(unsafe_code)]

pub mod dedupe;
pub mod event;
pub mod inbox;
pub mod occurrence;

// Public surface — what the rest of the daemon reaches into this module
// for. Kept narrow so internal refactors of `dedupe` / `inbox` /
// `occurrence` don't ripple into the rest of the workspace.
pub use dedupe::{
    build_normalized_canonical_json, compute_content_hash, derive_dedupe_key, derive_event_id,
    truncate_bytes,
};
pub use event::{
    format_unix_ms_as_iso, parse_iso_as_unix_ms, InboxAuditRow, NormalizedData,
    NormalizedDataError, NormalizedTriggerEvent, PersistedEventRow, PersistedOccurrence,
    RawArtifact, TrustFinding, TrustInfo, TrustLevel, TrustOrigin, TrustSeverity,
};
pub use inbox::{build_audit_row, InboxAdmission, InboxError, TriggerInbox};
pub use occurrence::{
    OccurrenceAdmissionLayer, OccurrenceDecision, OccurrenceError, OccurrenceInputs,
};
