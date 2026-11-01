//! Bounded candidate strategy for FTS terms that match a large share of the
//! mailbox ("the", long phrases, broad prefixes).
//!
//! The default FTS candidate query (`candidates::fts_candidate_sql`) resolves
//! every matching message, DISTINCTs them to threads and only then sorts and
//! keeps the newest 200. That is ideal for selective terms and ruinous for
//! common ones (about 2 s on a 164k-message mailbox). When a cheap probe shows
//! the term has more than `DENSE_HIT_THRESHOLD` hits, this module instead walks
//! `threads` newest-first through `idx_threads_unified` and stops at the 200th
//! thread that has a hit, so cost tracks the page, not the hit count. Both
//! shapes return the same thread set in the same `last_message_at DESC` order.
use super::candidates::{fts_anchored, scope_filter_sql, FTS_HIT_MEMBER};
use super::paging::{BASE_COLS, BASE_TAIL, OUTER_ORDER, PAGE_PRED};
use super::query::SearchTerms;
use super::store::SearchStore;
use super::thread_sql::*;

/// Hits above this switch to the newest-first walk. Below it the hit set is
/// small enough that resolving it first is the faster plan.
pub const DENSE_HIT_THRESHOLD: i64 = 2000;

/// True when `fts_query` has more than `threshold` hits. Early-terminating:
/// reads at most `threshold + 1` rowids from the index.
pub fn fts_is_dense<S: SearchStore>(
    store: &S,
    fts_query: &str,
    threshold: i64,
) -> Result<bool, String> {
    let sql = format!(
        "SELECT COUNT(*) FROM
           (SELECT rowid FROM fts_messages WHERE fts_messages MATCH ? LIMIT {})",
        threshold.saturating_add(1)
    );
    let n = store
        .query_count(&sql, &[fts_query.to_string()])
        .map_err(|e| format!("probe fts hits: {e}"))?;
    Ok(n > threshold)
}

pub fn dense_override<S: SearchStore>(
    store: &S,
    q: &SearchTerms,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
) -> Result<Option<(String, Vec<String>)>, String> {
    dense_override_with(store, q, account, label, category, DENSE_HIT_THRESHOLD)
}

pub fn dense_override_with<S: SearchStore>(
    store: &S,
    q: &SearchTerms,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
    threshold: i64,
) -> Result<Option<(String, Vec<String>)>, String> {
    if !fts_anchored(q) {
        return Ok(None);
    }
    let Some(fts) = q.fts() else { return Ok(None) };
    if !fts_is_dense(store, &fts, threshold)? {
        return Ok(None);
    }
    Ok(Some(dense_candidate_sql(&fts, account, label, category)))
}

/// Newest-first walk: `base` takes the first 200 threads, in index order, that
/// contain a message that passes the per-message full-text point lookup (the
/// hit set is never materialised, so a phrase or prefix is not read in full)
/// and satisfy the scope. Param order matches `fts_candidate_sql`: MATCH first, then scope.
pub fn dense_candidate_sql(
    fts_query: &str,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
) -> (String, Vec<String>) {
    let (scope, mut params) = scope_filter_sql(account, label, category);
    params.insert(0, fts_query.to_string());
    let sql = format!(
        "WITH base AS (
           {BASE_COLS}
           FROM threads t
           WHERE EXISTS(
             SELECT 1 FROM messages m
             WHERE m.account_id = t.account_id AND m.thread_id = t.id
               AND {FTS_HIT_MEMBER}{scope}
           ){PAGE_PRED}
           {BASE_TAIL}
         ), {MSG_AGG_CTE}, {LBL_AGG_CTE}
         SELECT {THREAD_COLS}
         FROM base t
         JOIN accounts a ON a.id=t.account_id
         {THREAD_JOINS}
         {OUTER_ORDER}"
    );
    (sql, params)
}
