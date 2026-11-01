use crate::{Result, Storage, StorageError};
use rightkit_db::rusqlite;

pub const MAX_COMPACTION_ARCHIVE_BYTES: usize = 96 * 1024 * 1024;
pub const MAX_COMPACTION_ARCHIVE_SESSION_BYTES: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionArchiveRow {
    pub id: String,
    pub session_id: String,
    pub created_at: String,
    pub sha256: String,
    pub byte_length: usize,
    pub message_count: usize,
    pub transcript_json: Option<Vec<u8>>,
}

impl CompactionArchiveRow {
    /// Verify metadata against the exact bytes returned by a recovery read.
    /// Metadata-only rows from `list_compaction_archives` intentionally return
    /// `false` because they do not carry payload bytes to authenticate.
    pub fn verify_integrity(&self) -> bool {
        let Some(bytes) = self.transcript_json.as_deref() else {
            return false;
        };
        bytes.len() == self.byte_length && digest_hex(bytes) == self.sha256
    }
}

impl Storage {
    /// Decision-bearing variant. `decision_key` is the canonical producer
    /// identity plus restart epoch and is consumed in the same transaction as
    /// the archive. A duplicate is refused; validator failure rolls the
    /// consumption back with every other staged write.
    pub fn commit_compaction_with_decision<F>(
        &mut self,
        archive_id: &str,
        session_id: &str,
        created_at: &str,
        transcript_json: &[u8],
        message_count: usize,
        projection_event: Option<(&str, &str)>,
        lineage_event: Option<(&str, &str)>,
        decision_key: (&str, u64),
        validator: F,
    ) -> Result<(CompactionArchiveRow, i64)>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<()>,
    {
        self.commit_compaction_inner(
            archive_id,
            session_id,
            created_at,
            transcript_json,
            message_count,
            projection_event,
            lineage_event,
            Some(decision_key),
            validator,
        )
    }

    /// Variant used by live compaction sinks. The validator is evaluated
    /// against the same single-writer state immediately before entering the
    /// commit boundary; a stale decision rolls back every staged write.
    pub fn commit_compaction_with_validation<F>(
        &mut self,
        archive_id: &str,
        session_id: &str,
        created_at: &str,
        transcript_json: &[u8],
        message_count: usize,
        projection_event: Option<(&str, &str)>,
        lineage_event: Option<(&str, &str)>,
        validator: F,
    ) -> Result<(CompactionArchiveRow, i64)>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<()>,
    {
        self.commit_compaction_inner(
            archive_id,
            session_id,
            created_at,
            transcript_json,
            message_count,
            projection_event,
            lineage_event,
            None,
            validator,
        )
    }

    pub fn commit_compaction(
        &mut self,
        archive_id: &str,
        session_id: &str,
        created_at: &str,
        transcript_json: &[u8],
        message_count: usize,
        projection_event: Option<(&str, &str)>,
        lineage_event: Option<(&str, &str)>,
    ) -> Result<(CompactionArchiveRow, i64)> {
        self.commit_compaction_inner(
            archive_id,
            session_id,
            created_at,
            transcript_json,
            message_count,
            projection_event,
            lineage_event,
            None,
            |_| Ok(()),
        )
    }

