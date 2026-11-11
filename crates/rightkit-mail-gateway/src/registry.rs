//! The core agent registry (`rightkit_agent_gateway::registry`) at this adapter's [`Profile`]
//! levels; an agent's scopes are the mail accounts it may use.

use super::outcome::Profile;
use rightkit_agent_gateway::registry as core;
use rightkit_db::rusqlite::Connection;

pub use core::{hash_key, invisible_or_control, revoke, set_scopes as set_accounts};

/// An agent as the gate sees it; `scopes` are its allowed account ids.
pub type Agent = core::Agent<Profile>;
/// Result of presenting a key.
pub type Verified = core::Verified<Profile>;

/// Create a read-only agent with no accounts: its id and its pairing key, shown once.
pub fn create_agent(conn: &Connection, name: &str) -> Result<(String, String), String> {
    core::create_agent::<Profile>(conn, name)
}

pub fn list_agents(conn: &Connection) -> Result<Vec<Agent>, String> {
    core::list_agents(conn)
}

pub fn get_agent(conn: &Connection, id: &str) -> Result<Option<Agent>, String> {
    core::get_agent(conn, id)
}

pub fn set_profile(conn: &Connection, id: &str, profile: Profile) -> Result<(), String> {
    core::set_profile(conn, id, profile)
}

pub fn verify_key(conn: &Connection, key: &str) -> Result<Verified, String> {
    core::verify_key(conn, key)
}
