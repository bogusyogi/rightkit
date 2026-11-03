//! Thread search over the store: the query compiler and planner are `rightkit-mail`'s
//! ([`rightkit_mail::search`]); this module runs their SQL against a connection and maps rows
//! to [`ThreadRow`].
use super::*;
use rightkit_db::rusqlite::types::ValueRef;
use rightkit_mail::search::{SearchRow, SearchStore};

pub use rightkit_mail::search::{SearchCursor, SEARCH_PAGE_SIZE};

impl SearchRow for ThreadRow {
    fn account_id(&self) -> &str {
        &self.account_id
    }

    fn thread_id(&self) -> &str {
        &self.thread_id
    }

    fn matching_message_id(&self) -> Option<&str> {
        self.matching_message_id.as_deref()
    }

    fn set_match_count(&mut self, count: i64) {
        self.match_count = count;
    }

    fn set_matching_message(&mut self, message_id: String, excerpt: Option<String>) {
        self.matching_message_id = Some(message_id);
        self.matching_message_excerpt = excerpt;
    }

    fn set_matching_snippet(&mut self, snippet: String) {
        self.matching_message_snippet = Some(snippet);
    }
}

/// A connection as the search planner's database.
struct ConnStore<'a>(&'a Connection);

impl SearchStore for ConnStore<'_> {
    type Row = ThreadRow;

    fn now_ms(&self) -> i64 {
        now_ms()
    }

    fn query_threads(&self, sql: &str, params: &[String]) -> Result<Vec<ThreadRow>, String> {
        let mut stmt = self
            .0
            .prepare(sql)
            .map_err(|e| format!("prepare search: {e}"))?;
        let rows = stmt
            .query_map(
                rightkit_db::rusqlite::params_from_iter(params.iter()),
                row_to_thread,
            )
            .map_err(|e| format!("query search: {e}"))?;
        rows.map(|r| r.map_err(|e| format!("row search: {e}")))
            .collect()
    }

    fn query_count(&self, sql: &str, params: &[String]) -> Result<i64, String> {
        self.0
            .query_row(
                sql,
                rightkit_db::rusqlite::params_from_iter(params.iter()),
                |r| r.get::<_, i64>(0),
            )
            .map_err(|e| format!("count search: {e}"))
    }

    fn query_text_rows(&self, sql: &str, params: &[String]) -> Result<Vec<Vec<String>>, String> {
        let mut stmt = self
            .0
            .prepare(sql)
            .map_err(|e| format!("prepare search: {e}"))?;
        let columns = stmt.column_count();
        let rows = stmt
            .query_map(
                rightkit_db::rusqlite::params_from_iter(params.iter()),
                |r| {
                    let mut out = Vec::with_capacity(columns);
                    for i in 0..columns {
                        out.push(match r.get_ref(i)? {
                            ValueRef::Null => String::new(),
                            ValueRef::Integer(n) => n.to_string(),
                            ValueRef::Real(f) => f.to_string(),
                            ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                            ValueRef::Blob(b) => String::from_utf8_lossy(b).into_owned(),
                        });
                    }
                    Ok(out)
                },
            )
            .map_err(|e| format!("query search: {e}"))?;
        rows.map(|r| r.map_err(|e| format!("row search: {e}")))
            .collect()
    }
}

/// Search threads inside the same mailbox scope as the visible list.
/// `account` None searches every account; `label` None searches every mirrored
/// thread; `ALL_MAIL` searches every non-trash/non-spam thread.
pub fn search_threads_scoped(
    conn: &Connection,
    query: &str,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
) -> Result<Vec<ThreadRow>, String> {
    rightkit_mail::search::search_threads_scoped(&ConnStore(conn), query, account, label, category)
}

/// One keyset page: up to `limit` threads strictly after `before` (or the
/// newest `limit` when `before` is None), newest first.
pub fn search_threads_page(
    conn: &Connection,
    query: &str,
    account: Option<&str>,
    label: Option<&str>,
    category: Option<&str>,
    before: Option<&SearchCursor>,
    limit: usize,
) -> Result<Vec<ThreadRow>, String> {
    rightkit_mail::search::search_threads_page(
        &ConnStore(conn),
        query,
        account,
        label,
        category,
        before,
        limit,
    )
}
