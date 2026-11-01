use crate::{Result, Storage};
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocationStoreRow {
    pub id: String,
    pub session_id: String,
    pub turn_id: String,
    pub tool_name: String,
    pub schema_version: String,
    pub args_hash: String,
    pub args_redacted_json: String,
    pub lifecycle_state: String,
    pub policy_verdict_id: Option<String>,
    pub approval_id: Option<String>,
    pub process_id: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub output_hash: Option<String>,
    pub artifact_ids_json: String,
    pub error_class: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocationEventStoreRow {
    pub invocation_id: String,
    pub session_id: String,
    pub seq: i64,
    pub kind: String,
    pub created_at: String,
    pub payload_redacted_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocationCancellationRow {
    pub invocation_id: String,
    pub session_id: String,
    pub reason: Option<String>,
    pub created_at: String,
}

impl Storage {
    pub fn upsert_tool_invocation(&self, row: &ToolInvocationStoreRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO tool_invocations \
             (id, session_id, turn_id, tool_name, schema_version, args_hash, args_redacted_json, \
              lifecycle_state, policy_verdict_id, approval_id, process_id, started_at, completed_at, \
              output_hash, artifact_ids_json, error_class, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                row.id,
                row.session_id,
                row.turn_id,
                row.tool_name,
                row.schema_version,
                row.args_hash,
                row.args_redacted_json,
                row.lifecycle_state,
                row.policy_verdict_id,
                row.approval_id,
                row.process_id,
                row.started_at,
                row.completed_at,
                row.output_hash,
                row.artifact_ids_json,
                row.error_class,
                row.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_tool_invocation(&self, id: &str) -> Result<Option<ToolInvocationStoreRow>> {
        self.conn
            .query_row(
                "SELECT id, session_id, turn_id, tool_name, schema_version, args_hash, args_redacted_json, \
                        lifecycle_state, policy_verdict_id, approval_id, process_id, started_at, completed_at, \
                        output_hash, artifact_ids_json, error_class, updated_at \
                 FROM tool_invocations WHERE id = ?1",
                [id],
                Self::map_tool_invocation,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_tool_invocations_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<ToolInvocationStoreRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, session_id, turn_id, tool_name, schema_version, args_hash, args_redacted_json, \
                    lifecycle_state, policy_verdict_id, approval_id, process_id, started_at, completed_at, \
                    output_hash, artifact_ids_json, error_class, updated_at \
             FROM tool_invocations WHERE session_id = ?1 ORDER BY updated_at ASC, id ASC",
        )?;
        let rows = stmt.query_map([session_id], Self::map_tool_invocation)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn append_tool_invocation_event(&self, row: &ToolInvocationEventStoreRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO tool_invocation_events \
             (invocation_id, session_id, seq, kind, created_at, payload_redacted_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                row.invocation_id,
                row.session_id,
                row.seq,
                row.kind,
                row.created_at,
                row.payload_redacted_json,
            ],
        )?;
        Ok(())
    }

    pub fn list_tool_invocation_events(
        &self,
        invocation_id: &str,
    ) -> Result<Vec<ToolInvocationEventStoreRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT invocation_id, session_id, seq, kind, created_at, payload_redacted_json \
             FROM tool_invocation_events WHERE invocation_id = ?1 ORDER BY seq ASC, kind ASC",
        )?;
        let rows = stmt.query_map([invocation_id], Self::map_tool_invocation_event)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn upsert_tool_invocation_cancellation(
        &self,
        row: &ToolInvocationCancellationRow,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO tool_invocation_cancellations \
             (invocation_id, session_id, reason, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                row.invocation_id,
                row.session_id,
                row.reason,
                row.created_at
            ],
        )?;
        Ok(())
    }

    pub fn get_tool_invocation_cancellation(
        &self,
        invocation_id: &str,
    ) -> Result<Option<ToolInvocationCancellationRow>> {
        self.conn
            .query_row(
                "SELECT invocation_id, session_id, reason, created_at \
                 FROM tool_invocation_cancellations WHERE invocation_id = ?1",
                [invocation_id],
                Self::map_tool_invocation_cancellation,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_tool_invocation_cancellations_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<ToolInvocationCancellationRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT invocation_id, session_id, reason, created_at \
             FROM tool_invocation_cancellations WHERE session_id = ?1 ORDER BY created_at ASC, invocation_id ASC",
        )?;
        let rows = stmt.query_map([session_id], Self::map_tool_invocation_cancellation)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn map_tool_invocation(row: &rusqlite::Row<'_>) -> rusqlite::Result<ToolInvocationStoreRow> {
        Ok(ToolInvocationStoreRow {
            id: row.get(0)?,
            session_id: row.get(1)?,
            turn_id: row.get(2)?,
            tool_name: row.get(3)?,
            schema_version: row.get(4)?,
            args_hash: row.get(5)?,
            args_redacted_json: row.get(6)?,
            lifecycle_state: row.get(7)?,
            policy_verdict_id: row.get(8)?,
            approval_id: row.get(9)?,
            process_id: row.get(10)?,
            started_at: row.get(11)?,
            completed_at: row.get(12)?,
            output_hash: row.get(13)?,
            artifact_ids_json: row.get(14)?,
            error_class: row.get(15)?,
            updated_at: row.get(16)?,
        })
    }

    fn map_tool_invocation_event(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<ToolInvocationEventStoreRow> {
        Ok(ToolInvocationEventStoreRow {
            invocation_id: row.get(0)?,
            session_id: row.get(1)?,
            seq: row.get(2)?,
            kind: row.get(3)?,
            created_at: row.get(4)?,
            payload_redacted_json: row.get(5)?,
        })
    }

    fn map_tool_invocation_cancellation(
        row: &rusqlite::Row<'_>,
    ) -> rusqlite::Result<ToolInvocationCancellationRow> {
        Ok(ToolInvocationCancellationRow {
            invocation_id: row.get(0)?,
            session_id: row.get(1)?,
            reason: row.get(2)?,
            created_at: row.get(3)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation(id: &str, state: &str, updated_at: &str) -> ToolInvocationStoreRow {
        ToolInvocationStoreRow {
            id: id.to_string(),
            session_id: "session-1".to_string(),
            turn_id: "turn-1".to_string(),
            tool_name: "shell_command".to_string(),
            schema_version: "schema-1".to_string(),
            args_hash: "hash-args".to_string(),
            args_redacted_json: r#"{"command":"<redacted>"}"#.to_string(),
            lifecycle_state: state.to_string(),
            policy_verdict_id: Some("policy-1".to_string()),
            approval_id: Some(id.to_string()),
            process_id: None,
            started_at: Some("unix:1".to_string()),
            completed_at: if state == "completed" {
                Some("unix:3".to_string())
            } else {
                None
            },
            output_hash: if state == "completed" {
                Some("hash-output".to_string())
            } else {
                None
            },
            artifact_ids_json: r#"["artifact-1"]"#.to_string(),
            error_class: None,
            updated_at: updated_at.to_string(),
        }
    }

    #[test]
    fn tool_invocation_rows_round_trip_and_overwrite() {
        let store = Storage::open_in_memory().unwrap();
        store
            .upsert_tool_invocation(&invocation("toolu-1", "requested", "unix:1"))
            .unwrap();
        store
            .upsert_tool_invocation(&invocation("toolu-1", "completed", "unix:3"))
            .unwrap();

        let row = store.get_tool_invocation("toolu-1").unwrap().unwrap();
        assert_eq!(row.lifecycle_state, "completed");
        assert_eq!(row.completed_at.as_deref(), Some("unix:3"));
        assert_eq!(row.output_hash.as_deref(), Some("hash-output"));

        let rows = store
            .list_tool_invocations_for_session("session-1")
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "toolu-1");
    }

    #[test]
    fn tool_invocation_events_and_cancellations_survive_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("tool-invocations.sqlite");

        {
            let store = Storage::open(&path).unwrap();
            store
                .upsert_tool_invocation(&invocation("toolu-1", "cancelled", "unix:2"))
                .unwrap();
            store
                .append_tool_invocation_event(&ToolInvocationEventStoreRow {
                    invocation_id: "toolu-1".to_string(),
                    session_id: "session-1".to_string(),
                    seq: 1,
                    kind: "toolCallStarted".to_string(),
                    created_at: "unix:1".to_string(),
                    payload_redacted_json: r#"{"toolCallId":"toolu-1"}"#.to_string(),
                })
                .unwrap();
            store
                .append_tool_invocation_event(&ToolInvocationEventStoreRow {
                    invocation_id: "toolu-1".to_string(),
                    session_id: "session-1".to_string(),
                    seq: 2,
                    kind: "cancelRequested".to_string(),
                    created_at: "unix:2".to_string(),
                    payload_redacted_json: r#"{"reason":"operator"}"#.to_string(),
                })
                .unwrap();
            store
                .upsert_tool_invocation_cancellation(&ToolInvocationCancellationRow {
                    invocation_id: "toolu-1".to_string(),
                    session_id: "session-1".to_string(),
                    reason: Some("operator".to_string()),
                    created_at: "unix:2".to_string(),
                })
                .unwrap();
        }

        let reopened = Storage::open(&path).unwrap();
        let row = reopened.get_tool_invocation("toolu-1").unwrap().unwrap();
        assert_eq!(row.lifecycle_state, "cancelled");

        let events = reopened.list_tool_invocation_events("toolu-1").unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].kind, "cancelRequested");

        let cancellation = reopened
            .get_tool_invocation_cancellation("toolu-1")
            .unwrap()
            .unwrap();
        assert_eq!(cancellation.reason.as_deref(), Some("operator"));

        let cancellations = reopened
            .list_tool_invocation_cancellations_for_session("session-1")
            .unwrap();
        assert_eq!(cancellations.len(), 1);
    }
}
