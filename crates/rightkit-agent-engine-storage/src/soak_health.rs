//! FM-189 — Soak health gates for the 8-hour soak.
//!
//! The JS harness (`qa/soak/`) samples heap/FDs/processes externally and
//! SQLite via `sqlite3` CLI. This module provides the same SQLite invariants
//! as unit-tested Rust so `cargo test` covers them without an installed
//! daemon. The harness and this module must agree on the invariants.
//!
//! Gates:
//!   - `PRAGMA integrity_check = ok`
//!   - freelist < threshold
//!   - WAL size bounded
//!   - no orphan events / sessions
//!   - per-session seq contiguous (no gaps)

use rightkit_db::rusqlite::Connection;

use crate::error::Result;

/// Result of `PRAGMA integrity_check`.
pub fn integrity_ok(conn: &Connection) -> Result<bool> {
    let v: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    Ok(v.eq_ignore_ascii_case("ok"))
}

/// Freelist page count (`PRAGMA freelist_count`).
pub fn freelist_count(conn: &Connection) -> Result<i64> {
    let v: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
    Ok(v)
}

/// Page count (`PRAGMA page_count`).
pub fn page_count(conn: &Connection) -> Result<i64> {
    let v: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
    Ok(v)
}

/// Page size (`PRAGMA page_size`).
pub fn page_size(conn: &Connection) -> Result<i64> {
    let v: i64 = conn.query_row("PRAGMA page_size", [], |r| r.get(0))?;
    Ok(v)
}

/// WAL size in bytes (best-effort; returns 0 when file absent). Caller is
/// responsible for resolving the DB path — inside unit tests we derive it
/// from `PRAGMA database_list` when available.
pub fn wal_size_for(conn: &Connection) -> i64 {
    // Try to resolve db file from pragma, then stat its -wal
    let db_file: Option<String> = conn
        .query_row("PRAGMA database_list", [], |r| r.get::<_, String>(2))
        .ok();
    if let Some(p) = db_file {
        if p.is_empty() || p == ":memory:" {
            return 0;
        }
        let wal = format!("{p}-wal");
        std::fs::metadata(&wal).map(|m| m.len() as i64).unwrap_or(0)
    } else {
        0
    }
}

/// Count events whose `session_id` has no matching `sessions` row.
pub fn orphan_event_count(conn: &Connection) -> Result<i64> {
    let v: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE session_id NOT IN (SELECT id FROM sessions)",
        [],
        |r| r.get(0),
    )?;
    Ok(v)
}

/// Count sessions with a `parent_session_id` that does not exist.
pub fn orphan_session_count(conn: &Connection) -> Result<i64> {
    let v: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sessions WHERE parent_session_id IS NOT NULL AND parent_session_id NOT IN (SELECT id FROM sessions)",
        [],
        |r| r.get(0),
    )?;
    Ok(v)
}

/// Return missing seqs for a session (gaps). Empty means contiguous 1..max.
pub fn seq_gaps(conn: &Connection, session_id: &str) -> Result<Vec<i64>> {
    let mut stmt = conn.prepare("SELECT seq FROM events WHERE session_id = ?1 ORDER BY seq ASC")?;
    let seqs: Vec<i64> = stmt
        .query_map([session_id], |r| r.get(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if seqs.is_empty() {
        return Ok(Vec::new());
    }
    let mut gaps = Vec::new();
    let mut expected = 1i64;
    for s in seqs {
        while expected < s {
            gaps.push(expected);
            expected += 1;
        }
        expected = s + 1;
    }
    Ok(gaps)
}

/// True when seqs for session are 1..N contiguous.
pub fn seq_contiguous(conn: &Connection, session_id: &str) -> Result<bool> {
    Ok(seq_gaps(conn, session_id)?.is_empty())
}

/// WAL + freelist snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalSnapshot {
    pub integrity_ok: bool,
    pub freelist: i64,
    pub page_count: i64,
    pub page_size: i64,
    pub wal_bytes: i64,
}

pub fn wal_snapshot(conn: &Connection) -> Result<WalSnapshot> {
    Ok(WalSnapshot {
        integrity_ok: integrity_ok(conn)?,
        freelist: freelist_count(conn)?,
        page_count: page_count(conn)?,
        page_size: page_size(conn)?,
        wal_bytes: wal_size_for(conn),
    })
}

/// Orphan snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanSnapshot {
    pub orphan_events: i64,
    pub orphan_sessions: i64,
}

pub fn orphan_snapshot(conn: &Connection) -> Result<OrphanSnapshot> {
    Ok(OrphanSnapshot {
        orphan_events: orphan_event_count(conn)?,
        orphan_sessions: orphan_session_count(conn)?,
    })
}
