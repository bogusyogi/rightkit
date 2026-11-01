//! Durable handoff state for epoch-fenced controller transfer (PL-008).
//!
//! `handoff_sessions` is the current ownership projection; `handoff_journal`
//! retains every accepted transition for restart/replay diagnosis.  Each
//! projection update & its journal append share one SQLite transaction, so a
//! recovered process never observes a state advance without its causal record.

use crate::{Result, Storage};
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::{params, OptionalExtension, Transaction};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffRow {
    pub session_id: String,
    pub controller_epoch: u64,
    pub source_target: String,
    pub destination_target: String,
    pub phase: String,
    pub replay_cursor: Option<u64>,
    pub terminal_detail: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffJournalRow {
    pub session_id: String,
    pub controller_epoch: u64,
    pub sequence: u64,
    pub phase: String,
    pub replay_cursor: Option<u64>,
    pub detail: Option<String>,
    pub recorded_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffTerminalState {
    Completed,
    Cancelled,
    Failed,
}

impl HandoffTerminalState {
    fn phase(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

/// A stale or phase-invalid request is an expected fence outcome, not a
/// storage failure. Callers must refuse it without retrying against another
/// epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffAdvance {
    Applied(HandoffRow),
    Missing,
    StaleEpoch,
    InvalidPhase,
    ActiveHandoff,
}

/// Proven-quiescence outcomes.  Only `ReadyToHandoff` permits source teardown;
/// all other outcomes leave source runtime untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuiescenceOutcome {
    Unverified,
    DraftPresent,
    DecisionPending,
    ActiveWork,
    ReadyToHandoff,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffGapMessage {
    pub session_id: String,
    pub controller_epoch: u64,
    pub message_key: String,
    pub payload_json: String,
    pub delivered_at: Option<String>,
    pub created_at: String,
}

impl Storage {
    /// Retain messages arriving during source fencing.  The composite primary
    /// key makes retries idempotent without synthesizing terminal history.
    pub fn retain_handoff_gap_message(&self, message: &HandoffGapMessage) -> Result<bool> {
        let inserted = self.conn.execute(
            "INSERT OR IGNORE INTO handoff_gap_messages (session_id, controller_epoch, message_key, payload_json, delivered_at, created_at) VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
            params![message.session_id, message.controller_epoch as i64, message.message_key, message.payload_json, message.created_at],
        )?;
        Ok(inserted == 1)
    }

    pub fn pending_handoff_gap_messages(
        &self,
        session_id: &str,
        epoch: u64,
    ) -> Result<Vec<HandoffGapMessage>> {
        let mut statement = self.conn.prepare("SELECT session_id, controller_epoch, message_key, payload_json, delivered_at, created_at FROM handoff_gap_messages WHERE session_id=?1 AND controller_epoch=?2 AND delivered_at IS NULL ORDER BY created_at, message_key")?;
        let messages = statement
            .query_map(params![session_id, epoch as i64], |row| {
                Ok(HandoffGapMessage {
                    session_id: row.get(0)?,
                    controller_epoch: row.get::<_, i64>(1)? as u64,
                    message_key: row.get(2)?,
                    payload_json: row.get(3)?,
                    delivered_at: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(messages)
    }

    pub fn mark_handoff_gap_delivered(
        &self,
        session_id: &str,
        epoch: u64,
        message_key: &str,
        delivered_at: &str,
    ) -> Result<bool> {
        Ok(self.conn.execute("UPDATE handoff_gap_messages SET delivered_at=?1 WHERE session_id=?2 AND controller_epoch=?3 AND message_key=?4 AND delivered_at IS NULL", params![delivered_at, session_id, epoch as i64, message_key])? == 1)
    }

    pub fn handoff_state(&self, session_id: &str) -> Result<Option<HandoffRow>> {
        self.conn
            .query_row(
                "SELECT session_id, controller_epoch, source_target, destination_target, phase, replay_cursor, terminal_detail, created_at, updated_at FROM handoff_sessions WHERE session_id = ?1",
                [session_id],
                row_to_handoff,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn handoff_journal(&self, session_id: &str) -> Result<Vec<HandoffJournalRow>> {
        let mut statement = self.conn.prepare(
            "SELECT session_id, controller_epoch, sequence, phase, replay_cursor, detail, recorded_at FROM handoff_journal WHERE session_id = ?1 ORDER BY sequence",
        )?;
        let rows = statement
            .query_map([session_id], row_to_handoff_journal)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Start a new handoff only when its epoch strictly supersedes any retained
    /// terminal record. An active record never permits replacement.
    pub fn begin_handoff(&self, row: HandoffRow) -> Result<HandoffAdvance> {
        let Some(phase) = (!row.session_id.trim().is_empty()
            && !row.source_target.trim().is_empty()
            && !row.destination_target.trim().is_empty()
            && row.source_target != row.destination_target)
            .then_some("quiescing")
        else {
            return Ok(HandoffAdvance::InvalidPhase);
        };
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        if let Some(existing) = handoff_state_tx(&tx, &row.session_id)? {
            if !is_terminal(&existing.phase) {
                return Ok(HandoffAdvance::ActiveHandoff);
            }
            if row.controller_epoch <= existing.controller_epoch {
                return Ok(HandoffAdvance::StaleEpoch);
            }
        }
        tx.execute(
            "INSERT INTO handoff_sessions (session_id, controller_epoch, source_target, destination_target, phase, replay_cursor, terminal_detail, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, NULL, NULL, ?6, ?7) ON CONFLICT(session_id) DO UPDATE SET controller_epoch=excluded.controller_epoch, source_target=excluded.source_target, destination_target=excluded.destination_target, phase=excluded.phase, replay_cursor=NULL, terminal_detail=NULL, created_at=excluded.created_at, updated_at=excluded.updated_at",
            params![row.session_id, row.controller_epoch as i64, row.source_target, row.destination_target, phase, row.created_at, row.updated_at],
        )?;
        let current = handoff_state_tx(&tx, &row.session_id)?.expect("inserted handoff must exist");
        append_transition(&tx, &current, None, None)?;
        tx.commit()?;
        Ok(HandoffAdvance::Applied(current))
    }

    pub fn quiesce_handoff(
        &self,
        session_id: &str,
        controller_epoch: u64,
        recorded_at: &str,
    ) -> Result<HandoffAdvance> {
        self.advance_handoff(
            session_id,
            controller_epoch,
            "quiescing",
            "source_quiesced",
            None,
            None,
            recorded_at,
        )
    }

    pub fn resume_handoff(
        &self,
        session_id: &str,
        controller_epoch: u64,
        replay_cursor: u64,
        recorded_at: &str,
    ) -> Result<HandoffAdvance> {
        self.advance_handoff(
            session_id,
            controller_epoch,
            "source_quiesced",
            "resumed",
            Some(replay_cursor),
            None,
            recorded_at,
        )
    }

    pub fn finish_handoff(
        &self,
        session_id: &str,
        controller_epoch: u64,
        terminal: HandoffTerminalState,
        detail: Option<&str>,
        recorded_at: &str,
    ) -> Result<HandoffAdvance> {
        let Some(current) = self.handoff_state(session_id)? else {
            return Ok(HandoffAdvance::Missing);
        };
        if current.controller_epoch != controller_epoch {
            return Ok(HandoffAdvance::StaleEpoch);
        }
        if !matches!(
            current.phase.as_str(),
            "quiescing" | "source_quiesced" | "resumed"
        ) {
            return Ok(HandoffAdvance::InvalidPhase);
        }
        self.advance_handoff(
            session_id,
            controller_epoch,
            &current.phase,
            terminal.phase(),
            None,
            detail,
            recorded_at,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn advance_handoff(
        &self,
        session_id: &str,
        controller_epoch: u64,
        expected_phase: &str,
        next_phase: &str,
        replay_cursor: Option<u64>,
        detail: Option<&str>,
        recorded_at: &str,
    ) -> Result<HandoffAdvance> {
        let tx = rightkit_db::write_txn_unchecked(&self.conn)?;
        let Some(existing) = handoff_state_tx(&tx, session_id)? else {
            return Ok(HandoffAdvance::Missing);
        };
        if existing.controller_epoch != controller_epoch {
            return Ok(HandoffAdvance::StaleEpoch);
        }
        if existing.phase != expected_phase {
            return Ok(HandoffAdvance::InvalidPhase);
        }
        tx.execute(
            "UPDATE handoff_sessions SET phase=?1, replay_cursor=COALESCE(?2, replay_cursor), terminal_detail=?3, updated_at=?4 WHERE session_id=?5 AND controller_epoch=?6 AND phase=?7",
            params![next_phase, replay_cursor.map(|cursor| cursor as i64), detail, recorded_at, session_id, controller_epoch as i64, expected_phase],
        )?;
        let current = handoff_state_tx(&tx, session_id)?.expect("updated handoff must exist");
        append_transition(&tx, &current, replay_cursor, detail)?;
        tx.commit()?;
        Ok(HandoffAdvance::Applied(current))
    }
}

fn handoff_state_tx(tx: &Transaction<'_>, session_id: &str) -> Result<Option<HandoffRow>> {
    tx.query_row(
        "SELECT session_id, controller_epoch, source_target, destination_target, phase, replay_cursor, terminal_detail, created_at, updated_at FROM handoff_sessions WHERE session_id = ?1",
        [session_id],
        row_to_handoff,
    )
    .optional()
    .map_err(Into::into)
}

fn append_transition(
    tx: &Transaction<'_>,
    row: &HandoffRow,
    replay_cursor: Option<u64>,
    detail: Option<&str>,
) -> Result<()> {
    let sequence: i64 = tx.query_row(
        "SELECT COALESCE(MAX(sequence), 0) + 1 FROM handoff_journal WHERE session_id = ?1",
        [&row.session_id],
        |result| result.get(0),
    )?;
    tx.execute(
        "INSERT INTO handoff_journal (session_id, controller_epoch, sequence, phase, replay_cursor, detail, recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![row.session_id, row.controller_epoch as i64, sequence, row.phase, replay_cursor.or(row.replay_cursor).map(|cursor| cursor as i64), detail.or(row.terminal_detail.as_deref()), row.updated_at],
    )?;
    Ok(())
}

fn row_to_handoff(row: &rusqlite::Row<'_>) -> rusqlite::Result<HandoffRow> {
    Ok(HandoffRow {
        session_id: row.get(0)?,
        controller_epoch: row.get::<_, i64>(1)? as u64,
        source_target: row.get(2)?,
        destination_target: row.get(3)?,
        phase: row.get(4)?,
        replay_cursor: row.get::<_, Option<i64>>(5)?.map(|value| value as u64),
        terminal_detail: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn row_to_handoff_journal(row: &rusqlite::Row<'_>) -> rusqlite::Result<HandoffJournalRow> {
    Ok(HandoffJournalRow {
        session_id: row.get(0)?,
        controller_epoch: row.get::<_, i64>(1)? as u64,
        sequence: row.get::<_, i64>(2)? as u64,
        phase: row.get(3)?,
        replay_cursor: row.get::<_, Option<i64>>(4)?.map(|value| value as u64),
        detail: row.get(5)?,
        recorded_at: row.get(6)?,
    })
}

fn is_terminal(phase: &str) -> bool {
    matches!(phase, "completed" | "cancelled" | "failed")
}
