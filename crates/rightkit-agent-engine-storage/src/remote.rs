use crate::{Result, Storage};
use rightkit_db::rusqlite;
use rightkit_db::rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteChannelRow {
    pub id: String,
    pub kind: String,
    pub profile_id: String,
    pub enabled: bool,
    pub configured: bool,
    pub toolset: String,
    pub delivery_policy: String,
    pub trust: String,
    pub state: String,
    pub last_success_at: Option<String>,
    pub last_error_at: Option<String>,
    pub last_error: Option<String>,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBindingRow {
    pub id: String,
    pub channel_id: String,
    pub target_key: String,
    pub session_id: Option<String>,
    pub target_label: Option<String>,
    pub target_redacted: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteDeliveryRow {
    pub id: String,
    pub channel_id: String,
    pub binding_id: Option<String>,
    pub target_redacted: String,
    pub status: String,
    pub delivery_kind: String,
    pub payload_json: String,
    pub error: Option<String>,
    pub created_at: String,
}

impl Storage {
    pub fn upsert_remote_channel(&self, row: &RemoteChannelRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO remote_channels \
             (id, kind, profile_id, enabled, configured, toolset, delivery_policy, trust, state, \
              last_success_at, last_error_at, last_error, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                row.id,
                row.kind,
                row.profile_id,
                if row.enabled { 1_i64 } else { 0_i64 },
                if row.configured { 1_i64 } else { 0_i64 },
                row.toolset,
                row.delivery_policy,
                row.trust,
                row.state,
                row.last_success_at,
                row.last_error_at,
                row.last_error,
                row.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_remote_channel(&self, id: &str) -> Result<Option<RemoteChannelRow>> {
        self.conn
            .query_row(
                "SELECT id, kind, profile_id, enabled, configured, toolset, delivery_policy, trust, \
                        state, last_success_at, last_error_at, last_error, updated_at \
                 FROM remote_channels WHERE id = ?1",
                [id],
                Self::map_remote_channel,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_remote_channels(&self) -> Result<Vec<RemoteChannelRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, profile_id, enabled, configured, toolset, delivery_policy, trust, \
                    state, last_success_at, last_error_at, last_error, updated_at \
             FROM remote_channels ORDER BY id ASC",
        )?;
        let rows = stmt.query_map([], Self::map_remote_channel)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn upsert_remote_binding(&self, row: &RemoteBindingRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO remote_bindings \
             (id, channel_id, target_key, session_id, target_label, target_redacted, status, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                row.id,
                row.channel_id,
                row.target_key,
                row.session_id,
                row.target_label,
                row.target_redacted,
                row.status,
                row.created_at,
                row.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn get_remote_binding(&self, id: &str) -> Result<Option<RemoteBindingRow>> {
        self.conn
            .query_row(
                "SELECT id, channel_id, target_key, session_id, target_label, target_redacted, \
                        status, created_at, updated_at \
                 FROM remote_bindings WHERE id = ?1",
                [id],
                Self::map_remote_binding,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_remote_bindings_for_channel(
        &self,
        channel_id: &str,
    ) -> Result<Vec<RemoteBindingRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, channel_id, target_key, session_id, target_label, target_redacted, \
                    status, created_at, updated_at \
             FROM remote_bindings WHERE channel_id = ?1 ORDER BY updated_at ASC, id ASC",
        )?;
        let rows = stmt.query_map([channel_id], Self::map_remote_binding)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn list_remote_bindings_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<RemoteBindingRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, channel_id, target_key, session_id, target_label, target_redacted, \
                    status, created_at, updated_at \
             FROM remote_bindings WHERE session_id = ?1 ORDER BY updated_at ASC, id ASC",
        )?;
        let rows = stmt.query_map([session_id], Self::map_remote_binding)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn record_remote_delivery(&self, row: &RemoteDeliveryRow) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO remote_deliveries \
             (id, channel_id, binding_id, target_redacted, status, delivery_kind, payload_json, error, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                row.id,
                row.channel_id,
                row.binding_id,
                row.target_redacted,
                row.status,
                row.delivery_kind,
                row.payload_json,
                row.error,
                row.created_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_remote_deliveries_for_channel(
        &self,
        channel_id: &str,
        limit: u32,
    ) -> Result<Vec<RemoteDeliveryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, channel_id, binding_id, target_redacted, status, delivery_kind, payload_json, error, created_at \
             FROM remote_deliveries WHERE channel_id = ?1 ORDER BY created_at DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![channel_id, limit.max(1) as i64],
            Self::map_remote_delivery,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn list_remote_deliveries_by_status(
        &self,
        status: &str,
        limit: u32,
    ) -> Result<Vec<RemoteDeliveryRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, channel_id, binding_id, target_redacted, status, delivery_kind, payload_json, error, created_at \
             FROM remote_deliveries WHERE status = ?1 ORDER BY created_at DESC, id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![status, limit.max(1) as i64],
            Self::map_remote_delivery,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn map_remote_channel(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteChannelRow> {
        Ok(RemoteChannelRow {
            id: row.get(0)?,
            kind: row.get(1)?,
            profile_id: row.get(2)?,
            enabled: row.get::<_, i64>(3)? != 0,
            configured: row.get::<_, i64>(4)? != 0,
            toolset: row.get(5)?,
            delivery_policy: row.get(6)?,
            trust: row.get(7)?,
            state: row.get(8)?,
            last_success_at: row.get(9)?,
            last_error_at: row.get(10)?,
            last_error: row.get(11)?,
            updated_at: row.get(12)?,
        })
    }

    fn map_remote_binding(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteBindingRow> {
        Ok(RemoteBindingRow {
            id: row.get(0)?,
            channel_id: row.get(1)?,
            target_key: row.get(2)?,
            session_id: row.get(3)?,
            target_label: row.get(4)?,
            target_redacted: row.get(5)?,
            status: row.get(6)?,
            created_at: row.get(7)?,
            updated_at: row.get(8)?,
        })
    }

    fn map_remote_delivery(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteDeliveryRow> {
        Ok(RemoteDeliveryRow {
            id: row.get(0)?,
            channel_id: row.get(1)?,
            binding_id: row.get(2)?,
            target_redacted: row.get(3)?,
            status: row.get(4)?,
            delivery_kind: row.get(5)?,
            payload_json: row.get(6)?,
            error: row.get(7)?,
            created_at: row.get(8)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel_row(id: &str, updated_at: &str) -> RemoteChannelRow {
        RemoteChannelRow {
            id: id.to_string(),
            kind: "chat".to_string(),
            profile_id: "default".to_string(),
            enabled: true,
            configured: true,
            toolset: "telegram-remote".to_string(),
            delivery_policy: "approval-required".to_string(),
            trust: "remoteAgentUntrusted".to_string(),
            state: "ready".to_string(),
            last_success_at: Some("unix:2".to_string()),
            last_error_at: None,
            last_error: None,
            updated_at: updated_at.to_string(),
        }
    }

    fn binding_row(id: &str, target_key: &str, session_id: &str) -> RemoteBindingRow {
        RemoteBindingRow {
            id: id.to_string(),
            channel_id: "telegram".to_string(),
            target_key: target_key.to_string(),
            session_id: Some(session_id.to_string()),
            target_label: Some("primary chat".to_string()),
            target_redacted: Some("telegram-chat:<redacted>".to_string()),
            status: "active".to_string(),
            created_at: "unix:1".to_string(),
            updated_at: "unix:2".to_string(),
        }
    }

    fn delivery_row(id: &str, status: &str, created_at: &str) -> RemoteDeliveryRow {
        RemoteDeliveryRow {
            id: id.to_string(),
            channel_id: "webhook".to_string(),
            binding_id: Some("binding-1".to_string()),
            target_redacted: "https://example.test/hook".to_string(),
            status: status.to_string(),
            delivery_kind: "schedule".to_string(),
            payload_json: r#"{"kind":"schedule","content":"done"}"#.to_string(),
            error: None,
            created_at: created_at.to_string(),
        }
    }

    #[test]
    fn remote_channels_round_trip_and_overwrite() {
        let store = Storage::open_in_memory().unwrap();
        store
            .upsert_remote_channel(&channel_row("telegram", "unix:1"))
            .unwrap();

        let mut replacement = channel_row("telegram", "unix:3");
        replacement.state = "degraded".to_string();
        replacement.last_error = Some("poll failed".to_string());
        store.upsert_remote_channel(&replacement).unwrap();

        let row = store.get_remote_channel("telegram").unwrap().unwrap();
        assert_eq!(row.state, "degraded");
        assert_eq!(row.last_error.as_deref(), Some("poll failed"));
        assert_eq!(store.list_remote_channels().unwrap(), vec![replacement]);
    }

    #[test]
    fn remote_bindings_round_trip_by_channel_and_session() {
        let store = Storage::open_in_memory().unwrap();
        store
            .upsert_remote_binding(&binding_row("binding-1", "opaque-chat-key", "session-1"))
            .unwrap();
        store
            .upsert_remote_binding(&binding_row("binding-2", "other-chat-key", "session-2"))
            .unwrap();

        let row = store.get_remote_binding("binding-1").unwrap().unwrap();
        assert_eq!(row.target_key, "opaque-chat-key");
        assert_eq!(
            row.target_redacted.as_deref(),
            Some("telegram-chat:<redacted>")
        );

        let channel_rows = store.list_remote_bindings_for_channel("telegram").unwrap();
        assert_eq!(channel_rows.len(), 2);

        let session_rows = store.list_remote_bindings_for_session("session-1").unwrap();
        assert_eq!(session_rows.len(), 1);
        assert_eq!(session_rows[0].id, "binding-1");
    }

    #[test]
    fn remote_deliveries_round_trip_by_channel_and_status() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("remote-deliveries.sqlite");

        {
            let store = Storage::open(&path).unwrap();
            store
                .record_remote_delivery(&delivery_row("delivery-1", "delivered", "unix:1"))
                .unwrap();
            store
                .record_remote_delivery(&delivery_row("delivery-2", "failed", "unix:2"))
                .unwrap();
        }

        let reopened = Storage::open(&path).unwrap();
        let channel_rows = reopened
            .list_remote_deliveries_for_channel("webhook", 10)
            .unwrap();
        assert_eq!(channel_rows.len(), 2);
        assert_eq!(channel_rows[0].id, "delivery-2");
        assert_eq!(channel_rows[0].target_redacted, "https://example.test/hook");

        let failed = reopened
            .list_remote_deliveries_by_status("failed", 10)
            .unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].id, "delivery-2");
    }
}
