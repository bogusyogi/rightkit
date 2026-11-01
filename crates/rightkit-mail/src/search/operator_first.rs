//! Operator-first candidate strategy: a selective structured operator
//! (`from:` `to:` `cc:` `bcc:` `has:attachment` `label:`) combined with a
//! full-text term.
//!
//! The general path evaluates the full-text term as `m.rowid IN (SELECT rowid
//! FROM fts_messages ... MATCH ?)`, which reads the entire hit set before the
//! selective filter can discard most of it. Here the candidate set is driven
//! from `messages` (the structured predicates, via the recipient/label
//! indexes) and each candidate rowid is tested against the index with a point
//! lookup. Every predicate, negation and thread-scope filter is built by the
//! same code as the general path, so the thread set and its newest-first order
//! are identical.
//!
//! Chosen only when two bounded probes say the structured side is the smaller
//! one; otherwise the general path runs unchanged.
use super::candidates::FTS_HIT_MEMBER;
use super::dense::fts_is_dense;
use super::paging::{BASE_COLS, BASE_TAIL, OUTER_ORDER, PAGE_PRED};
use super::predicates::{positive_message_predicates, thread_level_negation};
use super::query::SearchTerms;
use super::scope::thread_scope_filters;
use super::store::SearchStore;
use super::thread_sql::*;

/// Largest structured-side message count for which driving from it is used.
pub const OPERATOR_FIRST_MAX: i64 = 20_000;

#[derive(Clone, Copy)]
pub struct Tuning {
    /// Structured side must have at most this many messages (probe reads at
    /// most `max_structured + 1` rows).
    pub max_structured: i64,
    /// Also require the full-text hit count to exceed the structured count
    /// (probe reads at most that many + 1 rowids).
    pub require_fts_larger: bool,
    /// With at most this many full-text hits the general path (which drives
    /// from the hits) is already cheap, so the structured probe is skipped.
    /// The structured probe is itself bounded (`probe`), this just avoids it.
    pub fts_small: i64,
}

/// See [`Tuning::fts_small`].
pub const FTS_SMALL_HITS: i64 = 5_000;

pub const DEFAULT_TUNING: Tuning = Tuning {
    max_structured: OPERATOR_FIRST_MAX,
    require_fts_larger: true,
    fts_small: FTS_SMALL_HITS,
};

fn has_selective_operator(q: &SearchTerms) -> bool {
    q.from.is_some()
        || q.to.is_some()
        || q.cc.is_some()
        || q.bcc.is_some()
        || q.has_attachment
        || (!q.label_negated && q.label.as_deref().filter(|l| !l.is_empty()).is_some())
}

/// The per-message predicates of the general path without the two thread join
/// conditions and without the full-text predicate (always the third entry when
/// the query has a full-text term), plus their params in bind order.
fn structured_predicates(q: &SearchTerms) -> (Vec<String>, Vec<String>) {
    let (msg, p) = positive_message_predicates(q);
    debug_assert!(msg[2].contains("fts_messages"));
    (
        msg.into_iter().skip(3).collect(),
        p.into_iter().skip(1).collect(),
    )
}

pub fn operator_first_override<S: SearchStore>(
    store: &S,
    q: &SearchTerms,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
) -> Result<Option<(String, Vec<String>)>, String> {
    operator_first_with(store, q, account, label, category, DEFAULT_TUNING)
}

pub fn operator_first_with<S: SearchStore>(
    store: &S,
    q: &SearchTerms,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
    tuning: Tuning,
) -> Result<Option<(String, Vec<String>)>, String> {
    let Some(fts) = q.fts() else { return Ok(None) };
    if !has_selective_operator(q) {
        return Ok(None);
    }
    let (preds, params) = structured_predicates(q);
    if preds.is_empty() {
        return Ok(None);
    }
    if tuning.fts_small > 0 && !fts_is_dense(store, &fts, tuning.fts_small)? {
        return Ok(None);
    }
    let where_msg = preds.join(" AND ");
    // Bounded-work probe (see `probe`): never scans `messages`. No cheap probe
    // for this operator shape means the general path runs.
    let Some(structured) =
        super::operator_first_probe::structured_upper_bound(store, q, tuning.max_structured)?
    else {
        return Ok(None);
    };
    if structured > tuning.max_structured {
        return Ok(None);
    }
    if tuning.require_fts_larger && !fts_is_dense(store, &fts, structured)? {
        return Ok(None);
    }
    Ok(Some(operator_first_sql(
        q,
        &fts,
        &where_msg,
        params,
        account,
        label,
        category,
        store.now_ms(),
    )))
}

#[allow(clippy::too_many_arguments)]
fn operator_first_sql(
    q: &SearchTerms,
    fts: &str,
    where_msg: &str,
    mut params: Vec<String>,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
    now_ms: i64,
) -> (String, Vec<String>) {
    // Candidate params: structured first, then the MATCH expression.
    params.push(fts.to_string());
    let (neg_sql, neg_params) = thread_level_negation(q);
    let mut rest = format!("1=1{neg_sql}");
    params.extend(neg_params);
    thread_scope_filters(&mut rest, &mut params, q, account, label, category, now_ms);
    let sql = format!(
        "WITH cand AS (
           SELECT DISTINCT m.account_id, m.thread_id
           FROM messages m
           WHERE {where_msg} AND {FTS_HIT_MEMBER}
         ),
         base AS (
           {BASE_COLS}
           FROM cand
           JOIN threads t
             ON t.account_id = cand.account_id AND t.id = cand.thread_id
           WHERE {rest}{PAGE_PRED}
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
