//! Pairing and audit-view logic behind an app's agents settings. Pure over a connection and an
//! injected [`KeyStore`], so tests never touch the OS credential store. The pairing key goes
//! from the registry's generator to the key store and nowhere else: it is not returned, logged
//! or written to a file. Pairing again replaces the stored key and revokes the earlier active
//! agent (one local engine per app). New agents start at [`Level::LOWEST`].

use crate::audit;
use crate::gate::Level;
use crate::registry::{self, Agent};
use rightkit_db::rusqlite::Connection;
use serde::Serialize;

/// Where the engine's pairing key lives. The production implementation writes
/// the OS credential store item the engine reads.
pub trait KeyStore {
    fn store(&self, key: &str) -> Result<(), String>;
    /// Idempotent: deleting an absent key succeeds.
    fn delete(&self) -> Result<(), String>;
}

pub const DEFAULT_PAGE: usize = 50;
pub const MAX_PAGE: usize = 200;

/// An agent as a settings page sees it. No key, no hash.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AgentView {
    pub id: String,
    pub name: String,
    pub profile: String,
    pub scopes: Vec<String>,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

impl<P: Level> From<Agent<P>> for AgentView {
    fn from(a: Agent<P>) -> Self {
        AgentView {
            id: a.id,
            name: a.name,
            profile: a.profile.as_str().to_string(),
            scopes: a.scopes,
            created_at: a.created_at,
            revoked_at: a.revoked_at,
        }
    }
}

/// One audit row for a settings table.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AuditView {
    pub id: i64,
    pub at: i64,
    pub agent_name: Option<String>,
    pub scope: Option<String>,
    pub method: String,
    pub outcome: String,
    pub detail: Option<String>,
}

pub fn list<P: Level>(conn: &Connection) -> Result<Vec<AgentView>, String> {
    Ok(registry::list_agents::<P>(conn)?
        .into_iter()
        .map(AgentView::from)
        .collect())
}

/// Create an agent at the lowest level for `scopes`, store its key, and revoke any earlier
/// active agent. If the key cannot be stored, the new row is revoked and the earlier agent and
/// key are left untouched.
pub fn pair<P: Level>(
    conn: &Connection,
    keys: &dyn KeyStore,
    name: &str,
    scopes: &[String],
) -> Result<AgentView, String> {
    if scopes.is_empty() {
        return Err("Choose at least one scope for the agent.".into());
    }
    let previous: Vec<String> = registry::list_agents::<P>(conn)?
        .into_iter()
        .filter(|a| a.revoked_at.is_none())
        .map(|a| a.id)
        .collect();
    let (id, key) = registry::create_agent::<P>(conn, name)?;
    if let Err(e) = registry::set_scopes(conn, &id, scopes) {
        let _ = registry::revoke(conn, &id);
        return Err(e);
    }
    if keys.store(&key).is_err() {
        let _ = registry::revoke(conn, &id);
        return Err("Could not save the pairing key to this computer's credential store.".into());
    }
    drop(key);
    for old in previous {
        registry::revoke(conn, &old)?;
    }
    registry::get_agent::<P>(conn, &id)?
        .map(AgentView::from)
        .ok_or_else(|| "agent missing after pairing".to_string())
}

/// Revoke at once; then, if the agent was active (the engine's current agent),
/// delete the stored key.
pub fn revoke_agent<P: Level>(
    conn: &Connection,
    keys: &dyn KeyStore,
    id: &str,
) -> Result<(), String> {
    let agent = registry::get_agent::<P>(conn, id)?.ok_or("No such agent.")?;
    registry::revoke(conn, id)?;
    if agent.revoked_at.is_none() {
        keys.delete().map_err(|_| {
            "The agent is revoked, but its key could not be removed from the credential store."
                .to_string()
        })?;
    }
    Ok(())
}

/// Set an active agent's level from the app's settings. Pairing itself always creates an agent
/// at the lowest level. Level changes are not audited: the audit log is written by the gate for
/// agent requests only.
pub fn set_profile<P: Level>(conn: &Connection, id: &str, profile: P) -> Result<AgentView, String> {
    let agent = registry::get_agent::<P>(conn, id)?.ok_or("No such agent.")?;
    if agent.revoked_at.is_some() {
        return Err("A revoked agent cannot be changed.".into());
    }
    registry::set_profile(conn, id, profile)?;
    registry::get_agent::<P>(conn, id)?
        .map(AgentView::from)
        .ok_or_else(|| "agent missing after update".to_string())
}

/// Newest first. `limit` defaults to 50 and is capped at 200; `before_id`
/// returns only rows older than that id.
pub fn audit_page(
    conn: &Connection,
    limit: Option<usize>,
    before_id: Option<i64>,
) -> Result<Vec<AuditView>, String> {
    let limit = limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
    Ok(audit::page(conn, limit, before_id)?
        .into_iter()
        .map(|r| AuditView {
            id: r.seq,
            at: r.at,
            agent_name: r.agent_name,
            scope: r.scope,
            method: r.method,
            outcome: r.outcome,
            detail: r.detail,
        })
        .collect())
}
