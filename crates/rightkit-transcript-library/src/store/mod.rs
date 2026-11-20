//! Disk-backed `delivery_history` store over a keyed `rusqlite` connection.
//!
//! The host opens the connection (credential lookup, key application, recovery of an
//! unreadable file) and hands it to [`DeliveryHistoryStore::open_connection`]; this module owns the
//! `delivery_history` schema, the write-maintained `app_name`/`outcome_kind` columns and their
//! backfill, filter and search SQL, scoped deletes, retention prune, the 200-row load and the
//! one-time legacy `history.json` import. Verified imports preserve the source
//! and report pending cleanup; they never overwrite or remove a path here.
//!
//! With the `managed-store` feature the host can instead hand over an open
//! `rightkit_db::encrypted_lifecycle::EncryptedStore` ([`DeliveryHistoryStore::open_managed`]).
//! The store then runs every query, write and migration on that store's managed writer, so key
//! rotation, restore and recovery of the same `EncryptedStore` drain and reopen the connection the
//! history uses, and the host keeps no second connection.

use self::legacy_json_migration::migrate_legacy_json_if_present;
use self::migrations::initialize_schema;
use self::queries::{row_to_record, sql_filter_values, sqlite_i64_ms, HISTORY_FILTER_SQL};
use crate::delivery::DeliveryRecord;
use crate::history::{
    history_target_label, record_matches, trim_records, HistoryError, HistoryFilter, HistoryPage,
    HistoryQuery, HistoryStore, MAX_HISTORY_ITEMS,
};
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::OptionalExtension;
use std::path::Path;

mod connection;
mod legacy_json_migration;
mod migrations;
mod queries;

use connection::HistoryConnection;
#[cfg(feature = "managed-store")]
pub use connection::ManagedConnection;

/// Host event sink: `(event name, detail)`. The store reports history events through it
/// (`history_find_failed`, `history_migration_row_mismatch`); the host decides where they go.
pub type EventSink = fn(&str, &str);

/// The history store. `C` is where its connection comes from: a raw `rusqlite::Connection` the host
/// owns (the default, [`DeliveryHistoryStore::open_connection`]) or, with `managed-store`, an
/// `EncryptedStore`'s managed writer ([`DeliveryHistoryStore::open_managed`]).
#[derive(Debug)]
pub struct DeliveryHistoryStore<C = rusqlite::Connection> {
    conn: C,
    records: Vec<DeliveryRecord>,
    events: EventSink,
}

fn outcome_kind_str(outcome: &crate::delivery::DeliveryOutcome) -> &'static str {
    match outcome {
        crate::delivery::DeliveryOutcome::Pasted => "pasted",
        crate::delivery::DeliveryOutcome::PasteSentUnconfirmed { .. } => "paste_sent_unconfirmed",
        crate::delivery::DeliveryOutcome::CopiedFallback { .. } => "copied_fallback",
        crate::delivery::DeliveryOutcome::Error { .. } => "error",
        crate::delivery::DeliveryOutcome::Cancelled => "cancelled",
        crate::delivery::DeliveryOutcome::Draft => "draft",
    }
}

impl<C: HistoryConnection> DeliveryHistoryStore<C> {
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

    /// Create or migrate the schema on `conn`'s connection, import a leftover `legacy_json_path`
    /// when the table is empty, and load the newest `MAX_HISTORY_ITEMS` rows.
    fn open_backend(
        mut conn: C,
        legacy_json_path: &Path,
        events: EventSink,
    ) -> Result<Self, HistoryError> {
        let records = conn.write(|conn| {
            initialize_schema(conn)?;
            migrate_legacy_json_if_present(conn, legacy_json_path, events)?;
            load_all(conn)
        })?;
        Ok(Self {
            conn,
            records,
            events,
        })
    }
}

impl DeliveryHistoryStore {
    /// Take a connection the host already opened and keyed, create or migrate the schema, import a
    /// leftover `legacy_json_path` when the table is empty, and load the newest
    /// `MAX_HISTORY_ITEMS` rows. The host keeps key custody, rotation and recovery.
    pub fn open_connection(
        conn: rusqlite::Connection,
        legacy_json_path: &Path,
        events: EventSink,
    ) -> Result<Self, HistoryError> {
        Self::open_backend(conn, legacy_json_path, events)
    }

    /// The live connection, for host-owned operations on it (key rotation).
    pub fn connection(&self) -> &rusqlite::Connection {
        &self.conn
    }
}

