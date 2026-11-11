//! The audit row of one gateway call and where it goes.
//!
//! One row per call, written by the gate ([`crate::gate::conclude`]), never by a handler. A row
//! holds who called, by which door, the method, the outcome and non-content facts (counts, ids,
//! codes, argument shapes): never a path, a message body, a search term or a key.

use crate::Origin;
use serde_json::{Map, Value};

/// One audit row.
#[derive(Clone, Debug, Default)]
pub struct Entry<'a> {
    /// The paired agent's id, when the caller is one.
    pub agent_id: Option<&'a str>,
    /// The agent's name, or the client label the caller gave.
    pub agent_name: Option<&'a str>,
    /// The door the call came in by, when the app has more than one.
    pub origin: Option<Origin>,
    /// The scope the call acted in (an account, a folder).
    pub scope: Option<&'a str>,
    /// The method or tool name.
    pub method: &'a str,
    /// `ok`, or the refusal's code.
    pub outcome: &'a str,
    /// Non-content facts in one short line.
    pub detail: Option<&'a str>,
    /// Non-content facts as fields (argument shapes, counts), for a sink that keeps structure.
    pub fields: Option<&'a Map<String, Value>>,
    /// Epoch ms to record; `None` reads the system clock (an injected clock keeps time-window
    /// limits testable).
    pub at: Option<i64>,
}

impl Entry<'_> {
    /// The row's time, epoch ms.
    pub fn at_ms(&self) -> i64 {
        self.at.unwrap_or_else(crate::now_ms)
    }
}

/// Where audit rows go: a SQL table ([`SqlAudit`], feature `sql`), a JSON-lines file, the
/// host's log. An `Err` means the row is not durable; [`crate::gate::conclude`] decides what
/// that does to the call.
pub trait AuditSink {
    fn record(&self, entry: &Entry<'_>) -> Result<(), String>;
}

#[cfg(feature = "sql")]
pub use sql::{page, recent, record, Row, SqlAudit};

#[cfg(feature = "sql")]
mod sql {
    use super::{AuditSink, Entry};
    use rightkit_db::rusqlite::{params, Connection};

    /// A stored row, newest first in [`page`].
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Row {
        pub seq: i64,
        pub at: i64,
        pub agent_id: Option<String>,
        pub agent_name: Option<String>,
        /// The scope column (`account_id` in the table).
        pub scope: Option<String>,
        pub method: String,
        pub outcome: String,
        pub detail: Option<String>,
    }

    /// The host's append-only `agent_audit` table (`seq`, `at`, `agent_id`, `agent_name`,
    /// `account_id`, `method`, `outcome`, `detail`). The host owns the schema and its triggers
    /// that refuse UPDATE and DELETE; the gateway only appends and reads.
    pub struct SqlAudit<'c>(pub &'c Connection);

    impl AuditSink for SqlAudit<'_> {
        fn record(&self, entry: &Entry<'_>) -> Result<(), String> {
            record(self.0, entry)
        }
    }

    fn clip(s: &str, max: usize) -> String {
        s.chars().take(max).collect()
    }

    /// Append one row. Name clipped to 80 characters, method to 40, detail to 200.
    pub fn record(conn: &Connection, e: &Entry<'_>) -> Result<(), String> {
        conn.execute(
            "INSERT INTO agent_audit(at, agent_id, agent_name, account_id, method, outcome, detail)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                e.at_ms(),
                e.agent_id,
                e.agent_name.map(|s| clip(s, 80)),
                e.scope,
                clip(e.method, 40),
                e.outcome,
                e.detail.map(|s| clip(s, 200)),
            ],
        )
        .map(|_| ())
        .map_err(|e| format!("audit: {e}"))
    }

    /// Newest rows first.
    pub fn recent(conn: &Connection, limit: usize) -> Result<Vec<Row>, String> {
        page(conn, limit, None)
    }

    /// Newest first; with `before`, only rows whose seq is below it. Read only.
    pub fn page(conn: &Connection, limit: usize, before: Option<i64>) -> Result<Vec<Row>, String> {
        let mut st = conn
            .prepare(
                "SELECT seq, at, agent_id, agent_name, account_id, method, outcome, detail
                 FROM agent_audit WHERE (?2 IS NULL OR seq < ?2) ORDER BY seq DESC LIMIT ?1",
            )
            .map_err(|e| format!("audit read: {e}"))?;
        let rows = st
            .query_map(params![limit as i64, before], |r| {
                Ok(Row {
                    seq: r.get(0)?,
                    at: r.get(1)?,
                    agent_id: r.get(2)?,
                    agent_name: r.get(3)?,
                    scope: r.get(4)?,
                    method: r.get(5)?,
                    outcome: r.get(6)?,
                    detail: r.get(7)?,
                })
            })
            .map_err(|e| format!("audit read: {e}"))?;
        rows.map(|r| r.map_err(|e| format!("audit read: {e}")))
            .collect()
    }
}
