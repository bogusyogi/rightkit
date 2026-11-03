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
    #[allow(clippy::too_many_arguments)]
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
    #[allow(clippy::too_many_arguments)]
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

    #[allow(clippy::too_many_arguments)]
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
    #[allow(clippy::too_many_arguments)]
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
