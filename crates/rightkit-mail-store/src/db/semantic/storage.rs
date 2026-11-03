//! Vector storage for local semantic search (C5): the `message_vec` vec0
//! table + `semantic_index_state` progress-cursor rows (schema v13, see
//! db_schema.rs). Mirrors the `message_recipients`/FTS rowid invariant —
//! `message_vec.rowid = messages.rowid` — so per-message maintenance
//! (delete-on-message-delete, re-embed-on-change) is a rowid lookup, never a
//! scan.

use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use zerocopy::IntoBytes;

use super::embedder::STORED_DIM;

/// One row's worth of embedding ready to persist.
pub struct VecRow {
    pub rowid: i64,
    pub embedding: [f32; STORED_DIM],
}

/// Serialize a `[f32; STORED_DIM]` to the raw little-endian bytes vec0
/// expects for a `float[N]` column (KNN/insert both take this shape).
fn embedding_bytes(v: &[f32; STORED_DIM]) -> &[u8] {
    v.as_bytes()
}

/// Insert or replace one message's embedding. `rowid` MUST equal the row's
/// `messages.rowid` (the schema-v9-style invariant this table follows) —
/// callers resolve that via `SELECT rowid FROM messages WHERE account_id=?
/// AND id=?` before calling this. vec0 has no `ON CONFLICT`, so this
/// deletes-then-inserts inside the caller's transaction.
pub fn upsert_vector(
    conn: &Connection,
    rowid: i64,
    embedding: &[f32; STORED_DIM],
) -> Result<(), String> {
    conn.execute("DELETE FROM message_vec WHERE rowid = ?1", params![rowid])
        .map_err(|e| format!("delete stale vector: {e}"))?;
    conn.execute(
        "INSERT INTO message_vec(rowid, embedding) VALUES (?1, ?2)",
        params![rowid, embedding_bytes(embedding)],
    )
    .map_err(|e| format!("insert vector: {e}"))?;
    Ok(())
}

/// Remove vectors for message rowids (mirrors `delete_message_rows`'s FTS
/// cleanup — called from the same message-delete path so a deleted message
/// never leaves a stale, unreachable-by-join vector behind).
pub fn delete_vectors_by_rowids(conn: &Connection, rowids: &[i64]) -> Result<(), String> {
    for rowid in rowids {
        conn.execute("DELETE FROM message_vec WHERE rowid = ?1", params![rowid])
            .map_err(|e| format!("delete vector: {e}"))?;
    }
    Ok(())
}

/// KNN search: top `k` message rowids nearest `query_embedding`, restricted
/// to `candidate_rowids` when given (the hybrid path's FTS-candidate
/// narrowing) or unrestricted (the pure-vector fallback pass). Returns
/// `(rowid, distance)` ascending by distance (nearest first) — vec0's `MATCH
/// ... ORDER BY distance` is exactly the KNN query shape sqlite-vec expects
/// (a bare `MATCH` without an explicit `k = ?` in the WHERE clause uses the
/// `LIMIT` as the k value, per sqlite-vec's KNN query convention).
pub fn knn_search(
    conn: &Connection,
    query_embedding: &[f32; STORED_DIM],
    candidate_rowids: Option<&[i64]>,
    k: usize,
) -> Result<Vec<(i64, f64)>, String> {
    let bytes = embedding_bytes(query_embedding);
    match candidate_rowids {
        None => {
            let mut stmt = conn
                .prepare(
                    "SELECT rowid, distance FROM message_vec
                     WHERE embedding MATCH ?1
                     ORDER BY distance
                     LIMIT ?2",
                )
                .map_err(|e| format!("prepare knn: {e}"))?;
            let rows = stmt
                .query_map(params![bytes, k as i64], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
                })
                .map_err(|e| format!("query knn: {e}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("row knn: {e}"))
        }
        Some(candidates) => {
            if candidates.is_empty() {
                return Ok(Vec::new());
            }
            // vec0 KNN queries can't take an arbitrary IN-list restriction in
            // the same MATCH query in this sqlite-vec version, so the
            // candidate-restricted pass over-fetches a wider KNN window
            // then filters + truncates to k in Rust. The candidate set from
            // an FTS pre-filter (top ~200, per the hybrid query design) is
            // small, so this is cheap.
            let over_fetch = (candidates.len().max(k) * 2).min(2000) as i64;
            let mut stmt = conn
                .prepare(
                    "SELECT rowid, distance FROM message_vec
                     WHERE embedding MATCH ?1
                     ORDER BY distance
                     LIMIT ?2",
                )
                .map_err(|e| format!("prepare knn candidates: {e}"))?;
            let rows = stmt
                .query_map(params![bytes, over_fetch], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?))
                })
                .map_err(|e| format!("query knn candidates: {e}"))?;
            let candidate_set: std::collections::HashSet<i64> =
                candidates.iter().copied().collect();
            let mut out: Vec<(i64, f64)> = Vec::new();
            for r in rows {
                let (rowid, dist) = r.map_err(|e| format!("row knn candidates: {e}"))?;
                if candidate_set.contains(&rowid) {
                    out.push((rowid, dist));
                    if out.len() >= k {
                        break;
                    }
                }
            }
            Ok(out)
        }
    }
}

