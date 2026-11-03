//! Owner epochs and the transactional outbox (opt-in: `StoreConfig::owner_epochs`,
//! `StoreConfig::outbox`).
//!
//! Owner epoch. `rk_owner` holds one counter for the database. The process that owns the store
//! claims a new epoch when it starts ([`JobTable::claim_owner_epoch`],
//! [`JobStore::claim_owner_epoch`](super::JobStore::claim_owner_epoch)): the counter rises by one
//! and, in the same transaction, so does the attempt generation of every non-terminal job. Every
//! write an attempt of an older owner makes carries the generation it held, so it fails with
//! `stale_attempt` and changes nothing: the owner epoch and the attempt fence are one mechanism,
//! not two. A job can record the epoch that leased it in an app-declared extension column.
//!
//! Outbox. `rk_outbox` is an append-only log of events a consumer must see after the state change
//! that caused them. [`JobTable::outbox_append`] runs on the caller's connection, so the event
//! commits or rolls back with the writes beside it: no event for a change that did not happen,
//! none lost for one that did. A consumer reads past its own durable cursor
//! ([`JobTable::outbox_read`]), publishes, then advances the cursor
//! ([`JobTable::outbox_advance`]); a crash between the two publishes the rows again, so delivery
//! is at-least-once and each entry carries its `seq` for a consumer to drop repeats. An entry is
//! never readable before its transaction commits. Removing a job (`delete`, `prune`) removes its
//! outbox rows.

use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use super::table::atomic;
use super::JobTable;
use crate::error::{JobError, JobResult};

/// Crate-owned table of the outbox (feature `StoreConfig::outbox`).
pub const OUTBOX_TABLE: &str = "rk_outbox";
/// Crate-owned table of the consumers' durable outbox cursors.
pub const OUTBOX_CURSOR_TABLE: &str = "rk_outbox_cursor";
/// Crate-owned table holding the owner epoch (`StoreConfig::owner_epochs`).
pub const OWNER_TABLE: &str = "rk_owner";
/// Largest page [`JobTable::outbox_read`] returns.
pub const MAX_OUTBOX_READ: usize = 1000;

pub(super) const OWNER_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS rk_owner (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  epoch INTEGER NOT NULL
);
";

pub(super) const OUTBOX_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS rk_outbox (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  topic TEXT NOT NULL,
  job_id TEXT,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS rk_outbox_job ON rk_outbox(job_id) WHERE job_id IS NOT NULL;
CREATE TABLE IF NOT EXISTS rk_outbox_cursor (
  consumer TEXT PRIMARY KEY,
  seq INTEGER NOT NULL
);
";

/// One outbox row.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxEntry {
    /// Strictly increasing, never reused: the order of commit of the appends.
    pub seq: i64,
    pub topic: String,
    /// The job the event is about, when it is about one.
    pub job_id: Option<String>,
    pub payload: Value,
    pub created_at: String,
}

fn not_configured(what: &str, setting: &str) -> JobError {
    JobError::new(
        "not_configured",
        format!("store has no {what} (StoreConfig::{setting})"),
    )
}

impl JobTable {
    fn need_outbox(&self) -> JobResult<()> {
        if self.config().outbox {
            Ok(())
        } else {
            Err(not_configured("outbox", "outbox"))
        }
    }

    fn need_owner(&self) -> JobResult<()> {
        if self.config().owner_epochs {
            Ok(())
        } else {
            Err(not_configured("owner epoch", "owner_epochs"))
        }
    }

