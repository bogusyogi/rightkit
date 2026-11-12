//! Pairing and audit-view logic behind an app's agents settings. Pure over a connection and an
//! injected [`KeyStore`], so tests never touch the OS credential store. The pairing key goes
//! from the registry's generator to the key store and nowhere else: it is not returned, logged
//! or written to a file. Pairing again replaces the stored key and revokes the earlier active
//! agent (one local engine per app). New agents start at [`Level::LOWEST`].

use crate::audit;
use crate::gate::Level;
use crate::registry::{self, Agent};
use rightkit_db::rusqlite::Connection;
use serde::{Deserialize, Serialize, Serializer};

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
///
/// Wire compatibility for one release: 0.3 renamed `accounts` to `scopes`. The JSON form carries
/// both names with the same value and input accepts either, so a settings UI or reader written for
/// 0.2 keeps working; a value with both (this release's own output) reads back, `scopes` winning.
/// The `accounts` copy goes in the next breaking release.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(try_from = "AgentViewIn")]
pub struct AgentView {
    pub id: String,
    pub name: String,
    pub profile: String,
    pub scopes: Vec<String>,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
}

/// [`AgentView`]'s input form: `scopes`, its 0.2 name `accounts` (`#[serde(alias)]` alone would
/// refuse a value carrying both as a duplicate field), or both.
#[derive(Deserialize)]
struct AgentViewIn {
    id: String,
    name: String,
    profile: String,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default)]
    accounts: Option<Vec<String>>,
    created_at: i64,
    revoked_at: Option<i64>,
}

impl TryFrom<AgentViewIn> for AgentView {
    type Error = &'static str;

    fn try_from(v: AgentViewIn) -> Result<Self, Self::Error> {
        Ok(AgentView {
            id: v.id,
            name: v.name,
            profile: v.profile,
            scopes: v.scopes.or(v.accounts).ok_or("missing field `scopes`")?,
            created_at: v.created_at,
            revoked_at: v.revoked_at,
        })
    }
}

impl Serialize for AgentView {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            id: &'a str,
            name: &'a str,
            profile: &'a str,
            scopes: &'a [String],
            /// The 0.2 name of `scopes`, kept for one release.
            accounts: &'a [String],
            created_at: i64,
            revoked_at: Option<i64>,
        }
        Wire {
            id: &self.id,
            name: &self.name,
            profile: &self.profile,
            scopes: &self.scopes,
            accounts: &self.scopes,
            created_at: self.created_at,
            revoked_at: self.revoked_at,
        }
        .serialize(serializer)
    }
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
///
/// Wire compatibility for one release: 0.3 renamed `account_id` to `scope`. The JSON form carries
/// both names with the same value and input accepts either or both, `scope` winning. The
/// `account_id` copy goes in the next breaking release.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(from = "AuditViewIn")]
pub struct AuditView {
    pub id: i64,
    pub at: i64,
    pub agent_name: Option<String>,
    pub scope: Option<String>,
    pub method: String,
    pub outcome: String,
    pub detail: Option<String>,
}

/// [`AuditView`]'s input form: `scope`, its 0.2 name `account_id`, or both.
#[derive(Deserialize)]
struct AuditViewIn {
    id: i64,
    at: i64,
    agent_name: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
    method: String,
    outcome: String,
    detail: Option<String>,
}

impl From<AuditViewIn> for AuditView {
    fn from(v: AuditViewIn) -> Self {
        AuditView {
            id: v.id,
            at: v.at,
            agent_name: v.agent_name,
            scope: v.scope.or(v.account_id),
            method: v.method,
            outcome: v.outcome,
            detail: v.detail,
        }
    }
}

impl Serialize for AuditView {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Wire<'a> {
            id: i64,
            at: i64,
            agent_name: Option<&'a str>,
            scope: Option<&'a str>,
            /// The 0.2 name of `scope`, kept for one release.
            account_id: Option<&'a str>,
            method: &'a str,
            outcome: &'a str,
            detail: Option<&'a str>,
        }
        Wire {
            id: self.id,
            at: self.at,
            agent_name: self.agent_name.as_deref(),
            scope: self.scope.as_deref(),
            account_id: self.scope.as_deref(),
            method: &self.method,
            outcome: &self.outcome,
            detail: self.detail.as_deref(),
        }
        .serialize(serializer)
    }
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
