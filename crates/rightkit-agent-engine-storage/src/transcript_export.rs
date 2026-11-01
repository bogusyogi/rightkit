//! Durable cursor for the native session-transcript exporter (Lane C).
//!
//! One row per session records the producer-side view of the absorbed stream
//! in the bundled Membrane worker:
//!
//! * `last_stream_seq` — the absorbed-stream position the next append batch
//!   must extend (`expected_stream_seq` on `SessionTranscriptAppendV1`). The
//!   worker CAS-verifies it against its own committed tip.
//! * `last_source_seq` — the durable `events.seq` already exported; replay
//!   resumes strictly after it.
//!
//! All advancement is compare-and-set against the pair the exporter read, so
//! a stale writer cannot silently rewind or fork the stream. `Storage::
//! delete_session_tree` drops the row with the rest of the session tree, and
//! the exporter drops it itself when the worker answers
//! `transcript_session_gone` (410) — a forgotten session never resurrects its
//! cursor.

use crate::error::Result;
use crate::Storage;
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::OptionalExtension;

/// Producer-side view of one session's absorbed-stream tip.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TranscriptExportCursor {
    /// Last committed absorbed-stream position; `0` when nothing exported.
    pub last_stream_seq: i64,
    /// `events.seq` of the last exported durable row; `0` when none.
    pub last_source_seq: i64,
}