    /// Persist the exact pre-compaction transcript before the engine replaces
    /// any messages. Capacity exhaustion is an error so compaction fails closed.
    pub fn insert_compaction_archive(
        &self,
        id: &str,
        session_id: &str,
        created_at: &str,
        transcript_json: &[u8],
        message_count: usize,
    ) -> Result<CompactionArchiveRow> {
        if id.trim().is_empty() || session_id.trim().is_empty() || created_at.trim().is_empty() {
            return Err(StorageError::Serde(
                "compaction archive identity and timestamp must be non-empty".into(),
            ));
        }
        if transcript_json.is_empty() {
            return Err(StorageError::Serde(
                "compaction archive transcript must be non-empty".into(),
            ));
        }
        if transcript_json.len() > MAX_COMPACTION_ARCHIVE_BYTES {
            return Err(StorageError::Serde(format!(
                "compaction archive exceeds {} bytes",
                MAX_COMPACTION_ARCHIVE_BYTES
            )));
        }

        // Keep the capacity check and insert in one IMMEDIATE transaction. A
        // separate SUM followed by INSERT can race with another writer and
        // exceed the per-session retention budget while both calls still
        // report success.
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        let used: i64 = tx.query_row(
            "SELECT COALESCE(SUM(byte_length), 0) FROM compaction_archives WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )?;
        if used < 0
            || usize::try_from(used)
                .unwrap_or(usize::MAX)
                .saturating_add(transcript_json.len())
                > MAX_COMPACTION_ARCHIVE_SESSION_BYTES
        {
            drop(tx);
            return Err(StorageError::Serde(format!(
                "session compaction archives exceed {} bytes",
                MAX_COMPACTION_ARCHIVE_SESSION_BYTES
            )));
        }

        let sha256 = digest_hex(transcript_json);
        tx.execute(
            "INSERT INTO compaction_archives
             (id, session_id, created_at, sha256, byte_length, message_count, transcript_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                id,
                session_id,
                created_at,
                sha256,
                transcript_json.len() as i64,
                message_count as i64,
                transcript_json,
            ],
        )?;
        tx.commit()?;
        Ok(CompactionArchiveRow {
            id: id.to_string(),
            session_id: session_id.to_string(),
            created_at: created_at.to_string(),
            sha256,
            byte_length: transcript_json.len(),
            message_count,
            transcript_json: None,
        })
    }

    pub fn list_compaction_archives(&self, session_id: &str) -> Result<Vec<CompactionArchiveRow>> {
        let mut statement = self.conn.prepare(
            "SELECT id, session_id, created_at, sha256, byte_length, message_count
             FROM compaction_archives WHERE session_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = statement.query_map([session_id], archive_metadata_row)?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(StorageError::from)
    }

    pub fn get_compaction_archive(
        &self,
        session_id: &str,
        archive_id: &str,
    ) -> Result<CompactionArchiveRow> {
        let row = self
            .conn
            .query_row(
                "SELECT id, session_id, created_at, sha256, byte_length, message_count, transcript_json
                 FROM compaction_archives WHERE session_id = ?1 AND id = ?2",
                rusqlite::params![session_id, archive_id],
                |row| {
                    Ok(CompactionArchiveRow {
                        id: row.get(0)?,
                        session_id: row.get(1)?,
                        created_at: row.get(2)?,
                        sha256: row.get(3)?,
                        byte_length: usize::try_from(row.get::<_, i64>(4)?).unwrap_or_default(),
                        message_count: usize::try_from(row.get::<_, i64>(5)?).unwrap_or_default(),
                        transcript_json: Some(row.get(6)?),
                    })
                },
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => {
                    StorageError::NotFound(format!("compaction archive {archive_id}"))
                }
                other => StorageError::Sqlite(other),
            })?;
        if !row.verify_integrity() {
            return Err(StorageError::Serde(format!(
                "compaction archive {} failed integrity verification",
                row.id
            )));
        }
        Ok(row)
    }

    /// CRA-11 — atomic durable compaction commit.
    ///
    /// Commits the archive, an optional pre-encoded projection event, an
    /// optional pre-encoded lineage event, the session's durable
    /// `compaction_epoch` advance, and a `sessionCompacted` event inside ONE
    /// `IMMEDIATE` SQLite transaction. Any failure at any stage (capacity,
    /// encode, sqlite) rolls back everything staged so far: this is the
    /// direct replacement for the historical sequence of independently
    /// auto-committing calls (`insert_compaction_archive` alone, then a
    /// separate best-effort `append_event` for the projection, another for
    /// lineage) that could leave a durable archive on disk with no epoch
    /// advance and no `sessionCompacted` record, or an epoch advanced with a
    /// lost projection, if the process died or a later stage errored between
    /// those independent commits.
    ///
    /// `insert_compaction_archive` and `append_event`/`append_event_at_seq`
    /// MUST NOT be called from inside this function or from a caller already
    /// holding this transaction: each of those opens (and commits) its own
    /// transaction, and nesting a second `IMMEDIATE` transaction on the same
    /// `rusqlite::Connection` either fragments this commit's atomicity or
    /// errors outright. All row inserts here go through transaction-local raw
    /// SQL instead.
    ///
    /// `projection_event` / `lineage_event` are `(kind, payload_json)` pairs
    /// the caller has already encoded — including, for a projection, having
    /// already re-hashed it over canonical event rows (see
    /// `api_compaction_archives.rs::rehash_over_canonical_rows`), since that
    /// rehash needs `events_in_range` plus transcript rehydration logic that
    /// lives above this crate. The caller must perform that read (and any
    /// caller-side encode failure that turns a projection into `None`)
    /// while holding the same exclusive lock/connection it will call this
    /// function under, so no other writer can interleave between the read
    /// and this commit — the same single-writer-behind-one-mutex model
    /// `append_event`'s doc comment already relies on. `None` for either
    /// means "no such record for this commit"; the archive, epoch, and
    /// `sessionCompacted` event still commit.
    ///
    /// Returns the archive's metadata row (no `transcript_json` payload,
    /// matching `insert_compaction_archive`'s convention) and the new,
    /// strictly-increased `compaction_epoch`.
    fn commit_compaction_inner<F>(
        &mut self,
        archive_id: &str,
        session_id: &str,
        created_at: &str,
        transcript_json: &[u8],
        message_count: usize,
        projection_event: Option<(&str, &str)>,
        lineage_event: Option<(&str, &str)>,
        decision_key: Option<(&str, u64)>,
        validator: F,
    ) -> Result<(CompactionArchiveRow, i64)>
    where
        F: FnOnce(&rusqlite::Transaction<'_>) -> Result<()>,
    {
        if archive_id.trim().is_empty()
            || session_id.trim().is_empty()
            || created_at.trim().is_empty()
        {
            return Err(StorageError::Serde(
                "compaction commit identity and timestamp must be non-empty".into(),
            ));
        }
        if transcript_json.is_empty() {
            return Err(StorageError::Serde(
                "compaction commit transcript must be non-empty".into(),
            ));
        }
        if transcript_json.len() > MAX_COMPACTION_ARCHIVE_BYTES {
            return Err(StorageError::Serde(format!(
                "compaction archive exceeds {} bytes",
                MAX_COMPACTION_ARCHIVE_BYTES
            )));
        }

        // Same cached-high-water-mark strategy as `append_event`: this
        // connection, behind the daemon mutex, is the only writer, so a
        // cached value avoids a redundant MAX(seq) probe inside the
        // transaction below.
        let cached_seq = self.seq_cache.get(session_id).copied();

        let tx = rightkit_db::write_txn(&mut self.conn)?;

        // A decision-bearing compaction consumes its canonical identity only
        // inside this transaction. SQLite's primary key supplies the durable
        // duplicate fence; any later stale/error path drops `tx`, so the
        // consumption is rolled back together with the archive and events.
        if let Some((identity, restart_epoch)) = decision_key {
            if identity.trim().is_empty() {
                return Err(StorageError::Serde(
                    "compaction decision identity must be non-empty".into(),
                ));
            }
            tx.execute(
                "INSERT INTO compaction_decision_consumptions
                 (decision_identity, restart_epoch, archive_id, consumed_at)
                 VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![
                    identity,
                    i64::try_from(restart_epoch).map_err(|_| StorageError::Serde(
                        "compaction restart epoch exceeds SQLite integer range".into()
                    ))?,
                    archive_id,
                    created_at
                ],
            )
            .map_err(|error| match error {
                rusqlite::Error::SqliteFailure(ref code, _)
                    if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                        || code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE =>
                {
                    StorageError::Serde(
                        "compaction decision refused: canonical identity already consumed".into(),
                    )
                }
                other => StorageError::Sqlite(other),
            })?;
        }

        // -- capacity check: same IMMEDIATE-transaction race guard as
        // insert_compaction_archive, so a concurrent commit_compaction /
        // insert_compaction_archive pair cannot both observe headroom and
        // together exceed the per-session retention budget. --
        let used: i64 = tx.query_row(
            "SELECT COALESCE(SUM(byte_length), 0) FROM compaction_archives WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )?;
        if used < 0
            || usize::try_from(used)
                .unwrap_or(usize::MAX)
                .saturating_add(transcript_json.len())
                > MAX_COMPACTION_ARCHIVE_SESSION_BYTES
        {
            // `tx` drops here without `commit()` — implicit rollback. Neither
            // the archive, projection/lineage events, epoch, nor
            // sessionCompacted event are written.
            return Err(StorageError::Serde(format!(
                "session compaction archives exceed {} bytes",
                MAX_COMPACTION_ARCHIVE_SESSION_BYTES
            )));
        }

        let sha256 = digest_hex(transcript_json);
        tx.execute(
            "INSERT INTO compaction_archives
             (id, session_id, created_at, sha256, byte_length, message_count, transcript_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                archive_id,
                session_id,
                created_at,
                sha256,
                transcript_json.len() as i64,
                message_count as i64,
                transcript_json,
            ],
        )?;

        // Contiguous seq allocation for this transaction's event rows. Mirrors
        // append_event's cached-high-water/MAX(seq)-fallback strategy exactly,
        // so the unified seq contract (Conduit::publish / append_event /
        // append_event_at_seq / commit_compaction all sharing one monotonic
        // seq space per session) holds for compaction's events too.
        let mut next_seq: i64 = match cached_seq {
            Some(last) => last + 1,
            None => {
                let current: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?1",
                    [session_id],
                    |row| row.get(0),
                )?;
                current + 1
            }
        };

        if let Some((kind, payload_json)) = projection_event {
            insert_event_row(&tx, session_id, next_seq, kind, payload_json, created_at)?;
            next_seq += 1;
        }
        if let Some((kind, payload_json)) = lineage_event {
            insert_event_row(&tx, session_id, next_seq, kind, payload_json, created_at)?;
            next_seq += 1;
        }

        // CRA-11: durable epoch, sole source of truth, bumped only on this
        // committed boundary — never derived from live Compact markers or
        // MAX(seq) over prunable events. A missing session row is a hard
        // error rather than a silent 0-row UPDATE: there is nothing to
        // atomically commit a compaction against.
        let current_epoch: i64 = tx
            .query_row(
                "SELECT COALESCE(compaction_epoch, 0) FROM sessions WHERE id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .map_err(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => {
                    StorageError::NotFound(format!("session {session_id}"))
                }
                other => StorageError::Sqlite(other),
            })?;
        let new_epoch = current_epoch + 1;
        tx.execute(
            "UPDATE sessions SET compaction_epoch = ?2 WHERE id = ?1",
            rusqlite::params![session_id, new_epoch],
        )?;

        let event_seq = next_seq;
        let session_compacted_payload = serde_json::to_string(&SessionCompactedPayload {
            session_id: session_id.to_string(),
            epoch: new_epoch,
            archive_id: archive_id.to_string(),
            created_at: created_at.to_string(),
        })
        .map_err(|error| StorageError::Serde(error.to_string()))?;
        insert_event_row(
            &tx,
            session_id,
            event_seq,
            SESSION_COMPACTED_EVENT_KIND,
            &session_compacted_payload,
            created_at,
        )?;

        validator(&tx).map_err(|error| match error {
            StorageError::Serde(message) if message == "stale compaction snapshot" => {
                StorageError::StaleSnapshot
            }
            other => other,
        })?;
        tx.commit()?;

        // Only update the cache — and only to the final seq used — after the
        // commit actually lands, mirroring append_event/append_event_at_seq.
        self.seq_cache.insert(session_id.to_string(), event_seq);

        Ok((
            CompactionArchiveRow {
                id: archive_id.to_string(),
                session_id: session_id.to_string(),
                created_at: created_at.to_string(),
                sha256,
                byte_length: transcript_json.len(),
                message_count,
                transcript_json: None,
            },
            new_epoch,
        ))
    }
}

