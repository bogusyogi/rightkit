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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::store::SearchRow;
    use std::cell::RefCell;

    struct Row;
    impl SearchRow for Row {
        fn account_id(&self) -> &str {
            "a"
        }
        fn thread_id(&self) -> &str {
            "t"
        }
        fn matching_message_id(&self) -> Option<&str> {
            None
        }
        fn set_match_count(&mut self, _: i64) {}
        fn set_matching_message(&mut self, _: String, _: Option<String>) {}
        fn set_matching_snippet(&mut self, _: String) {}
    }

    /// Records every statement; reports a small full-text hit set so the general paths run.
    #[derive(Default)]
    struct Recorder {
        thread_sql: RefCell<Vec<(String, Vec<String>)>>,
    }
    impl SearchStore for Recorder {
        type Row = Row;
        fn now_ms(&self) -> i64 {
            1_000
        }
        fn query_threads(&self, sql: &str, params: &[String]) -> Result<Vec<Row>, String> {
            self.thread_sql
                .borrow_mut()
                .push((sql.to_string(), params.to_vec()));
            Ok(Vec::new())
        }
        fn query_count(&self, _: &str, _: &[String]) -> Result<i64, String> {
            Ok(0)
        }
        fn query_text_rows(&self, _: &str, _: &[String]) -> Result<Vec<Vec<String>>, String> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn empty_query_runs_nothing() {
        let store = Recorder::default();
        let rows = search_threads_scoped(&store, "   ", None, None, None).unwrap();
        assert!(rows.is_empty());
        assert!(store.thread_sql.borrow().is_empty());
    }

    #[test]
    fn page_markers_are_resolved_and_limit_applied() {
        let store = Recorder::default();
        search_threads_page(
            &store,
            "invoice",
            Some("a@x.com"),
            Some("INBOX"),
            None,
            None,
            7,
        )
        .unwrap();
        let calls = store.thread_sql.borrow();
        let (sql, params) = &calls[0];
        assert!(sql.contains("LIMIT 7"), "{sql}");
        assert!(!sql.contains("page_pred"), "{sql}");
        assert!(params.iter().any(|p| p == "a@x.com"), "{params:?}");
    }

    #[test]
    fn snoozed_scope_binds_the_store_clock() {
        let store = Recorder::default();
        search_threads_page(&store, "from:bob", None, Some("SNOOZED"), None, None, 10).unwrap();
        let calls = store.thread_sql.borrow();
        assert!(calls[0].1.iter().any(|p| p == "1000"), "{:?}", calls[0].1);
    }
}
