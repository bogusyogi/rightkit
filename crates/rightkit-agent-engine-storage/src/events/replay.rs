use crate::error::Result;
use crate::events::{map_event, EventRow};
use crate::Storage;

impl Storage {
    /// SSE catch-up query: all events with `seq > after_seq`, ordered by seq
    /// ascending. Pass `after_seq = 0` to get the entire log.
    pub fn events_after(&self, session_id: &str, after_seq: i64) -> Result<Vec<EventRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, seq, kind, payload_json, created_at \
             FROM events WHERE session_id = ?1 AND seq > ?2 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map((session_id, after_seq), map_event)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }
}

/// One bounded page of durable replay.
#[derive(Debug, Clone, PartialEq)]
pub struct EventPage {
    pub rows: Vec<EventRow>,
    /// Cursor to pass as `after_seq` for the following page: the last row's
    /// seq, or the requested `after_seq` when the page is empty.
    pub next: i64,
    /// True when storage holds no rows beyond this page.
    pub complete: bool,
}

/// Rows per page when resume streams a whole log through the read pool.
pub const RESUME_PAGE_ROWS: usize = 512;

fn query_event_page(
    conn: &rightkit_db::rusqlite::Connection,
    session_id: &str,
    after_seq: i64,
    limit: usize,
) -> Result<EventPage> {
    let fetch = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
    let mut stmt = conn.prepare(
        "SELECT session_id, seq, kind, payload_json, created_at          FROM events WHERE session_id = ?1 AND seq > ?2          ORDER BY seq ASC LIMIT ?3",
    )?;
    let rows = stmt.query_map((session_id, after_seq, fetch), map_event)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    let complete = out.len() <= limit;
    out.truncate(limit);
    let next = out.last().map(|row| row.seq).unwrap_or(after_seq);
    Ok(EventPage {
        rows: out,
        next,
        complete,
    })
}

impl Storage {
    /// Paged durable replay: at most `limit` events with `seq > after_seq`,
    /// ascending. Reads `limit + 1` rows to learn whether more remain without
    /// a second query. `limit == 0` returns an empty page whose `complete`
    /// flag reports whether any row remains.
    pub fn events_page_after(
        &self,
        session_id: &str,
        after_seq: i64,
        limit: usize,
    ) -> Result<EventPage> {
        query_event_page(&self.conn, session_id, after_seq, limit)
    }
}

impl crate::StoragePool {
    /// One bounded page of durable replay read through the pool, never the
    /// writer connection (audit M12). Each call checks a connection out and
    /// returns it, so a large resume never holds the writer mutex or a pooled
    /// connection across pages.
    pub fn events_page_after(
        &self,
        session_id: &str,
        after_seq: i64,
        limit: usize,
    ) -> Result<EventPage> {
        let conn = self.get()?;
        query_event_page(&conn, session_id, after_seq, limit)
    }

    /// The whole event log for a session, read page by page through the pool.
    /// Returns exactly what `Storage::all_events` would.
    pub fn all_events_paged(&self, session_id: &str, page_rows: usize) -> Result<Vec<EventRow>> {
        let page_rows = page_rows.max(1);
        let mut out = Vec::new();
        let mut after = 0i64;
        loop {
            let page = self.events_page_after(session_id, after, page_rows)?;
            after = page.next;
            let complete = page.complete;
            out.extend(page.rows);
            if complete {
                return Ok(out);
            }
        }
    }

    /// Latest known cumulative cost recorded for a session (`usageUpdated`
    /// rows with a known, non-negative cost), or 0.0. Evaluated in SQL so a
    /// lineage cost needs no event decoding or log load.
    pub fn latest_known_cost_usd(&self, session_id: &str) -> Result<f64> {
        let conn = self.get()?;
        let cost: Option<f64> = conn
            .query_row(
                "SELECT json_extract(payload_json, '$.costUsd') FROM events                  WHERE session_id = ?1 AND kind = 'usageUpdated'                    AND json_valid(payload_json)                    AND COALESCE(json_extract(payload_json, '$.costKnown'), 1) != 0                    AND json_extract(payload_json, '$.costUsd') >= 0                  ORDER BY seq DESC LIMIT 1",
                [session_id],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rightkit_db::rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(cost.filter(|value| value.is_finite()).unwrap_or(0.0))
    }
}

#[cfg(test)]
mod tests {
    use crate::Storage;

    #[test]
    fn events_page_after_pages_to_completion_without_gaps() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        for i in 1..=5 {
            storage
                .append_event("s-page", "kind", &format!(r#"{{"i":{i}}}"#))
                .expect("append");
        }
        let first = storage.events_page_after("s-page", 0, 2).expect("page 1");
        assert_eq!(
            first.rows.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(first.next, 2);
        assert!(!first.complete);
        let second = storage
            .events_page_after("s-page", first.next, 2)
            .expect("page 2");
        assert_eq!(
            second.rows.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert!(!second.complete);
        let third = storage
            .events_page_after("s-page", second.next, 2)
            .expect("page 3");
        assert_eq!(
            third.rows.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![5]
        );
        assert_eq!(third.next, 5);
        assert!(third.complete);
    }

    #[test]
    fn events_page_after_exact_fit_is_complete_and_empty_keeps_cursor() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        storage.append_event("s-fit", "kind", "{}").expect("append");
        storage.append_event("s-fit", "kind", "{}").expect("append");
        let exact = storage.events_page_after("s-fit", 0, 2).expect("exact");
        assert_eq!(exact.rows.len(), 2);
        assert!(exact.complete, "limit == remaining rows must be complete");
        let empty = storage.events_page_after("s-fit", 2, 2).expect("empty");
        assert!(empty.rows.is_empty());
        assert_eq!(empty.next, 2);
        assert!(empty.complete);
    }

    #[test]
    fn events_page_after_does_not_cross_sessions() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        storage.append_event("p-a", "kind", "{}").expect("append");
        storage.append_event("p-b", "kind", "{}").expect("append");
        let page = storage.events_page_after("p-a", 0, 10).expect("page");
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].session_id, "p-a");
    }