impl Storage {
    /// Root sessions return to `idle` after a turn. Mine only settled sessions
    /// whose durable terminal event has reached the authenticated worker.
    pub fn session_ready_for_insights(&self, session: &crate::SessionRow) -> Result<bool> {
        if !matches!(
            session.status.as_str(),
            "idle" | "completed" | "failed" | "cancelled" | "interrupted" | "done" | "dormant"
        ) {
            return Ok(false);
        }
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM events e \
                 JOIN membrane_transcript_cursors c ON c.session_id = e.session_id \
                 WHERE e.session_id = ?1 AND e.kind = 'sessionTurnFinished' \
                 AND e.seq <= c.last_source_seq AND c.last_stream_seq > 0)",
                [&session.id],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Read the exporter cursor for `session_id`. `None` means this session
    /// has never exported (or was forgotten); export starts from `0`.
    pub fn transcript_export_cursor(
        &self,
        session_id: &str,
    ) -> Result<Option<TranscriptExportCursor>> {
        let row = self
            .conn
            .query_row(
                "SELECT last_stream_seq, last_source_seq \
                 FROM membrane_transcript_cursors WHERE session_id = ?1",
                [session_id],
                |row| {
                    Ok(TranscriptExportCursor {
                        last_stream_seq: row.get(0)?,
                        last_source_seq: row.get(1)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Advance the cursor from `expected` to `next` in one statement.
    ///
    /// * No row exists: the insert succeeds only because nothing else
    ///   committed first — the caller passes `expected` = `(0, 0)` and a
    ///   conflicting insert loses by `UNIQUE` and reports `false`.
    /// * Row exists and matches `expected`: the guarded `DO UPDATE` commits
    ///   the advance.
    /// * Row exists with any other value: `WHERE` rejects the update and the
    ///   caller resynchronizes from the current row (or the worker tip)
    ///   rather than silently overwriting a position it did not read.
    ///
    /// Returns `true` when the cursor row now equals `next`.
    pub fn transcript_export_advance(
        &self,
        session_id: &str,
        expected: &TranscriptExportCursor,
        next: &TranscriptExportCursor,
    ) -> Result<bool> {
        let updated_at = now_iso8601();
        let changed = self.conn.execute(
            "INSERT INTO membrane_transcript_cursors \
                 (session_id, last_stream_seq, last_source_seq, updated_at) \
             VALUES (?1, ?4, ?5, ?6) \
             ON CONFLICT(session_id) DO UPDATE SET \
                 last_stream_seq = excluded.last_stream_seq, \
                 last_source_seq = excluded.last_source_seq, \
                 updated_at = excluded.updated_at \
             WHERE membrane_transcript_cursors.last_stream_seq = ?2 \
               AND membrane_transcript_cursors.last_source_seq = ?3",
            rusqlite::params![
                session_id,
                expected.last_stream_seq,
                expected.last_source_seq,
                next.last_stream_seq,
                next.last_source_seq,
                updated_at,
            ],
        )?;
        Ok(changed == 1)
    }

    /// Server-authoritative resynchronization: overwrite the cursor with the
    /// worker-reported tip. Only a caller holding the per-session export
    /// serialization may invoke this — it bypasses the `expected` guard
    /// because the worker's committed tip is the arbiter of truth.
    pub fn transcript_export_set(
        &self,
        session_id: &str,
        cursor: &TranscriptExportCursor,
    ) -> Result<()> {
        let updated_at = now_iso8601();
        self.conn.execute(
            "INSERT INTO membrane_transcript_cursors \
                 (session_id, last_stream_seq, last_source_seq, updated_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(session_id) DO UPDATE SET \
                 last_stream_seq = excluded.last_stream_seq, \
                 last_source_seq = excluded.last_source_seq, \
                 updated_at = excluded.updated_at",
            rusqlite::params![
                session_id,
                cursor.last_stream_seq,
                cursor.last_source_seq,
                updated_at,
            ],
        )?;
        Ok(())
    }

    /// Drop the cursor row. Called when the worker reports the session's
    /// stream tombstoned (`410`) and from `delete_session_tree`, so a
    /// forgotten session can never resume export from a stale position.
    pub fn delete_transcript_export_cursor(&self, session_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM membrane_transcript_cursors WHERE session_id = ?1",
            [session_id],
        )?;
        Ok(())
    }
}

/// Best-effort wall-clock timestamp in the same epoch-seconds text form the
/// events modules persist, so cursor rows stay consistent with the rest of
/// the store.
pub(crate) fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Storage;

    fn cursor(stream: i64, source: i64) -> TranscriptExportCursor {
        TranscriptExportCursor {
            last_stream_seq: stream,
            last_source_seq: source,
        }
    }

    #[test]
    fn insights_requires_an_exported_terminal_turn_and_nonrunning_session() {
        let mut storage = Storage::open_in_memory().unwrap();
        let mut session: crate::SessionRow = serde_json::from_value(serde_json::json!({
            "id":"s", "title":"Insights", "workspace_root":"workspace",
            "created_at":"t", "status":"idle", "config_json":"{}",
        }))
        .unwrap();
        storage.create_session(&session).unwrap();
        assert!(!storage.session_ready_for_insights(&session).unwrap());
        let tool_seq = storage.append_event("s", "toolResultAdded", "{}").unwrap();
        storage
            .transcript_export_set("s", &cursor(1, tool_seq))
            .unwrap();
        assert!(!storage.session_ready_for_insights(&session).unwrap());
        let terminal = rightkit_agent_engine_protocol::ProtocolEvent::SessionTurnFinished {
            session_id: "s".into(),
            seq: tool_seq + 1,
            created_at: "t".into(),
            turn_id: "turn-1".into(),
            status: "completed".into(),
        };
        let seq = storage
            .append_event(
                "s",
                "sessionTurnFinished",
                &serde_json::to_string(&terminal).unwrap(),
            )
            .unwrap();
        assert!(!storage.session_ready_for_insights(&session).unwrap());
        storage.transcript_export_set("s", &cursor(2, seq)).unwrap();
        assert!(storage.session_ready_for_insights(&session).unwrap());
        session.status = "running".into();
        assert!(!storage.session_ready_for_insights(&session).unwrap());
        session.status = "cancelled".into();
        assert!(storage.session_ready_for_insights(&session).unwrap());
        storage.delete_transcript_export_cursor("s").unwrap();
        assert!(!storage.session_ready_for_insights(&session).unwrap());
    }

    #[test]
    fn cursor_roundtrip_and_cas() {
        let storage = Storage::open_in_memory().expect("in-memory storage");
        assert_eq!(storage.transcript_export_cursor("s").unwrap(), None);

        // First advance from the empty position inserts the row.
        assert!(storage
            .transcript_export_advance("s", &cursor(0, 0), &cursor(2, 7))
            .unwrap());
        assert_eq!(
            storage.transcript_export_cursor("s").unwrap(),
            Some(cursor(2, 7))
        );

        // A stale expected pair cannot rewind or fork the stream.
        assert!(!storage
            .transcript_export_advance("s", &cursor(0, 0), &cursor(1, 3))
            .unwrap());
        assert_eq!(
            storage.transcript_export_cursor("s").unwrap(),
            Some(cursor(2, 7))
        );

        // The matching expected pair advances the cursor.
        assert!(storage
            .transcript_export_advance("s", &cursor(2, 7), &cursor(5, 9))
            .unwrap());
        assert_eq!(
            storage.transcript_export_cursor("s").unwrap(),
            Some(cursor(5, 9))
        );
    }

    #[test]
    fn set_overwrites_and_delete_removes() {
        let storage = Storage::open_in_memory().expect("in-memory storage");
        storage
            .transcript_export_advance("s", &cursor(0, 0), &cursor(2, 7))
            .unwrap();
        storage
            .transcript_export_set("s", &cursor(10, 4))
            .expect("worker-tip resync");
        assert_eq!(
            storage.transcript_export_cursor("s").unwrap(),
            Some(cursor(10, 4))
        );
        storage.delete_transcript_export_cursor("s").unwrap();
        assert_eq!(storage.transcript_export_cursor("s").unwrap(), None);
        // Deleting a missing row is a no-op.
        storage.delete_transcript_export_cursor("s").unwrap();
    }

    #[test]
    fn cursors_do_not_cross_session_boundaries() {
        let storage = Storage::open_in_memory().expect("in-memory storage");
        storage
            .transcript_export_advance("s-a", &cursor(0, 0), &cursor(1, 5))
            .unwrap();
        assert_eq!(storage.transcript_export_cursor("s-b").unwrap(), None);
        storage.delete_transcript_export_cursor("s-b").unwrap();
        assert_eq!(
            storage.transcript_export_cursor("s-a").unwrap(),
            Some(cursor(1, 5))
        );
    }
}
