//! Chunked, resumable-by-construction account rollup rebuild.
//!
//! `rebuild_rollups` recomputes every thread, label and full-text row of an
//! account in one transaction, which holds the writer for as long as the whole
//! mailbox takes. This module does the same work in key-ordered chunks of
//! threads, one short transaction per chunk, so the caller can release the
//! writer mutex between chunks and queued UI writes interleave.
//!
//! Safety of interleaving: every chunk recomputes its own threads from
//! `messages` as they are when the chunk runs (no snapshot is carried between
//! chunks) and replaces only its own rows. A write that lands between chunks
//! is either to a thread not yet reached (the later chunk recomputes it from
//! the same `messages`) or to one already done (the ordinary incremental apply
//! path recomputes it). No cursor needs persisting: a restart simply starts
//! the (idempotent) rebuild again, or skips it; nothing is ever left empty.

use super::*;

fn chunk_thread_ids(
    conn: &Connection,
    account_id: &str,
    after: Option<&str>,
    max_threads: usize,
) -> Result<Vec<String>, String> {
    let base = "SELECT DISTINCT thread_id FROM messages WHERE account_id = ?1";
    let sql = match after {
        Some(_) => format!("{base} AND thread_id > ?2 ORDER BY thread_id LIMIT ?3"),
        None => format!("{base} ORDER BY thread_id LIMIT ?3"),
    };
    let mut st = conn
        .prepare(&sql)
        .map_err(|e| format!("select rollup chunk: {e}"))?;
    let limit = max_threads as i64;
    let rows = st
        .query_map(params![account_id, after, limit], |r| r.get(0))
        .map_err(|e| format!("select rollup chunk: {e}"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| format!("read rollup chunk: {e}"))
}

/// Drop `threads` / `thread_labels` rows with no message, within the chunk's
/// key range (`after`, `upper`]; `upper = None` means to the end. Keeps the
/// full rebuild's "orphans disappear" behaviour without scanning the account.
fn drop_orphans_in_range(
    conn: &Connection,
    account_id: &str,
    after: Option<&str>,
    upper: Option<&str>,
) -> Result<(), String> {
    for (table, col) in [("threads", "id"), ("thread_labels", "thread_id")] {
        let mut sql = format!("DELETE FROM {table} WHERE account_id = ?1");
        let mut binds: Vec<&str> = vec![account_id];
        if let Some(a) = after {
            binds.push(a);
            sql.push_str(&format!(" AND {col} > ?{}", binds.len()));
        }
        if let Some(u) = upper {
            binds.push(u);
            sql.push_str(&format!(" AND {col} <= ?{}", binds.len()));
        }
        sql.push_str(&format!(
            " AND NOT EXISTS (SELECT 1 FROM messages m
               WHERE m.account_id = {table}.account_id AND m.thread_id = {table}.{col})"
        ));
        conn.execute(&sql, rightkit_db::rusqlite::params_from_iter(binds))
            .map_err(|e| format!("drop orphan {table}: {e}"))?;
    }
    Ok(())
}

/// Threads checked per transaction by [`repair_rollups_chunk`]. The check is a
/// single aggregate over indexed `messages` rows and writes nothing for a
/// consistent thread, so chunks can be larger than a full rebuild's.
pub const REPAIR_CHUNK_THREADS: usize = 500;

/// Threads among `ids` whose stored rollup is missing or disagrees with
/// `messages` on message count, newest date or unread flag. One query.
fn inconsistent_threads(
    conn: &Connection,
    account_id: &str,
    ids: &[String],
) -> Result<HashSet<String>, String> {
    let ids_json = serde_json::to_string(ids).map_err(|e| format!("encode chunk: {e}"))?;
    let mut st = conn
        .prepare(
            "SELECT m.thread_id
             FROM messages m
             LEFT JOIN threads t ON t.account_id = m.account_id AND t.id = m.thread_id
             WHERE m.account_id = ?1 AND m.thread_id IN (SELECT value FROM json_each(?2))
             GROUP BY m.thread_id
             HAVING t.id IS NULL
                 OR t.message_count IS NOT COUNT(*)
                 OR t.last_message_at IS NOT MAX(m.internal_date)
                 OR t.unread IS NOT MAX(m.unread)",
        )
        .map_err(|e| format!("select inconsistent threads: {e}"))?;
    let rows = st
        .query_map(params![account_id, ids_json], |r| r.get::<_, String>(0))
        .map_err(|e| format!("select inconsistent threads: {e}"))?;
    rows.collect::<Result<_, _>>()
        .map_err(|e| format!("read inconsistent threads: {e}"))
}

/// Cheap consistency repair of the next `max_threads` threads after `after`,
/// in ONE transaction: recompute the rollup (`threads`, `thread_labels`,
/// starred/important) only for threads whose stored rollup disagrees with
/// `messages`, and drop orphan rollup rows in the chunk's key range. It never
/// touches the full-text index or the semantic vectors: apply paths keep both
/// current per batch, and re-indexing unchanged text would only destroy
/// embeddings. Cursor contract: `after` is the last thread id of the previous
/// chunk (`None` starts at the beginning); the result is the next cursor, or
/// `None` once the account is done.
pub fn repair_rollups_chunk(
    conn: &Connection,
    account_id: &str,
    after: Option<&str>,
    max_threads: usize,
) -> Result<Option<String>, String> {
    let max_threads = max_threads.max(1);
    let mut next: Option<String> = None;
    in_txn(conn, || {
        let ids = chunk_thread_ids(conn, account_id, after, max_threads)?;
        let full = ids.len() >= max_threads;
        let last = ids.last().cloned();
        if !ids.is_empty() {
            let bad = inconsistent_threads(conn, account_id, &ids)?;
            if !bad.is_empty() {
                rebuild_threads_for_ids(conn, account_id, &bad)?;
            }
        }
        let upper = if full { last.as_deref() } else { None };
        drop_orphans_in_range(conn, account_id, after, upper)?;
        if full {
            next = last;
        }
        Ok(())
    })?;
    Ok(next)
}
