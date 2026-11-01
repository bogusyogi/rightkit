use super::candidates::FTS_HIT_MEMBER;
use super::query::{contains_any_term, excerpt_around, SearchTerms};
use super::store::{SearchRow, SearchStore};

/// Since schema v9 `fts_messages.rowid == messages.rowid`. Each statement
/// first narrows to the messages of the returned threads (the `keys` triples)
/// and only then asks the full-text index, per message, whether that rowid
/// matches (`MATCH ? AND rowid = m.rowid`, a point lookup). The hit set of the
/// whole mailbox is never materialised, so cost tracks the page (at most 200
/// threads) rather than the number of matches.
///
/// Returns `(count_sql, excerpt_sql)` given the `keys(k, a, t)` row-source SQL.
/// Parameter order for both: the `keys` triples first, then the FTS MATCH
/// expression last.
pub fn enrich_statements(key_pairs_sql: &str) -> (String, String) {
    let count_sql = format!(
        "WITH keys(k, a, t) AS ({key_pairs_sql})
         SELECT keys.k, COUNT(DISTINCT m.id)
         FROM keys
         JOIN messages m
           ON m.account_id = keys.a AND m.thread_id = keys.t
         WHERE {FTS_HIT_MEMBER}
         GROUP BY keys.k"
    );
    let excerpt_sql = format!(
        "WITH keys(k, a, t) AS ({key_pairs_sql})
         SELECT keys.k, m.id,
                COALESCE(m.subject, ''),
                COALESCE(m.snippet, ''),
                COALESCE(
                  (SELECT body FROM message_bodies b
                   WHERE b.account_id = m.account_id
                     AND b.message_id = m.id
                     AND b.mime_type = 'text/plain'
                   LIMIT 1),
                  m.snippet,
                  m.subject,
                  ''
                )
         FROM keys
         JOIN messages m
           ON m.account_id = keys.a AND m.thread_id = keys.t
         WHERE {FTS_HIT_MEMBER}
         GROUP BY keys.k
         HAVING m.internal_date = MAX(m.internal_date)"
    );
    (count_sql, excerpt_sql)
}

/// Fill `match_count`, the best matching message and its excerpt on every row of an FTS query.
pub fn enrich_search_matches<S: SearchStore>(
    store: &S,
    out: &mut [S::Row],
    q: &SearchTerms,
) -> Result<(), String> {
    // Batch-enrich all result rows with FTS match_count + excerpt in two
    // queries instead of 2×N queries.
    if let Some(fts) = q.fts() {
        if !out.is_empty() {
            // Build a (account_id, thread_id) IN list. SQLite doesn't support
            // tuple IN, so we use a single-column approach: match on thread_id
            // constrained by account_id via a per-row AND — equivalent but
            // simpler is a CTE / VALUES row. Simplest: iterate with
            // "account_id||'|'||thread_id IN (...)" pattern using a composite
            // string key, or use a temporary table. Simplest portable approach:
            // just build an IN clause on thread_id and also re-check account_id
            // in the WHERE (all rows from a scoped search share account_id in
            // practice, and any cross-account rows are safely filtered by the
            // account_id = m.account_id condition below).

            // Collect the (account_id, thread_id) keys we need to enrich. Same
            // thread_id can appear under two accounts — keying on the composite
            // avoids overwriting the first account's excerpt with the second's.
            let row_keys: Vec<(String, String)> = out
                .iter()
                .map(|r| (r.account_id().to_string(), r.thread_id().to_string()))
                .collect();
            let n_rows = row_keys.len();

            // --- Query 1: match_count per (account_id, thread_id) ---
            // Bind a (account_id, thread_id) pair, joined by the sentinel byte
            // '\u{1f}' (unit separator — never appears in a Gmail id) so the
            // result rows group exactly under the same composite key the
            // HashMap uses. SQLite's lack of tuple-IN means a CTE / VALUES row
            // is the simplest portable way to express this.
            let key_param = |k: &(String, String)| format!("{}\u{1f}{}", k.0, k.1);
            let key_pairs_sql = row_keys
                .iter()
                .map(|_| "SELECT ? AS k, ? AS a, ? AS t".to_string())
                .collect::<Vec<_>>()
                .join(" UNION ALL ");
            let (count_sql, excerpt_sql) = enrich_statements(&key_pairs_sql);
            let mut count_params: Vec<String> = Vec::with_capacity(n_rows * 3 + 1);
            for k in &row_keys {
                count_params.push(key_param(k));
                count_params.push(k.0.clone());
                count_params.push(k.1.clone());
            }
            count_params.push(fts.clone());

            let counts: std::collections::HashMap<String, i64> = store
                .query_text_rows(&count_sql, &count_params)
                .map_err(|e| format!("query batch count: {e}"))?
                .into_iter()
                .filter_map(|mut r| {
                    let n = r.get(1)?.parse::<i64>().ok()?;
                    Some((std::mem::take(&mut r[0]), n))
                })
                .collect();

            // --- Query 2: best matching message per (account_id, thread_id) ---
            // Pick the most recent message in the (account_id, thread_id)
            // group whose FTS row matches.
            let mut excerpt_params: Vec<String> = Vec::with_capacity(n_rows * 3 + 1);
            for k in &row_keys {
                excerpt_params.push(key_param(k));
                excerpt_params.push(k.0.clone());
                excerpt_params.push(k.1.clone());
            }
            excerpt_params.push(fts.clone());

            // (composite_key -> (message_id, subject, snippet, body))
            let excerpts: std::collections::HashMap<String, (String, String, String, String)> =
                store
                    .query_text_rows(&excerpt_sql, &excerpt_params)
                    .map_err(|e| format!("query batch excerpt: {e}"))?
                    .into_iter()
                    .filter_map(|r| {
                        let [k, mid, subject, snippet, body]: [String; 5] = r.try_into().ok()?;
                        Some((k, (mid, subject, snippet, body)))
                    })
                    .collect();

            // Apply batch results back to each row using the composite key.
            let terms = q.positive_text_terms();
            for row in out {
                let k = key_param(&(row.account_id().to_string(), row.thread_id().to_string()));
                if let Some(&cnt) = counts.get(&k) {
                    row.set_match_count(cnt);
                    if let Some((mid, subject, snippet, body)) = excerpts.get(&k) {
                        let text = [subject, snippet, body]
                            .into_iter()
                            .find(|candidate| contains_any_term(candidate, &terms))
                            .unwrap_or(snippet);
                        let excerpt = Some(excerpt_around(&text.replace('\n', " "), &terms, 180))
                            .filter(|s| !s.trim().is_empty());
                        row.set_matching_message(mid.clone(), excerpt);
                    }
                }
            }
        }
    }

    Ok(())
}