/// Per-account indexing progress row (creates a default row on first read).
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticIndexState {
    pub account_id: String,
    pub content_hash: String,
    pub embed_model_id: String,
    pub embed_dim: i64,
    pub cursor_rowid: i64,
    pub done: bool,
    pub indexed_count: i64,
    /// Embedder `index_key` that produced the stored vectors; `None` for
    /// rows written before schema v20 (unknown, so re-embedded once).
    pub pipeline_key: Option<String>,
}

pub fn get_index_state(conn: &Connection, account_id: &str) -> Result<SemanticIndexState, String> {
    let row = conn
        .query_row(
            "SELECT account_id, content_hash, embed_model_id, embed_dim, cursor_rowid, done, indexed_count, pipeline_key
             FROM semantic_index_state WHERE account_id = ?1",
            params![account_id],
            |r| {
                Ok(SemanticIndexState {
                    account_id: r.get(0)?,
                    content_hash: r.get(1)?,
                    embed_model_id: r.get(2)?,
                    embed_dim: r.get(3)?,
                    cursor_rowid: r.get(4)?,
                    done: r.get::<_, i64>(5)? != 0,
                    indexed_count: r.get(6)?,
                    pipeline_key: r.get(7)?,
                })
            },
        )
        .optional()
        .map_err(|e| format!("get semantic index state: {e}"))?;
    Ok(row.unwrap_or(SemanticIndexState {
        account_id: account_id.to_string(),
        content_hash: String::new(),
        embed_model_id: String::new(),
        embed_dim: 0,
        cursor_rowid: 0,
        done: false,
        indexed_count: 0,
        pipeline_key: None,
    }))
}

