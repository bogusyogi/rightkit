//! Bounded scrollback paging over the durable event log. A client keeps a
//! small in-memory window and fetches older ranges from SQLite on demand;
//! every query here is index-served on `(session_id, seq)` with a `LIMIT`,
//! so an arbitrary historical range never scans the table or materializes
//! more rows than the page size.

use crate::error::Result;
use crate::events::{map_event, EventRow};
use crate::Storage;

impl Storage {
    /// Backwards scrollback paging: the most recent events with
    /// `seq < before_seq`, at most `limit` rows, returned in seq ascending
    /// order (the same direction as `events_after`, so pages stitch into a
    /// transcript without reordering). Pass the lowest seq the client still
    /// holds as `before_seq` to fetch the page just before its window; an
    /// empty result means the log is exhausted.
    pub fn events_before(
        &self,
        session_id: &str,
        before_seq: i64,
        limit: usize,
    ) -> Result<Vec<EventRow>> {
        let limit = limit_i64(limit);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT session_id, seq, kind, payload_json, created_at \
             FROM events WHERE session_id = ?1 AND seq < ?2 \
             ORDER BY seq DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map((session_id, before_seq, limit), map_event)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        // Scanned newest-first for the bounded DESC walk; restore the
        // transcript's ascending order before returning.
        out.reverse();
        Ok(out)
    }

    /// An arbitrary window of events with `from_seq <= seq <= to_seq`, at
    /// most `limit` rows, ordered by seq ascending. A degenerate window
    /// (`from_seq > to_seq`) or `limit == 0` returns an empty page rather
    /// than an error.
    pub fn events_in_range(
        &self,
        session_id: &str,
        from_seq: i64,
        to_seq: i64,
        limit: usize,
    ) -> Result<Vec<EventRow>> {
        let limit = limit_i64(limit);
        if limit == 0 || from_seq > to_seq {
            return Ok(Vec::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT session_id, seq, kind, payload_json, created_at \
             FROM events WHERE session_id = ?1 AND seq >= ?2 AND seq <= ?3 \
             ORDER BY seq ASC LIMIT ?4",
        )?;
        let rows = stmt.query_map((session_id, from_seq, to_seq, limit), map_event)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

/// Clamp a caller-supplied page size into the range SQLite's `LIMIT`
/// accepts. Saturates rather than erroring: an oversized request simply
/// means "no practical bound", the WHERE clauses still apply.
fn limit_i64(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}
