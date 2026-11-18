//! Query helpers for `DeliveryHistoryStore`: the shared WHERE clause, the
//! filter-to-bind-params translation, row decoding, and the u64→i64 timestamp
//! conversion SQLite's INTEGER range requires.

use crate::delivery::DeliveryRecord;
use crate::history::{HistoryError, HistoryFilter};
use rightkit_db::rusqlite;

// `app_name`/`outcome_kind` are plain indexed columns (write-maintained by
// `push()`, backfilled once by `migrate_search_columns`) — filtering on them
// here is an index lookup instead of a `json_extract`/CASE evaluation against
// every row on every query, including the default (no-filter) load. Only the
// free-text `search` term still scans `record_json`; that term is empty on
// the default load and on the common outcome-only tab switches, so those hit
// the `idx_outcome_delivered` index for both the filter and the `ORDER BY
// delivered_at_ms DESC` and never touch `record_json` at all.
pub(super) const HISTORY_FILTER_SQL: &str = r#"
WHERE (?1 = '' OR lower(record_json) LIKE ?2 ESCAPE '\')
  AND (?3 IS NULL OR delivered_at_ms >= ?3)
  AND (?4 IS NULL OR delivered_at_ms < ?4)
  AND (?5 = '' OR app_name = lower(?5))
  AND CASE WHEN ?6 = 1
    THEN outcome_kind = 'cancelled'
    ELSE outcome_kind NOT IN ('cancelled', 'draft')
  END
"#;

pub(super) type SqlFilterValues = (String, String, Option<i64>, Option<i64>, String, i64);

pub(super) fn sql_filter_values(filter: &HistoryFilter) -> Result<SqlFilterValues, HistoryError> {
    let search = filter.search.trim().to_string();
    let escaped = search
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    Ok((
        search,
        format!("%{}%", escaped.to_ascii_lowercase()),
        filter.from_ms.map(sqlite_i64_ms).transpose()?,
        filter.until_ms.map(sqlite_i64_ms).transpose()?,
        filter.app.trim().to_string(),
        if filter.cancelled_only { 1 } else { 0 },
    ))
}

pub(super) fn row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeliveryRecord> {
    let json: String = row.get(0)?;
    serde_json::from_str(&json).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

pub(super) fn sqlite_i64_ms(ms: u64) -> Result<i64, HistoryError> {
    i64::try_from(ms).map_err(|_| HistoryError::new("timestamp exceeds sqlite INTEGER range"))
}
