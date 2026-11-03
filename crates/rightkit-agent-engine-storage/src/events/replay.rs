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
