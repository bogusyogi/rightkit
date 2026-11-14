//! Coarse snapshot of an assistant reply that is still streaming.
//!
//! Token deltas are live projections and are never replay truth. This one row
//! per session (overwritten, never appended per delta) keeps the text streamed
//! so far, so a crash mid-reply does not lose it. The completed reply, or any
//! later settled fact, supersedes it. The table is created on first use and is
//! read only on resume; it is not part of the event stream any client reads.

use crate::error::Result;
use crate::Storage;
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::OptionalExtension;

pub(crate) const PARTIAL_REPLY_SQL: &str = "CREATE TABLE IF NOT EXISTS partial_assistant_replies (\
    session_id TEXT PRIMARY KEY, \
    anchor_seq INTEGER NOT NULL, \
    text       TEXT NOT NULL, \
    updated_at TEXT NOT NULL\
)";

/// A snapshot of the reply that was streaming when the process stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialReply {
    /// The session's durable high-water seq when the reply began streaming.
    /// Any settled fact with a later seq supersedes the snapshot.
    pub anchor_seq: i64,
    pub text: String,
    pub updated_at: String,
}

impl Storage {
    /// Replace the session's partial-reply snapshot.
    pub fn save_partial_reply(
        &mut self,
        session_id: &str,
        anchor_seq: i64,
        text: &str,
        updated_at: &str,
    ) -> Result<()> {
        self.conn.execute_batch(PARTIAL_REPLY_SQL)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO partial_assistant_replies(session_id, anchor_seq, text, updated_at) \
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, anchor_seq, text, updated_at],
        )?;
        Ok(())
    }

    /// Drop the session's snapshot (the reply settled). A no-op when none exists.
    pub fn clear_partial_reply(&mut self, session_id: &str) -> Result<()> {
        self.conn.execute_batch(PARTIAL_REPLY_SQL)?;
        self.conn.execute(
            "DELETE FROM partial_assistant_replies WHERE session_id = ?1",
            [session_id],
        )?;
        Ok(())
    }

    /// Read the session's snapshot, if one is left over. This only reads: a database another writer holds
    /// locked must not make a resume fail over a table that may not even exist, so the table is not created here
    /// (no table means no snapshot).
    pub fn load_partial_reply(&self, session_id: &str) -> Result<Option<PartialReply>> {
        let read = self
            .conn
            .query_row(
                "SELECT anchor_seq, text, updated_at FROM partial_assistant_replies \
                 WHERE session_id = ?1",
                [session_id],
                |row| {
                    Ok(PartialReply {
                        anchor_seq: row.get(0)?,
                        text: row.get(1)?,
                        updated_at: row.get(2)?,
                    })
                },
            )
            .optional();
        match read {
            Ok(found) => Ok(found),
            Err(error) if error.to_string().contains("no such table") => Ok(None),
            Err(error) => Err(error.into()),
        }
    }
}
