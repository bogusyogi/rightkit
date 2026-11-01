//! Candidate-driven search planner: a query anchored on `has:attachment` or on a text term
//! resolves its `(account_id, thread_id)` candidate set from the messages / `fts_messages` side
//! FIRST, then joins `threads`, instead of driving from `threads` (a full scan on a large
//! mailbox) and asking "does a matching message exist?" per row.
use super::attachment_path::attachment_candidate_sql;
use super::paging::{BASE_COLS, BASE_TAIL, OUTER_ORDER, PAGE_PRED};
use super::query::SearchTerms;
use super::thread_sql::*;

/// Per-message full-text membership test: `m.rowid` against the hit set,
/// which SQLite evaluates once per statement (an uncorrelated list subquery).
/// Binds one `?` (the MATCH expression). Measured on a 164k-message mailbox:
/// 10 to 50 ms, against 0.7 to 0.9 s for a correlated point lookup per message.
pub const FTS_HIT_MEMBER: &str =
    "m.rowid IN (SELECT rowid FROM fts_messages WHERE fts_messages MATCH ?)";

/// The account/label/category scope filter shared by every candidate query
/// (has:attachment, FTS-anchored text). Extracted from the old
/// `attachment_candidate_sql` so the FTS candidate path applies the exact
/// same scoping rules instead of drifting from them.
pub fn scope_filter_sql(
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
) -> (String, Vec<String>) {
    let mut params: Vec<String> = Vec::new();
    let mut scope = String::new();
    if let Some(a) = account.filter(|a| !a.trim().is_empty()) {
        scope.push_str(" AND m.account_id = ?");
        params.push(a.to_string());
    }
    if let Some(l) = label.filter(|l| !l.trim().is_empty()) {
        if l == ALL_MAIL {
            scope.push_str(
                " AND NOT EXISTS(
                    SELECT 1 FROM thread_labels tls
                    WHERE tls.account_id = m.account_id
                      AND tls.thread_id = m.thread_id
                      AND tls.label IN ('TRASH','SPAM')
                  )",
            );
        } else {
            scope.push_str(
                " AND EXISTS(
                    SELECT 1 FROM thread_labels tls
                    WHERE tls.account_id = m.account_id
                      AND tls.thread_id = m.thread_id
                      AND tls.label = ?
                  )",
            );
            params.push(l.to_string());
            if l == "INBOX" {
                if let Some(cat) = category {
                    if cat == "PRIMARY" {
                        scope.push_str(&format!(
                            " AND NOT EXISTS(
                                SELECT 1 FROM thread_labels tlc
                                WHERE tlc.account_id = m.account_id
                                  AND tlc.thread_id = m.thread_id
                                  AND tlc.label IN ({SEARCH_OTHER_CATEGORIES})
                              )"
                        ));
                    } else if cat.starts_with("CATEGORY_") {
                        scope.push_str(
                            " AND EXISTS(
                                SELECT 1 FROM thread_labels tlc
                                WHERE tlc.account_id = m.account_id
                                  AND tlc.thread_id = m.thread_id
                                  AND tlc.label = ?
                              )",
                        );
                        params.push(cat.to_string());
                    }
                }
            }
        }
    }
    (scope, params)
}

/// True when the query is anchored purely on FTS text (see the gate comment in
/// `select_search_sql`). Shared with the dense-hit override in `dense.rs`.
pub fn fts_anchored(q: &SearchTerms) -> bool {
    q.fts().is_some()
        && !q.has_attachment
        && q.from.is_none()
        && q.to.is_none()
        && q.cc.is_none()
        && q.bcc.is_none()
        && q.subject.is_none()
        && q.filename.is_none()
        && q.label.as_deref().filter(|l| !l.is_empty()).is_none()
        && q.negated_from.is_empty()
        && q.negated_to.is_empty()
        && q.cc_negated.is_empty()
        && q.bcc_negated.is_empty()
        && q.negated_text.is_empty()
        && !q.label_negated
        && q.after_ms.is_none()
        && q.before_ms.is_none()
        && q.newer_ms.is_none()
        && q.older_ms.is_none()
        && !q.unread
        && !q.starred
        && !q.important
        && q.is_read.is_none()
        // same reasoning as has_attachments_only above — the FTS
        // candidate-CTE shape has no size:/rfc822msgid: predicate either.
        && q.size_min.is_none()
        && q.size_max.is_none()
        && q.rfc822_message_id.is_none()
}

