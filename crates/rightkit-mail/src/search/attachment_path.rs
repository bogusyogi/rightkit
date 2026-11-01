use super::candidates::scope_filter_sql;
use super::paging::{BASE_COLS, BASE_TAIL, OUTER_ORDER, PAGE_PRED};
use super::thread_sql::*;

pub fn attachment_candidate_sql(
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
) -> (String, Vec<String>) {
    // Drive from the messages index: `idx_messages_has_attachments`
    // already exists on (account_id, has_attachments). The DISTINCT is
    // dedup-by-(account_id, thread_id) since a single thread can have
    // many messages with attachments.
    let (scope, params) = scope_filter_sql(account, label, category);
    let sql = format!(
        "WITH cand AS (
           SELECT DISTINCT m.account_id, m.thread_id
           FROM messages m
           WHERE m.has_attachments = 1{scope}
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
