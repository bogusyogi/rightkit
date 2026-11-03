//! Candidate lookups for the hybrid query: the keyword ranking and the vector
//! ranking, both limited to the same (account, label, category) scope.
//!
//! The scope mirrors `db::search::candidates::scope_filter_sql`'s message-level
//! gate; the semantic path never needs the operator predicates.

use rightkit_db::rusqlite::types::Value;
use rightkit_db::rusqlite::{params_from_iter, Connection};

use super::fusion::Hit;

/// SQL fragment (starting with ` AND`) over alias `m` plus its `?` values.
fn scope_clause(
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
) -> (String, Vec<Value>) {
    let mut scope = String::new();
    let mut values: Vec<Value> = Vec::new();
    if let Some(a) = account.filter(|a| !a.trim().is_empty()) {
        scope.push_str(" AND m.account_id = ?");
        values.push(Value::Text(a.to_string()));
    }
    if let Some(l) = label.filter(|l| !l.trim().is_empty()) {
        if l == ALL_MAIL {
            scope.push_str(
                " AND NOT EXISTS(SELECT 1 FROM thread_labels tls
                    WHERE tls.account_id = m.account_id AND tls.thread_id = m.thread_id
                      AND tls.label IN ('TRASH','SPAM'))",
            );
        } else {
            scope.push_str(
                " AND EXISTS(SELECT 1 FROM thread_labels tls
                    WHERE tls.account_id = m.account_id AND tls.thread_id = m.thread_id
                      AND tls.label = ?)",
            );
            values.push(Value::Text(l.to_string()));
            if l == "INBOX" {
                if let Some(cat) = category {
                    if cat == "PRIMARY" {
                        scope.push_str(
                            " AND NOT EXISTS(SELECT 1 FROM thread_labels tlc
                                WHERE tlc.account_id = m.account_id AND tlc.thread_id = m.thread_id
                                  AND tlc.label IN ('CATEGORY_PROMOTIONS','CATEGORY_SOCIAL','CATEGORY_UPDATES','CATEGORY_FORUMS'))",
                        );
                    } else if cat.starts_with("CATEGORY_") {
                        scope.push_str(
                            " AND EXISTS(SELECT 1 FROM thread_labels tlc
                                WHERE tlc.account_id = m.account_id AND tlc.thread_id = m.thread_id
                                  AND tlc.label = ?)",
                        );
                        values.push(Value::Text(cat.to_string()));
                    }
                }
            }
        }
    }
    (scope, values)
}

fn hit_of(r: &rightkit_db::rusqlite::Row<'_>) -> rightkit_db::rusqlite::Result<Hit> {
    Ok(Hit {
        rowid: r.get(0)?,
        internal_date: r.get(1)?,
        message_id: r.get(2)?,
    })
}

/// Keyword ranking: FTS5 matches in scope, best (bm25) first, at most `limit`.
pub(super) fn fts_ranked_hits(
    conn: &Connection,
    fts_query: &str,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
    limit: usize,
) -> Result<Vec<Hit>, String> {
    let (scope, mut values) = scope_clause(account, label, category);
    values.insert(0, Value::Text(fts_query.to_string()));
    let sql = format!(
        "SELECT m.rowid, m.internal_date, m.id
         FROM fts_messages JOIN messages m ON m.rowid = fts_messages.rowid
         WHERE fts_messages MATCH ?{scope}
         ORDER BY fts_messages.rank, m.internal_date DESC, m.id
         LIMIT {limit}"
    );
    crate::db::query_map_collect(
        conn,
        &sql,
        params_from_iter(values.iter()),
        "fts_ranked_hits",
        hit_of,
    )
}

/// Vector ranking: keep the nearest rows (given best-first) that are in scope,
/// at most `limit`, in the same order. The nearest-neighbour scan is global,
/// so this is where the scope is applied to it.
pub(super) fn in_scope_hits(
    conn: &Connection,
    nearest: &[i64],
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
    limit: usize,
) -> Result<Vec<Hit>, String> {
    if nearest.is_empty() {
        return Ok(Vec::new());
    }
    let (scope, scope_values) = scope_clause(account, label, category);
    let placeholders = vec!["?"; nearest.len()].join(",");
    let sql = format!(
        "SELECT m.rowid, m.internal_date, m.id FROM messages m
         WHERE m.rowid IN ({placeholders}){scope}"
    );
    let mut values: Vec<Value> = nearest.iter().map(|r| Value::Integer(*r)).collect();
    values.extend(scope_values);
    let found: Vec<Hit> = crate::db::query_map_collect(
        conn,
        &sql,
        params_from_iter(values.iter()),
        "in_scope_hits",
        hit_of,
    )?;
    let mut by_rowid: std::collections::HashMap<i64, Hit> =
        found.into_iter().map(|h| (h.rowid, h)).collect();
    Ok(nearest
        .iter()
        .filter_map(|r| by_rowid.remove(r))
        .take(limit)
        .collect())
}
