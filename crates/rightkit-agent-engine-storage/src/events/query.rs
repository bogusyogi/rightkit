use crate::error::Result;
use crate::events::{map_event, EventRow, SqliteOpTimer};
use crate::Storage;
use std::collections::HashSet;

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

    /// All events for a session, ordered by seq ascending.
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
