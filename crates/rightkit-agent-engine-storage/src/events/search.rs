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
