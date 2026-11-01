//! FM-175 / FM-177 / S:E11 — torn-record recovery for the session event log.
//!
//! Donor correction (`docs/canon/daemon-master-canon-atoms.md`,
//! donor-verification corrections, 2026-08-29): jcode
//! `crates/jcode-base/src/session/persistence.rs` treats its JSONL session
//! journal as a parse-and-salvage problem — `salvage_glued_journal_entries`
//! recovers two records fused by a torn write, `replay_journal_lines`
//! tolerates a torn trailing line, and a corrupt-journal checkpoint
//! quarantines a corrupt tail and continues from the last good prefix.
//!
//! the host app's session journal is the `events` table, not a JSONL file — a
//! literal "two JSON objects glued onto one line" cannot occur, because each
//! row is its own SQLite record under `PRIMARY KEY (session_id, seq)`
//! (`append.rs`'s `crash_journal_recovery_tests` module proves the
//! atomicity boundary this gives us for free). What jcode's three legs map
//! onto here, at the row level, is:
//!
//! - **glued-entry salvage** — a row whose `payload_json` is not valid JSON
//!   (the row-level analog of two journal lines fused together) is
//!   quarantined instead of poisoning replay of every other row in the
//!   session.
//! - **torn-final-line tolerance** — replay (`events_after`/`all_events`)
//!   never sees quarantined rows, so a session with one bad tail row keeps
//!   replaying its good prefix instead of erroring out.
//! - **corrupt-journal checkpoint** — when a session's `seq` sequence has a
//!   gap (any row from the first missing seq onward is untrustworthy: it
//!   can no longer be proven to be the writer's real next entry), everything
//!   at/after the gap is quarantined in one batch and the good, contiguous
//!   prefix becomes the new checkpointed tail.
//!
//! Quarantined rows are retained (not deleted) in `events_quarantine` for
//! forensics/replay-for-support, never re-surfaced through the live replay
//! path.

use crate::error::Result;
use crate::soak_health::seq_gaps;
use crate::Storage;
use rightkit_db::rusqlite;

/// Outcome of a quarantine pass: which seqs were moved out of the live log,
/// and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineOutcome {
    pub session_id: String,
    pub quarantined_seqs: Vec<i64>,
    pub reason: &'static str,
}

impl Storage {
    /// Glued-entry salvage: quarantine any event row for `session_id` whose
    /// `payload_json` does not parse as JSON, leaving every other row intact
    /// and still replayable. Returns the seqs moved.
    pub fn quarantine_malformed_payload_events(
        &mut self,
        session_id: &str,
    ) -> Result<QuarantineOutcome> {
        let candidates: Vec<(i64, String, String, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT seq, kind, payload_json, created_at FROM events WHERE session_id = ?1",
            )?;
            let rows = stmt.query_map([session_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            out
        };

        let malformed: Vec<(i64, String, String, String)> = candidates
            .into_iter()
            .filter(|(_, _, payload, _)| {
                serde_json::from_str::<serde_json::Value>(payload).is_err()
            })
            .collect();

        let mut quarantined_seqs = Vec::new();
        if !malformed.is_empty() {
            let tx = rightkit_db::write_txn(&mut self.conn)?;
            let quarantined_at = now_iso8601();
            for (seq, kind, payload_json, created_at) in &malformed {
                tx.execute(
                    "INSERT OR REPLACE INTO events_quarantine \
                     (session_id, seq, kind, payload_json, created_at, reason, quarantined_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    (
                        session_id,
                        seq,
                        kind,
                        payload_json,
                        created_at,
                        "malformed-payload-glued-entry",
                        &quarantined_at,
                    ),
                )?;
                tx.execute(
                    "DELETE FROM events WHERE session_id = ?1 AND seq = ?2",
                    (session_id, seq),
                )?;
                quarantined_seqs.push(*seq);
            }
            tx.commit()?;
        }

        Ok(QuarantineOutcome {
            session_id: session_id.to_string(),
            quarantined_seqs,
            reason: "malformed-payload-glued-entry",
        })
    }