#[cfg(feature = "managed-store")]
impl DeliveryHistoryStore<ManagedConnection> {
    /// Take an open `EncryptedStore`, create or migrate the schema on its managed writer, import a
    /// leftover `legacy_json_path` when the table is empty, and load the newest
    /// `MAX_HISTORY_ITEMS` rows. Key selection, rotation, restore and recovery stay with the
    /// `EncryptedStore` (read what the open learned, such as `rebuilt()` or `degraded()`, before
    /// handing it over; [`Self::store`] gives it back by reference).
    ///
    /// The host selects SQLCipher through rightkit-db's provider feature, exactly as for
    /// `EncryptedStore` itself.
    pub fn open_managed(
        store: rightkit_db::encrypted_lifecycle::EncryptedStore,
        legacy_json_path: &Path,
        events: EventSink,
    ) -> Result<Self, HistoryError> {
        Self::open_backend(ManagedConnection(store), legacy_json_path, events)
    }

    /// The lifecycle owner behind the managed writer, for backup, restore, recovery and status.
    /// Do not hold a connection from it across a call into this store: the store's operations
    /// take the same writer.
    pub fn store(&self) -> &rightkit_db::encrypted_lifecycle::EncryptedStore {
        &self.conn.0
    }

    /// Rotate the key through the same `EncryptedStore` that serves every history operation: the
    /// store drains its writer, rekeys, and reopens it with the new key. While it runs, history
    /// operations return a busy error. The in-memory mirror is unaffected (a rotation changes the
    /// key, not the rows). The report is the owner's; read it as documented on
    /// `rightkit_db::encrypted_lifecycle::RotationReport`.
    pub fn rotate_key(
        &self,
        request: rightkit_db::encrypted_lifecycle::RotationRequest,
    ) -> rightkit_db::encrypted_lifecycle::RotationReport {
        self.conn.0.rotate_key(request)
    }
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

impl<C: HistoryConnection> HistoryStore for DeliveryHistoryStore<C> {
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
        let delivered_at_ms = sqlite_i64_ms(record.delivered_at_ms)?;
        self.conn.write(|conn| {
            conn.execute(
                "INSERT OR REPLACE INTO delivery_history
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
            .map(|_| ())
            .map_err(|e| HistoryError::new(format!("insert: {e}")))
        })?;
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
        let delivered_at_ms = sqlite_i64_ms(record.delivered_at_ms)?;
        self.conn.write(|conn| {
            let tx = rightkit_db::write_txn(conn)
                .map_err(|e| HistoryError::new(format!("begin finalize session: {e}")))?;
            tx.execute(
                "INSERT OR REPLACE INTO delivery_history
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
            .map_err(|e| HistoryError::new(format!("insert final record: {e}")))?;
            if draft_delivery_id != record.delivery_id {
                tx.execute(
                    "DELETE FROM delivery_history WHERE delivery_id = ?1",
                    rusqlite::params![draft_delivery_id],
                )
                .map_err(|e| HistoryError::new(format!("delete superseded draft: {e}")))?;
            }
            tx.commit()
                .map_err(|e| HistoryError::new(format!("commit finalize session: {e}")))
        })?;
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
        self.conn.write(|conn| {
            let tx = rightkit_db::write_txn(conn)
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
                .map_err(|e| HistoryError::new(format!("commit history batch: {e}")))
        })?;

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
            .read(|conn| {
                conn.query_row(
                    "SELECT record_json FROM delivery_history WHERE delivery_id = ?1",
                    rusqlite::params![delivery_id],
                    row_to_record,
                )
                .optional()
                .map_err(|e| HistoryError::new(e.to_string()))
            })
            .unwrap_or_else(|err| {
                (self.events)(
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
            let json = self.conn.read(|conn| {
                conn.query_row(
                    "SELECT record_json FROM delivery_history WHERE delivery_id = ?1",
                    rusqlite::params![delivery_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|e| HistoryError::new(format!("find for update: {e}")))
            })?;
            let Some(json) = json else {
                return Ok(false);
            };
            serde_json::from_str(&json)
                .map_err(|e| HistoryError::new(format!("parse record for update: {e}")))?
        };
        updated.transcript = transcript;
        let json = serde_json::to_string(&updated)
            .map_err(|e| HistoryError::new(format!("serialize record: {e}")))?;
        self.conn.write(|conn| {
            conn.execute(
                "UPDATE delivery_history SET record_json = ?2 WHERE delivery_id = ?1",
                rusqlite::params![delivery_id, json],
            )
            .map(|_| ())
            .map_err(|e| HistoryError::new(format!("update: {e}")))
        })?;
        if let Some(idx) = cached_idx {
            self.records[idx] = updated;
        }
        Ok(true)
    }

    fn delete(&mut self, delivery_id: &str) -> Result<bool, HistoryError> {
        let changed = self.conn.write(|conn| {
            conn.execute(
                "DELETE FROM delivery_history WHERE delivery_id = ?1",
                rusqlite::params![delivery_id],
            )
            .map_err(|e| HistoryError::new(format!("delete: {e}")))
        })?;
        let before = self.records.len();
        self.records.retain(|r| r.delivery_id != delivery_id);
        Ok(changed > 0 || self.records.len() != before)
    }

    fn query(&self, query: &HistoryQuery) -> Result<HistoryPage, HistoryError> {
        let (search, search_pattern, from_ms, until_ms, app, cancelled_only) =
            sql_filter_values(&query.filter)?;
        let limit = query.bounded_limit() as i64;
        let offset = i64::from(query.offset);
        let (items, total) = self.conn.read(|conn| {
            let total: i64 = conn
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

            let mut stmt = conn
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
            Ok((items, total))
        })?;
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
        let deleted = self.conn.write(|conn| {
            let tx = rightkit_db::write_txn(conn)
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
            Ok(deleted)
        })?;
        self.records
            .retain(|record| !record_matches(record, filter));
        Ok(deleted)
    }

    fn prune_before(&mut self, cutoff_ms: u64) -> Result<usize, HistoryError> {
        let cutoff = sqlite_i64_ms(cutoff_ms)?;
        let deleted = self.conn.write(|conn| {
            let tx = rightkit_db::write_txn(conn)
                .map_err(|e| HistoryError::new(format!("begin history prune: {e}")))?;
            let deleted = tx
                .execute(
                    "DELETE FROM delivery_history WHERE delivered_at_ms < ?1",
                    rusqlite::params![cutoff],
                )
                .map_err(|e| HistoryError::new(format!("history prune: {e}")))?;
            tx.commit()
                .map_err(|e| HistoryError::new(format!("commit history prune: {e}")))?;
            Ok(deleted)
        })?;
        self.records
            .retain(|record| record.delivered_at_ms >= cutoff_ms);
        Ok(deleted)
    }

    fn clear(&mut self) -> Result<(), HistoryError> {
        // The delete and the checkpoint run on one borrow of the connection. A failed delete
        // returns before the mirror is touched; the checkpoint's own result is judged below.
        let checkpoint = self.conn.write(|conn| {
            conn.execute("DELETE FROM delivery_history", [])
                .map_err(|e| HistoryError::new(format!("clear: {e}")))?;
            Ok(rightkit_db::checkpoint_truncate(conn).map_err(|e| e.to_string()))
        })?;
        // Clearing everything must not leave the old rows in the WAL file. The owner reports what
        // the TRUNCATE checkpoint achieved; a blocked or partial one has NOT removed them.
        clear_mirror_then_checkpoint(&mut self.records, move || checkpoint)
    }
}

fn clear_mirror_then_checkpoint<E: std::fmt::Display>(
    records: &mut Vec<DeliveryRecord>,
    checkpoint: impl FnOnce() -> Result<rightkit_db::CheckpointStatus, E>,
) -> Result<(), HistoryError> {
    records.clear();
    let status = checkpoint().map_err(|e| HistoryError::new(format!("clear checkpoint: {e}")))?;
    if !checkpoint_is_complete(&status) {
        return Err(HistoryError::new(format!(
            "clear checkpoint incomplete: busy={} log_frames={} checkpointed_frames={}",
            status.busy, status.log_frames, status.checkpointed_frames
        )));
    }
    Ok(())
}

/// A TRUNCATE checkpoint removed the cleared rows from the WAL only when no reader blocked it
/// (`busy == 0`) and every logged frame was checkpointed.
fn checkpoint_is_complete(status: &rightkit_db::CheckpointStatus) -> bool {
    status.busy == 0 && status.checkpointed_frames >= status.log_frames
}

#[cfg(test)]
mod tests {
    use super::clear_mirror_then_checkpoint;
    use crate::delivery::DeliveryRecord;

    #[test]
    fn clear_drops_mirror_before_a_failed_checkpoint() {
        let mut records = vec![DeliveryRecord::new_draft("session", "unfinished")];

        let result = clear_mirror_then_checkpoint(&mut records, || Err("checkpoint blocked"));

        assert!(result.is_err());
        assert!(records.is_empty());
    }
}
