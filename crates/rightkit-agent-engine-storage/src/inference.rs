//! Durable non-secret inference authority rows (schema v23).
use crate::{Result, Storage};
use rightkit_db::rusqlite::{params, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectInferenceConnectionRow {
    pub id: String,
    pub name: String,
    pub protocol: String,
    pub base_url: String,
    pub auth_kind: String,
    pub auth_header_name: Option<String>,
    pub public_headers_json: String,
    pub manual_models_json: String,
    pub trust_private_network: bool,
    pub credential_configured: bool,
    pub credential_revision: u64,
    pub configuration_revision: u64,
    pub created_at: String,
    pub updated_at: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferenceRouteAttestationRow {
    pub id: String,
    pub authority: String,
    pub connection_id: String,
    pub protocol: String,
    pub model_id: String,
    pub route_policy_id: Option<String>,
    pub selection_fingerprint: String,
    pub catalog_entry_hash: String,
    pub security_policy_hash: String,
    pub tool_schema_hash: String,
    pub configuration_revision: u64,
    pub credential_revision: u64,
    pub status: String,
    pub verified_at: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InferenceRuntimeBindingRow {
    pub id: String,
    pub session_id: String,
    pub agent_id: String,
    pub parent_agent_id: Option<String>,
    pub authority: String,
    pub connection_id: String,
    pub protocol: String,
    pub model_id: String,
    pub route_policy_id: Option<String>,
    pub selection_fingerprint: String,
    pub attestation_id: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

impl Storage {
    pub fn upsert_direct_inference_connection(
        &self,
        r: &DirectInferenceConnectionRow,
    ) -> Result<()> {
        self.conn.execute("INSERT INTO direct_inference_connections (id,name,protocol,base_url,auth_kind,auth_header_name,public_headers_json,manual_models_json,trust_private_network,credential_configured,credential_revision,configuration_revision,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14) ON CONFLICT(id) DO UPDATE SET name=excluded.name,protocol=excluded.protocol,base_url=excluded.base_url,auth_kind=excluded.auth_kind,auth_header_name=excluded.auth_header_name,public_headers_json=excluded.public_headers_json,manual_models_json=excluded.manual_models_json,trust_private_network=excluded.trust_private_network,credential_configured=excluded.credential_configured,credential_revision=excluded.credential_revision,configuration_revision=excluded.configuration_revision,updated_at=excluded.updated_at",params![r.id,r.name,r.protocol,r.base_url,r.auth_kind,r.auth_header_name,r.public_headers_json,r.manual_models_json,i64::from(r.trust_private_network),i64::from(r.credential_configured),r.credential_revision as i64,r.configuration_revision as i64,r.created_at,r.updated_at])?;
        Ok(())
    }
    pub fn direct_inference_connection(
        &self,
        id: &str,
    ) -> Result<Option<DirectInferenceConnectionRow>> {
        self.conn.query_row("SELECT id,name,protocol,base_url,auth_kind,auth_header_name,public_headers_json,manual_models_json,trust_private_network,credential_configured,credential_revision,configuration_revision,created_at,updated_at FROM direct_inference_connections WHERE id=?1",[id],|r|Ok(DirectInferenceConnectionRow{id:r.get(0)?,name:r.get(1)?,protocol:r.get(2)?,base_url:r.get(3)?,auth_kind:r.get(4)?,auth_header_name:r.get(5)?,public_headers_json:r.get(6)?,manual_models_json:r.get(7)?,trust_private_network:r.get::<_,i64>(8)?!=0,credential_configured:r.get::<_,i64>(9)?!=0,credential_revision:r.get::<_,i64>(10)? as u64,configuration_revision:r.get::<_,i64>(11)? as u64,created_at:r.get(12)?,updated_at:r.get(13)?})).optional().map_err(Into::into)
    }
    pub fn list_direct_inference_connections(&self) -> Result<Vec<DirectInferenceConnectionRow>> {
        let mut s=self.conn.prepare("SELECT id,name,protocol,base_url,auth_kind,auth_header_name,public_headers_json,manual_models_json,trust_private_network,credential_configured,credential_revision,configuration_revision,created_at,updated_at FROM direct_inference_connections ORDER BY name,id")?;
        let rows = s.query_map([], |r| {
            Ok(DirectInferenceConnectionRow {
                id: r.get(0)?,
                name: r.get(1)?,
                protocol: r.get(2)?,
                base_url: r.get(3)?,
                auth_kind: r.get(4)?,
                auth_header_name: r.get(5)?,
                public_headers_json: r.get(6)?,
                manual_models_json: r.get(7)?,
                trust_private_network: r.get::<_, i64>(8)? != 0,
                credential_configured: r.get::<_, i64>(9)? != 0,
                credential_revision: r.get::<_, i64>(10)? as u64,
                configuration_revision: r.get::<_, i64>(11)? as u64,
                created_at: r.get(12)?,
                updated_at: r.get(13)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::StorageError::from)
    }
    pub fn delete_direct_inference_connection(&self, id: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM direct_inference_connections WHERE id=?1", [id])?
            == 1)
    }

    pub fn upsert_inference_route_attestation(
        &self,
        row: &InferenceRouteAttestationRow,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO inference_route_attestations (id,authority,connection_id,protocol,model_id,route_policy_id,selection_fingerprint,catalog_entry_hash,security_policy_hash,tool_schema_hash,configuration_revision,credential_revision,status,verified_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14) ON CONFLICT(id) DO UPDATE SET selection_fingerprint=excluded.selection_fingerprint,catalog_entry_hash=excluded.catalog_entry_hash,security_policy_hash=excluded.security_policy_hash,tool_schema_hash=excluded.tool_schema_hash,configuration_revision=excluded.configuration_revision,credential_revision=excluded.credential_revision,status=excluded.status,verified_at=excluded.verified_at",
            params![row.id,row.authority,row.connection_id,row.protocol,row.model_id,row.route_policy_id,row.selection_fingerprint,row.catalog_entry_hash,row.security_policy_hash,row.tool_schema_hash,row.configuration_revision as i64,row.credential_revision as i64,row.status,row.verified_at],
        )?;
        Ok(())
    }

    pub fn upsert_inference_runtime_binding(&self, row: &InferenceRuntimeBindingRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO inference_runtime_bindings (id,session_id,agent_id,parent_agent_id,authority,connection_id,protocol,model_id,route_policy_id,selection_fingerprint,attestation_id,status,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14) ON CONFLICT(session_id,agent_id) DO UPDATE SET parent_agent_id=excluded.parent_agent_id,authority=excluded.authority,connection_id=excluded.connection_id,protocol=excluded.protocol,model_id=excluded.model_id,route_policy_id=excluded.route_policy_id,selection_fingerprint=excluded.selection_fingerprint,attestation_id=excluded.attestation_id,status=excluded.status,updated_at=excluded.updated_at",
            params![row.id,row.session_id,row.agent_id,row.parent_agent_id,row.authority,row.connection_id,row.protocol,row.model_id,row.route_policy_id,row.selection_fingerprint,row.attestation_id,row.status,row.created_at,row.updated_at],
        )?;
        Ok(())
    }

    pub fn inference_runtime_binding(
        &self,
        session_id: &str,
        agent_id: &str,
    ) -> Result<Option<InferenceRuntimeBindingRow>> {
        self.conn.query_row(
            "SELECT id,session_id,agent_id,parent_agent_id,authority,connection_id,protocol,model_id,route_policy_id,selection_fingerprint,attestation_id,status,created_at,updated_at FROM inference_runtime_bindings WHERE session_id=?1 AND agent_id=?2",
            params![session_id, agent_id],
            |r| Ok(InferenceRuntimeBindingRow { id:r.get(0)?,session_id:r.get(1)?,agent_id:r.get(2)?,parent_agent_id:r.get(3)?,authority:r.get(4)?,connection_id:r.get(5)?,protocol:r.get(6)?,model_id:r.get(7)?,route_policy_id:r.get(8)?,selection_fingerprint:r.get(9)?,attestation_id:r.get(10)?,status:r.get(11)?,created_at:r.get(12)?,updated_at:r.get(13)? }),
        ).optional().map_err(Into::into)
    }
}
