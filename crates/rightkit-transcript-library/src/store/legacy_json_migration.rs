//! One-time migration from `FileHistoryStore`'s `history.json` into the
//! SQLCipher-backed `delivery_history` table. Plaintext cleanup remains pending
//! until a safe identity-bound cleanup protocol is available.

use super::queries::sqlite_i64_ms;
use super::{outcome_kind_str, EventSink};
use crate::delivery::DeliveryRecord;
use crate::history::{history_target_label, migration_verified, HistoryError};
use rightkit_db::rusqlite;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// One-time migration from FileHistoryStore JSON if present + no rows.
///
/// Takes `&mut Connection` (not `&Connection`) so the migration transaction
/// can still be opened with `transaction_with_behavior(Immediate)` exactly as
/// `open_at` did before this was split out — `Connection::unchecked_transaction`
/// would silently downgrade this to a DEFERRED transaction, which is a
/// behaviour change.
pub(super) fn migrate_legacy_json_if_present(
    conn: &mut rusqlite::Connection,
    legacy_json: &Path,
    events: EventSink,
) -> Result<(), HistoryError> {
    match fs::metadata(legacy_json) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(HistoryError::new(format!(
                "inspect legacy history {}: {error}",
                legacy_json.display()
            )))
        }
    }

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM delivery_history", [], |row| row.get(0))
        .map_err(|e| HistoryError::new(format!("count history before migration: {e}")))?;
    if count != 0 {
        events(
            "history_migration_cleanup_pending",
            &format!(
                "database already contains {count} rows; preserving legacy plaintext at {}",
                legacy_json.display()
            ),
        );
        return Ok(());
    }

    let raw = fs::read_to_string(legacy_json).map_err(|e| {
        HistoryError::new(format!(
            "read legacy history {}: {e}",
            legacy_json.display()
        ))
    })?;
    let records = serde_json::from_str::<Vec<DeliveryRecord>>(&raw).map_err(|e| {
        HistoryError::new(format!(
            "parse legacy history {}: {e}",
            legacy_json.display()
        ))
    })?;

    let mut inserted = 0usize;
    with_full_synchronous_commit(conn, |conn| {
        let tx = rightkit_db::write_txn(conn)
            .map_err(|e| HistoryError::new(format!("begin migration tx: {e}")))?;
        let mut seen_ids = HashSet::with_capacity(records.len());
        for record in &records {
            // Preserve the old INSERT OR IGNORE behavior for duplicate IDs,
            // while ensuring unrelated SQL constraint failures abort the batch.
            if !seen_ids.insert(record.delivery_id.as_str()) {
                continue;
            }
            let json = serde_json::to_string(record)
                .map_err(|e| HistoryError::new(format!("serialize migrated record: {e}")))?;
            let delivered_at_ms = sqlite_i64_ms(record.delivered_at_ms)?;
            // Keep indexed values in sync with ordinary writes; a bare three-column
            // insert would leave migrated rows invisible to app/outcome filters.
            let app_name = history_target_label(record).to_ascii_lowercase();
            let outcome_kind = outcome_kind_str(&record.outcome);
            inserted += tx
                .execute(
                    "INSERT INTO delivery_history
                     (delivery_id, delivered_at_ms, record_json, app_name, outcome_kind)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        record.delivery_id,
                        delivered_at_ms,
                        json,
                        app_name,
                        outcome_kind
                    ],
                )
                .map_err(|e| HistoryError::new(format!("insert migrated record: {e}")))?;
        }
        let row_count: i64 = tx
            .query_row("SELECT COUNT(*) FROM delivery_history", [], |row| row.get(0))
            .map_err(|e| HistoryError::new(format!("count history after migration: {e}")))?;
        if !migration_verified(&records, row_count) {
            events(
                "history_migration_row_mismatch",
                &format!(
                    "json_records={} db_rows={} inserted_this_run={} rolling back and preserving source {}",
                    records.len(),
                    row_count,
                    inserted,
                    legacy_json.display()
                ),
            );
            return Err(HistoryError::new(format!(
                "legacy history row mismatch: {} distinct records, {row_count} database rows; source preserved",
                seen_ids.len()
            )));
        }
        tx.commit()
            .map_err(|e| HistoryError::new(format!("commit migration tx: {e}")))?;
        Ok(row_count)
    })?;

    // Cleanup stays pending until the host provides an identity-bound, durable
    // cleanup protocol. Never overwrite a path that may have been replaced.
    events(
        "history_migration_cleanup_pending",
        &format!("migrated legacy history; preserving plaintext at {}", legacy_json.display()),
    );
    Ok(())
}

