//! Hybrid query path (C5 item 4): the FTS5 keyword ranking and the vector
//! ranking, both in the caller's scope, merged by reciprocal rank fusion
//! (`fusion.rs`). Meaning-based search
//! ("emails about the budget meeting" matches "Q3 forecast discussion") is a
//! natural-language layer on top of the existing keyword search — it never
//! replaces FTS5 (still the default/primary engine; this module is additive)
//! and never touches the Gmail-operator query language (from:/to:/has:/...)
//! from L11 — those keep going through `db::search::search_threads_scoped`
//! unchanged. Semantic search takes a plain natural-language string.
//!
//! All DB access here runs through the reader connection + `spawn_blocking`
//! at the command layer (commands/semantic.rs), per L3's contract — this
//! module itself is connection-agnostic (takes `&Connection`) so it composes
//! with that without duplicating the async wiring.

use rightkit_db::rusqlite::Connection;

use super::embedder::{Embedder, STORED_DIM};
use super::fusion::{fuse, RRF_K};
use super::query_scope::{fts_ranked_hits, in_scope_hits};
use super::storage;
use crate::db::{row_to_thread, ThreadRow, LBL_AGG_CTE, MSG_AGG_CTE, THREAD_COLS, THREAD_JOINS};

/// FTS5 escaping for a free-text semantic query: treat the whole string as
/// an AND-of-words match (same shape `SearchTerms::fts()` produces for bare
/// text) rather than exposing FTS5 operator syntax to what is meant to be a
/// natural-language box.
fn fts_match_expr(query: &str) -> Option<String> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|w: &String| !w.is_empty())
        .collect();
    if words.is_empty() {
        return None;
    }
    Some(
        words
            .iter()
            .map(|w| format!("\"{w}\"*"))
            .collect::<Vec<_>>()
            .join(" AND "),
    )
}

/// Resolve a set of message rowids into `ThreadRow`s (one row per distinct
/// thread, most-recent-vector-hit-first via the caller's rowid ordering),
/// reusing the exact CTE/projection the FTS/list paths already use so
/// semantic results render identically to any other search result.
fn thread_rows_for_message_rowids(
    conn: &Connection,
    rowids: &[i64],
) -> Result<Vec<ThreadRow>, String> {
    if rowids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = vec!["?"; rowids.len()].join(",");
    let sql = format!(
        "WITH hit_threads AS (
           SELECT DISTINCT m.account_id, m.thread_id
           FROM messages m
           WHERE m.rowid IN ({placeholders})
         ),
         base AS (
           SELECT t.* FROM hit_threads ht
           JOIN threads t ON t.account_id = ht.account_id AND t.id = ht.thread_id
         ), {MSG_AGG_CTE}, {LBL_AGG_CTE}
         SELECT {THREAD_COLS}
         FROM base t
         JOIN accounts a ON a.id = t.account_id
         {THREAD_JOINS}"
    );
    let params_vec: Vec<&dyn rightkit_db::rusqlite::ToSql> = rowids
        .iter()
        .map(|r| r as &dyn rightkit_db::rusqlite::ToSql)
        .collect();
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("prepare semantic thread rows: {e}"))?;
    let rows = stmt
        .query_map(params_vec.as_slice(), row_to_thread)
        .map_err(|e| format!("query semantic thread rows: {e}"))?;
    rows.map(|r| r.map_err(|e| format!("row semantic thread rows: {e}")))
        .collect()
}

/// Order-preserving: `thread_rows_for_message_rowids` doesn't guarantee
/// output order matches `rowids`' rank order, so re-sort the resulting
/// `ThreadRow`s by the best (lowest-ranked) rowid that hit each thread.
fn reorder_by_rank(
    conn: &Connection,
    mut rows: Vec<ThreadRow>,
    ranked_rowids: &[i64],
) -> Result<Vec<ThreadRow>, String> {
    if rows.is_empty() {
        return Ok(rows);
    }
    // rowid -> (account_id, thread_id)
    let placeholders = vec!["?"; ranked_rowids.len()].join(",");
    let sql = format!(
        "SELECT rowid, account_id, thread_id FROM messages WHERE rowid IN ({placeholders})"
    );
    let params_vec: Vec<&dyn rightkit_db::rusqlite::ToSql> = ranked_rowids
        .iter()
        .map(|r| r as &dyn rightkit_db::rusqlite::ToSql)
        .collect();
    let mapping: Vec<(i64, String, String)> =
        crate::db::query_map_collect(conn, &sql, params_vec.as_slice(), "reorder_by_rank", |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
    let mapping_by_rowid: std::collections::HashMap<i64, (String, String)> = mapping
        .into_iter()
        .map(|(rowid, aid, tid)| (rowid, (aid, tid)))
        .collect();
    let mut rank: std::collections::HashMap<(String, String), usize> =
        std::collections::HashMap::new();
    for (i, rowid) in ranked_rowids.iter().enumerate() {
        if let Some(key) = mapping_by_rowid.get(rowid) {
            rank.entry(key.clone()).or_insert(i);
        }
    }
    rows.sort_by_key(|r| {
        rank.get(&(r.account_id.clone(), r.thread_id.clone()))
            .copied()
            .unwrap_or(usize::MAX)
    });
    Ok(rows)
}

/// Embed the query text (no database access, so call it BEFORE taking a
/// connection). `None` for a blank query.
pub fn embed_query_vec(
    query: &str,
    embedder: &dyn Embedder,
) -> Result<Option<[f32; STORED_DIM]>, String> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let query_native = embedder.embed_query(trimmed)?;
    let mut arr = [0f32; STORED_DIM];
    arr.copy_from_slice(&query_native);
    Ok(Some(arr))
}

/// The database half of [`hybrid_search_threads`], given the pre-computed
/// query embedding and the embedder's `index_key`. Returns no results (the
/// same "not ready" answer as a disabled index) while any account in scope
/// holds vectors from a different pipeline, so wrong-space matches are never
/// served during a re-embed.
pub fn hybrid_search_with_vec(
    conn: &Connection,
    query: &str,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
    query_vec: &[f32; STORED_DIM],
    index_key: &str,
) -> Result<Vec<ThreadRow>, String> {
    const FTS_CANDIDATE_LIMIT: usize = 200;
    const VECTOR_K: usize = 50;
    // The nearest-neighbour scan is global; over-fetch so enough survive the
    // scope filter, then keep the nearest VECTOR_K of them.
    const VECTOR_SCAN: usize = VECTOR_K * 4;

    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    if !storage::vectors_match_key(conn, account, index_key)? {
        return Ok(Vec::new());
    }

    let keyword = match fts_match_expr(trimmed) {
        Some(expr) => fts_ranked_hits(conn, &expr, account, label, category, FTS_CANDIDATE_LIMIT)?,
        None => Vec::new(),
    };
    let nearest: Vec<i64> = storage::knn_search(conn, query_vec, None, VECTOR_SCAN)?
        .into_iter()
        .map(|(rowid, _)| rowid)
        .collect();
    let vector = in_scope_hits(conn, &nearest, account, label, category, VECTOR_K)?;

    let ranked_rowids = fuse(&keyword, &vector, RRF_K);
    if ranked_rowids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = thread_rows_for_message_rowids(conn, &ranked_rowids)?;
    reorder_by_rank(conn, rows, &ranked_rowids)
}
