use crate::error::Result;
use crate::Storage;

/// One session that matched a full-text search, with a highlighted snippet.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionMatch {
    pub session_id: String,
    pub title: String,
    pub snippet: String,
}

impl Storage {
    /// Full-text search across all session event payloads (cross-session recall).
    /// Returns the best-ranked snippet per matching session, most relevant first.
    ///
    /// The query is sanitized into a safe FTS5 AND-of-terms expression, so raw
    /// user input can't trip FTS syntax errors. An empty/term-less query returns
    /// no matches.
    pub fn search_sessions(&self, query: &str, limit: usize) -> Result<Vec<SessionMatch>> {
        let Some(match_expr) = fts_match_expr(query) else {
            return Ok(Vec::new());
        };
        // rightkit-search owns the match expression and the snippet expression
        // (MATCH_START/MATCH_END markers). Its compiled WHERE is an `IN (SELECT
        // rowid ...)` subquery, which cannot feed `snippet()` or `rank`; see
        // docs/rightkit-exceptions/outside-M-rows.md E-3.
        let sql = format!(
            "SELECT events_fts.session_id, COALESCE(s.title, ''), {snippet} \
             FROM {table} \
             LEFT JOIN sessions s ON s.id = {table}.session_id \
             WHERE {table} MATCH ?1 \
             ORDER BY rank",
            snippet = rightkit_search::snippet::fts_snippet_expr(FTS_TABLE, 12),
            table = FTS_TABLE,
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map((&match_expr,), |row| {
            Ok(SessionMatch {
                session_id: row.get(0)?,
                title: row.get(1)?,
                snippet: row.get(2)?,
            })
        })?;

        // Dedupe to one (best-ranked) hit per session, preserving rank order.
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for r in rows {
            let m = r?;
            if seen.insert(m.session_id.clone()) {
                out.push(m);
                if out.len() >= limit {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Rebuild the `events_fts` mirror from `events` so the index converges
    /// with the event log. Idempotent and safe to run at any time: drops the
    /// index contents, then reinserts every `events` row. The triggers
    /// installed in migration v3 keep the index in sync going forward, so
    /// this is the explicit recovery path for a database that was upgraded
    /// from a pre-trigger version with a partial backfill, or for one where
    /// `events_fts` was somehow corrupted by direct SQL.
    ///
    /// Returns the number of `events` rows re-mirrored.
    pub fn reindex_events_fts(&mut self) -> Result<usize> {
        let tx = rightkit_db::write_txn(&mut self.conn)?;
        tx.execute("DELETE FROM events_fts", [])?;
        let n = tx.execute(
            "INSERT INTO events_fts(rowid, session_id, kind, payload) \
             SELECT rowid, session_id, kind, payload_json FROM events",
            [],
        )?;
        tx.commit()?;
        Ok(n)
    }

    /// The number of rows currently in the `events_fts` mirror. Used by
    /// the replay/reindex tests to confirm the trigger invariant: any row
    /// appended to `events` must produce a matching row in `events_fts`
    /// within the same transaction, and any row deleted from `events`
    /// must remove the matching `events_fts` row.
    #[cfg(test)]
    pub fn events_fts_count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT count(*) FROM events_fts", [], |r| r.get(0))?;
        Ok(n as usize)
    }
}

const FTS_TABLE: &str = "events_fts";

/// Turn arbitrary user text into a safe FTS5 query: each whitespace-separated word
/// becomes a quoted prefix term, ANDed. `rightkit-search` compiles the expression;
/// `None` when no term has an alphanumeric character (matches nothing).
fn fts_match_expr(query: &str) -> Option<String> {
    let mut terms = rightkit_search::Query::default();
    terms.text = query.split_whitespace().map(str::to_owned).collect();
    let compiled = rightkit_search::Schema::new()
        .fts(FTS_TABLE, "events.rowid")
        .compile(&terms)
        .ok()?;
    compiled.fts_match.filter(|expr| !expr.is_empty())
}

#[cfg(test)]
mod tests {
    use crate::{SessionRow, Storage};

    fn store() -> Storage {
        Storage::open_in_memory().unwrap()
    }

    fn sess(s: &mut Storage, id: &str, title: &str) {
        s.create_session(&SessionRow {
            id: id.to_string(),
            title: title.to_string(),
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

    #[test]
    fn finds_the_session_that_mentions_a_term() {
        let mut s = store();
        sess(&mut s, "a", "Auth work");
        sess(&mut s, "b", "Marketing");
        s.append_event(
            "a",
            "msg",
            r#"{"text":"add a healthcheck endpoint to axum"}"#,
        )
        .unwrap();
        s.append_event("b", "msg", r#"{"text":"write homepage copy"}"#)
            .unwrap();

        let hits = s.search_sessions("healthcheck axum", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "a");
        assert_eq!(hits[0].title, "Auth work");
        let spans = rightkit_search::snippet::split_highlights(&hits[0].snippet);
        let matched: Vec<&str> = spans
            .iter()
            .filter(|span| span.matched)
            .map(|span| span.text.as_str())
            .collect();
        assert!(
            matched.iter().any(|m| m.starts_with("healthcheck")),
            "owner markers must wrap the match: {:?}",
            hits[0].snippet
        );
        assert!(
            !hits[0].snippet.contains('['),
            "no app-local bracket markers"
        );
    }

    #[test]
    fn dedupes_to_one_hit_per_session() {
        let mut s = store();
        sess(&mut s, "a", "T");
        for _ in 0..3 {
            s.append_event("a", "msg", r#"{"text":"tokio runtime tokio"}"#)
                .unwrap();
        }
        let hits = s.search_sessions("tokio", 10).unwrap();
        assert_eq!(
            hits.len(),
            1,
            "one session, even with multiple matching events"
        );
    }

    #[test]
    fn empty_or_punctuation_query_returns_nothing_safely() {
        let mut s = store();
        sess(&mut s, "a", "T");
        s.append_event("a", "msg", r#"{"text":"hello"}"#).unwrap();
        assert!(s.search_sessions("   ", 10).unwrap().is_empty());
        // Punctuation that would be FTS syntax must not error.
        assert!(s.search_sessions("\"*(", 10).unwrap().is_empty());
    }
}
