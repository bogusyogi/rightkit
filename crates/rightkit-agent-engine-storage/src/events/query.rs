use crate::error::Result;
use crate::events::{map_event, EventRow, SqliteOpTimer};
use crate::Storage;
use std::collections::HashSet;

/// The end of a session's conversation, as stored. See [`Storage::session_tail`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionTail {
    /// `userMessageSubmitted`, `sessionTurnFinished`, `runInterrupted` or `runFailed`; `None`
    /// for a session with no turn.
    pub last_event_kind: Option<String>,
    /// When that event was recorded.
    pub last_event_at: Option<String>,
    /// `completed`, `cancelled` or `failed`, when the last event is a finished turn.
    pub last_turn_status: Option<String>,
    /// The answer that ended the last finished turn.
    pub last_answer: Option<String>,
}

impl Storage {
    /// Session ids that contain a real user submission or a compaction marker
    /// proving earlier chat existed. Session setup, blueprint markers, and
    /// other synthetic assistant events alone do not qualify a row for Recents.
    pub fn session_ids_with_chat_history(&self) -> Result<HashSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT session_id FROM events \
             WHERE kind IN ('userMessageSubmitted', 'sessionCompacted')",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = HashSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out)
    }

    /// Text of a session's first user message (earliest `userMessageSubmitted`
    /// event), or `None` when the session has no user turn. Reads one row.
    pub fn first_user_message_text(&self, session_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT payload_json FROM events \
             WHERE session_id = ?1 AND kind = 'userMessageSubmitted' \
             ORDER BY seq ASC LIMIT 1",
        )?;
        let mut rows = stmt.query_map([session_id], |row| row.get::<_, String>(0))?;
        let Some(payload) = rows.next().transpose()? else {
            return Ok(None);
        };
        let text = serde_json::from_str::<serde_json::Value>(&payload)
            .ok()
            .and_then(|value| {
                value
                    .get("text")
                    .and_then(|t| t.as_str())
                    .map(str::to_owned)
            });
        Ok(text)
    }

    /// How a session's conversation last ended, for the session list's state mark. Reads at
    /// most two rows: the last event that starts or ends a turn, and, when that is a finished
    /// turn, the answer that ended it.
    pub fn session_tail(&self, session_id: &str) -> Result<SessionTail> {
        let mut tail = SessionTail::default();
        let last: Option<(String, String, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT kind, payload_json, seq FROM events \
                 WHERE session_id = ?1 AND kind IN \
                 ('userMessageSubmitted', 'sessionTurnFinished', 'runInterrupted', 'runFailed') \
                 ORDER BY seq DESC LIMIT 1",
            )?;
            let mut rows = stmt.query_map([session_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })?;
            rows.next().transpose()?
        };
        let Some((kind, payload, seq)) = last else {
            return Ok(tail);
        };
        let payload = serde_json::from_str::<serde_json::Value>(&payload).unwrap_or_default();
        let text_of = |key: &str| payload.get(key).and_then(|v| v.as_str()).map(str::to_owned);
        tail.last_event_at = text_of("createdAt");
        if kind == "sessionTurnFinished" {
            tail.last_turn_status = text_of("status");
            let mut stmt = self.conn.prepare(
                "SELECT payload_json FROM events \
                 WHERE session_id = ?1 AND kind = 'assistantMessageCompleted' AND seq < ?2 \
                 ORDER BY seq DESC LIMIT 1",
            )?;
            let mut rows = stmt
                .query_map(rightkit_db::rusqlite::params![session_id, seq], |row| {
                    row.get::<_, String>(0)
                })?;
            if let Some(answer) = rows.next().transpose()? {
                tail.last_answer = serde_json::from_str::<serde_json::Value>(&answer)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("message")
                            .and_then(|m| m.get("content"))
                            .and_then(|c| c.as_str())
                            .map(str::to_owned)
                    });
            }
        }
        tail.last_event_kind = Some(kind);
        Ok(tail)
    }

    /// All events for a session, ordered by seq ascending. Oversized payloads
    /// are returned whole: the spill stub is a storage-internal representation,
    /// so fork, export and summaries see the real event. The paged readers
    /// (`events_page_after`, `events_before`, `events_in_range`) keep the stub
    /// on purpose.
    pub fn all_events(&self, session_id: &str) -> Result<Vec<EventRow>> {
        let _op_timer = SqliteOpTimer::start(super::SqliteOpKind::ReadEvents);
        let mut stmt = self.conn.prepare(
            "SELECT session_id, seq, kind, payload_json, created_at \
             FROM events WHERE session_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map([session_id], map_event)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        self.restore_spilled_payloads(session_id, &mut out)?;
        Ok(out)
    }

    /// The highest seq for a session, or 0 if there are no events.
    pub fn latest_seq(&self, session_id: &str) -> Result<i64> {
        let seq: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )?;
        Ok(seq)
    }
}
