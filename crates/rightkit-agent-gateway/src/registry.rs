//! Agent registry: pairing keys (stored only as SHA-256 hashes), profiles and
//! allowed accounts. All functions take a connection, so the caller picks the
//! gated writer or reader. Nothing here returns a key hash.

use super::outcome::Profile;
use rightkit_db::rusqlite::{params, Connection, OptionalExtension};
use rightkit_fs::digest::sha256_bytes;
use rightkit_search::values::now_ms;

/// An agent as the gate sees it. No key, no hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    pub id: String,
    pub name: String,
    pub profile: Profile,
    pub created_at: i64,
    pub revoked_at: Option<i64>,
    pub accounts: Vec<String>,
}

/// Result of presenting a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verified {
    Agent(Agent),
    /// Key matched an agent that has been revoked.
    Revoked {
        id: String,
        name: String,
    },
    Unknown,
}

pub fn hash_key(key: &str) -> String {
    sha256_bytes(key.as_bytes())
}

fn random_bytes<const N: usize>() -> Result<[u8; N], String> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| format!("random: {e}"))?;
    Ok(b)
}

/// Create an agent with profile `read_only` and no accounts. Returns the new
/// id and the pairing key. The key is shown once and never stored.
pub fn create_agent(conn: &Connection, name: &str) -> Result<(String, String), String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err("agent name must be 1 to 80 characters".into());
    }
    if name.chars().any(super::draft::invisible_or_control) {
        return Err("agent name must not contain control or invisible characters".into());
    }
    let key = rightkit_crypto::base64url_encode(&random_bytes::<32>()?);
    let id = rightkit_crypto::base64url_encode(&random_bytes::<12>()?);
    conn.execute(
        "INSERT INTO agents(id, name, key_hash, profile, created_at) VALUES(?1, ?2, ?3, 'read_only', ?4)",
        params![id, name, hash_key(&key), now_ms()],
    )
    .map_err(|e| format!("create agent: {e}"))?;
    Ok((id, key))
}

fn accounts_of(conn: &Connection, agent_id: &str) -> Result<Vec<String>, String> {
    let mut st = conn
        .prepare("SELECT account_id FROM agent_accounts WHERE agent_id = ?1 ORDER BY account_id")
        .map_err(|e| format!("agent accounts: {e}"))?;
    let rows = st
        .query_map([agent_id], |r| r.get::<_, String>(0))
        .map_err(|e| format!("agent accounts: {e}"))?;
    rows.map(|r| r.map_err(|e| format!("agent accounts: {e}")))
        .collect()
}

type Row = (String, String, String, String, i64, Option<i64>);

fn read_rows(conn: &Connection, filter_id: Option<&str>) -> Result<Vec<Row>, String> {
    let mut st = conn
        .prepare(
            "SELECT id, name, key_hash, profile, created_at, revoked_at FROM agents
             WHERE (?1 IS NULL OR id = ?1) ORDER BY created_at, rowid",
        )
        .map_err(|e| format!("agents: {e}"))?;
    let rows = st
        .query_map([filter_id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .map_err(|e| format!("agents: {e}"))?;
    rows.map(|r| r.map_err(|e| format!("agents: {e}")))
        .collect()
}

fn to_agent(conn: &Connection, row: Row) -> Result<Agent, String> {
    let (id, name, _hash, profile, created_at, revoked_at) = row;
    let profile = Profile::parse(&profile).ok_or("unknown profile in registry")?;
    let accounts = accounts_of(conn, &id)?;
    Ok(Agent {
        id,
        name,
        profile,
        created_at,
        revoked_at,
        accounts,
    })
}

/// All agents, oldest first. Never includes a key or hash.
pub fn list_agents(conn: &Connection) -> Result<Vec<Agent>, String> {
    read_rows(conn, None)?
        .into_iter()
        .map(|r| to_agent(conn, r))
        .collect()
}

pub fn get_agent(conn: &Connection, id: &str) -> Result<Option<Agent>, String> {
    match read_rows(conn, Some(id))?.into_iter().next() {
        Some(r) => Ok(Some(to_agent(conn, r)?)),
        None => Ok(None),
    }
}

/// Only the Settings section calls this; no request path does.
pub fn set_profile(conn: &Connection, id: &str, profile: Profile) -> Result<(), String> {
    let n = conn
        .execute(
            "UPDATE agents SET profile = ?2 WHERE id = ?1",
            params![id, profile.as_str()],
        )
        .map_err(|e| format!("set profile: {e}"))?;
    if n == 0 {
        return Err("no such agent".into());
    }
    Ok(())
}

/// Replace the agent's allowed accounts. Every account must exist.
pub fn set_accounts(conn: &Connection, id: &str, accounts: &[String]) -> Result<(), String> {
    let tx = conn
        .unchecked_transaction()
        .map_err(|e| format!("set accounts: {e}"))?;
    let exists: Option<i64> = tx
        .query_row("SELECT 1 FROM agents WHERE id = ?1", [id], |r| r.get(0))
        .optional()
        .map_err(|e| format!("set accounts: {e}"))?;
    if exists.is_none() {
        return Err("no such agent".into());
    }
    tx.execute("DELETE FROM agent_accounts WHERE agent_id = ?1", [id])
        .map_err(|e| format!("set accounts: {e}"))?;
    for a in accounts {
        tx.execute(
            "INSERT OR IGNORE INTO agent_accounts(agent_id, account_id) VALUES(?1, ?2)",
            params![id, a],
        )
        .map_err(|e| format!("set accounts: {e}"))?;
    }
    tx.commit().map_err(|e| format!("set accounts: {e}"))
}

/// Revoke: the key stops working at once. Idempotent; keeps the first time.
pub fn revoke(conn: &Connection, id: &str) -> Result<(), String> {
    let n = conn
        .execute(
            "UPDATE agents SET revoked_at = COALESCE(revoked_at, ?2) WHERE id = ?1",
            params![id, now_ms()],
        )
        .map_err(|e| format!("revoke: {e}"))?;
    if n == 0 {
        return Err("no such agent".into());
    }
    Ok(())
}

/// Check a presented key against every stored hash in constant time per hash.
/// The loop never exits early on a match, so timing does not reveal which row.
pub fn verify_key(conn: &Connection, key: &str) -> Result<Verified, String> {
    let presented = hash_key(key);
    let mut hit: Option<Row> = None;
    for row in read_rows(conn, None)? {
        if rightkit_crypto::ct_eq(presented.as_bytes(), row.2.as_bytes()) {
            hit = Some(row);
        }
    }
    match hit {
        None => Ok(Verified::Unknown),
        Some(row) if row.5.is_some() => Ok(Verified::Revoked {
            id: row.0,
            name: row.1,
        }),
        Some(row) => Ok(Verified::Agent(to_agent(conn, row)?)),
    }
}
