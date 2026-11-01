//! SQLCipher-backed history store.
//!
//! Whole-DB encryption via SQLCipher, key in Windows Credential Manager. First
//! open generates a key; subsequent opens read it. If a history.json from the
//! previous JSON-based store exists, its records are migrated into SQLCipher and
//! the JSON file is shredded (or kept as .bak if the migration gate fails).

use super::legacy_json_migration::migrate_legacy_json_if_present;
use super::migrations::initialize_schema;
use super::queries::{row_to_record, sql_filter_values, sqlite_i64_ms, HISTORY_FILTER_SQL};
use super::retention;
use super::sqlcipher_conn::open_and_key;
use crate::delivery::DeliveryRecord;
use crate::history::{
    history_target_label, record_matches, trim_records, HistoryError, HistoryFilter, HistoryPage,
    HistoryQuery, HistoryStore, MAX_HISTORY_ITEMS,
};
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::OptionalExtension;
use std::fs;
use std::path::PathBuf;

pub use super::sqlcipher_conn::{
    open_connection_keyed_for_test, validate_sqlcipher_key_for_pragma,
};

#[derive(Debug)]
pub struct SQLCipherHistoryStore {
    pub(super) conn: rusqlite::Connection,
    records: Vec<DeliveryRecord>,
}

/// Canonical DB string for a `DeliveryOutcome`, matching the exact string
/// `#[serde(tag = "kind", rename_all = "snake_case")]` puts at
/// `$.outcome.kind` in `record_json` — so the write-time `outcome_kind`
/// column and the old per-query `json_extract(record_json, '$.outcome.kind')`
/// it replaces agree bit-for-bit (see `migrate_search_columns`, which backfills
/// existing rows with the `json_extract` form for exactly this reason).
#[doc(hidden)]
pub fn outcome_kind_str_for_test(outcome: &crate::delivery::DeliveryOutcome) -> &'static str {
    outcome_kind_str(outcome)
}

pub(super) fn outcome_kind_str(outcome: &crate::delivery::DeliveryOutcome) -> &'static str {
    match outcome {
        crate::delivery::DeliveryOutcome::Pasted => "pasted",
        crate::delivery::DeliveryOutcome::PasteSentUnconfirmed { .. } => "paste_sent_unconfirmed",
        crate::delivery::DeliveryOutcome::CopiedFallback { .. } => "copied_fallback",
        crate::delivery::DeliveryOutcome::Error { .. } => "error",
        crate::delivery::DeliveryOutcome::Cancelled => "cancelled",
        crate::delivery::DeliveryOutcome::Draft => "draft",
    }
}

impl SQLCipherHistoryStore {
    /// In-memory mirror of `load_all`'s ORDER BY delivered_at_ms DESC,
    /// delivery_id DESC LIMIT
    /// MAX_HISTORY_ITEMS, without a full table reload. `delivered_at_ms` is
    /// set to "now" at record creation (see DeliveryRecord::new), so a fresh
    /// push is newest — find its sorted-descending insertion point rather
    /// than assuming index 0 (a delivery_id collision replaces in place, and
    /// clock skew is possible), then re-apply the same cap.
    fn mirror_upsert(&mut self, record: DeliveryRecord) {
        self.records.retain(|r| r.delivery_id != record.delivery_id);
        let pos = self
            .records
            .iter()
            .position(|r| {
                r.delivered_at_ms < record.delivered_at_ms
                    || (r.delivered_at_ms == record.delivered_at_ms
                        && r.delivery_id.as_str() <= record.delivery_id.as_str())
            })
            .unwrap_or(self.records.len());
        self.records.insert(pos, record);
        trim_records(&mut self.records);
    }

    pub fn open_default() -> Result<Self, HistoryError> {
        let base = crate::host::host().data_root();
        fs::create_dir_all(&base)
            .map_err(|err| HistoryError::new(format!("create data dir failed: {err}")))?;
        Self::open_at(
            base.join(crate::host::host().sqlite_file_name()),
            base.join("history.json"),
        )
    }

    pub fn open_at(db_path: PathBuf, legacy_json_path: PathBuf) -> Result<Self, HistoryError> {
        let mut conn = open_and_key(&db_path)?;
        initialize_schema(&mut conn)?;

        migrate_legacy_json_if_present(&mut conn, &legacy_json_path)?;

        let records = Self::load_all(&conn)?;
        Ok(Self { conn, records })
    }

