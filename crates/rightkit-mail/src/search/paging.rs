//! Keyset paging for search results.
//!
//! Every search path (general, FTS-candidate, dense, operator-first and
//! has:attachment) builds a `base` CTE ordered newest first and capped at one
//! page. The builders emit the markers below; `apply_page` swaps them for the
//! real page size and, when a cursor is given, a predicate that keeps only
//! threads strictly after it in `(last_message_at DESC, rowid ASC)` order (a
//! `rightkit_search` keyset with a lookup-resolved rowid tiebreak).
//! The `rowid` tiebreak is the order `idx_threads_unified` already delivers, so
//! the dense newest-first walk still needs no sort.
use rightkit_search::page::{Cursor, Keyset};
use rightkit_search::sql::{Compiled, Param};
use serde::Deserialize;

/// Threads per search page.
pub const SEARCH_PAGE_SIZE: usize = 200;

/// Position of the last thread of the previous page.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SearchCursor {
    pub last_message_at: i64,
    pub account_id: String,
    pub thread_id: String,
}

/// Extra column on `base` carrying the tiebreak for the outer ORDER BY.
pub const BASE_COLS: &str = "SELECT t.*, t.rowid AS page_rid";
/// Where the cursor predicate goes (always inside a WHERE of `base`).
pub const PAGE_PRED: &str = " /*page_pred*/";
/// Order and cap of the `base` CTE.
pub const BASE_TAIL: &str = "ORDER BY t.last_message_at DESC, t.rowid ASC LIMIT 200";
/// Order of the final SELECT (over `base`).
pub const OUTER_ORDER: &str = "ORDER BY t.last_message_at DESC, t.page_rid ASC";

pub fn apply_page(
    sql: String,
    mut params: Vec<String>,
    before: Option<&SearchCursor>,
    limit: usize,
) -> (String, Vec<String>) {
    debug_assert!(sql.contains(BASE_TAIL) && sql.contains(PAGE_PRED));
    let tail = format!("ORDER BY t.last_message_at DESC, t.rowid ASC LIMIT {limit}");
    let sql = sql.replacen(BASE_TAIL, &tail, 1);
    let Some(c) = before else {
        return (sql.replacen(PAGE_PRED, "", 1), params);
    };
    // The cursor predicate comes from `rightkit_search`'s keyset: newest first, then rowid.
    // The cursor's thread is a lookup tiebreak (its rowid, resolved per account; a deleted
    // thread resolves to -1, which keeps the whole tie group), and the range bound on
    // `last_message_at` lets the index seek.
    let keyset = Keyset::new()
        .desc("t.last_message_at")
        .asc("t.rowid")
        .range_bound()
        .cursor_expr(0, "CAST(? AS INTEGER)", Vec::new())
        .cursor_expr(
            1,
            "COALESCE((SELECT x.rowid FROM threads x WHERE x.account_id = ? AND x.id = ?), -1)",
            vec![Param::Text(c.account_id.clone())],
        );
    let cursor = Cursor::new(vec![
        Param::Int(c.last_message_at),
        Param::Text(c.thread_id.clone()),
    ]);
    let any = Compiled {
        where_sql: "1=1".to_string(),
        params: Vec::new(),
        fts_match: None,
    };
    let clauses = any
        .page_clauses(&keyset, Some(&cursor))
        .expect("the search keyset is static and matches its cursor");
    params.extend(clauses.params.into_iter().map(|p| match p {
        Param::Int(i) => i.to_string(),
        Param::Text(s) => s,
    }));
    let pred = format!(" AND {}", clauses.where_sql);
    (sql.replacen(PAGE_PRED, &pred, 1), params)
}