/// Event-ledger kind under which `commit_compaction` records the durable
/// epoch advance it just committed. Payload is [`SessionCompactedPayload`].
/// Storage-owned (not a `rightkit_agent_engine_protocol::ProtocolEvent` variant) because
/// the fact it carries — "this session's compaction epoch is now N as of
/// this archive" — is a storage-native commit-boundary record, the same
/// footing as `PROJECTION_EVENT_KIND`/`LINEAGE_EVENT_KIND` one layer up in
/// the engine crate.
pub const SESSION_COMPACTED_EVENT_KIND: &str = "sessionCompacted";

/// Payload for a [`SESSION_COMPACTED_EVENT_KIND`] event. `epoch` is the
/// post-commit (already-incremented) value — identical to what
/// `Storage::compaction_epoch` reads back afterward — so replay/rehydrate
/// never has to guess whether a persisted epoch value is pre- or
/// post-advance.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionCompactedPayload {
    pub session_id: String,
    pub epoch: i64,
    pub archive_id: String,
    pub created_at: String,
}

/// Transaction-local raw insert. Deliberately NOT `append_event` /
/// `append_event_at_seq`: those each own (begin + commit) their own
/// transaction, and calling either one here — inside `commit_compaction`'s
/// already-open `IMMEDIATE` transaction — would nest transactions on the
/// same connection. `events_fts` stays SQLite-trigger-owned (migration v3
/// AFTER INSERT trigger) exactly as it is for every other event insert path;
/// this function does not touch it.
fn insert_event_row(
    tx: &rusqlite::Transaction<'_>,
    session_id: &str,
    seq: i64,
    kind: &str,
    payload_json: &str,
    created_at: &str,
) -> Result<()> {
    // A195: raw secrets never reach disk; persisted rows carry irreversible placeholders.
    let redacted_payload =
        rightkit_agent_engine_protocol::secret_scan::redact_json_payload(payload_json);
    let payload_json: &str = &redacted_payload;
    let spilled = crate::events::spill_oversized_payload(
        tx,
        session_id,
        seq,
        kind,
        payload_json,
        created_at,
    )?;
    let payload_json: &str = spilled.as_deref().unwrap_or(payload_json);
    tx.execute(
        "INSERT INTO events (session_id, seq, kind, payload_json, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        (session_id, seq, kind, payload_json, created_at),
    )?;
    Ok(())
}

