use crate::error::Result;
use crate::events::SqliteOpTimer;
use crate::Storage;
use rightkit_db::rusqlite;

/// Largest payload stored inline in the live `events` table (audit M9). A
/// larger payload would be re-read on every resume and replay page, so the
/// full bytes spill to `events_quarantine` (retained for forensics, never
/// replayed) and the live row carries a small, valid-JSON stub.
pub const MAX_EVENT_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;

const OVERSIZED_PAYLOAD_REASON: &str = "oversized-payload-spilled";

/// Deterministic stub for an oversized payload. It does not decode as a
/// protocol event, so rehydration quarantines it instead of re-loading the
/// bloat.
pub(crate) fn oversized_payload_stub(kind: &str, payload_json: &str) -> String {
    let preview: String = payload_json.chars().take(512).collect();
    serde_json::json!({
        "$kind": kind,
        "oversizedPayloadSpilled": true,
        "originalBytes": payload_json.len(),
        "preview": preview,
    })
    .to_string()
}

/// Spill an oversized payload inside the caller's transaction. Returns the
/// stub to store in its place, or `None` when the payload fits.
pub(crate) fn spill_oversized_payload(
    tx: &rusqlite::Transaction<'_>,
    session_id: &str,
    seq: i64,
    kind: &str,
    payload_json: &str,
    created_at: &str,
) -> Result<Option<String>> {
    if payload_json.len() <= MAX_EVENT_PAYLOAD_BYTES {
        return Ok(None);
    }
    tx.execute(
        "INSERT OR REPLACE INTO events_quarantine (session_id, seq, kind, payload_json, created_at, reason, quarantined_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        (
            session_id,
            seq,
            kind,
            payload_json,
            created_at,
            OVERSIZED_PAYLOAD_REASON,
            created_at,
        ),
    )?;
    Ok(Some(oversized_payload_stub(kind, payload_json)))
}

impl Storage {
    /// Append an event to the session's log. Assigns the next contiguous `seq`
    /// (starting at 1 for an empty session) atomically inside a transaction and
    /// returns the newly assigned seq.
    ///
    /// This function opens and commits its own `IMMEDIATE` transaction. Do
    /// NOT call it (or [`Storage::append_event_at_seq`]) from inside another
    /// already-open transaction on this connection — in particular, never
    /// from inside `Storage::commit_compaction` (`compaction_archives.rs`),
    /// which needs the archive, projection/lineage events, the durable
    /// `compaction_epoch` advance, and the `sessionCompacted` event to commit
    /// or roll back together as one unit. `commit_compaction` inserts its
    /// event rows through its own transaction-local `insert_event_row`
    /// helper for exactly this reason.
    pub fn append_event(
        &mut self,
        session_id: &str,
        kind: &str,
        payload_json: &str,
    ) -> Result<i64> {
        // A195: raw secrets never reach disk; persisted rows carry irreversible placeholders.
        let redacted_payload =
            rightkit_agent_engine_protocol::secret_scan::redact_json_payload(payload_json);
        let payload_json: &str = &redacted_payload;
        // Use the cached high-water mark when we have one (this connection,
        // behind the daemon mutex, is the only writer); otherwise fall back to a
        // one-time MAX(seq) probe. Avoids a SELECT on every append.
        let _op_timer = SqliteOpTimer::start(super::SqliteOpKind::AppendEvent);
        let cached = self.seq_cache.get(session_id).copied();
        let tx = rightkit_db::write_txn(&mut self.conn)?;
        let next_seq: i64 = match cached {
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
        let created_at = now_iso8601();
        let spilled =
            spill_oversized_payload(&tx, session_id, next_seq, kind, payload_json, &created_at)?;
        let payload_json: &str = spilled.as_deref().unwrap_or(payload_json);
        tx.execute(
            "INSERT INTO events (session_id, seq, kind, payload_json, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (session_id, next_seq, kind, payload_json, &created_at),
        )?;
        // The `events_fts` mirror is owned by SQLite triggers installed in
        // migration v3 (AFTER INSERT/UPDATE/DELETE on `events`). Inserting
        // here from the application would either duplicate rows (once the
        // triggers are active) or drift against the triggers' payload
        // extraction — so the application never touches `events_fts`.
        tx.commit()?;
        self.seq_cache.insert(session_id.to_string(), next_seq);
        Ok(next_seq)
    }
}

/// Best-effort wall-clock timestamp. Uses seconds since the Unix epoch encoded
/// as a string so we have no chrono dependency.
fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.to_string()
}
