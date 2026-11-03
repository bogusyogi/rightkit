//! The search orchestrator: parse the query, assemble the per-message predicates, thread-level
//! negation and scope filters, pick the cheapest candidate strategy, run one keyset page and
//! decorate the rows with match counts, excerpts and snippets.
use super::candidates::select_search_sql;
use super::dense::dense_override;
use super::enrich::enrich_search_matches;
use super::operator_first::operator_first_override;
use super::paging::{apply_page, SearchCursor, SEARCH_PAGE_SIZE};
use super::predicates::{positive_message_predicates, thread_level_negation};
use super::query::parse_search;
use super::scope::thread_scope_filters;
use super::snippet::attach_snippets;
use super::store::SearchStore;

/// Search threads inside the same mailbox scope as the visible list.
/// `account` None searches every account; `label` None searches every mirrored
/// thread; [`ALL_MAIL`](super::thread_sql::ALL_MAIL) searches every non-trash/non-spam thread.
pub fn search_threads_scoped<S: SearchStore>(
    store: &S,
    query: &str,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
) -> Result<Vec<S::Row>, String> {
    search_threads_page(
        store,
        query,
        account,
        label,
        category,
        None,
        SEARCH_PAGE_SIZE,
    )
}

/// One keyset page: up to `limit` threads strictly after `before` (or the
/// newest `limit` when `before` is None), newest first.
pub fn search_threads_page<S: SearchStore>(
    store: &S,
    query: &str,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
    before: Option<&SearchCursor>,
    limit: usize,
) -> Result<Vec<S::Row>, String> {
    let q = parse_search(query);
    if q.is_empty() {
        return Ok(Vec::new());
    }

    let (msg, mut p) = positive_message_predicates(&q);
    let mut where_sql = format!(
        "EXISTS(SELECT 1 FROM messages m WHERE {})",
        msg.join(" AND ")
    );

    let (neg_sql, neg_params) = thread_level_negation(&q);
    where_sql.push_str(&neg_sql);
    p.extend(neg_params);

    thread_scope_filters(
        &mut where_sql,
        &mut p,
        &q,
        account,
        label,
        category,
        store.now_ms(),
    );

    // Candidate-driven planner: has:attachment and FTS-text-anchored queries resolve their
    // `(account_id, thread_id)` set from the messages/FTS side first instead of driving from
    // `threads`.
    let (sql, extra_params) = select_search_sql(&q, &where_sql, p, account, label, category);
    p = extra_params;
    let bounded = match dense_override(store, &q, account, label, category)? {
        Some(o) => Some(o),
        None => operator_first_override(store, &q, account, label, category)?,
    };
    let (sql, p) = bounded.unwrap_or((sql, p));
    let (sql, p) = apply_page(sql, p, before, limit);
    let mut out = store
        .query_threads(&sql, &p)
        .map_err(|e| format!("query search: {e}"))?;

    enrich_search_matches(store, &mut out, &q)?;
    attach_snippets(store, &mut out, &q)?;

    Ok(out)
}