pub fn upsert_index_state(conn: &Connection, state: &SemanticIndexState) -> Result<(), String> {
    conn.execute(
        "INSERT INTO semantic_index_state
           (account_id, content_hash, embed_model_id, embed_dim, cursor_rowid, done, indexed_count, updated_at, pipeline_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(account_id) DO UPDATE SET
           content_hash = excluded.content_hash,
           embed_model_id = excluded.embed_model_id,
           embed_dim = excluded.embed_dim,
           cursor_rowid = excluded.cursor_rowid,
           done = excluded.done,
           indexed_count = excluded.indexed_count,
           updated_at = excluded.updated_at,
           pipeline_key = excluded.pipeline_key",
        params![
            state.account_id,
            state.content_hash,
            state.embed_model_id,
            state.embed_dim,
            state.cursor_rowid,
            state.done as i64,
            state.indexed_count,
            crate::db::now_ms(),
            state.pipeline_key,
        ],
    )
    .map_err(|e| format!("upsert semantic index state: {e}"))?;
    Ok(())
}

/// True when the persisted (model_id, dim, pipeline key) for this account
/// differs from the active configuration — the "needs reindex" signal (C5 spec
/// item 1/3: a model, truncation-dim or pipeline change triggers
/// reindex-from-local-bodies). A missing pipeline key counts as a difference.
pub fn needs_reindex(
    state: &SemanticIndexState,
    active_model_id: &str,
    active_dim: usize,
    active_key: &str,
) -> bool {
    if state.embed_model_id.is_empty() {
        return false; // never indexed yet — that's "not started", not "stale"
    }
    state.embed_model_id != active_model_id
        || state.embed_dim != active_dim as i64
        || state.pipeline_key.as_deref() != Some(active_key)
}

/// True when no indexed account in scope holds vectors made by a pipeline
/// other than `active_key`. The vector pass is not account-restricted, so with
/// `account = None` (or a blank one) every account is checked. A false result
/// means search must report "not ready" until the indexer re-embeds.
pub fn vectors_match_key(
    conn: &Connection,
    account: Option<&str>,
    active_key: &str,
) -> Result<bool, String> {
    let account = account.filter(|a| !a.trim().is_empty());
    let stale: i64 = conn
        .query_row(
            "SELECT count(*) FROM semantic_index_state
             WHERE embed_model_id <> ''
               AND COALESCE(pipeline_key, '') <> ?1
               AND (?2 IS NULL OR account_id = ?2)",
            params![active_key, account],
            |r| r.get(0),
        )
        .map_err(|e| format!("check semantic pipeline key: {e}"))?;
    Ok(stale == 0)
}

/// Reset an account's index progress (used when a model/dim change is
/// detected, or the user hits "re-index" in Settings). Clears the vector
/// rows for that account's messages too, so a stale wrong-space embedding
/// never lingers to be KNN-matched against the new model's queries.
pub fn reset_index_for_account(conn: &Connection, account_id: &str) -> Result<(), String> {
    conn.execute(
        "DELETE FROM message_vec WHERE rowid IN
           (SELECT rowid FROM messages WHERE account_id = ?1)",
        params![account_id],
    )
    .map_err(|e| format!("clear vectors for reindex: {e}"))?;
    conn.execute(
        "DELETE FROM semantic_index_state WHERE account_id = ?1",
        params![account_id],
    )
    .map_err(|e| format!("clear index state for reindex: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::semantic::vec_ext;

    fn conn_with_schema() -> Connection {
        vec_ext::ensure_registered();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::migrate(&conn).unwrap();
        conn
    }

    fn unit_vec(seed: f32) -> [f32; STORED_DIM] {
        let mut v = [0f32; STORED_DIM];
        v[0] = seed;
        v[1] = (1.0 - seed * seed).max(0.0).sqrt();
        v
    }

    #[test]
    fn vector_round_trip_insert_then_knn_returns_the_row() {
        let conn = conn_with_schema();
        conn.execute("INSERT INTO accounts(id, created_at) VALUES ('a1', 0)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO messages(account_id, id, thread_id, internal_date) VALUES ('a1','m1','t1',0)",
            [],
        )
        .unwrap();
        let rowid: i64 = conn
            .query_row(
                "SELECT rowid FROM messages WHERE account_id='a1' AND id='m1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let v = unit_vec(0.9);
        upsert_vector(&conn, rowid, &v).unwrap();

        let results = knn_search(&conn, &v, None, 5).unwrap();
        assert!(!results.is_empty(), "expected at least one KNN hit");
        assert_eq!(results[0].0, rowid);
        assert!(
            results[0].1 < 1e-3,
            "exact match should have ~0 distance, got {}",
            results[0].1
        );
    }

    #[test]
    fn knn_with_candidate_restriction_only_returns_candidates() {
        let conn = conn_with_schema();
        conn.execute("INSERT INTO accounts(id, created_at) VALUES ('a1', 0)", [])
            .unwrap();
        for (id, seed) in [("m1", 0.9f32), ("m2", 0.1), ("m3", -0.9)] {
            conn.execute(
                "INSERT INTO messages(account_id, id, thread_id, internal_date) VALUES ('a1',?1,'t1',0)",
                params![id],
            )
            .unwrap();
            let rowid: i64 = conn
                .query_row(
                    "SELECT rowid FROM messages WHERE account_id='a1' AND id=?1",
                    params![id],
                    |r| r.get(0),
                )
                .unwrap();
            upsert_vector(&conn, rowid, &unit_vec(seed)).unwrap();
        }
        let m2_rowid: i64 = conn
            .query_row(
                "SELECT rowid FROM messages WHERE account_id='a1' AND id='m2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let query = unit_vec(0.9); // closest to m1, but candidates restrict to m2
        let results = knn_search(&conn, &query, Some(&[m2_rowid]), 5).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, m2_rowid);
    }

    #[test]
    fn index_state_round_trips_and_detects_model_change() {
        let conn = conn_with_schema();
        let mut state = get_index_state(&conn, "a1").unwrap();
        assert_eq!(state.cursor_rowid, 0);
        assert!(!needs_reindex(
            &state,
            "embeddinggemma-300m-q4",
            STORED_DIM,
            "k"
        ));

        state.embed_model_id = "embeddinggemma-300m-q4".to_string();
        state.embed_dim = STORED_DIM as i64;
        state.pipeline_key = Some("k".to_string());
        state.cursor_rowid = 42;
        state.indexed_count = 10;
        upsert_index_state(&conn, &state).unwrap();

        let reloaded = get_index_state(&conn, "a1").unwrap();
        assert_eq!(reloaded.cursor_rowid, 42);
        assert_eq!(reloaded.indexed_count, 10);
        assert!(!needs_reindex(
            &reloaded,
            "embeddinggemma-300m-q4",
            STORED_DIM,
            "k"
        ));
        assert!(needs_reindex(&reloaded, "bge-small-en-v1.5", 384, "k"));
    }

    #[test]
    fn reset_index_for_account_clears_vectors_and_state() {
        let conn = conn_with_schema();
        conn.execute("INSERT INTO accounts(id, created_at) VALUES ('a1', 0)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO messages(account_id, id, thread_id, internal_date) VALUES ('a1','m1','t1',0)",
            [],
        )
        .unwrap();
        let rowid: i64 = conn
            .query_row("SELECT rowid FROM messages WHERE id='m1'", [], |r| r.get(0))
            .unwrap();
        upsert_vector(&conn, rowid, &unit_vec(0.5)).unwrap();
        let mut state = get_index_state(&conn, "a1").unwrap();
        state.embed_model_id = "embeddinggemma-300m-q4".to_string();
        upsert_index_state(&conn, &state).unwrap();

        reset_index_for_account(&conn, "a1").unwrap();

        let count: i64 = conn
            .query_row("SELECT count(*) FROM message_vec", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        let reloaded = get_index_state(&conn, "a1").unwrap();
        assert_eq!(reloaded.embed_model_id, "");
    }

    #[test]
    fn delete_vectors_by_rowids_removes_rows() {
        let conn = conn_with_schema();
        conn.execute("INSERT INTO accounts(id, created_at) VALUES ('a1', 0)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO messages(account_id, id, thread_id, internal_date) VALUES ('a1','m1','t1',0)",
            [],
        )
        .unwrap();
        let rowid: i64 = conn
            .query_row("SELECT rowid FROM messages WHERE id='m1'", [], |r| r.get(0))
            .unwrap();
        upsert_vector(&conn, rowid, &unit_vec(0.5)).unwrap();
        delete_vectors_by_rowids(&conn, &[rowid]).unwrap();
        let count: i64 = conn
            .query_row("SELECT count(*) FROM message_vec", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }
}
