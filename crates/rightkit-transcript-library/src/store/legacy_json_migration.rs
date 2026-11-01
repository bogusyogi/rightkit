//! One-time migration from `FileHistoryStore`'s `history.json` into the
//! SQLCipher-backed `delivery_history` table, plus the plaintext shred that
//! follows a verified migration.

use super::queries::sqlite_i64_ms;
use super::sqlcipher_store::outcome_kind_str;
use crate::delivery::DeliveryRecord;
use crate::history::{history_target_label, migration_verified, HistoryError};
use rightkit_db::rusqlite;
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
) -> Result<(), HistoryError> {
    if legacy_json.exists() {
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM delivery_history", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);
        if count == 0 {
            if let Ok(raw) = fs::read_to_string(legacy_json) {
                if let Ok(records) = serde_json::from_str::<Vec<DeliveryRecord>>(&raw) {
                    let mut inserted = 0usize;
                    // P-M9: one INSERT per record in SQLite's default autocommit
                    // mode makes each row its own disk fsync'd transaction, the
                    // dominant cost of a large one-time migration held under
                    // `runtime_lock` (see docs/PERF_AUDIT_2026-07-15.md).
                    // Wrapping the whole batch in one transaction keeps the
                    // exact same rows / exact same final DB state, just
                    // committed once instead of N times — same output, much
                    // less wall-clock time on the thread that's blocking the
                    // caller.
                    let tx = rightkit_db::write_txn(conn)
                        .map_err(|e| HistoryError::new(format!("begin migration tx: {e}")))?;
                    for r in &records {
                        if let Ok(json) = serde_json::to_string(r) {
                            let delivered_at_ms = sqlite_i64_ms(r.delivered_at_ms)?;
                            // Same write-maintained app_name/outcome_kind as
                            // push() (see there) — `initialize_schema` above
                            // already ran `migrate_search_columns` and added
                            // these columns before this loop runs, so a JSON
                            // row migrated here must populate them itself; a
                            // bare 3-column INSERT would leave both '' and
                            // silently break the cancelled/draft scope (and
                            // the app filter) for every legacy-migrated row.
                            let app_name = history_target_label(r).to_ascii_lowercase();
                            let outcome_kind = outcome_kind_str(&r.outcome);
                            if tx
                                .execute(
                                    "INSERT OR IGNORE INTO delivery_history
                                     (delivery_id, delivered_at_ms, record_json, app_name, outcome_kind)
                                     VALUES (?1, ?2, ?3, ?4, ?5)",
                                    rusqlite::params![
                                        r.delivery_id,
                                        delivered_at_ms,
                                        json,
                                        app_name,
                                        outcome_kind
                                    ],
                                )
                                .map(|n| n > 0)
                                .unwrap_or(false)
                            {
                                inserted += 1;
                            }
                        }
                    }
                    // Verify row count and SHRED the plaintext JSON before
                    // unlinking — leaving history.json(.bak) on disk defeats
                    // the SQLCipher stolen-laptop threat model. The gate
                    // compares against DISTINCT delivery_ids (see
                    // crate::history::migration_verified).
                    let row_count: i64 = tx
                        .query_row("SELECT COUNT(*) FROM delivery_history", [], |row| {
                            row.get(0)
                        })
                        .unwrap_or(-1);
                    tx.commit()
                        .map_err(|e| HistoryError::new(format!("commit migration tx: {e}")))?;
                    if migration_verified(&records, row_count) {
                        let _ = shred_and_remove(legacy_json);
                    } else {
                        crate::host::host().log(
                            "history_migration_row_mismatch",
                            &format!(
                                "json_records={} db_rows={} inserted_this_run={} keeping {} as .bak",
                                records.len(),
                                row_count,
                                inserted,
                                legacy_json.display()
                            ),
                        );
                        // rightkit-fs 0.4.0 (pinned until rightkit#319) has no
                        // publish_file: write the backup atomically, then drop
                        // the source only once the backup landed.
                        if let Ok(bytes) = fs::read(legacy_json) {
                            if rightkit_fs::atomic::atomic_write_private(
                                &legacy_json.with_extension("json.bak"),
                                &bytes,
                            )
                            .is_ok()
                            {
                                let _ = fs::remove_file(legacy_json);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Overwrite a file with zeros (twice + flush) then unlink. Best-effort defense
/// against trivial unlink-only recovery from cold storage. SSD wear-leveling may
/// keep stale blocks; that's an FDE problem, not solvable at the app layer.
fn shred_and_remove(path: &Path) -> std::io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    let len = fs::metadata(path)?.len();
    {
        let mut f = fs::OpenOptions::new().write(true).open(path)?;
        // Two zero-passes + sync between to push past common page caches.
        let zeros = vec![0u8; 8192];
        for _pass in 0..2 {
            f.seek(SeekFrom::Start(0))?;
            let mut written: u64 = 0;
            while written < len {
                let n = std::cmp::min(zeros.len() as u64, len - written) as usize;
                f.write_all(&zeros[..n])?;
                written += n as u64;
            }
            f.flush()?;
            f.sync_all()?;
        }
    }
    fs::remove_file(path)?;
    Ok(())
}
