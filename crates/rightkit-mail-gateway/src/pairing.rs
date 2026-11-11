//! Pairing behind the Settings "Agents" section: the core's pairing
//! (`rightkit_agent_gateway::pairing`) at this adapter's [`Profile`] levels, in its words. v1
//! pairs one local engine; new agents are always read-only.

use super::outcome::Profile;
use rightkit_agent_gateway::pairing as core;
use rightkit_db::rusqlite::Connection;

pub use core::{audit_page, AgentView, AuditView, KeyStore, DEFAULT_PAGE, MAX_PAGE};

pub fn list(conn: &Connection) -> Result<Vec<AgentView>, String> {
    core::list::<Profile>(conn)
}

/// Create a read-only agent for `accounts`, store its key, and revoke any earlier active agent.
pub fn pair(
    conn: &Connection,
    keys: &dyn KeyStore,
    name: &str,
    accounts: &[String],
) -> Result<AgentView, String> {
    if accounts.is_empty() {
        return Err("Choose at least one account for the agent.".into());
    }
    core::pair::<Profile>(conn, keys, name, accounts)
}

/// Revoke at once; delete the stored key if the agent was the active one.
pub fn revoke_agent(conn: &Connection, keys: &dyn KeyStore, id: &str) -> Result<(), String> {
    core::revoke_agent::<Profile>(conn, keys, id)
}

/// Set an active agent's profile from Settings: `read_only`, `draft` or `send_with_approval`
/// (also with hyphens); anything else is refused.
pub fn set_profile(conn: &Connection, id: &str, profile: &str) -> Result<AgentView, String> {
    let profile = match profile {
        "read_only" | "read-only" => Profile::ReadOnly,
        "draft" => Profile::Draft,
        "send_with_approval" | "send-with-approval" => Profile::SendWithApproval,
        _ => return Err("Choose read-only, draft or send with approval.".into()),
    };
    core::set_profile(conn, id, profile)
}
