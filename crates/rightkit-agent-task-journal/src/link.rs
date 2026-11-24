//! Device-link request replay memory, kept in the journal (`pulse_link_requests`, schema 2).
//!
//! A device-link lease is at-least-once: after a reconnect or a restart the same signed request
//! arrives again. The row written here is how the second delivery gets the first one's answer
//! instead of running the work twice. The table keeps the name it was created under, so an existing
//! journal opens as it is.
//!
//! One transaction. An accepted request's row is inserted by the store hook (`schema::JournalHook`)
//! inside the transaction that commits the task row and its `task.accepted` event, so a crash
//! leaves both or neither. `pulse_link_requests.request_id` is the primary key: when two
//! deliveries race (two engines on one journal; one process serialises them on the gate), the
//! second insert fails, its task rolls back, and `Journal::submit_linked` returns the row the first
//! one wrote: the original task, or a refusal when the content differs.
//!
//! The types here are the journal's own; the product maps them to its transport's (`LinkEntry` is
//! what the authorizer judged `Fresh`).

use std::sync::{Arc, Mutex};

use rightkit_fs::digest::hex;
use rightkit_job_queue::rusqlite::{params, Connection, OptionalExtension};
use rightkit_job_queue::{JobError, JobResult};

use crate::schema::now_ms;

/// The link request the acceptance in progress belongs to. Set by `Journal::submit_linked` under
/// the journal gate, read by the store hook inside the acceptance transaction, cleared after.
pub(crate) type PendingLink = Arc<Mutex<Option<LinkEntry>>>;

/// Error code the hook raises when the row's key already exists. The acceptance rolls back.
pub(crate) const CONFLICT: &str = "link_replay_conflict";

/// The stable code a link request fails with when the replay store cannot be used.
pub const UNAVAILABLE_CODE: &str = "journal_unavailable";

/// Default margin a row outlives `expires_at_ms` by (the clock skew the authorizer tolerates), so a
/// request is never forgotten while it could still be accepted. `JournalHost::link_prune_margin_ms`
/// overrides it.
pub const DEFAULT_LINK_PRUNE_MARGIN_MS: u64 = 30_000;

/// What a request id left behind: one authorized request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkEntry {
    pub request_id: String,
    pub device_id: String,
    pub pairing_id: String,
    /// SHA-256 of the signed request: its canonical signing bytes and its signature.
    pub body_sha256: [u8; 32],
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

/// How a request ended, as stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkOutcome {
    Accepted {
        task_id: String,
    },
    /// Refused after authorization (for example a missing owner approval); `reason` is the
    /// stable refusal code, never request content.
    Refused {
        reason: String,
    },
}

/// A stored row, as the journal returns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkRecord {
    pub entry: LinkEntry,
    pub outcome: LinkOutcome,
}

fn int(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn uint(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Inserts one row. `Ok(false)` when `request_id` already has a row (nothing written).
fn insert(
    conn: &Connection,
    entry: &LinkEntry,
    outcome: &str,
    task_id: Option<&str>,
    reason: Option<&str>,
) -> JobResult<bool> {
    let changed = conn.execute(
        "INSERT INTO pulse_link_requests \
         (request_id, device_id, pairing_id, body_sha256, issued_at_ms, expires_at_ms, \
          outcome, task_id, reason, created_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(request_id) DO NOTHING",
        params![
            entry.request_id,
            entry.device_id,
            entry.pairing_id,
            hex(&entry.body_sha256),
            int(entry.issued_at_ms),
            int(entry.expires_at_ms),
            outcome,
            task_id,
            reason,
            now_ms()
        ],
    )?;
    Ok(changed > 0)
}

/// The accepted row, inside the acceptance transaction (called by the store hook). A key that
/// already exists fails the transaction with [`CONFLICT`].
pub(crate) fn insert_accepted(
    conn: &Connection,
    entry: &LinkEntry,
    task_id: &str,
) -> JobResult<()> {
    if insert(conn, entry, "accepted", Some(task_id), None)? {
        Ok(())
    } else {
        Err(JobError::new(
            CONFLICT,
            "this link request id already has a recorded outcome",
        ))
    }
}

/// A refusal row. `Ok(false)` when the id already had a row.
pub(crate) fn insert_refused(
    conn: &Connection,
    entry: &LinkEntry,
    reason: &str,
) -> JobResult<bool> {
    insert(conn, entry, "refused", None, Some(reason))
}

/// An accepted row with no new task (the idempotency key already resolved to one). Not part of
/// an acceptance, so it is its own write. `Ok(false)` when the id already had a row.
pub(crate) fn insert_accepted_existing(
    conn: &Connection,
    entry: &LinkEntry,
    task_id: &str,
) -> JobResult<bool> {
    insert(conn, entry, "accepted", Some(task_id), None)
}

/// Reads one row. Read only.
pub(crate) fn read_row(conn: &Connection, request_id: &str) -> JobResult<Option<LinkRecord>> {
    let row = conn
        .query_row(
            "SELECT device_id, pairing_id, body_sha256, issued_at_ms, expires_at_ms, \
                    outcome, task_id, reason \
             FROM pulse_link_requests WHERE request_id=?1",
            [request_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, Option<String>>(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((device_id, pairing_id, sha, issued, expires, outcome, task_id, reason)) = row else {
        return Ok(None);
    };
    let unreadable = || {
        JobError::new(
            "journal_schema_unreadable",
            "a link request row is unreadable",
        )
    };
    let outcome = match (outcome.as_str(), task_id, reason) {
        ("accepted", Some(task_id), _) => LinkOutcome::Accepted { task_id },
        ("refused", _, reason) => LinkOutcome::Refused {
            reason: reason.unwrap_or_default(),
        },
        _ => return Err(unreadable()),
    };
    Ok(Some(LinkRecord {
        entry: LinkEntry {
            request_id: request_id.to_string(),
            device_id,
            pairing_id,
            body_sha256: rightkit_fs::digest::unhex(&sha)
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(unreadable)?,
            issued_at_ms: uint(issued),
            expires_at_ms: uint(expires),
        },
        outcome,
    }))
}

/// Deletes rows whose `expires_at_ms` plus the margin is past. Returns the count.
pub(crate) fn prune(conn: &Connection, now_ms: i64, margin_ms: u64) -> JobResult<usize> {
    let cutoff = now_ms.saturating_sub(int(margin_ms));
    Ok(conn.execute(
        "DELETE FROM pulse_link_requests WHERE expires_at_ms < ?1",
        [cutoff],
    )?)
}

/// What a link request's acceptance came to (`Journal::submit_linked`).
#[derive(Debug, Clone, PartialEq)]
pub enum LinkSubmit {
    /// Accepted now; the `task.submit` receipt.
    Accepted(serde_json::Value),
    /// The same signed request was handled before. `receipt` is the original task's receipt
    /// (`deduplicated: true`) when it was accepted, none for a stored refusal.
    Duplicate {
        outcome: LinkOutcome,
        receipt: Option<serde_json::Value>,
    },
    /// The request id exists with different content. The caller refuses.
    Mismatch,
}
