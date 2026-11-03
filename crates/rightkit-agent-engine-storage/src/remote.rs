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
