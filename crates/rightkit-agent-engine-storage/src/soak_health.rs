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
#[cfg(test)]
use rightkit_db::rusqlite;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Storage;

    fn mem_storage() -> Storage {
        Storage::open_in_memory().expect("mem storage")
    }

    #[test]
    fn integrity_ok_on_fresh_db() {
        let s = mem_storage();
        assert!(integrity_ok(&s.conn).unwrap());
    }

    #[test]
    fn freelist_and_page_count_are_sane() {
        let s = mem_storage();
        let fc = freelist_count(&s.conn).unwrap();
        let pc = page_count(&s.conn).unwrap();
        let ps = page_size(&s.conn).unwrap();
        assert!(fc >= 0);
        assert!(pc > 0);
        assert!(ps >= 512);
    }

    #[test]
    fn wal_snapshot_on_memory_is_zero_wal() {
        let s = mem_storage();
        let snap = wal_snapshot(&s.conn).unwrap();
        assert!(snap.integrity_ok);
        assert_eq!(snap.wal_bytes, 0);
    }

    #[test]
    fn no_orphans_on_fresh_db() {
        let s = mem_storage();
        assert_eq!(orphan_event_count(&s.conn).unwrap(), 0);
        assert_eq!(orphan_session_count(&s.conn).unwrap(), 0);
        let o = orphan_snapshot(&s.conn).unwrap();
        assert_eq!(o.orphan_events, 0);
        assert_eq!(o.orphan_sessions, 0);
    }

    #[test]
    fn seq_contiguous_after_sequential_appends() {
        let mut s = mem_storage();
        for i in 1..=5 {
            s.append_event("sess-a", "kind", &format!(r#"{{"i":{i}}}"#))
                .unwrap();
        }
        assert!(seq_contiguous(&s.conn, "sess-a").unwrap());
        assert!(seq_gaps(&s.conn, "sess-a").unwrap().is_empty());
    }

    #[test]
    fn seq_gaps_detects_missing_seq() {
        let s = mem_storage();
        // Insert sparsely via raw SQL to simulate a gap.
        s.conn
            .execute(
                "INSERT INTO events (session_id, seq, kind, payload_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["sess-gap", 1, "k", "{}", "2026-01-01T00:00:00Z"],
            )
            .unwrap();
        s.conn
            .execute(
                "INSERT INTO events (session_id, seq, kind, payload_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["sess-gap", 3, "k", "{}", "2026-01-01T00:00:00Z"],
            )
            .unwrap();
        s.conn
            .execute(
                "INSERT INTO events (session_id, seq, kind, payload_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["sess-gap", 4, "k", "{}", "2026-01-01T00:00:00Z"],
            )
            .unwrap();
        assert!(!seq_contiguous(&s.conn, "sess-gap").unwrap());
        assert_eq!(seq_gaps(&s.conn, "sess-gap").unwrap(), vec![2]);
    }

    #[test]
    fn orphan_events_detected() {
        let mut s = mem_storage();
        // Need a valid session for FK baseline; create one, then inject event for missing session.
        s.create_session(&crate::SessionRow {
            id: "real".to_string(),
            title: "t".to_string(),
            workspace_root: "/tmp".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            parent_session_id: None,
            role: None,
            status: "idle".to_string(),
            config_json: "{}".to_string(),
            tags: "".to_string(),
            recovery_marker: None,
        })
        .unwrap();
        s.append_event("real", "k", "{}").unwrap();
        // Orphan event for non-existent session — inserted directly.
        s.conn
            .execute(
                "INSERT INTO events (session_id, seq, kind, payload_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["ghost", 1, "k", "{}", "2026-01-01T00:00:00Z"],
            )
            .unwrap();
        assert_eq!(orphan_event_count(&s.conn).unwrap(), 1);
    }

    #[test]
    fn orphan_sessions_detected() {
        let s = mem_storage();
        s.create_session(&crate::SessionRow {
            id: "child".to_string(),
            title: "t".to_string(),
            workspace_root: "/tmp".to_string(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            updated_at: "2026-01-01T00:00:00Z".to_string(),
            parent_session_id: Some("missing-parent".to_string()),
            role: None,
            status: "idle".to_string(),
            config_json: "{}".to_string(),
            tags: "".to_string(),
            recovery_marker: None,
        })
        .unwrap();
        assert_eq!(orphan_session_count(&s.conn).unwrap(), 1);
    }

    #[test]
    fn seq_gaps_empty_for_unknown_session() {
        let s = mem_storage();
        assert!(seq_contiguous(&s.conn, "nope").unwrap());
        assert!(seq_gaps(&s.conn, "nope").unwrap().is_empty());
    }

    #[test]
    fn wal_snapshot_freelist_bounded_after_burst() {
        let mut s = mem_storage();
        // Burst of events + deletions to generate freelist churn.
        for i in 0..200 {
            s.append_event("burst", "k", &format!(r#"{{"n":{i}}}"#))
                .unwrap();
        }
        // Delete half the events to create freelist entries.
        s.conn
            .execute(
                "DELETE FROM events WHERE seq % 2 = 0 AND session_id = 'burst'",
                [],
            )
            .unwrap();
        let snap = wal_snapshot(&s.conn).unwrap();
        assert!(snap.integrity_ok);
        // Freelist should exist but be bounded (< 5000 per gate).
        assert!(snap.freelist < 5000, "freelist {} too high", snap.freelist);
    }
}
