//! Shared task board: peers claim, progress, and complete work items durably.

use rightkit_db::rusqlite;
use rightkit_db::rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};

use rightkit_agent_engine_protocol::ids::uuid_v7;
use rightkit_agent_engine_protocol::time::now_iso;

use super::store::{TeamCoordinationError, TeamCoordinationStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskBoardStatus {
    Open,
    Claimed,
    InProgress,
    Done,
    Abandoned,
}

impl TaskBoardStatus {
    fn as_str(self) -> &'static str {
        match self {
            TaskBoardStatus::Open => "open",
            TaskBoardStatus::Claimed => "claimed",
            TaskBoardStatus::InProgress => "in_progress",
            TaskBoardStatus::Done => "done",
            TaskBoardStatus::Abandoned => "abandoned",
        }
    }

    fn from_str(s: &str) -> Result<Self, TeamCoordinationError> {
        Ok(match s {
            "open" => TaskBoardStatus::Open,
            "claimed" => TaskBoardStatus::Claimed,
            "in_progress" => TaskBoardStatus::InProgress,
            "done" => TaskBoardStatus::Done,
            "abandoned" => TaskBoardStatus::Abandoned,
            other => {
                return Err(TeamCoordinationError::Invalid(format!(
                    "unknown task board status: {other}"
                )))
            }
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskBoardItem {
    pub item_id: String,
    pub team_id: String,
    pub title: String,
    pub status: TaskBoardStatus,
    pub claimed_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// Free-form payload (description, acceptance shape, links) carried
    /// opaquely — mirrors the donor's untyped task body.
    pub payload: serde_json::Value,
}

impl TeamCoordinationStore {
    /// Post a new open item to `team_id`'s task board.
    pub fn board_post(
        &self,
        team_id: &str,
        title: &str,
        payload: serde_json::Value,
    ) -> Result<TaskBoardItem, TeamCoordinationError> {
        let conn = self.connect()?;
        let now = now_iso();
        let item = TaskBoardItem {
            item_id: uuid_v7(),
            team_id: team_id.to_string(),
            title: title.to_string(),
            status: TaskBoardStatus::Open,
            claimed_by: None,
            created_at: now.clone(),
            updated_at: now,
            payload,
        };
        conn.execute(
            "INSERT INTO task_board (item_id, team_id, title, status, claimed_by, created_at, updated_at, payload_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                item.item_id,
                item.team_id,
                item.title,
                item.status.as_str(),
                item.claimed_by,
                item.created_at,
                item.updated_at,
                serde_json::to_string(&item.payload)?,
            ],
        )?;
        Ok(item)
    }

    /// Atomically claim the oldest `open` item for `team_id`, or `None` if
    /// the board is empty of unclaimed work. `UPDATE ... WHERE status='open'`
    /// is race-safe under SQLite's serialized writers, so two peers racing
    /// to claim never both succeed on the same item.
    pub fn board_claim_next(
        &self,
        team_id: &str,
        peer_id: &str,
    ) -> Result<Option<TaskBoardItem>, TeamCoordinationError> {
        let conn = self.connect()?;
        let candidate: Option<String> = conn
            .query_row(
                "SELECT item_id FROM task_board WHERE team_id = ?1 AND status = 'open' ORDER BY created_at ASC LIMIT 1",
                params![team_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(item_id) = candidate else {
            return Ok(None);
        };
        let now = now_iso();
        let updated = conn.execute(
            "UPDATE task_board SET status = 'claimed', claimed_by = ?1, updated_at = ?2
             WHERE item_id = ?3 AND status = 'open'",
            params![peer_id, now, item_id],
        )?;
        if updated == 0 {
            // Lost the race to another peer between SELECT and UPDATE.
            return Ok(None);
        }
        self.board_get(team_id, &item_id)
    }

    pub fn board_set_status(
        &self,
        team_id: &str,
        item_id: &str,
        status: TaskBoardStatus,
    ) -> Result<(), TeamCoordinationError> {
        let conn = self.connect()?;
        let now = now_iso();
        conn.execute(
            "UPDATE task_board SET status = ?1, updated_at = ?2 WHERE team_id = ?3 AND item_id = ?4",
            params![status.as_str(), now, team_id, item_id],
        )?;
        Ok(())
    }

    pub fn board_get(
        &self,
        team_id: &str,
        item_id: &str,
    ) -> Result<Option<TaskBoardItem>, TeamCoordinationError> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT item_id, team_id, title, status, claimed_by, created_at, updated_at, payload_json
             FROM task_board WHERE team_id = ?1 AND item_id = ?2",
            params![team_id, item_id],
            row_to_item,
        )
        .optional()
        .map_err(TeamCoordinationError::from)
    }

    pub fn board_list(&self, team_id: &str) -> Result<Vec<TaskBoardItem>, TeamCoordinationError> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT item_id, team_id, title, status, claimed_by, created_at, updated_at, payload_json
             FROM task_board WHERE team_id = ?1 ORDER BY created_at ASC",
        )?;
        let rows = stmt
            .query_map(params![team_id], row_to_item)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn row_to_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskBoardItem> {
    let status_str: String = row.get(3)?;
    let payload_str: String = row.get(7)?;
    Ok(TaskBoardItem {
        item_id: row.get(0)?,
        team_id: row.get(1)?,
        title: row.get(2)?,
        status: TaskBoardStatus::from_str(&status_str).unwrap_or(TaskBoardStatus::Open),
        claimed_by: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        payload: serde_json::from_str(&payload_str).unwrap_or(serde_json::Value::Null),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_store() -> TeamCoordinationStore {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("board.sqlite");
        let store = TeamCoordinationStore::open(&path).unwrap();
        std::mem::forget(dir); // keep temp dir alive for test duration
        store
    }

    #[test]
    fn post_then_claim_transitions_open_to_claimed() {
        let store = open_store();
        let item = store
            .board_post("team-a", "do the thing", serde_json::json!({"n": 1}))
            .unwrap();
        assert_eq!(item.status, TaskBoardStatus::Open);

        let claimed = store.board_claim_next("team-a", "peer-1").unwrap().unwrap();
        assert_eq!(claimed.item_id, item.item_id);
        assert_eq!(claimed.status, TaskBoardStatus::Claimed);
        assert_eq!(claimed.claimed_by.as_deref(), Some("peer-1"));

        // Second claim attempt finds nothing open left.
        assert!(store
            .board_claim_next("team-a", "peer-2")
            .unwrap()
            .is_none());
    }

    #[test]
    fn set_status_and_list_round_trip() {
        let store = open_store();
        let item = store
            .board_post("team-a", "ship it", serde_json::json!(null))
            .unwrap();
        store
            .board_set_status("team-a", &item.item_id, TaskBoardStatus::Done)
            .unwrap();
        let listed = store.board_list("team-a").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].status, TaskBoardStatus::Done);
    }

    #[test]
    fn board_is_scoped_per_team() {
        let store = open_store();
        store
            .board_post("team-a", "a-item", serde_json::json!(null))
            .unwrap();
        store
            .board_post("team-b", "b-item", serde_json::json!(null))
            .unwrap();
        assert_eq!(store.board_list("team-a").unwrap().len(), 1);
        assert_eq!(store.board_list("team-b").unwrap().len(), 1);
    }
}
