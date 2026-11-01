//! Durable producer cursor for authenticated H4 Analyze windows.
use crate::error::Result;
use crate::Storage;
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::OptionalExtension;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdaptObservationCursor {
    pub last_input_sequence: i64,
    pub last_source_seq: i64,
    pub provider: Option<String>,
    pub model: Option<String>,
}

impl Storage {
    pub fn adapt_observation_cursor(
        &self,
        session_id: &str,
        task_id: &str,
    ) -> Result<Option<AdaptObservationCursor>> {
        Ok(self.conn.query_row(
            "SELECT last_input_sequence, last_source_seq, provider, model FROM adapt_observation_cursors WHERE session_id = ?1 AND task_id = ?2",
            rusqlite::params![session_id, task_id],
            |row| Ok(AdaptObservationCursor { last_input_sequence: row.get(0)?, last_source_seq: row.get(1)?, provider: row.get(2)?, model: row.get(3)? }),
        ).optional()?)
    }

    pub fn adapt_observation_advance(
        &self,
        session_id: &str,
        task_id: &str,
        expected: AdaptObservationCursor,
        next: AdaptObservationCursor,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "INSERT INTO adapt_observation_cursors (session_id, task_id, last_input_sequence, last_source_seq, provider, model, updated_at) VALUES (?1, ?2, ?5, ?6, ?7, ?8, ?9) ON CONFLICT(session_id, task_id) DO UPDATE SET last_input_sequence = excluded.last_input_sequence, last_source_seq = excluded.last_source_seq, provider = excluded.provider, model = excluded.model, updated_at = excluded.updated_at WHERE adapt_observation_cursors.last_input_sequence = ?3 AND adapt_observation_cursors.last_source_seq = ?4",
            rusqlite::params![session_id, task_id, expected.last_input_sequence, expected.last_source_seq, next.last_input_sequence, next.last_source_seq, next.provider, next.model, crate::transcript_export::now_iso8601()],
        )?;
        Ok(changed == 1)
    }

    pub fn delete_adapt_observation_cursors(&self, session_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM adapt_observation_cursors WHERE session_id = ?1",
            [session_id],
        )?;
        Ok(())
    }
}
