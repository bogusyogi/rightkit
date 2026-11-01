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

#[cfg(test)]
mod crash_journal_recovery_tests {
    //! S:E11 — Torn-record crash-journal recovery proof.
    //!
    //! The donor (`jcode` pinned `9ead5286`) treats its JSONL session journal
    //! as a parse-and-salvage problem: a torn line or glued entry is recovered
    //! by walking candidate starts in a corrupt line. the host app's live path
    //! does NOT keep a parallel JSONL journal — it stores events in the
    //! `events` table behind a SQLite WAL/rollback journal managed by
    //! `Storage::open` and an `IMMEDIATE` transaction in `append_event`. The
    //! atomicity boundary is therefore SQLite's commit, not a JSONL line, and
    //! the equivalent of the donor's "torn line" is "an uncommitted
    //! transaction abandoned by a process that exits before commit".
    //!
    //! The two tests below prove the seam's guarantee using real on-disk
    //! storage (no mocks), and assert that:
    //!
    //! 1. The committed prefix survives reopen and replays through
    //!    `events_after` with strictly contiguous per-session `seq`.
    //! 2. An uncommitted `events` insert held by a child process that exits
    //!    before commit never appears as a partial event after parent reopen.
    //!
    //! Production code is unchanged: SQLite's transactional journal already
    //! satisfies the atom. The proof is what the seam does not currently
    //! document, so it lives here, in the same file, in the same module.
    use crate::Storage;

    /// Recovery: committed prefix survives reopen, replay is contiguous.
    #[test]
    fn committed_prefix_survives_reopen_and_replays_contiguously() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crash-journal-committed.sqlite");

        // Writer 1: commit three events, then drop without checkpointing.
        let mut writer = Storage::open(&path).expect("storage opens");
        for i in 1..=3 {
            let seq = writer
                .append_event("s-recover", "kind", &format!(r#"{{"i":{i}}}"#))
                .expect("append succeeds");
            assert_eq!(seq, i, "per-session seq must stay contiguous");
        }
        drop(writer);

        // Reopen and replay. The committed prefix is the entire log; nothing
        // was uncommitted, so a torn tail is impossible by construction, but
        // the seam must still return the rows in order.
        let reader = Storage::open(&path).expect("storage reopens");
        let replayed = reader.events_after("s-recover", 0).expect("replay");
        let seqs: Vec<i64> = replayed.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3], "committed prefix must survive");
        assert_eq!(reader.latest_seq("s-recover").unwrap(), 3);
    }

    const CRASH_CHILD_DB_ENV: &str = "RIGHTKIT_S_E11_CHILD_DB";
    const CRASH_CHILD_READY_ENV: &str = "RIGHTKIT_S_E11_CHILD_READY";

    /// Child-only harness: hold an uncommitted row, prove insertion reached
    /// SQLite, signal readiness, then exit without running transaction or
    /// connection destructors. Parent test launches this exact test by name.
    #[test]
    fn child_process_exits_with_uncommitted_tail() {
        let Some(path) = std::env::var_os(CRASH_CHILD_DB_ENV) else {
            return;
        };
        let ready = std::env::var_os(CRASH_CHILD_READY_ENV).expect("child ready path");
        let mut writer = Storage::open(std::path::Path::new(&path)).expect("child storage opens");
        let tx = rightkit_db::write_txn(&mut writer.conn).expect("child begins IMMEDIATE");
        tx.execute(
            "INSERT INTO events (session_id, seq, kind, payload_json, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                "s-torn",
                5_i64,
                "kind-torn",
                r#"{"i":"never-committed"}"#,
                "0",
            ),
        )
        .expect("child inserts uncommitted tail");
        let visible_inside_tx: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM events WHERE session_id = ?1 AND seq = 5",
                ["s-torn"],
                |row| row.get(0),
            )
            .expect("child reads uncommitted tail");
        assert_eq!(visible_inside_tx, 1);
        std::fs::write(ready, b"uncommitted-tail-visible").expect("child signals readiness");
        std::process::exit(86);
    }

    /// Recovery: a child process exits while an `IMMEDIATE` transaction
    /// contains an uncommitted event. Parent reopen must retain committed
    /// prefix, exclude phantom row, & preserve contiguous seq.
    #[test]
    fn uncommitted_tail_is_rolled_back_and_never_appears_as_partial_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crash-journal-torn.sqlite");
        let ready = dir.path().join("child-ready");

        // Commit a real prefix so the recovery target is non-trivial.
        let mut writer = Storage::open(&path).expect("storage opens");
        for i in 1..=4 {
            let seq = writer
                .append_event("s-torn", "kind", &format!(r#"{{"i":{i}}}"#))
                .expect("append");
            assert_eq!(seq, i);
        }
        let committed_prefix_seq = writer.latest_seq("s-torn").unwrap();
        assert_eq!(committed_prefix_seq, 4);

        drop(writer);

        #[allow(clippy::disallowed_methods)]
        let mut child_command = std::process::Command::new(std::env::current_exe().unwrap());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            child_command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let child_status = child_command
            .arg("--exact")
            .arg(
                "events::append::crash_journal_recovery_tests::child_process_exits_with_uncommitted_tail",
            )
            .arg("--nocapture")
            .env(CRASH_CHILD_DB_ENV, &path)
            .env(CRASH_CHILD_READY_ENV, &ready)
            .status()
            .expect("launch crash child");
        assert_eq!(child_status.code(), Some(86), "child must skip destructors");
        assert_eq!(
            std::fs::read(&ready).unwrap(),
            b"uncommitted-tail-visible",
            "child must reach active uncommitted transaction before abort"
        );

        // Reopen on the same on-disk path. The torn tail must be gone.
        let reader = Storage::open(&path).expect("storage reopens after torn write");
        let replayed = reader.events_after("s-torn", 0).expect("replay");
        let seqs: Vec<i64> = replayed.iter().map(|e| e.seq).collect();
        assert_eq!(
            seqs,
            vec![1, 2, 3, 4],
            "only the committed prefix must be visible — the uncommitted row is the torn tail and must be rolled back"
        );
        // No partial / phantom row should be reachable, neither by seq nor
        // by kind.
        assert!(
            replayed.iter().all(|e| e.kind == "kind"),
            "the uncommitted kind-torn row must not survive"
        );
        // Contiguity: the recovery target's per-session seq numbering remains
        // strictly contiguous from 1 through the last committed seq.
        for (idx, ev) in replayed.iter().enumerate() {
            assert_eq!(ev.seq, idx as i64 + 1, "seq must remain contiguous");
        }
        assert_eq!(reader.latest_seq("s-torn").unwrap(), 4);
    }

    /// FM-177 — durable append failure surfaces as `Err`, never a silent
    /// success. `api_sessions/lifecycle.rs`'s session-create/fork handlers
    /// depend on this: they treat `last_error`/append failure as a hard 503
    /// rather than warn-and-continue. Proven here at the storage boundary by
    /// dropping the `events` table out from under an open connection so the
    /// next `append_event` hits a real SQLite error, not a mock.
    #[test]
    fn fm177_durable_append_failure_surfaces_as_err_not_silent_success() {
        let mut storage = crate::Storage::open_in_memory().expect("in-memory storage");
        storage
            .append_event("s-durable", "kind", "{}")
            .expect("first append succeeds while the table exists");

        storage
            .conn
            .execute("DROP TABLE events", [])
            .expect("drop events table to force a durable append failure");

        let result = storage.append_event("s-durable", "kind", "{}");
        assert!(
            result.is_err(),
            "append_event must return Err when the durable write cannot land, \
             so the API layer can surface 503 instead of a false success"
        );
    }
}

