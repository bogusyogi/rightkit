//! Point-to-point and broadcast messaging between peers in a team, delivered
//! at-least-once and consumed once per recipient.

use rightkit_db::rusqlite;
use rightkit_db::rusqlite::params;
use serde::{Deserialize, Serialize};

use rightkit_agent_engine_protocol::ids::uuid_v7;
use rightkit_agent_engine_protocol::time::now_iso;

use super::store::{TeamCoordinationError, TeamCoordinationStore};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxMessage {
    pub message_id: String,
    pub team_id: String,
    pub from_peer: String,
    /// `None` means broadcast to every peer on the team.
    pub to_peer: Option<String>,
    pub created_at: String,
    pub consumed_at: Option<String>,
    pub body: serde_json::Value,
}

impl TeamCoordinationStore {
    /// Send a message. `to_peer = None` broadcasts to the whole team.
    pub fn mailbox_send(
        &self,
        team_id: &str,
        from_peer: &str,
        to_peer: Option<&str>,
        body: serde_json::Value,
    ) -> Result<MailboxMessage, TeamCoordinationError> {
        let conn = self.connect()?;
        let msg = MailboxMessage {
            message_id: uuid_v7(),
            team_id: team_id.to_string(),
            from_peer: from_peer.to_string(),
            to_peer: to_peer.map(|s| s.to_string()),
            created_at: now_iso(),
            consumed_at: None,
            body,
        };
        conn.execute(
            "INSERT INTO mailbox (message_id, team_id, from_peer, to_peer, created_at, consumed_at, body_json)
             VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6)",
            params![
                msg.message_id,
                msg.team_id,
                msg.from_peer,
                msg.to_peer,
                msg.created_at,
                serde_json::to_string(&msg.body)?,
            ],
        )?;
        Ok(msg)
    }

    /// Fetch every unconsumed message addressed to `peer_id` (directly or via
    /// broadcast) without marking it consumed. Use [`Self::mailbox_ack`] once
    /// the recipient has durably processed a message.
    pub fn mailbox_peek(
        &self,
        team_id: &str,
        peer_id: &str,
    ) -> Result<Vec<MailboxMessage>, TeamCoordinationError> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT message_id, team_id, from_peer, to_peer, created_at, consumed_at, body_json
             FROM mailbox
             WHERE team_id = ?1 AND consumed_at IS NULL AND (to_peer = ?2 OR to_peer IS NULL)
               AND NOT EXISTS (
                   SELECT 1 FROM mailbox_consumptions c
                   WHERE c.message_id = mailbox.message_id AND c.peer_id = ?2
               )
             ORDER BY created_at ASC",
        )?;
        let rows = stmt
            .query_map(params![team_id, peer_id], row_to_message)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Legacy global acknowledgement for callers that intentionally retire a
    /// message for every recipient. Peer runtime/API paths use
    /// [`Self::mailbox_ack_for_peer`] so broadcasts remain independently
    /// consumable by each recipient.
    pub fn mailbox_ack(
        &self,
        team_id: &str,
        message_id: &str,
    ) -> Result<(), TeamCoordinationError> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE mailbox SET consumed_at = ?1 WHERE team_id = ?2 AND message_id = ?3",
            params![now_iso(), team_id, message_id],
        )?;
        Ok(())
    }

    /// Mark one message consumed by one recipient. Broadcast messages remain
    /// visible to every other peer until that peer acknowledges independently.
    pub fn mailbox_ack_for_peer(
        &self,
        team_id: &str,
        message_id: &str,
        peer_id: &str,
    ) -> Result<(), TeamCoordinationError> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT OR IGNORE INTO mailbox_consumptions (message_id, peer_id, consumed_at)
             SELECT message_id, ?1, ?2 FROM mailbox
             WHERE team_id = ?3 AND message_id = ?4",
            params![peer_id, now_iso(), team_id, message_id],
        )?;
        Ok(())
    }
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<MailboxMessage> {
    let body_str: String = row.get(6)?;
    Ok(MailboxMessage {
        message_id: row.get(0)?,
        team_id: row.get(1)?,
        from_peer: row.get(2)?,
        to_peer: row.get(3)?,
        created_at: row.get(4)?,
        consumed_at: row.get(5)?,
        body: serde_json::from_str(&body_str).unwrap_or(serde_json::Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_store() -> TeamCoordinationStore {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mailbox.sqlite");
        let store = TeamCoordinationStore::open(&path).unwrap();
        std::mem::forget(dir);
        store
    }

    #[test]
    fn direct_message_delivered_only_to_recipient() {
        let store = open_store();
        store
            .mailbox_send(
                "team-a",
                "peer-1",
                Some("peer-2"),
                serde_json::json!({"hi": true}),
            )
            .unwrap();
        assert_eq!(store.mailbox_peek("team-a", "peer-2").unwrap().len(), 1);
        assert_eq!(store.mailbox_peek("team-a", "peer-3").unwrap().len(), 0);
    }

    #[test]
    fn broadcast_message_visible_to_every_peer() {
        let store = open_store();
        store
            .mailbox_send("team-a", "peer-1", None, serde_json::json!({"all": true}))
            .unwrap();
        assert_eq!(store.mailbox_peek("team-a", "peer-2").unwrap().len(), 1);
        assert_eq!(store.mailbox_peek("team-a", "peer-3").unwrap().len(), 1);
    }

    #[test]
    fn ack_removes_message_from_peek() {
        let store = open_store();
        let msg = store
            .mailbox_send("team-a", "peer-1", Some("peer-2"), serde_json::json!(null))
            .unwrap();
        store.mailbox_ack("team-a", &msg.message_id).unwrap();
        assert_eq!(store.mailbox_peek("team-a", "peer-2").unwrap().len(), 0);
    }
}
