//! Bounded-work size probes for the operator-first gate.
//!
//! The gate only needs "is the structured side at most `cap` messages?". Each
//! probe below reads at most `cap + 1` index entries and never joins or scans
//! `messages` as a whole, so its cost is bounded by the cap, not the mailbox:
//!
//! * `from:` `to:` `cc:` `bcc:` count `message_recipients` rows for that kind
//!   through `idx_message_recipients_lookup (account_id, addr_norm, kind)`
//!   (one range seek per account). Recipient rows are an upper bound on distinct
//!   messages. `LIKE 'term%'` cannot use the index on a BINARY column, so the
//!   equivalent range `addr_norm >= term AND addr_norm <= term||U+10FFFF` is
//!   used; stored values and the term are both lowercase, so the two agree.
//!   A bare term also matches `name_norm`, which has no index: that side is
//!   not probed, so for bare terms the bound is an address-side estimate. The
//!   gate only picks a strategy; the candidate SQL still applies the full
//!   predicate, so results never depend on it.
//! * `has:attachment` counts the partial index
//!   `idx_messages_has_attachments_partial` (only attachment rows are in it).
//! * positive `label:` walks `idx_thread_labels_label_account_thread (label,..)`
//!   and `idx_messages_thread` for the messages of those threads (exact count).
//!
//! Several operators: the smallest single bound is a valid upper bound for the
//! conjunction. No probe-able operator means no bound: the caller declines.
use super::query::SearchTerms;
use super::store::SearchStore;

type ProbeSql = (String, Vec<String>);

fn count_wrapper(inner: &str, cap: i64) -> String {
    format!(
        "SELECT COUNT(*) FROM (SELECT 1 FROM {inner} LIMIT {})",
        cap.saturating_add(1)
    )
}

/// Index-range probe for one recipient operator, or `None` when the term shape
/// (`@domain` leading wildcard, or LIKE wildcard characters) has no cheap form.
fn recipient_probe(kind: &str, term: &str, cap: i64) -> Option<ProbeSql> {
    let t = term.trim().to_lowercase();
    if t.starts_with('@') || t.contains(['%', '_', '\\']) {
        return None;
    }
    let hi = format!("{t}\u{10FFFF}");
    let inner = format!(
        "message_recipients mr
         WHERE mr.account_id IN (SELECT id FROM accounts)
           AND mr.kind = '{kind}'
           AND mr.addr_norm >= ? AND mr.addr_norm <= ?"
    );
    Some((count_wrapper(&inner, cap), vec![t, hi]))
}

/// Every cheap probe applicable to `q`, each with its params in bind order.
pub fn probe_sqls(q: &SearchTerms, cap: i64) -> Vec<ProbeSql> {
    let mut out = Vec::new();
    for (kind, term) in [
        ("from", &q.from),
        ("to", &q.to),
        ("cc", &q.cc),
        ("bcc", &q.bcc),
    ] {
        if let Some(p) = term.as_deref().and_then(|t| recipient_probe(kind, t, cap)) {
            out.push(p);
        }
    }
    if q.has_attachment {
        out.push((
            count_wrapper("messages att WHERE att.has_attachments = 1", cap),
            vec![],
        ));
    }
    if let Some(l) = q
        .label
        .as_deref()
        .filter(|l| !q.label_negated && !l.is_empty())
    {
        let inner = "thread_labels tl CROSS JOIN messages lm
             ON lm.account_id = tl.account_id AND lm.thread_id = tl.thread_id
             WHERE tl.label = ?";
        out.push((count_wrapper(inner, cap), vec![l.to_string()]));
    }
    out
}

/// Smallest upper bound over the applicable probes, capped at `cap + 1`
/// ("over the cap"). `None` when no cheap probe applies.
pub fn structured_upper_bound<S: SearchStore>(
    store: &S,
    q: &SearchTerms,
    cap: i64,
) -> Result<Option<i64>, String> {
    let mut best: Option<i64> = None;
    for (sql, params) in probe_sqls(q, cap) {
        let n = store
            .query_count(&sql, &params)
            .map_err(|e| format!("probe structured candidates: {e}"))?;
        best = Some(best.map_or(n, |b| b.min(n)));
        if n == 0 {
            break;
        }
    }
    Ok(best)
}
