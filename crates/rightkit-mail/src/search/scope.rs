//! Thread-level flag/date/folder/account/label scope filters.
//! `thread_scope_filters` appends every WHERE
//! clause that applies at thread grain (unread/starred/important/date
//! bounds/in:/account/label/category) — everything that isn't a per-message
//! predicate ([`super::predicates`]) or a negation (also in [`super::predicates`]).

use super::query::SearchTerms;
use super::thread_sql::*;

/// Append every thread-scoped (non-negated, non-per-message) filter to
/// `where_sql`, pushing bound params onto `p` in the same order the SQL
/// appends its `?` placeholders. `now_ms` is the clock for the `SNOOZED` scope.
pub fn thread_scope_filters(
    where_sql: &mut String,
    p: &mut Vec<String>,
    q: &SearchTerms,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
    now_ms: i64,
) {
    if q.unread {
        where_sql.push_str(" AND t.unread = 1");
    }
    if q.starred {
        where_sql.push_str(" AND t.starred = 1");
    }
    if q.important {
        where_sql.push_str(" AND t.important = 1");
    }
    if let Some(true) = q.is_read {
        where_sql.push_str(" AND t.unread = 0");
    }
    // Bind date bounds as params like every other clause. last_message_at has
    // INTEGER affinity, so SQLite applies numeric affinity to these text
    // params and the comparison stays numeric — no format!-into-SQL
    // interpolation.
    if let Some(a) = q.after_ms {
        where_sql.push_str(" AND t.last_message_at >= ?");
        p.push(a.to_string());
    }
    if let Some(b) = q.before_ms {
        where_sql.push_str(" AND t.last_message_at < ?");
        p.push(b.to_string());
    }
    if let Some(n) = q.newer_ms {
        where_sql.push_str(" AND t.last_message_at >= ?");
        p.push(n.to_string());
    }
    if let Some(o) = q.older_ms {
        where_sql.push_str(" AND t.last_message_at < ?");
        p.push(o.to_string());
    }
    if let Some(folder) = &q.in_folder {
        let folder_clause = match folder.as_str() {
            "INBOX" => Some("'INBOX'"),
            "SENT" => Some("'SENT'"),
            "DRAFT" => Some("'DRAFT'"),
            "DRAFTS" => Some("'DRAFT'"),
            "SPAM" => Some("'SPAM'"),
            "TRASH" => Some("'TRASH'"),
            "STARRED" => Some("'STARRED'"),
            "IMPORTANT" => Some("'IMPORTANT'"),
            "ANYWHERE" => None, // no extra constraint
            _ => None,
        };
        if let Some(c) = folder_clause {
            where_sql.push_str(&format!(
                " AND EXISTS(SELECT 1 FROM thread_labels tlf WHERE tlf.account_id = t.account_id AND tlf.thread_id = t.id AND tlf.label = {})",
                c
            ));
        }
    }
    if let Some(account) = account.filter(|a| !a.trim().is_empty()) {
        where_sql.push_str(" AND t.account_id = ?");
        p.push(account.to_string());
    }
    if let Some(label) = label.filter(|l| !l.trim().is_empty()) {
        match label {
            ALL_MAIL => {
                where_sql.push_str(
                    " AND NOT EXISTS(
                        SELECT 1 FROM thread_labels tlscope
                        WHERE tlscope.account_id = t.account_id
                          AND tlscope.thread_id = t.id
                          AND tlscope.label IN ('TRASH', 'SPAM')
                      )",
                );
            }
            "SNOOZED" => {
                where_sql.push_str(
                    " AND EXISTS(
                        SELECT 1 FROM snoozes sscope
                        WHERE sscope.account_id = t.account_id
                          AND sscope.thread_id = t.id
                          AND sscope.until_ms > ?
                      )",
                );
                p.push(now_ms.to_string());
            }
            _ => {
                where_sql.push_str(
                    " AND EXISTS(
                        SELECT 1 FROM thread_labels tlscope
                        WHERE tlscope.account_id = t.account_id
                          AND tlscope.thread_id = t.id
                          AND tlscope.label = ?
                      )",
                );
                p.push(label.to_string());
            }
        }
        if label == "INBOX" {
            match category {
                Some("PRIMARY") => where_sql.push_str(&format!(
                    " AND NOT EXISTS(
                        SELECT 1 FROM thread_labels tlcat
                        WHERE tlcat.account_id = t.account_id
                          AND tlcat.thread_id = t.id
                          AND tlcat.label IN ({SEARCH_OTHER_CATEGORIES})
                      )"
                )),
                Some(cat) if cat.starts_with("CATEGORY_") => {
                    where_sql.push_str(
                        " AND EXISTS(
                            SELECT 1 FROM thread_labels tlcat
                            WHERE tlcat.account_id = t.account_id
                              AND tlcat.thread_id = t.id
                              AND tlcat.label = ?
                          )",
                    );
                    p.push(cat.to_string());
                }
                _ => {}
            }
        }
    }
}
