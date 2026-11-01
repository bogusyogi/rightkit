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

#[cfg(test)]
mod tests {
    use crate::{SessionRow, Storage};

    fn store() -> Storage {
        Storage::open_in_memory().unwrap()
    }

    fn sess(s: &mut Storage, id: &str) {
        s.create_session(&SessionRow {
            id: id.to_string(),
            title: "t".into(),
            workspace_root: "/ws".into(),
            created_at: "t".into(),
            updated_at: "t".into(),
            parent_session_id: None,
            role: None,
            status: "idle".into(),
            config_json: "{}".into(),
            tags: String::new(),
            recovery_marker: None,
        })
        .unwrap();
    }

    /// Append `n` contiguous events, returning the highest seq written.
    fn fill(s: &mut Storage, id: &str, n: i64) -> i64 {
        for i in 0..n {
            s.append_event(id, "msg", &format!(r#"{{"n":{}}}"#, i + 1))
                .unwrap();
        }
        n
    }

    fn seqs(rows: &[crate::events::EventRow]) -> Vec<i64> {
        rows.iter().map(|r| r.seq).collect()
    }

    #[test]
    fn empty_range_returns_nothing() {
        let mut s = store();
        sess(&mut s, "s");
        fill(&mut s, "s", 5);

        // Below the first persisted seq there is nothing to page back to.
        assert!(s.events_before("s", 1, 10).unwrap().is_empty());
        // A window past the high-water mark is empty too.
        assert!(s.events_in_range("s", 6, 10, 10).unwrap().is_empty());
        // Degenerate windows and zero limits are empty, not errors.
        assert!(s.events_in_range("s", 4, 2, 10).unwrap().is_empty());
        assert!(s.events_before("s", 6, 0).unwrap().is_empty());
        // An unknown session pages as empty as well.
        assert!(s.events_before("ghost", i64::MAX, 10).unwrap().is_empty());
    }

    #[test]
    fn backwards_pages_stitch_across_the_ring_boundary() {
        let mut s = store();
        sess(&mut s, "s");
        let latest = fill(&mut s, "s", 12);
        // The daemon's in-memory ring would hold only the tail here
        // (say seq 5..=12); pages 1 and 3 live entirely on one side of
        // that boundary, page 2 straddles it.
        let page1 = s.events_before("s", latest + 1, 4).unwrap();
        assert_eq!(seqs(&page1), vec![9, 10, 11, 12], "ascending within a page");

        let page2 = s.events_before("s", page1[0].seq, 4).unwrap();
        assert_eq!(seqs(&page2), vec![5, 6, 7, 8], "spans the ring boundary");

        let page3 = s.events_before("s", page2[0].seq, 4).unwrap();
        assert_eq!(seqs(&page3), vec![1, 2, 3, 4], "predates the ring");

        let exhausted = s.events_before("s", page3[0].seq, 4).unwrap();
        assert!(
            exhausted.is_empty(),
            "log exhausted signals the client to stop"
        );

        let stitched: Vec<i64> = [page3.as_slice(), page2.as_slice(), page1.as_slice()]
            .concat()
            .iter()
            .map(|r| r.seq)
            .collect();
        assert_eq!(
            stitched,
            (1..=12).collect::<Vec<_>>(),
            "pages reconstruct the full log in order"
        );
    }

    #[test]
    fn range_predating_the_in_memory_ring_is_served_from_sqlite() {
        let mut s = store();
        sess(&mut s, "s");
        fill(&mut s, "s", 20);
        // With an 8-slot ring the daemon memory would only retain
        // seq 13..=20; a window entirely below that must still come back
        // complete and in order from the durable log.
        let rows = s.events_in_range("s", 1, 12, 12).unwrap();
        assert_eq!(seqs(&rows), (1..=12).collect::<Vec<_>>());
        assert_eq!(rows[0].session_id, "s");
        assert_eq!(rows[0].payload_json, r#"{"n":1}"#);
    }

    #[test]
    fn range_window_honours_bounds_and_limit() {
        let mut s = store();
        sess(&mut s, "s");
        fill(&mut s, "s", 10);

        let rows = s.events_in_range("s", 3, 7, 100).unwrap();
        assert_eq!(seqs(&rows), vec![3, 4, 5, 6, 7], "inclusive bounds");

        let capped = s.events_in_range("s", 1, 10, 3).unwrap();
        assert_eq!(
            seqs(&capped),
            vec![1, 2, 3],
            "LIMIT caps an oversized window"
        );
    }

    #[test]
    fn fifty_concurrent_sessions_page_bounded_and_isolated() {
        let mut s = store();
        for index in 0..50 {
            sess(&mut s, &format!("session-{index}"));
            fill(&mut s, &format!("session-{index}"), 30);
        }
        // Every session pages back with the same small window size, so the
        // materialized page stays bounded regardless of session count, and
        // seq spaces are per-session: no session can observe another's rows.
        for index in 0..50 {
            let id = format!("session-{index}");
            let page = s.events_before(&id, 31, 8).unwrap();
            assert_eq!(seqs(&page), vec![23, 24, 25, 26, 27, 28, 29, 30]);
            assert!(page.iter().all(|row| row.session_id == id));
            let window = s.events_in_range(&id, 1, 30, 8).unwrap();
            assert_eq!(window.len(), 8, "window stays capped at the page size");
            assert!(window.iter().all(|row| row.session_id == id));
        }
    }
}