    #[test]
    fn events_after_zero_returns_the_entire_log_in_order() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        for i in 1..=4 {
            storage
                .append_event("s-replay", "kind", &format!(r#"{{"i":{i}}}"#))
                .expect("append");
        }
        let events = storage.events_after("s-replay", 0).expect("replay");
        let seqs: Vec<i64> = events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4]);
    }

    #[test]
    fn events_after_a_seq_returns_only_the_strict_tail() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        for i in 1..=5 {
            storage
                .append_event("s-tail", "kind", &format!(r#"{{"i":{i}}}"#))
                .expect("append");
        }
        let events = storage.events_after("s-tail", 3).expect("replay");
        let seqs: Vec<i64> = events.iter().map(|e| e.seq).collect();
        assert_eq!(
            seqs,
            vec![4, 5],
            "seq > after_seq must be strict, not inclusive"
        );
    }

    #[test]
    fn events_after_the_latest_seq_is_empty() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        storage.append_event("s-end", "kind", "{}").expect("append");
        storage.append_event("s-end", "kind", "{}").expect("append");
        let events = storage.events_after("s-end", 2).expect("replay");
        assert!(events.is_empty());
    }

    #[test]
    fn events_after_does_not_cross_session_boundaries() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        storage.append_event("s-a", "kind", "{}").expect("append");
        storage.append_event("s-b", "kind", "{}").expect("append");
        let events = storage.events_after("s-a", 0).expect("replay");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session_id, "s-a");
    }

    /// Determinism: replaying the identical committed sequence twice must
    /// produce byte-identical output, proving replay has no hidden ordering
    /// dependency on statement caching, connection state, or wall-clock time.
    #[test]
    fn replaying_the_same_input_sequence_twice_is_identical() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        for i in 1..=6 {
            storage
                .append_event("s-det", "kind", &format!(r#"{{"i":{i}}}"#))
                .expect("append");
        }
        let first = storage.events_after("s-det", 0).expect("replay");
        let second = storage.events_after("s-det", 0).expect("replay");
        assert_eq!(
            first, second,
            "replay of the same committed log must be deterministic"
        );
    }

    #[test]
    fn pool_paged_resume_returns_the_same_events_as_the_writer_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resume-paged.sqlite");
        let mut storage = Storage::open(&path).expect("storage opens");
        for i in 1..=25 {
            storage
                .append_event("s-resume", "kind", &format!(r#"{{"i":{i}}}"#))
                .expect("append");
        }
        storage
            .append_event("s-other", "kind", "{}")
            .expect("append");
        let pool = storage.read_pool().expect("file storage has a read pool");
        let expected = storage.all_events("s-resume").expect("writer read");
        for page_rows in [1usize, 7, 25, 26, 512] {
            let paged = pool.all_events_paged("s-resume", page_rows).expect("paged");
            assert_eq!(paged, expected, "page size {page_rows}");
        }
        let first = pool.events_page_after("s-resume", 0, 10).expect("page");
        assert_eq!(first.rows.len(), 10);
        assert!(!first.complete);
        assert!(pool
            .all_events_paged("missing", 4)
            .expect("empty")
            .is_empty());
    }

    #[test]
    fn pool_reads_do_not_need_the_writer_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resume-reader.sqlite");
        let mut storage = Storage::open(&path).expect("storage opens");
        storage.append_event("s-r", "kind", "{}").expect("append");
        let pool = storage.read_pool().expect("read pool");
        // The pool is an owned, cloneable handle that outlives any borrow of
        // the writer, so a resume can read on another thread without the
        // writer mutex.
        let handle =
            std::thread::spawn(move || pool.all_events_paged("s-r", 8).map(|rows| rows.len()));
        storage
            .append_event("s-r", "kind", "{}")
            .expect("writer still appends");
        assert!(handle.join().unwrap().expect("pool read") >= 1);
    }

    #[test]
    fn latest_known_cost_reads_the_last_known_usage_row_in_sql() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("resume-cost.sqlite");
        let mut storage = Storage::open(&path).expect("storage opens");
        storage
            .append_event("s-c", "usageUpdated", r#"{"costUsd":1.5,"costKnown":true}"#)
            .unwrap();
        storage
            .append_event("s-c", "usageUpdated", r#"{"costUsd":2.5}"#)
            .unwrap();
        storage
            .append_event(
                "s-c",
                "usageUpdated",
                r#"{"costUsd":9.0,"costKnown":false}"#,
            )
            .unwrap();
        storage
            .append_event("s-c", "other", r#"{"costUsd":7.0}"#)
            .unwrap();
        let pool = storage.read_pool().expect("read pool");
        assert_eq!(pool.latest_known_cost_usd("s-c").unwrap(), 2.5);
        assert_eq!(pool.latest_known_cost_usd("none").unwrap(), 0.0);
    }
}