fn archive_metadata_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CompactionArchiveRow> {
    Ok(CompactionArchiveRow {
        id: row.get(0)?,
        session_id: row.get(1)?,
        created_at: row.get(2)?,
        sha256: row.get(3)?,
        byte_length: usize::try_from(row.get::<_, i64>(4)?).unwrap_or_default(),
        message_count: usize::try_from(row.get::<_, i64>(5)?).unwrap_or_default(),
        transcript_json: None,
    })
}

fn digest_hex(bytes: &[u8]) -> String {
    rightkit_crypto::sha256(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_survives_event_retention_and_database_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        {
            let storage = Storage::open(&path).unwrap();
            storage
                .insert_compaction_archive(
                    "archive-1",
                    "session-1",
                    "2026-07-17T00:00:00Z",
                    br#"[{"role":"user","content":"keep me"}]"#,
                    1,
                )
                .unwrap();
            storage
                .delete_events_before_seq("session-1", u32::MAX)
                .unwrap();
        }
        let storage = Storage::open(&path).unwrap();
        let restored = storage
            .get_compaction_archive("session-1", "archive-1")
            .unwrap();
        assert_eq!(
            restored.transcript_json.as_deref(),
            Some(br#"[{"role":"user","content":"keep me"}]"#.as_slice())
        );
        assert_eq!(
            storage.list_compaction_archives("session-1").unwrap().len(),
            1
        );
    }

    #[test]
    fn oversized_archive_fails_closed() {
        let storage = Storage::open_in_memory().unwrap();
        let bytes = vec![b'x'; MAX_COMPACTION_ARCHIVE_BYTES + 1];
        assert!(storage
            .insert_compaction_archive("archive", "session", "now", &bytes, 1)
            .is_err());
    }

    /// CRA-11 killer cases for `Storage::commit_compaction`, colocated here
    /// because this is the one function that owns the atomic transaction.
    mod commit_compaction_tests {
        use super::*;
        use crate::{SessionRow, StorageError};

        fn seeded(id: &str) -> Storage {
            let storage = Storage::open_in_memory().unwrap();
            seed_session(storage, id)
        }

        fn seeded_disk(path: &std::path::Path, id: &str) -> Storage {
            let storage = Storage::open(path).unwrap();
            seed_session(storage, id)
        }

        fn seed_session(storage: Storage, id: &str) -> Storage {
            storage
                .create_session(&SessionRow {
                    id: id.to_string(),
                    title: "t".into(),
                    workspace_root: "/ws".into(),
                    created_at: "t".into(),
                    updated_at: "t".into(),
                    parent_session_id: None,
                    role: None,
                    status: "idle".into(),
                    config_json: "{}".into(),
                    tags: String::new(),
                    recovery_marker: None,
                })
                .unwrap();
            storage
        }

        /// Three successful compactions strictly increase the epoch 1, 2, 3;
        /// a restart/rehydrate (reopen) reads back exactly 3; a fourth
        /// success continues to 4. Epochs never regress or skip.
        #[test]
        fn three_compactions_restart_then_a_fourth_strictly_increase_the_epoch() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("epoch-sequence.sqlite");
            {
                let mut storage = Storage::open(&path).unwrap();
                storage
                    .create_session(&SessionRow {
                        id: "s".into(),
                        title: "t".into(),
                        workspace_root: "/ws".into(),
                        created_at: "t".into(),
                        updated_at: "t".into(),
                        parent_session_id: None,
                        role: None,
                        status: "idle".into(),
                        config_json: "{}".into(),
                        tags: String::new(),
                        recovery_marker: None,
                    })
                    .unwrap();
                for (n, epoch) in [(1, 1), (2, 2), (3, 3)] {
                    let (_, epoch_out) = storage
                        .commit_compaction(
                            &format!("archive-{n}"),
                            "s",
                            "2026-09-09T00:00:00Z",
                            format!(r#"[{{"i":{n}}}]"#).as_bytes(),
                            1,
                            None,
                            None,
                        )
                        .unwrap();
                    assert_eq!(epoch_out, epoch);
                }
            }

            // Restart/rehydrate: a fresh open must read back the committed
            // epoch exactly, never a value derived from live markers.
            let mut storage = Storage::open(&path).unwrap();
            assert_eq!(storage.compaction_epoch("s").unwrap(), 3);

            let (_, fourth) = storage
                .commit_compaction(
                    "archive-4",
                    "s",
                    "2026-09-09T00:01:00Z",
                    br#"[{"i":4}]"#,
                    1,
                    None,
                    None,
                )
                .unwrap();
            assert_eq!(fourth, 4);
            assert_eq!(storage.compaction_epoch("s").unwrap(), 4);
        }

        /// Negative control (CRA-11): a failed commit (capacity exceeded)
        /// must not advance the epoch, must not leave a partial archive row,
        /// and must not leave a stray event.
        #[test]
        fn epoch_advanced_on_failed_commit_fails() {
            let mut storage = seeded("s");
            let oversized = vec![b'x'; MAX_COMPACTION_ARCHIVE_BYTES + 1];
            let result = storage.commit_compaction(
                "archive-oversized",
                "s",
                "2026-09-09T00:00:00Z",
                &oversized,
                1,
                None,
                None,
            );
            assert!(result.is_err(), "oversized commit must fail closed");
            assert_eq!(
                storage.compaction_epoch("s").unwrap(),
                0,
                "a failed commit must never advance the epoch"
            );
            assert!(storage.list_compaction_archives("s").unwrap().is_empty());
            assert!(storage.all_events("s").unwrap().is_empty());
        }

        /// A no-op / preparation-style failure (unknown session — nothing to
        /// atomically commit a compaction against) writes neither the epoch
        /// field nor any event, and the archive itself is rolled back too:
        /// the whole set commits together or not at all.
        #[test]
        fn unknown_session_rolls_back_the_whole_commit_including_the_archive() {
            let mut storage = Storage::open_in_memory().unwrap();
            let result = storage.commit_compaction(
                "archive-orphan",
                "does-not-exist",
                "2026-09-09T00:00:00Z",
                br#"[{"i":1}]"#,
                1,
                None,
                None,
            );
            assert!(matches!(result, Err(StorageError::NotFound(_))));
            assert!(storage
                .list_compaction_archives("does-not-exist")
                .unwrap()
                .is_empty());
            assert!(storage.all_events("does-not-exist").unwrap().is_empty());
        }

        /// A committed compaction writes the archive, the projection event,
        /// the lineage event, and the `sessionCompacted` event as one
        /// contiguous, gap-free seq run — never a mixed transcript where some
        /// land and others don't.
        #[test]
        fn commit_writes_archive_projection_lineage_and_session_compacted_atomically() {
            let mut storage = seeded("s");
            let (archive, epoch) = storage
                .commit_compaction(
                    "archive-1",
                    "s",
                    "2026-09-09T00:00:00Z",
                    br#"[{"role":"user","content":"keep me"}]"#,
                    1,
                    Some(("compactionProjection", r#"{"p":1}"#)),
                    Some(("compactionLineage", r#"{"l":1}"#)),
                )
                .unwrap();
            assert_eq!(epoch, 1);
            assert_eq!(archive.id, "archive-1");

            let events = storage.all_events("s").unwrap();
            assert_eq!(events.len(), 3, "projection + lineage + sessionCompacted");
            let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
            assert_eq!(
                kinds,
                vec![
                    "compactionProjection",
                    "compactionLineage",
                    "sessionCompacted"
                ]
            );
            let seqs: Vec<i64> = events.iter().map(|e| e.seq).collect();
            assert_eq!(
                seqs,
                vec![1, 2, 3],
                "seq run must be contiguous and gap-free"
            );

            let payload: SessionCompactedPayload =
                serde_json::from_str(&events[2].payload_json).unwrap();
            assert_eq!(payload.epoch, 1);
            assert_eq!(payload.archive_id, "archive-1");
            assert_eq!(payload.session_id, "s");

            let retrieved = storage.get_compaction_archive("s", "archive-1").unwrap();
            assert_eq!(
                retrieved.transcript_json.as_deref(),
                Some(br#"[{"role":"user","content":"keep me"}]"#.as_slice())
            );
        }

        #[test]
        fn stale_validation_after_staging_rolls_back_archive_events_and_epoch() {
            let mut storage = seeded("s");
            let result = storage.commit_compaction_with_validation(
                "rejected",
                "s",
                "2026-09-10T00:00:00Z",
                b"[]",
                0,
                Some(("compactionProjection", "{}")),
                Some(("compactionLineage", "{}")),
                |tx| {
                    let staged: i64 = tx.query_row(
                        "SELECT COUNT(*) FROM events WHERE session_id = 's'",
                        [],
                        |row| row.get(0),
                    )?;
                    assert_eq!(staged, 3, "validation must run after all event writes");
                    Err(StorageError::StaleSnapshot)
                },
            );
            assert!(matches!(result, Err(StorageError::StaleSnapshot)));
            assert!(storage.list_compaction_archives("s").unwrap().is_empty());
            assert!(storage.all_events("s").unwrap().is_empty());
            assert_eq!(storage.compaction_epoch("s").unwrap(), 0);
        }

        #[test]
        fn stale_decision_validation_rolls_back_consumption_and_retry_succeeds() {
            let mut storage = seeded("s");
            let decision = ("federation:decision-1", 7_u64);
            let rejected = storage.commit_compaction_with_decision(
                "archive-stale",
                "s",
                "2026-09-10T00:00:00Z",
                br#"[{"i":1}]"#,
                1,
                None,
                None,
                decision,
                |_| Err(StorageError::StaleSnapshot),
            );
            assert!(matches!(rejected, Err(StorageError::StaleSnapshot)));
            assert!(storage.list_compaction_archives("s").unwrap().is_empty());
            assert!(storage.all_events("s").unwrap().is_empty());
            assert_eq!(storage.compaction_epoch("s").unwrap(), 0);
            let consumed: i64 = storage.conn.query_row(
                "SELECT count(*) FROM compaction_decision_consumptions WHERE decision_identity=?1 AND restart_epoch=?2",
                rusqlite::params![decision.0, decision.1 as i64], |row| row.get(0),
            ).unwrap();
            assert_eq!(consumed, 0, "stale validation must consume no decision");

            let (_, epoch) = storage
                .commit_compaction_with_decision(
                    "archive-retry",
                    "s",
                    "2026-09-10T00:01:00Z",
                    br#"[{"i":2}]"#,
                    1,
                    None,
                    None,
                    decision,
                    |_| Ok(()),
                )
                .unwrap();
            assert_eq!(epoch, 1);
            assert_eq!(storage.list_compaction_archives("s").unwrap().len(), 1);
        }

        #[test]
        fn consumed_decision_survives_reopen_and_duplicate_refuses_without_extra_events() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("decision-consumption.sqlite");
            let decision = ("federation:stable-decision", 11_u64);
            {
                let mut storage = seeded_disk(&path, "s");
                storage
                    .commit_compaction_with_decision(
                        "archive-first",
                        "s",
                        "2026-09-10T00:00:00Z",
                        br#"[{"i":1}]"#,
                        1,
                        None,
                        None,
                        decision,
                        |_| Ok(()),
                    )
                    .unwrap();
            }
            let mut storage = Storage::open(&path).unwrap();
            let before_events = storage.all_events("s").unwrap().len();
            let duplicate = storage.commit_compaction_with_decision(
                "archive-duplicate",
                "s",
                "2026-09-10T00:01:00Z",
                br#"[{"i":2}]"#,
                1,
                None,
                None,
                decision,
                |_| Ok(()),
            );
            assert!(
                matches!(duplicate, Err(StorageError::Serde(message)) if message.contains("already consumed"))
            );
            assert_eq!(storage.all_events("s").unwrap().len(), before_events);
            assert_eq!(storage.list_compaction_archives("s").unwrap().len(), 1);
            assert_eq!(storage.compaction_epoch("s").unwrap(), 1);
        }

        #[test]
        fn new_restart_epoch_is_a_distinct_decision_consumption_key() {
            let mut storage = seeded("s");
            for (epoch, archive) in [(3_u64, "archive-epoch-3"), (4_u64, "archive-epoch-4")] {
                let (_, committed_epoch) = storage
                    .commit_compaction_with_decision(
                        archive,
                        "s",
                        "2026-09-10T00:00:00Z",
                        br#"[{"i":1}]"#,
                        1,
                        None,
                        None,
                        ("federation:rolling", epoch),
                        |_| Ok(()),
                    )
                    .unwrap();
                assert_eq!(committed_epoch, i64::from((epoch - 2) as i32));
            }
            assert_eq!(storage.list_compaction_archives("s").unwrap().len(), 2);
            let duplicate = storage.commit_compaction_with_decision(
                "archive-epoch-3-duplicate",
                "s",
                "2026-09-10T00:02:00Z",
                br#"[{"i":2}]"#,
                1,
                None,
                None,
                ("federation:rolling", 3),
                |_| Ok(()),
            );
            assert!(
                matches!(duplicate, Err(StorageError::Serde(message)) if message.contains("already consumed"))
            );
        }

        /// Interruption before commit (capacity failure on the second call)
        /// preserves the first commit's exact committed state; reopen proves
        /// the original state, never a mixed transcript.
        #[test]
        fn interruption_before_commit_preserves_prior_committed_state_on_reopen() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("interrupt-before-commit.sqlite");
            {
                let mut storage = Storage::open(&path).unwrap();
                storage
                    .create_session(&SessionRow {
                        id: "s".into(),
                        title: "t".into(),
                        workspace_root: "/ws".into(),
                        created_at: "t".into(),
                        updated_at: "t".into(),
                        parent_session_id: None,
                        role: None,
                        status: "idle".into(),
                        config_json: "{}".into(),
                        tags: String::new(),
                        recovery_marker: None,
                    })
                    .unwrap();
                storage
                    .commit_compaction(
                        "archive-good",
                        "s",
                        "2026-09-09T00:00:00Z",
                        br#"[{"i":1}]"#,
                        1,
                        None,
                        None,
                    )
                    .unwrap();
                let oversized = vec![b'x'; MAX_COMPACTION_ARCHIVE_BYTES + 1];
                assert!(storage
                    .commit_compaction(
                        "archive-bad",
                        "s",
                        "2026-09-09T00:01:00Z",
                        &oversized,
                        1,
                        None,
                        None,
                    )
                    .is_err());
            }

            let storage = Storage::open(&path).unwrap();
            assert_eq!(storage.compaction_epoch("s").unwrap(), 1);
            let archives = storage.list_compaction_archives("s").unwrap();
            assert_eq!(archives.len(), 1);
            assert_eq!(archives[0].id, "archive-good");
        }
    }
}