fn with_full_synchronous_commit<T>(
    conn: &mut rusqlite::Connection,
    operation: impl FnOnce(&mut rusqlite::Connection) -> Result<T, HistoryError>,
) -> Result<T, HistoryError> {
    let previous: i64 = conn
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .map_err(|e| HistoryError::new(format!("read SQLite synchronous mode: {e}")))?;

    let operation_result = conn
        .execute_batch("PRAGMA synchronous = FULL")
        .map_err(|e| HistoryError::new(format!("enable SQLite FULL sync for migration: {e}")))
        .and_then(|()| operation(conn));
    let restore_result = conn
        .execute_batch(&format!("PRAGMA synchronous = {previous}"))
        .map_err(|e| HistoryError::new(format!("restore SQLite synchronous mode: {e}")));

    match (operation_result, restore_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(operation_error), Ok(())) => Err(operation_error),
        (Ok(_), Err(restore_error)) => Err(restore_error),
        (Err(operation_error), Err(restore_error)) => Err(HistoryError::new(format!(
            "{operation_error}; additionally {restore_error}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::migrate_legacy_json_if_present;
    use crate::delivery::{DeliveryOutcome, DeliveryRecord, TargetSnapshot};
    use rightkit_db::rusqlite;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static CLEANUP_PENDING: AtomicBool = AtomicBool::new(false);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock before epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "rightkit-transcript-store-{}-{stamp}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn event_sink(name: &str, _detail: &str) {
        if name == "history_migration_cleanup_pending" {
            CLEANUP_PENDING.store(true, Ordering::SeqCst);
        }
    }

    fn connection() -> rusqlite::Connection {
        let mut conn = rusqlite::Connection::open_in_memory().expect("open SQLite connection");
        super::super::migrations::initialize_schema(&mut conn).expect("initialize schema");
        conn
    }

    fn record(id: &str) -> DeliveryRecord {
        DeliveryRecord::new(
            id,
            format!("transcript {id}"),
            DeliveryOutcome::Pasted,
            TargetSnapshot::empty(),
        )
    }

    fn synchronous(conn: &rusqlite::Connection) -> i64 {
        conn.query_row("PRAGMA synchronous", [], |row| row.get(0))
            .expect("read synchronous mode")
    }

    #[test]
    fn malformed_legacy_json_is_reported_and_preserved() {
        let temp = TempDir::new();
        let legacy = temp.path().join("history.json");
        let bytes = b"not valid history JSON";
        fs::write(&legacy, bytes).expect("write test source");
        let mut conn = connection();

        let result = migrate_legacy_json_if_present(&mut conn, &legacy, event_sink);

        assert!(result.is_err());
        assert_eq!(fs::read(&legacy).expect("read preserved source"), bytes);
    }

    #[test]
    fn read_failure_is_reported_without_removing_source_path() {
        let temp = TempDir::new();
        let legacy = temp.path().join("history.json");
        fs::create_dir(&legacy).expect("make unreadable source path a directory");
        let mut conn = connection();

        let result = migrate_legacy_json_if_present(&mut conn, &legacy, event_sink);

        assert!(result.is_err());
        assert!(legacy.is_dir());
    }

    #[test]
    fn failed_insert_rolls_back_batch_restores_sync_and_preserves_source() {
        let temp = TempDir::new();
        let legacy = temp.path().join("history.json");
        let bytes = serde_json::to_vec(&vec![record("first"), record("fail")])
            .expect("serialize test records");
        fs::write(&legacy, &bytes).expect("write test source");
        let mut conn = connection();
        conn.execute_batch(
            "CREATE TRIGGER fail_migrated_insert BEFORE INSERT ON delivery_history
             WHEN NEW.delivery_id = 'fail'
             BEGIN SELECT RAISE(ABORT, 'injected insert failure'); END;
             PRAGMA synchronous = NORMAL;",
        )
        .expect("install failure trigger and set sync mode");
        let previous_sync = synchronous(&conn);

        let result = migrate_legacy_json_if_present(&mut conn, &legacy, event_sink);

        assert!(result.is_err());
        assert_eq!(synchronous(&conn), previous_sync);
        let row_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM delivery_history", [], |row| row.get(0))
            .expect("count rolled-back rows");
        assert_eq!(row_count, 0);
        assert_eq!(fs::read(&legacy).expect("read preserved source"), bytes);
    }

    #[test]
    fn verified_migration_keeps_plaintext_and_reports_pending_cleanup() {
        CLEANUP_PENDING.store(false, Ordering::SeqCst);
        let temp = TempDir::new();
        let legacy = temp.path().join("history.json");
        let bytes = serde_json::to_vec(&vec![record("one")]).expect("serialize test record");
        fs::write(&legacy, &bytes).expect("write test source");
        let mut conn = connection();
        conn.execute_batch("PRAGMA synchronous = NORMAL;")
            .expect("set normal sync mode");
        let previous_sync = synchronous(&conn);

        migrate_legacy_json_if_present(&mut conn, &legacy, event_sink)
            .expect("migrate test record");

        assert_eq!(synchronous(&conn), previous_sync);
        assert!(CLEANUP_PENDING.load(Ordering::SeqCst));
        assert_eq!(fs::read(&legacy).expect("read retained plaintext"), bytes);
        let row_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM delivery_history", [], |row| row.get(0))
            .expect("count migrated row");
        assert_eq!(row_count, 1);
    }

    #[test]
    fn mismatch_keeps_source_and_does_not_touch_existing_backup_path() {
        let temp = TempDir::new();
        let legacy = temp.path().join("history.json");
        let backup = temp.path().join("history.json.bak");
        let source_bytes = serde_json::to_vec(&vec![record("ignored")])
            .expect("serialize test record");
        let backup_bytes = b"unrelated existing file";
        fs::write(&legacy, &source_bytes).expect("write test source");
        fs::write(&backup, backup_bytes).expect("write unrelated backup path");
        let mut conn = connection();
        conn.execute_batch(
            "CREATE TRIGGER ignore_migrated_insert BEFORE INSERT ON delivery_history
             BEGIN SELECT RAISE(IGNORE); END;",
        )
        .expect("install ignored-insert trigger");

        let result = migrate_legacy_json_if_present(&mut conn, &legacy, event_sink);

        assert!(result.is_err());
        assert_eq!(fs::read(&legacy).expect("read preserved source"), source_bytes);
        assert_eq!(fs::read(&backup).expect("read untouched backup"), backup_bytes);
        let row_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM delivery_history", [], |row| row.get(0))
            .expect("count rolled-back rows");
        assert_eq!(row_count, 0);
    }
}
