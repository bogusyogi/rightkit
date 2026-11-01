//! Schema concern for the SQLCipher store: the base table/index DDL plus the
//! idempotent column migration that adds (and backfills) the write-maintained
//! `app_name`/`outcome_kind` search columns.

use crate::history::HistoryError;
use rightkit_db::rusqlite;

pub(super) fn initialize_schema(conn: &mut rusqlite::Connection) -> Result<(), HistoryError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS delivery_history (
           delivery_id TEXT PRIMARY KEY,
           delivered_at_ms INTEGER NOT NULL,
           record_json TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_delivered
           ON delivery_history (delivered_at_ms DESC);",
    )
    .map_err(|err| HistoryError::new(format!("schema init: {err}")))?;
    migrate_search_columns(conn)
}

/// Exactly the app-label CASE expression `HISTORY_FILTER_SQL` used to
/// evaluate per-row, per-query (mirrors `crate::history::
/// history_target_label`). Kept only for the one-time backfill below —
/// `push()` computes `app_name` in Rust for every row from here on.
const APP_NAME_CASE_SQL: &str = r#"
CASE
  WHEN json_extract(record_json, '$.source.kind') = 'file_transcription' THEN
    CASE
      WHEN coalesce(json_extract(record_json, '$.source.file_name'), '') = ''
        THEN 'File transcription'
      ELSE 'File · ' || json_extract(record_json, '$.source.file_name')
    END
  ELSE coalesce(
    json_extract(record_json, '$.target.process_name'),
    json_extract(record_json, '$.target.window_title'),
    'Unknown app'
  )
END
"#;

/// Add the write-maintained `app_name`/`outcome_kind` columns (+ their
/// indexes) if a prior version of this DB predates them, and backfill any
/// existing rows once. Idempotent: a no-op once both columns exist, so this
/// runs unconditionally on every open at negligible cost (two
/// `pragma_table_info` lookups). The backfill UPDATE uses the exact same
/// `json_extract`/CASE expressions `HISTORY_FILTER_SQL` used to evaluate
/// live, so backfilled values match what every prior query would have
/// computed, bit-for-bit — no observable behavior change, only where the
/// work happens (once here, not on every keystroke).
fn migrate_search_columns(conn: &mut rusqlite::Connection) -> Result<(), HistoryError> {
    let has_column = |name: &str| -> Result<bool, HistoryError> {
        rightkit_db::column_exists(conn, "delivery_history", name)
            .map_err(|e| HistoryError::new(format!("check column {name}: {e}")))
    };
    let had_app_name = has_column("app_name")?;
    let had_outcome_kind = has_column("outcome_kind")?;
    if had_app_name && had_outcome_kind {
        return Ok(());
    }

    // The owner's write transaction begins IMMEDIATE, so the ALTER and the backfill reserve the
    // writer slot up front and a locked database fails at the begin, not mid-migration.
    let tx = rightkit_db::write_txn(conn)
        .map_err(|e| HistoryError::new(format!("begin column migration: {e}")))?;
    if !had_app_name {
        tx.execute_batch(
            "ALTER TABLE delivery_history ADD COLUMN app_name TEXT NOT NULL DEFAULT '';",
        )
        .map_err(|e| HistoryError::new(format!("add app_name column: {e}")))?;
    }
    if !had_outcome_kind {
        tx.execute_batch(
            "ALTER TABLE delivery_history ADD COLUMN outcome_kind TEXT NOT NULL DEFAULT '';",
        )
        .map_err(|e| HistoryError::new(format!("add outcome_kind column: {e}")))?;
    }
    tx.execute_batch(&format!(
        "UPDATE delivery_history SET
           app_name = lower({APP_NAME_CASE_SQL}),
           outcome_kind = lower(coalesce(json_extract(record_json, '$.outcome.kind'), ''));
         CREATE INDEX IF NOT EXISTS idx_outcome_delivered
           ON delivery_history (outcome_kind, delivered_at_ms DESC);
         CREATE INDEX IF NOT EXISTS idx_app_name ON delivery_history (app_name);"
    ))
    .map_err(|e| HistoryError::new(format!("backfill search columns: {e}")))?;
    tx.commit()
        .map_err(|e| HistoryError::new(format!("commit column migration: {e}")))
}
