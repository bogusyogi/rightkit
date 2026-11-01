//! Append-only mission log: what every peer did, in team-wide sequence order,
//! so a late-joining peer can reconstruct state and audits have a durable
//! trail.

use rightkit_db::rusqlite;
use rightkit_db::rusqlite::params;
use serde::{Deserialize, Serialize};

use rightkit_agent_engine_protocol::ids::uuid_v7;
use rightkit_agent_engine_protocol::time::now_iso;

use super::store::{TeamCoordinationError, TeamCoordinationStore};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionLogEntry {
    pub entry_id: String,
    pub team_id: String,
    pub seq: i64,
    pub peer_id: String,
    pub created_at: String,
    pub kind: String,
    pub detail: serde_json::Value,
}

impl TeamCoordinationStore {
    /// Append one entry to `team_id`'s mission log. `seq` is assigned as
    /// `max(existing seq) + 1` within the same connection, so entries stay in
    /// strict arrival order per team even under concurrent writers (SQLite
    /// serializes the read-then-insert under its writer lock).
    pub fn missionlog_append(
        &self,
        team_id: &str,
        peer_id: &str,
        kind: &str,
        detail: serde_json::Value,
    ) -> Result<MissionLogEntry, TeamCoordinationError> {
        let mut conn = self.connect()?;
        let tx = rightkit_db::write_txn(&mut conn)?;
        let next_seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM mission_log WHERE team_id = ?1",
            params![team_id],
            |row| row.get(0),
        )?;
        let entry = MissionLogEntry {
            entry_id: uuid_v7(),
            team_id: team_id.to_string(),
            seq: next_seq,
            peer_id: peer_id.to_string(),
            created_at: now_iso(),
            kind: kind.to_string(),
            detail,
        };
        tx.execute(
            "INSERT INTO mission_log (entry_id, team_id, seq, peer_id, created_at, kind, detail_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                entry.entry_id,
                entry.team_id,
                entry.seq,
                entry.peer_id,
                entry.created_at,
                entry.kind,
                serde_json::to_string(&entry.detail)?,
            ],
        )?;
        tx.commit()?;
        Ok(entry)
    }

    /// Read every mission-log entry for `team_id` in sequence order, starting
    /// strictly after `since_seq` (pass `0` for the full log). Late-joining
    /// peers use this to catch up.
    pub fn missionlog_since(
        &self,
        team_id: &str,
        since_seq: i64,
    ) -> Result<Vec<MissionLogEntry>, TeamCoordinationError> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT entry_id, team_id, seq, peer_id, created_at, kind, detail_json
             FROM mission_log WHERE team_id = ?1 AND seq > ?2 ORDER BY seq ASC",
        )?;
        let rows = stmt
            .query_map(params![team_id, since_seq], row_to_entry)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<MissionLogEntry> {
    let detail_str: String = row.get(6)?;
    Ok(MissionLogEntry {
        entry_id: row.get(0)?,
        team_id: row.get(1)?,
        seq: row.get(2)?,
        peer_id: row.get(3)?,
        created_at: row.get(4)?,
        kind: row.get(5)?,
        detail: serde_json::from_str(&detail_str).unwrap_or(serde_json::Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_store() -> TeamCoordinationStore {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missionlog.sqlite");
        let store = TeamCoordinationStore::open(&path).unwrap();
        std::mem::forget(dir);
        store
    }

    #[test]
    fn append_assigns_monotonic_per_team_sequence() {
        let store = open_store();
        let e1 = store
            .missionlog_append(
                "team-a",
                "peer-1",
                "claimed",
                serde_json::json!({"item": "x"}),
            )
            .unwrap();
        let e2 = store
            .missionlog_append("team-a", "peer-2", "done", serde_json::json!({"item": "x"}))
            .unwrap();
        assert_eq!(e1.seq, 1);
        assert_eq!(e2.seq, 2);
    }

    #[test]
    fn since_returns_only_entries_after_watermark() {
        let store = open_store();
        store
            .missionlog_append("team-a", "peer-1", "claimed", serde_json::json!(null))
            .unwrap();
        let e2 = store
            .missionlog_append("team-a", "peer-1", "done", serde_json::json!(null))
            .unwrap();
        let since = store.missionlog_since("team-a", 1).unwrap();
        assert_eq!(since.len(), 1);
        assert_eq!(since[0].entry_id, e2.entry_id);
    }

    #[test]
    fn sequences_are_independent_per_team() {
        let store = open_store();
        store
            .missionlog_append("team-a", "peer-1", "claimed", serde_json::json!(null))
            .unwrap();
        let team_b_first = store
            .missionlog_append("team-b", "peer-9", "claimed", serde_json::json!(null))
            .unwrap();
        assert_eq!(team_b_first.seq, 1);
    }
}