/// Decide which SQL to run for this parsed query: the has:attachment
/// candidate path, the FTS-text candidate path, or the general threads-first
/// path, and build the params for whichever is chosen.
///
/// `where_sql` is the fully-built general-path WHERE clause (with `p` as its
/// already-collected params) for when neither candidate gate fires.
pub fn select_search_sql(
    q: &SearchTerms,
    where_sql: &str,
    general_params: Vec<String>,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
) -> (String, Vec<String>) {
    // Candidate-driven short-circuit for high-selectivity operators. When the
    // query is anchored on a boolean that lives on `messages` (has_attachments,
    // filename:, or any text term that maps to fts_messages), we resolve the
    // candidate `(account_id, thread_id)` set FIRST from the messages side,
    // then join threads + apply the remaining filters. Without this, the
    // planner was driving from `threads` (full SCAN t USING INDEX
    // idx_threads_unified = 125k rows) and then asking "is there a matching
    // message?" per row — multi-second p95 on the 690 MB DB.
    let has_attachments_only = q.has_attachment
        && q.fts().is_none()
        && q.from.is_none()
        && q.to.is_none()
        && q.cc.is_none()
        && q.bcc.is_none()
        && q.subject.is_none()
        && q.filename.is_none()
        && q.label.as_deref().filter(|l| !l.is_empty()).is_none()
        && q.or_groups.is_empty()
        && q.negated_from.is_empty()
        && q.negated_to.is_empty()
        && q.negated_text.is_empty()
        && !q.label_negated
        // size:/larger:/smaller:/rfc822msgid: have no representation in
        // the has:attachment candidate-CTE shape (attachment_candidate_sql
        // only filters on has_attachments + scope) — fall through to the
        // general path when any of these are present so the predicate is
        // never silently dropped.
        && q.size_min.is_none()
        && q.size_max.is_none()
        && q.rfc822_message_id.is_none();

    // a query anchored purely on FTS text terms (free text and/or OR
    // groups, no other structured operator) resolves candidates from
    // `fts_messages` FIRST via the same candidate-CTE shape as
    // has:attachment, rather than driving from `threads` and probing FTS per
    // row. Any structured operator (from:/to:/cc:/bcc:/subject:/filename:/
    // label:/negation/has:attachment) falls through to the general path
    // below, since those still need the full per-message EXISTS/NOT EXISTS
    // machinery this lane doesn't change.
    let fts_candidates_only = fts_anchored(q);

    if has_attachments_only {
        attachment_candidate_sql(account, label, category)
    } else if fts_candidates_only {
        fts_candidate_sql(
            &q.fts().expect("checked by fts_candidates_only"),
            account,
            label,
            category,
        )
    } else {
        let sql = format!(
            "WITH base AS (
               {BASE_COLS}
               FROM threads t
               WHERE {where_sql}{PAGE_PRED}
               {BASE_TAIL}
             ), {MSG_AGG_CTE}, {LBL_AGG_CTE}
             SELECT {THREAD_COLS}
             FROM base t
             JOIN accounts a ON a.id=t.account_id
             {THREAD_JOINS}
             {OUTER_ORDER}"
        );
        (sql, general_params)
    }
}

/// Build the candidate-driven SQL for a query anchored on an FTS text match
/// (free text and/or OR-groups, with no other structured operator active —
/// see `fts_anchored` for the exact gate). Mirrors
/// `attachment_candidate_sql`'s shape: a `cand` CTE narrows to matching
/// `(account_id, thread_id)` pairs FIRST via the `fts_messages` MATCH index,
/// then joins `threads` — never a threads-first scan.
pub fn fts_candidate_sql(
    fts_query: &str,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
) -> (String, Vec<String>) {
    let (scope, mut params) = scope_filter_sql(account, label, category);
    // fts MATCH param must be bound first (it's the first `?` in WHERE order).
    params.insert(0, fts_query.to_string());
    let sql = format!(
        "WITH cand AS (
           SELECT DISTINCT m.account_id, m.thread_id
           FROM messages m
           WHERE m.rowid IN (SELECT rowid FROM fts_messages WHERE fts_messages MATCH ?){scope}
         ),
         base AS (
           {BASE_COLS}
           FROM cand
           JOIN threads t
             ON t.account_id = cand.account_id AND t.id = cand.thread_id
           WHERE 1=1{PAGE_PRED}
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

/// Relevance-ordered variant of `fts_candidate_sql` (issue #12): each thread
/// is scored by its best message, `MIN(bm25(fts_messages))` (bm25 is lower for
/// a better match), and `base` takes the best `limit` after `offset`, ties
/// broken newest first then by rowid. There is no keyset cursor: bm25 scores
/// are not a stable position, so paging is by offset. Param order matches
/// `fts_candidate_sql` (MATCH first, then scope).
pub fn fts_ranked_sql(
    fts_query: &str,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
    offset: usize,
    limit: usize,
) -> (String, Vec<String>) {
    let (scope, mut params) = scope_filter_sql(account, label, category);
    params.insert(0, fts_query.to_string());
    let sql = format!(
        "WITH hits AS MATERIALIZED (
           -- bm25() is valid only while FTS5 itself is scanning the MATCH, so it is read in a
           -- query over fts_messages alone; joining and grouping happen on its result.
           SELECT rowid AS hit_rid, bm25(fts_messages) AS hit_score
           FROM fts_messages
           WHERE fts_messages MATCH ?
         ),
         cand AS (
           SELECT m.account_id, m.thread_id, MIN(hits.hit_score) AS rel_score
           FROM hits
           JOIN messages m ON m.rowid = hits.hit_rid
           WHERE 1=1{scope}
           GROUP BY m.account_id, m.thread_id
         ),
         base AS (
           SELECT t.*, t.rowid AS page_rid, cand.rel_score AS rel_score
           FROM cand
           JOIN threads t
             ON t.account_id = cand.account_id AND t.id = cand.thread_id
           ORDER BY cand.rel_score ASC, t.last_message_at DESC, t.rowid ASC
           LIMIT {limit} OFFSET {offset}
         ), {MSG_AGG_CTE}, {LBL_AGG_CTE}
         SELECT {THREAD_COLS}
         FROM base t
         JOIN accounts a ON a.id=t.account_id
         {THREAD_JOINS}
         ORDER BY t.rel_score ASC, t.last_message_at DESC, t.page_rid ASC"
    );
    (sql, params)
}
