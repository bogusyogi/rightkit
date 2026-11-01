//! Query helpers for `SQLCipherHistoryStore`: the shared WHERE clause, the
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_filter_values_trims_and_escapes_like_wildcards_for_search() {
        let filter = HistoryFilter {
            search: "  50%_off  ".to_string(),
            app: "  Notes  ".to_string(),
            cancelled_only: true,
            ..HistoryFilter::default()
        };
        let (search, pattern, from_ms, until_ms, app, cancelled) =
            sql_filter_values(&filter).expect("valid filter");
        assert_eq!(search, "50%_off", "search field is trimmed, not escaped");
        assert!(pattern.starts_with('%') && pattern.ends_with('%'));
        // Literal % and _ in the term must be ESCAPE-prefixed so LIKE treats
        // them as literal characters, not wildcards (HISTORY_FILTER_SQL uses
        // `ESCAPE '\'`).
        assert!(
            pattern.contains("\\%"),
            "literal % must be escaped: {pattern}"
        );
        assert!(
            pattern.contains("\\_"),
            "literal _ must be escaped: {pattern}"
        );
        assert_eq!(app, "Notes", "app is trimmed; case-folding happens in SQL");
        assert_eq!(cancelled, 1);
        assert!(from_ms.is_none());
        assert!(until_ms.is_none());
    }

    #[test]
    fn sql_filter_values_defaults_to_dictations_scope_flag() {
        let (.., cancelled) = sql_filter_values(&HistoryFilter::default()).unwrap();
        assert_eq!(
            cancelled, 0,
            "cancelled_only=false must bind 0, matching HISTORY_FILTER_SQL's `?6 = 1` check"
        );
    }

    #[test]
    fn sql_filter_values_converts_timestamps_to_sqlite_i64() {
        let filter = HistoryFilter {
            from_ms: Some(1_000),
            until_ms: Some(2_000),
            ..HistoryFilter::default()
        };
        let (_, _, from_ms, until_ms, _, _) = sql_filter_values(&filter).unwrap();
        assert_eq!(from_ms, Some(1_000));
        assert_eq!(until_ms, Some(2_000));
    }
}
