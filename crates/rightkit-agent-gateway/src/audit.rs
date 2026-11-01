//! Append-only audit log. Only the gate calls `record`; handlers never do.
//! `detail` holds non-content facts (counts, ids, error codes) only. The
//! database triggers (schema v16) refuse UPDATE and DELETE of any row.

use rightkit_db::rusqlite::{params, Connection};

pub struct Entry<'a> {
    pub agent_id: Option<&'a str>,
    pub agent_name: Option<&'a str>,
    pub account_id: Option<&'a str>,
    pub method: &'a str,
    pub outcome: &'a str,
    pub detail: Option<&'a str>,
    /// Epoch ms to record; `None` reads the system clock. The gate passes its
    /// injected clock so time-window limits are testable.
    pub at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub seq: i64,
    pub at: i64,
    pub agent_id: Option<String>,
    pub agent_name: Option<String>,
    pub account_id: Option<String>,
    pub method: String,
    pub outcome: String,
    pub detail: Option<String>,
}

fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

pub(crate) fn record(conn: &Connection, e: &Entry<'_>) -> Result<(), String> {
    let at = e.at.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    });
    conn.execute(
        "INSERT INTO agent_audit(at, agent_id, agent_name, account_id, method, outcome, detail)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            at,
            e.agent_id,
            e.agent_name.map(|s| clip(s, 80)),
            e.account_id,
            clip(e.method, 40),
            e.outcome,
            e.detail.map(|s| clip(s, 200)),
        ],
    )
    .map(|_| ())
    .map_err(|e| format!("audit: {e}"))
}

/// Newest rows first, for the Settings audit view.
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
                account_id: r.get(4)?,
                method: r.get(5)?,
                outcome: r.get(6)?,
                detail: r.get(7)?,
            })
        })
        .map_err(|e| format!("audit read: {e}"))?;
    rows.map(|r| r.map_err(|e| format!("audit read: {e}")))
        .collect()
}
