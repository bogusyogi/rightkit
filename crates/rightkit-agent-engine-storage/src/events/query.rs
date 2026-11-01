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

#[cfg(test)]
mod tests {
    use crate::Storage;

    #[test]
    fn session_ids_with_chat_history_includes_user_message_and_compaction_kinds() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        storage
            .append_event("s-chat", "userMessageSubmitted", "{}")
            .expect("append");
        storage
            .append_event("s-compacted", "sessionCompacted", "{}")
            .expect("append");
        storage
            .append_event("s-synthetic", "sessionSetup", "{}")
            .expect("append");
        storage
            .append_event("s-synthetic", "blueprintMarker", "{}")
            .expect("append");

        let qualifying = storage
            .session_ids_with_chat_history()
            .expect("query succeeds");
        assert!(qualifying.contains("s-chat"));
        assert!(qualifying.contains("s-compacted"));
        assert!(
            !qualifying.contains("s-synthetic"),
            "synthetic-only session must not qualify for Recents"
        );
        assert_eq!(qualifying.len(), 2);
    }

    #[test]
    fn session_ids_with_chat_history_is_empty_for_no_events() {
        let storage = Storage::open_in_memory().expect("in-memory storage");
        let qualifying = storage
            .session_ids_with_chat_history()
            .expect("query succeeds");
        assert!(qualifying.is_empty());
    }

    #[test]
    fn all_events_returns_rows_ordered_by_seq_ascending() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        for i in 1..=5 {
            storage
                .append_event("s-order", "kind", &format!(r#"{{"i":{i}}}"#))
                .expect("append");
        }
        let events = storage.all_events("s-order").expect("query succeeds");
        let seqs: Vec<i64> = events.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn all_events_only_returns_the_requested_session() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        storage.append_event("s-a", "kind", "{}").expect("append");
        storage.append_event("s-b", "kind", "{}").expect("append");
        storage.append_event("s-b", "kind", "{}").expect("append");

        let a_events = storage.all_events("s-a").expect("query succeeds");
        let b_events = storage.all_events("s-b").expect("query succeeds");
        assert_eq!(a_events.len(), 1);
        assert_eq!(b_events.len(), 2);
        assert!(a_events.iter().all(|e| e.session_id == "s-a"));
    }

    #[test]
    fn all_events_for_unknown_session_is_empty() {
        let storage = Storage::open_in_memory().expect("in-memory storage");
        let events = storage
            .all_events("does-not-exist")
            .expect("query succeeds");
        assert!(events.is_empty());
    }

    #[test]
    fn latest_seq_is_zero_when_session_has_no_events() {
        let storage = Storage::open_in_memory().expect("in-memory storage");
        assert_eq!(storage.latest_seq("nothing-here").unwrap(), 0);
    }

    #[test]
    fn latest_seq_tracks_the_highest_appended_seq() {
        let mut storage = Storage::open_in_memory().expect("in-memory storage");
        for _ in 0..7 {
            storage.append_event("s-max", "kind", "{}").expect("append");
        }
        assert_eq!(storage.latest_seq("s-max").unwrap(), 7);
    }
}
