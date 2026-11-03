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
