//! MailRight's two durable queues, `outbox` (sends) and `pending_ops` (offline
//! label mutations), as `rightkit-job-queue` job tables. The crate owns the row
//! shape, the status model and the transitions; every call here runs on the
//! caller's connection, so a queue write still commits with the local mail state
//! it belongs to.
//!
//! Outbox statuses (the crate's terminal `complete`/`cancelled` are the sent and
//! undone states; MailRight's "failed" is retryable, so it is the non-terminal
//! `send_failed`):
//!
//! | MailRight (API / UI) | job table                                   |
//! |----------------------|---------------------------------------------|
//! | queued               | `queued`, `send_at` NULL                    |
//! | scheduled            | `queued`, `send_at` set                     |
//! | failed               | `send_failed`                               |
//! | sent                 | `complete`                                  |
//! | sending / uncertain  | `sending` (never picked up by a drain)      |
//!
//! `sending` holds two kinds of row: the legacy in-flight rows, and a send whose delivery is
//! unknown (`outbox_update_status(.., "uncertain", ..)`: the request may have reached Gmail). No
//! drain, retry sweep or quit drain picks it up; the user checks Sent and cancels or resends.

use rightkit_db::rusqlite::Connection;
use rightkit_job_queue::{ExtColumn, JobError, JobRecord, JobTable, StatusModel, StoreConfig};
use std::sync::OnceLock;

pub(crate) const OUTBOX_TABLE: &str = "outbox";
pub(crate) const PENDING_OPS_TABLE: &str = "pending_ops";

pub(crate) const ST_QUEUED: &str = "queued";
pub(crate) const ST_SENDING: &str = "sending";
pub(crate) const ST_SEND_FAILED: &str = "send_failed";

/// Most rows a whole-queue view or a replay scan will read. The queues hold a
/// handful of rows; the cap bounds a runaway table, and a scan that reaches it
/// fails ([`scan_due`]) instead of returning a silently shortened queue.
pub(crate) const SCAN_CAP: usize = 10_000;

pub(crate) fn outbox_config() -> StoreConfig {
    StoreConfig::new()
        .jobs_table(OUTBOX_TABLE)
        .statuses(
            StatusModel::new(ST_QUEUED, [ST_QUEUED, ST_SENDING, ST_SEND_FAILED])
                .allow(ST_QUEUED, ST_SEND_FAILED)
                .allow(ST_QUEUED, ST_SENDING)
                .allow(ST_SEND_FAILED, ST_SENDING)
                .allow(ST_SENDING, ST_SEND_FAILED),
        )
        .column(ExtColumn::text("account_id").not_null())
        .column(ExtColumn::text("rfc822").not_null())
        .column(ExtColumn::text("draft_id"))
        .column(ExtColumn::integer("send_at"))
        .column(ExtColumn::integer("sent_at"))
        .column(ExtColumn::integer("created_ms").not_null())
        .column(ExtColumn::integer("next_attempt_at"))
        .due_column("next_attempt_at")
        .user_version(None)
}

pub(crate) fn pending_ops_config() -> StoreConfig {
    StoreConfig::new()
        .jobs_table(PENDING_OPS_TABLE)
        .statuses(StatusModel::new(ST_QUEUED, [ST_QUEUED]))
        .column(ExtColumn::text("account_id").not_null())
        .column(ExtColumn::integer("created_ms").not_null())
        .column(ExtColumn::integer("next_attempt_at"))
        .due_column("next_attempt_at")
        .user_version(None)
}

pub(crate) fn outbox_table() -> &'static JobTable {
    static T: OnceLock<JobTable> = OnceLock::new();
    T.get_or_init(|| JobTable::new(&outbox_config()).expect("outbox job table config is valid"))
}

pub(crate) fn pending_ops_table() -> &'static JobTable {
    static T: OnceLock<JobTable> = OnceLock::new();
    T.get_or_init(|| {
        JobTable::new(&pending_ops_config()).expect("pending_ops job table config is valid")
    })
}

/// Create both job tables (and the crate's side tables) on `conn`. Idempotent.
pub(crate) fn init_schemas(conn: &Connection) -> Result<(), JobError> {
    outbox_table().init_schema(conn)?;
    pending_ops_table().init_schema(conn)?;
    Ok(())
}

/// Every live row of `table`, oldest due first. Fails when the table holds more
/// than [`SCAN_CAP`] live rows: a view or replay built from a truncated scan
/// would hide queued sends and mutations, so the caller gets an error instead.
pub(crate) fn scan_due(conn: &Connection, table: &JobTable) -> Result<Vec<JobRecord>, String> {
    scan_due_capped(conn, table, SCAN_CAP)
}

/// [`scan_due`] with the cap injected (tests).
pub(crate) fn scan_due_capped(
    conn: &Connection,
    table: &JobTable,
    cap: usize,
) -> Result<Vec<JobRecord>, String> {
    // One row past the cap tells "exactly full" from "cut off".
    let rows = table
        .due(conn, &json!(i64::MAX), cap.saturating_add(1))
        .map_err(|e| format!("{}: {e}", table.name()))?;
    if rows.len() > cap {
        return Err(format!(
            "{} holds more than {cap} queued rows; refusing to show or replay a truncated queue",
            table.name()
        ));
    }
    Ok(rows)
}

/// Every live (non-terminal) row of `table`, whether or not it has a due time: a claimed send
/// clears its due time, so [`scan_due`] cannot see a row that is `sending` or parked. Same cap
/// and the same refusal to return a truncated list.
pub(crate) fn scan_live(conn: &Connection, table: &JobTable) -> Result<Vec<JobRecord>, String> {
    let name = table.name();
    let mut st = conn
        .prepare(&format!(
            "SELECT job_id FROM {name} WHERE status NOT IN ('complete', 'cancelled') ORDER BY created_at, rowid LIMIT ?1"
        ))
        .map_err(|e| format!("{name}: {e}"))?;
    let ids = st
        .query_map([(SCAN_CAP + 1) as i64], |r| r.get::<_, String>(0))
        .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
        .map_err(|e| format!("{name}: {e}"))?;
    if ids.len() > SCAN_CAP {
        return Err(format!(
            "{name} holds more than {SCAN_CAP} live rows; refusing to show or replay a truncated queue"
        ));
    }
    ids.iter()
        .map(|id| table.record(conn, id).map_err(|e| format!("{name}: {e}")))
        .collect()
}

/// Drop a finished row (sent-and-removed, undone, replayed). The crate has no
/// delete on a caller connection (`JobStore::delete`/`prune` own a connection),
/// so retention of terminal rows is this single statement; it never touches a
/// live row.
pub(crate) fn purge_terminal(
    conn: &Connection,
    table: &JobTable,
    job_id: &str,
) -> Result<(), String> {
    conn.execute(
        &format!(
            "DELETE FROM {} WHERE job_id = ?1 AND status IN ('complete', 'cancelled')",
            table.name()
        ),
        [job_id],
    )
    .map(|_| ())
    .map_err(|e| format!("purge {}: {e}", table.name()))
}

/// Remove every queued row of a removed account (the old `ON DELETE CASCADE`).
pub(crate) fn delete_account_rows(conn: &Connection, account_id: &str) -> Result<(), String> {
    for table in [OUTBOX_TABLE, PENDING_OPS_TABLE] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE account_id = ?1"),
            [account_id],
        )
        .map_err(|e| format!("delete {table}: {e}"))?;
    }
    Ok(())
}