    /// Append an event to the outbox on `conn` and return its `seq`. `topic` must not be empty.
    /// Commit or roll back with the caller's transaction.
    pub fn outbox_append(
        &self,
        conn: &Connection,
        topic: &str,
        job_id: Option<&str>,
        payload: &Value,
    ) -> JobResult<i64> {
        self.need_outbox()?;
        if topic.is_empty() {
            return Err(JobError::bad_input("outbox topic is empty"));
        }
        conn.execute(
            "INSERT INTO rk_outbox (topic, job_id, payload_json, created_at) VALUES (?1,?2,?3,?4)",
            params![topic, job_id, payload.to_string(), self.config().now()],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// Up to `limit` (`1..=MAX_OUTBOX_READ`, else `bad_input`) entries with `seq` greater than
    /// `after`, oldest first.
    pub fn outbox_read(
        &self,
        conn: &Connection,
        after: i64,
        limit: usize,
    ) -> JobResult<Vec<OutboxEntry>> {
        self.need_outbox()?;
        if limit == 0 || limit > MAX_OUTBOX_READ {
            return Err(JobError::bad_input(format!(
                "outbox read limit must be 1..={MAX_OUTBOX_READ}"
            )));
        }
        let mut st = conn.prepare(
            "SELECT seq, topic, job_id, payload_json, created_at FROM rk_outbox \
             WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )?;
        let rows = st
            .query_map(params![after, limit as i64], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(seq, topic, job_id, payload, created_at)| {
                let payload = serde_json::from_str(&payload).map_err(|e| {
                    JobError::new("db", format!("outbox entry {seq} is not JSON: {e}"))
                })?;
                Ok(OutboxEntry {
                    seq,
                    topic,
                    job_id,
                    payload,
                    created_at,
                })
            })
            .collect()
    }

    /// The `seq` the consumer has published up to (0 for a consumer that has none yet).
    pub fn outbox_cursor(&self, conn: &Connection, consumer: &str) -> JobResult<i64> {
        self.need_outbox()?;
        Ok(conn
            .query_row(
                "SELECT seq FROM rk_outbox_cursor WHERE consumer=?1",
                [consumer],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    /// The entries the consumer has not yet published: [`JobTable::outbox_read`] past its cursor.
    pub fn outbox_pending(
        &self,
        conn: &Connection,
        consumer: &str,
        limit: usize,
    ) -> JobResult<Vec<OutboxEntry>> {
        let after = self.outbox_cursor(conn, consumer)?;
        self.outbox_read(conn, after, limit)
    }

    /// Record that `consumer` has published up to `seq`. The cursor only moves forward: a lower
    /// `seq` leaves it where it is. Returns the cursor after the call.
    pub fn outbox_advance(&self, conn: &Connection, consumer: &str, seq: i64) -> JobResult<i64> {
        self.need_outbox()?;
        if consumer.is_empty() {
            return Err(JobError::bad_input("outbox consumer name is empty"));
        }
        atomic(conn, || {
            conn.execute(
                "INSERT INTO rk_outbox_cursor (consumer, seq) VALUES (?1,?2) \
                 ON CONFLICT(consumer) DO UPDATE SET seq=MAX(seq, excluded.seq)",
                params![consumer, seq],
            )?;
            self.outbox_cursor(conn, consumer)
        })
    }

    /// Remove the entries every registered consumer has published past (up to the lowest
    /// cursor). Nothing is removed while no consumer has a cursor. Returns the rows removed.
    pub fn outbox_prune(&self, conn: &Connection) -> JobResult<usize> {
        self.need_outbox()?;
        Ok(conn.execute(
            "DELETE FROM rk_outbox WHERE seq <= COALESCE((SELECT MIN(seq) FROM rk_outbox_cursor), 0)",
            [],
        )?)
    }

    /// The owner epoch: 0 until the first claim.
    pub fn owner_epoch(&self, conn: &Connection) -> JobResult<i64> {
        self.need_owner()?;
        Ok(conn
            .query_row("SELECT epoch FROM rk_owner WHERE id=1", [], |r| r.get(0))
            .optional()?
            .unwrap_or(0))
    }

    /// Claim the next owner epoch on `conn` and return it. In the same savepoint the attempt
    /// generation of every non-terminal job rises by one (and its row revision, with a revision
    /// column), which fences out every attempt of an older owner. Commit it before leasing
    /// anything. Terminal jobs are untouched.
    pub fn claim_owner_epoch(&self, conn: &Connection) -> JobResult<i64> {
        self.need_owner()?;
        atomic(conn, || {
            conn.execute(
                "INSERT INTO rk_owner (id, epoch) VALUES (1, 1) \
                 ON CONFLICT(id) DO UPDATE SET epoch=epoch+1",
                [],
            )?;
            conn.execute(
                &format!(
                    "UPDATE {} SET attempt_gen=attempt_gen+1{} WHERE status IN {}",
                    self.name(),
                    self.rev_bump(),
                    self.config().statuses.sql_set()
                ),
                [],
            )?;
            self.owner_epoch(conn)
        })
    }
}