    /// Move rows that rehydration proved undecodable out of the live log
    /// (audit M9). The caller decides decodability (it owns the protocol
    /// types); storage retains the raw bytes in `events_quarantine` under
    /// `reason` and removes the live rows so they neither bloat later replay
    /// pages nor get silently skipped on every resume. Only seqs that exist
    /// for `session_id` are touched; returns the seqs actually moved.
    pub fn quarantine_undecodable_events(
        &mut self,
        session_id: &str,
        seqs: &[i64],
        reason: &str,
    ) -> Result<Vec<i64>> {
        if seqs.is_empty() {
            return Ok(Vec::new());
        }
        let tx = rightkit_db::write_txn(&mut self.conn)?;
        let quarantined_at = now_iso8601();
        let mut moved = Vec::new();
        for seq in seqs {
            let row: Option<(String, String, String)> = tx
                .query_row(
                    "SELECT kind, payload_json, created_at FROM events WHERE session_id = ?1 AND seq = ?2",
                    (session_id, seq),
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .ok();
            let Some((kind, payload_json, created_at)) = row else {
                continue;
            };
            tx.execute(
                "INSERT OR REPLACE INTO events_quarantine (session_id, seq, kind, payload_json, created_at, reason, quarantined_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                (
                    session_id,
                    seq,
                    &kind,
                    &payload_json,
                    &created_at,
                    reason,
                    &quarantined_at,
                ),
            )?;
            tx.execute(
                "DELETE FROM events WHERE session_id = ?1 AND seq = ?2",
                (session_id, seq),
            )?;
            moved.push(*seq);
        }
        tx.commit()?;
        Ok(moved)
    }

    /// Corrupt-journal checkpoint: if `session_id`'s seq sequence has a gap,
    /// quarantine every row from the first missing seq onward (the
    /// untrustworthy tail) and checkpoint on the contiguous good prefix.
    /// Returns `None` when the session has no gap (nothing to checkpoint).
    pub fn checkpoint_events_after_gap(
        &mut self,
        session_id: &str,
    ) -> Result<Option<QuarantineOutcome>> {
        let gaps = seq_gaps(&self.conn, session_id)?;
        let Some(&first_gap) = gaps.first() else {
            return Ok(None);
        };

        // Everything from the first missing seq onward is untrustworthy —
        // it can never again be proven contiguous with the good prefix.
        let tail: Vec<(i64, String, String, String)> = {
            let mut stmt = self.conn.prepare(
                "SELECT seq, kind, payload_json, created_at FROM events \
                 WHERE session_id = ?1 AND seq >= ?2 ORDER BY seq ASC",
            )?;
            let rows = stmt.query_map(rusqlite::params![session_id, first_gap], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            out
        };

        let mut quarantined_seqs = Vec::new();
        if !tail.is_empty() {
            let tx = rightkit_db::write_txn(&mut self.conn)?;
            let quarantined_at = now_iso8601();
            for (seq, kind, payload_json, created_at) in &tail {
                tx.execute(
                    "INSERT OR REPLACE INTO events_quarantine \
                     (session_id, seq, kind, payload_json, created_at, reason, quarantined_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    (
                        session_id,
                        seq,
                        kind,
                        payload_json,
                        created_at,
                        "corrupt-journal-checkpoint-gap-tail",
                        &quarantined_at,
                    ),
                )?;
                tx.execute(
                    "DELETE FROM events WHERE session_id = ?1 AND seq = ?2",
                    (session_id, seq),
                )?;
                quarantined_seqs.push(*seq);
            }
            tx.commit()?;
        }

        // The checkpointed tail moves the cached high-water mark back down to
        // the good prefix so the next append resumes contiguously.
        let good_prefix_max = first_gap - 1;
        if good_prefix_max >= 0 {
            self.seq_cache
                .insert(session_id.to_string(), good_prefix_max);
        }

        Ok(Some(QuarantineOutcome {
            session_id: session_id.to_string(),
            quarantined_seqs,
            reason: "corrupt-journal-checkpoint-gap-tail",
        }))
    }
}

fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.to_string()
}

#[cfg(test)]
mod tests {
    use crate::Storage;
    use rightkit_db::rusqlite;

    #[test]
    fn quarantine_undecodable_events_moves_rows_and_keeps_raw_bytes() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        storage.append_event("s-u", "k", r#"{"ok":1}"#).unwrap();
        storage.append_event("s-u", "k", r#"{"bad":2}"#).unwrap();
        storage.append_event("s-u", "k", r#"{"ok":3}"#).unwrap();
        let moved = storage
            .quarantine_undecodable_events("s-u", &[2, 99], "undecodable-on-rehydrate")
            .expect("quarantine");
        assert_eq!(moved, vec![2], "only existing seqs move");
        let live: Vec<i64> = storage
            .all_events("s-u")
            .unwrap()
            .iter()
            .map(|e| e.seq)
            .collect();
        assert_eq!(live, vec![1, 3]);
        let (payload, reason): (String, String) = storage
            .conn
            .query_row(
                "SELECT payload_json, reason FROM events_quarantine WHERE session_id = 's-u' AND seq = 2",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("raw row retained");
        assert_eq!(payload, r#"{"bad":2}"#);
        assert_eq!(reason, "undecodable-on-rehydrate");
    }

    /// FM-175 — glued-entry salvage: a row with unparsable `payload_json`
    /// (the row-level analog of two JSONL records fused by a torn write) is
    /// quarantined without disturbing any other row.
    #[test]
    fn fm175_quarantines_malformed_payload_without_disturbing_good_rows() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        for i in 1..=4 {
            storage
                .append_event("s-glued", "kind", &format!(r#"{{"i":{i}}}"#))
                .expect("append");
        }
        // Simulate a glued/corrupted row by corrupting an already-appended
        // row's payload in place — the row-level analog of a torn write
        // fusing two records onto one physical slot in a flat-file journal.
        // Corrupting in place (rather than raw-INSERTing a new row) keeps
        // the seq high-water mark cache consistent with the table, exactly
        // as the daemon's single writer would leave it.
        storage
            .conn
            .execute(
                "UPDATE events SET payload_json = ?1 WHERE session_id = ?2 AND seq = ?3",
                rusqlite::params![
                    r#"{"i":4}{"i":5}"#, // two objects glued together — invalid JSON
                    "s-glued",
                    4_i64
                ],
            )
            .expect("corrupt row in place");
        storage
            .append_event("s-glued", "kind", r#"{"i":5}"#)
            .expect("append after glued row");

        let outcome = storage
            .quarantine_malformed_payload_events("s-glued")
            .expect("quarantine pass");
        assert_eq!(outcome.quarantined_seqs, vec![4]);
        assert_eq!(outcome.reason, "malformed-payload-glued-entry");

        let remaining = storage.all_events("s-glued").expect("replay");
        let seqs: Vec<i64> = remaining.iter().map(|e| e.seq).collect();
        assert_eq!(
            seqs,
            vec![1, 2, 3, 5],
            "good rows on both sides of the glued entry survive"
        );
        assert!(remaining
            .iter()
            .all(|e| serde_json::from_str::<serde_json::Value>(&e.payload_json).is_ok()));
    }

    /// FM-175 — a session with no malformed payloads is left untouched.
    #[test]
    fn fm175_quarantine_is_a_noop_when_all_payloads_are_valid() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        for i in 1..=4 {
            storage
                .append_event("s-clean", "kind", &format!(r#"{{"i":{i}}}"#))
                .expect("append");
        }
        let outcome = storage
            .quarantine_malformed_payload_events("s-clean")
            .expect("quarantine pass");
        assert!(outcome.quarantined_seqs.is_empty());
        assert_eq!(storage.all_events("s-clean").unwrap().len(), 4);
    }

    /// FM-177/S:E11 — torn-final-line tolerance: a malformed trailing row is
    /// quarantined and replay serves the good prefix rather than erroring.
    #[test]
    fn fm177_torn_final_row_is_tolerated_and_good_prefix_still_replays() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        for i in 1..=5 {
            storage
                .append_event("s-torn-final", "kind", &format!(r#"{{"i":{i}}}"#))
                .expect("append");
        }
        // Torn final line: truncated mid-value, not valid JSON.
        storage
            .conn
            .execute(
                "INSERT INTO events (session_id, seq, kind, payload_json, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["s-torn-final", 6_i64, "kind", r#"{"i":"unterm"#, "0"],
            )
            .expect("inject torn tail row");

        let outcome = storage
            .quarantine_malformed_payload_events("s-torn-final")
            .expect("quarantine pass");
        assert_eq!(outcome.quarantined_seqs, vec![6]);

        let replayed = storage.events_after("s-torn-final", 0).expect("replay");
        let seqs: Vec<i64> = replayed.iter().map(|e| e.seq).collect();
        assert_eq!(
            seqs,
            vec![1, 2, 3, 4, 5],
            "replay must not fail on the torn tail — it serves the good prefix"
        );
        assert_eq!(storage.latest_seq("s-torn-final").unwrap(), 5);
    }

    /// Corrupt-journal checkpoint: a mid-session seq gap quarantines
    /// everything from the gap onward and checkpoints on the good prefix.
    #[test]
    fn corrupt_journal_checkpoint_quarantines_gap_tail_and_keeps_good_prefix() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        // Committed good prefix: seq 1..3.
        for i in 1..=3 {
            storage
                .append_event("s-gap", "kind", &format!(r#"{{"i":{i}}}"#))
                .expect("append");
        }
        // Corrupt middle: seq 4 is missing, but seq 5 and 6 exist (as if a
        // torn write dropped one record but the writer resumed numbering
        // past it before a checkpoint caught the gap).
        storage
            .conn
            .execute(
                "INSERT INTO events (session_id, seq, kind, payload_json, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["s-gap", 5_i64, "kind", r#"{"i":5}"#, "0"],
            )
            .expect("inject post-gap row");
        storage
            .conn
            .execute(
                "INSERT INTO events (session_id, seq, kind, payload_json, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params!["s-gap", 6_i64, "kind", r#"{"i":6}"#, "0"],
            )
            .expect("inject post-gap row");

        let outcome = storage
            .checkpoint_events_after_gap("s-gap")
            .expect("checkpoint pass")
            .expect("a gap must be found");
        assert_eq!(outcome.quarantined_seqs, vec![5, 6]);
        assert_eq!(outcome.reason, "corrupt-journal-checkpoint-gap-tail");

        let replayed = storage.all_events("s-gap").expect("replay");
        let seqs: Vec<i64> = replayed.iter().map(|e| e.seq).collect();
        assert_eq!(
            seqs,
            vec![1, 2, 3],
            "only the contiguous good prefix remains live"
        );
        assert_eq!(
            storage.latest_seq("s-gap").unwrap(),
            3,
            "checkpoint resumes numbering from the good prefix"
        );

        // Continue: a fresh append after checkpoint resumes contiguously
        // from the good prefix, proving the daemon can keep operating.
        let next_seq = storage
            .append_event("s-gap", "kind", r#"{"resumed":true}"#)
            .expect("append after checkpoint");
        assert_eq!(
            next_seq, 4,
            "numbering resumes contiguously after checkpoint"
        );
    }

    /// Corrupt-journal checkpoint is a no-op for a session with no gap.
    #[test]
    fn corrupt_journal_checkpoint_is_a_noop_without_a_gap() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        for i in 1..=3 {
            storage
                .append_event("s-no-gap", "kind", &format!(r#"{{"i":{i}}}"#))
                .expect("append");
        }
        let outcome = storage
            .checkpoint_events_after_gap("s-no-gap")
            .expect("checkpoint pass");
        assert!(outcome.is_none());
        assert_eq!(storage.all_events("s-no-gap").unwrap().len(), 3);
    }
}