#[cfg(test)]
mod secret_persistence_tests {
    use crate::Storage;

    #[test]
    fn append_event_never_persists_raw_secret() {
        let fake = format!("sk-{}", "FAKE".repeat(8));
        let payload = serde_json::json!({"content": format!("my key {fake}")}).to_string();
        let mut s = Storage::open_in_memory().expect("storage");
        s.append_event("s-secret", "userMessageSubmitted", &payload)
            .expect("append");
        let rows = s.all_events("s-secret").expect("rows");
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].payload_json.contains("FAKE"));
        assert!(rows[0].payload_json.contains("CR_SECRET_APIKEY_REDACTED"));
    }

    #[test]
    fn clean_payload_is_stored_byte_identical() {
        let payload = r#"{"content":"hello"}"#;
        let mut s = Storage::open_in_memory().expect("storage");
        s.append_event("s-clean", "k", payload).expect("append");
        assert_eq!(s.all_events("s-clean").unwrap()[0].payload_json, payload);
    }
}

#[cfg(test)]
mod oversized_payload_tests {
    use super::*;

    #[test]
    fn oversized_payload_spills_and_live_row_stays_small_valid_json() {
        let mut storage = Storage::open_in_memory().expect("storage");
        let big = format!(
            r#"{{"blob":"{}"}}"#,
            "z".repeat(MAX_EVENT_PAYLOAD_BYTES + 10)
        );
        let seq = storage
            .append_event("s-big", "someKind", &big)
            .expect("append");
        let rows = storage.all_events("s-big").expect("rows");
        assert_eq!(rows.len(), 1);
        assert!(rows[0].payload_json.len() < 4096);
        let stub: serde_json::Value =
            serde_json::from_str(&rows[0].payload_json).expect("valid json");
        assert_eq!(stub["oversizedPayloadSpilled"], true);
        assert_eq!(stub["originalBytes"].as_u64().unwrap() as usize, big.len());
        let (spilled_len, reason): (i64, String) = storage
            .conn
            .query_row(
                "SELECT length(payload_json), reason FROM events_quarantine WHERE session_id=?1 AND seq=?2",
                (&"s-big", seq),
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("spilled row");
        assert_eq!(reason, "oversized-payload-spilled");
        assert!(spilled_len as usize >= MAX_EVENT_PAYLOAD_BYTES);
    }

    #[test]
    fn payload_under_the_cap_is_stored_inline() {
        let mut storage = Storage::open_in_memory().expect("storage");
        let payload = format!(r#"{{"b":"{}"}}"#, "q".repeat(1000));
        storage.append_event("s-ok", "k", &payload).expect("append");
        assert_eq!(storage.all_events("s-ok").unwrap()[0].payload_json, payload);
    }
}
