use crate::error::Result;
use crate::Storage;
use rightkit_agent_engine_protocol::TaskNode;

impl Storage {
    /// Insert or replace a task node for a session.
    pub fn upsert_task(&self, session_id: &str, node: &TaskNode) -> Result<()> {
        let node_json = serde_json::to_string(node)?;
        self.conn.execute(
            "INSERT OR REPLACE INTO tasks (session_id, id, node_json) \
             VALUES (?1, ?2, ?3)",
            (session_id, &node.id, &node_json),
        )?;
        Ok(())
    }

    /// All task nodes for a session, ordered by task id.
    pub fn get_tasks(&self, session_id: &str) -> Result<Vec<TaskNode>> {
        let mut stmt = self
            .conn
            .prepare("SELECT node_json FROM tasks WHERE session_id = ?1 ORDER BY id ASC")?;
        let rows = stmt.query_map([session_id], |row| {
            let json: String = row.get(0)?;
            Ok(json)
        })?;
        let mut out = Vec::new();
        for r in rows {
            let json = r?;
            let node: TaskNode = serde_json::from_str(&json)?;
            out.push(node);
        }
        Ok(out)
    }
}