    fn load_all(conn: &rusqlite::Connection) -> Result<Vec<DeliveryRecord>, HistoryError> {
        let mut stmt = conn
            .prepare(
                "SELECT record_json
                 FROM delivery_history
                 ORDER BY delivered_at_ms DESC, delivery_id DESC
                 LIMIT ?1",
            )
            .map_err(|e| HistoryError::new(format!("prepare load: {e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![MAX_HISTORY_ITEMS as i64], |row| {
                let json: String = row.get(0)?;
                let rec: DeliveryRecord = serde_json::from_str(&json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                Ok(rec)
            })
            .map_err(|e| HistoryError::new(format!("query_map: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| HistoryError::new(format!("collect rows: {e}")))?;
        Ok(rows)
    }
}

impl HistoryStore for SQLCipherHistoryStore {
    fn rotate_encryption_key(&mut self) -> Result<(), HistoryError> {
        self.rotate_key()
    }

    fn push(&mut self, record: DeliveryRecord) -> Result<(), HistoryError> {
        let json = serde_json::to_string(&record)
            .map_err(|e| HistoryError::new(format!("serialize record: {e}")))?;
        // `app_name`/`outcome_kind` are write-maintained: without them,
        // query()'s WHERE clause would run `json_extract` + a CASE expression
        // against every row on every query — including the default load,
        // since the outcome CASE always runs — with no usable index.
        // Computing them once here, from the
        // already-in-hand Rust `record` (same logic `history_target_label`
        // and outcome tagging already use elsewhere), lets query() filter on
        // plain indexed columns instead.
        let app_name = history_target_label(&record).to_ascii_lowercase();
        let outcome_kind = outcome_kind_str(&record.outcome);
        self.conn
            .execute(
                "INSERT OR REPLACE INTO delivery_history
                 (delivery_id, delivered_at_ms, record_json, app_name, outcome_kind)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    record.delivery_id,
                    sqlite_i64_ms(record.delivered_at_ms)?,
                    json,
                    app_name,
                    outcome_kind
                ],
            )
            .map_err(|e| HistoryError::new(format!("insert: {e}")))?;
        self.mirror_upsert(record);
        Ok(())
    }

    fn finalize_session(
        &mut self,
        draft_delivery_id: &str,
        record: DeliveryRecord,
    ) -> Result<(), HistoryError> {
        let json = serde_json::to_string(&record)
            .map_err(|e| HistoryError::new(format!("serialize record: {e}")))?;
        let app_name = history_target_label(&record).to_ascii_lowercase();
        let outcome_kind = outcome_kind_str(&record.outcome);
        let tx = rightkit_db::write_txn(&mut self.conn)
            .map_err(|e| HistoryError::new(format!("begin finalize session: {e}")))?;
        tx.execute(
            "INSERT OR REPLACE INTO delivery_history
             (delivery_id, delivered_at_ms, record_json, app_name, outcome_kind)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                record.delivery_id,
                sqlite_i64_ms(record.delivered_at_ms)?,
                json,
                app_name,
                outcome_kind
            ],
        )
        .map_err(|e| HistoryError::new(format!("insert final record: {e}")))?;
        if draft_delivery_id != record.delivery_id {
            tx.execute(
                "DELETE FROM delivery_history WHERE delivery_id = ?1",
                rusqlite::params![draft_delivery_id],
            )
            .map_err(|e| HistoryError::new(format!("delete superseded draft: {e}")))?;
        }
        tx.commit()
            .map_err(|e| HistoryError::new(format!("commit finalize session: {e}")))?;
        if draft_delivery_id != record.delivery_id {
            self.records.retain(|r| r.delivery_id != draft_delivery_id);
        }
        self.mirror_upsert(record);
        Ok(())
    }

    fn push_batch(&mut self, records: Vec<DeliveryRecord>) -> Result<(), HistoryError> {
        if records.is_empty() {
            return Ok(());
        }
        let rows = records
            .iter()
            .map(|record| {
                Ok((
                    record.delivery_id.clone(),
                    sqlite_i64_ms(record.delivered_at_ms)?,
                    serde_json::to_string(record)
                        .map_err(|e| HistoryError::new(format!("serialize record: {e}")))?,
                    history_target_label(record).to_ascii_lowercase(),
                    outcome_kind_str(&record.outcome),
                ))
            })
            .collect::<Result<Vec<_>, HistoryError>>()?;
        let tx = rightkit_db::write_txn(&mut self.conn)
            .map_err(|e| HistoryError::new(format!("begin history batch: {e}")))?;
        {
            // One statement prepared per batch: `rightkit_db::rusqlite` is built
            // without the statement-cache feature, so no `prepare_cached`.
            let mut statement = tx
                .prepare(
                    "INSERT OR REPLACE INTO delivery_history
                     (delivery_id, delivered_at_ms, record_json, app_name, outcome_kind)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(|e| HistoryError::new(format!("prepare history batch: {e}")))?;
            for (delivery_id, delivered_at_ms, json, app_name, outcome_kind) in &rows {
                statement
                    .execute(rusqlite::params![
                        delivery_id,
                        delivered_at_ms,
                        json,
                        app_name,
                        outcome_kind
                    ])
                    .map_err(|e| HistoryError::new(format!("insert history batch: {e}")))?;
            }
        }
        tx.commit()
            .map_err(|e| HistoryError::new(format!("commit history batch: {e}")))?;

        for record in records {
            self.records
                .retain(|item| item.delivery_id != record.delivery_id);
            let position = self
                .records
                .iter()
                .position(|item| {
                    item.delivered_at_ms < record.delivered_at_ms
                        || (item.delivered_at_ms == record.delivered_at_ms
                            && item.delivery_id.as_str() <= record.delivery_id.as_str())
                })
                .unwrap_or(self.records.len());
            self.records.insert(position, record);
        }
        trim_records(&mut self.records);
        Ok(())
    }

    fn recent(&self) -> &[DeliveryRecord] {
        &self.records
    }

    fn find(&self, delivery_id: &str) -> Option<DeliveryRecord> {
        if let Some(record) = self.records.iter().find(|r| r.delivery_id == delivery_id) {
            return Some(record.clone());
        }
        // `records` holds only the newest MAX_HISTORY_ITEMS. The scoped
        // delete/update defaults gate on `find`, so a cache-only lookup made
        // every older (paged-in) row impossible to delete or edit.
        self.conn
            .query_row(
                "SELECT record_json FROM delivery_history WHERE delivery_id = ?1",
                rusqlite::params![delivery_id],
                row_to_record,
            )
            .optional()
            .unwrap_or_else(|err| {
                crate::host::host().log(
                    "history_find_failed",
                    &format!("delivery_id={delivery_id} error={err}"),
                );
                None
            })
    }

    fn upsert_draft(&mut self, session_id: &str, text: String) -> Result<(), HistoryError> {
        // `push()` already performs INSERT OR REPLACE keyed by delivery_id and
        // mirrors the same dedup-then-insert into the in-memory cache, and
        // `draft_delivery_id` is stable per session — reusing it here gives
        // the required "one row per session_id" semantics for free.
        self.push(DeliveryRecord::new_draft(session_id, text))
    }

    fn update_transcript(
        &mut self,
        delivery_id: &str,
        transcript: String,
    ) -> Result<bool, HistoryError> {
        let cached_idx = self
            .records
            .iter()
            .position(|r| r.delivery_id == delivery_id);
        let mut updated = if let Some(idx) = cached_idx {
            self.records[idx].clone()
        } else {
            let json = self
                .conn
                .query_row(
                    "SELECT record_json FROM delivery_history WHERE delivery_id = ?1",
                    rusqlite::params![delivery_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|e| HistoryError::new(format!("find for update: {e}")))?;
            let Some(json) = json else {
                return Ok(false);
            };
            serde_json::from_str(&json)
                .map_err(|e| HistoryError::new(format!("parse record for update: {e}")))?
        };
        updated.transcript = transcript;
        let json = serde_json::to_string(&updated)
            .map_err(|e| HistoryError::new(format!("serialize record: {e}")))?;
        self.conn
            .execute(
                "UPDATE delivery_history SET record_json = ?2 WHERE delivery_id = ?1",
                rusqlite::params![delivery_id, json],
            )
            .map_err(|e| HistoryError::new(format!("update: {e}")))?;
        if let Some(idx) = cached_idx {
            self.records[idx] = updated;
        }
        Ok(true)
    }

    fn delete(&mut self, delivery_id: &str) -> Result<bool, HistoryError> {
        let changed = self
            .conn
            .execute(
                "DELETE FROM delivery_history WHERE delivery_id = ?1",
                rusqlite::params![delivery_id],
            )
            .map_err(|e| HistoryError::new(format!("delete: {e}")))?;
        let before = self.records.len();
        self.records.retain(|r| r.delivery_id != delivery_id);
        Ok(changed > 0 || self.records.len() != before)
    }

    fn query(&self, query: &HistoryQuery) -> Result<HistoryPage, HistoryError> {
        let (search, search_pattern, from_ms, until_ms, app, cancelled_only) =
            sql_filter_values(&query.filter)?;
        let total: i64 = self
            .conn
            .query_row(
                &format!("SELECT COUNT(*) FROM delivery_history {HISTORY_FILTER_SQL}"),
                rusqlite::params![
                    search,
                    search_pattern,
                    from_ms,
                    until_ms,
                    app,
                    cancelled_only
                ],
                |row| row.get(0),
            )
            .map_err(|e| HistoryError::new(format!("count history query: {e}")))?;

        let limit = query.bounded_limit() as i64;
        let offset = i64::from(query.offset);
        let mut stmt = self
            .conn
            .prepare(&format!(
                "SELECT record_json FROM delivery_history {HISTORY_FILTER_SQL} \
                 ORDER BY delivered_at_ms DESC, delivery_id DESC LIMIT ?7 OFFSET ?8"
            ))
            .map_err(|e| HistoryError::new(format!("prepare history query: {e}")))?;
        let items = stmt
            .query_map(
                rusqlite::params![
                    search,
                    search_pattern,
                    from_ms,
                    until_ms,
                    app,
                    cancelled_only,
                    limit,
                    offset
                ],
                row_to_record,
            )
            .map_err(|e| HistoryError::new(format!("run history query: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| HistoryError::new(format!("collect history query: {e}")))?;
        let total = usize::try_from(total)
            .map_err(|_| HistoryError::new("history count exceeded usize"))?;
        Ok(HistoryPage {
            has_more: (query.offset as usize).saturating_add(items.len()) < total,
            items,
            total,
        })
    }

    fn delete_matching(&mut self, filter: &HistoryFilter) -> Result<usize, HistoryError> {
        let (search, search_pattern, from_ms, until_ms, app, cancelled_only) =
            sql_filter_values(filter)?;
        let tx = rightkit_db::write_txn(&mut self.conn)
            .map_err(|e| HistoryError::new(format!("begin filtered delete: {e}")))?;
        let deleted = tx
            .execute(
                &format!("DELETE FROM delivery_history {HISTORY_FILTER_SQL}"),
                rusqlite::params![
                    search,
                    search_pattern,
                    from_ms,
                    until_ms,
                    app,
                    cancelled_only
                ],
            )
            .map_err(|e| HistoryError::new(format!("filtered delete: {e}")))?;
        tx.commit()
            .map_err(|e| HistoryError::new(format!("commit filtered delete: {e}")))?;
        self.records
            .retain(|record| !record_matches(record, filter));
        Ok(deleted)
    }

    fn prune_before(&mut self, cutoff_ms: u64) -> Result<usize, HistoryError> {
        let cutoff = sqlite_i64_ms(cutoff_ms)?;
        let tx = rightkit_db::write_txn(&mut self.conn)
            .map_err(|e| HistoryError::new(format!("begin history prune: {e}")))?;
        let deleted = tx
            .execute(
                "DELETE FROM delivery_history WHERE delivered_at_ms < ?1",
                rusqlite::params![cutoff],
            )
            .map_err(|e| HistoryError::new(format!("history prune: {e}")))?;
        tx.commit()
            .map_err(|e| HistoryError::new(format!("commit history prune: {e}")))?;
        retention::prune_records_before(&mut self.records, cutoff_ms);
        Ok(deleted)
    }

    fn clear(&mut self) -> Result<(), HistoryError> {
        self.conn
            .execute("DELETE FROM delivery_history", [])
            .map_err(|e| HistoryError::new(format!("clear: {e}")))?;
        // Clearing everything must not leave the old rows in the WAL file. The owner reports what
        // the TRUNCATE checkpoint achieved; a blocked or partial one has NOT removed them.
        let status = rightkit_db::checkpoint_truncate(&self.conn)
            .map_err(|e| HistoryError::new(format!("clear checkpoint: {e}")))?;
        if !checkpoint_is_complete(&status) {
            return Err(HistoryError::new(format!(
                "clear checkpoint incomplete: busy={} log_frames={} checkpointed_frames={}",
                status.busy, status.log_frames, status.checkpointed_frames
            )));
        }
        self.records.clear();
        Ok(())
    }
}

/// A TRUNCATE checkpoint removed the cleared rows from the WAL only when no reader blocked it
/// (`busy == 0`) and every logged frame was checkpointed.
pub(crate) fn checkpoint_is_complete(status: &rightkit_db::CheckpointStatus) -> bool {
    status.busy == 0 && status.checkpointed_frames >= status.log_frames
}

#[cfg(test)]
mod checkpoint_tests {
    use super::checkpoint_is_complete;
    use rightkit_db::CheckpointStatus;

    fn status(busy: i64, log_frames: i64, checkpointed_frames: i64) -> CheckpointStatus {
        CheckpointStatus {
            busy,
            log_frames,
            checkpointed_frames,
        }
    }

    #[test]
    fn a_blocked_or_partial_truncate_checkpoint_is_incomplete() {
        assert!(!checkpoint_is_complete(&status(1, 0, 0)));
        assert!(!checkpoint_is_complete(&status(0, 5, 3)));
        assert!(checkpoint_is_complete(&status(0, 0, 0)));
        assert!(checkpoint_is_complete(&status(0, 4, 4)));
    }
}
