//! Profile rows as sync carries them (ME-4): one row per entry or proposal, with a JSON body the other computer can apply.

use crate::entry::Entry;
use crate::profile::ProfileState;
use serde_json::{json, Value};

/// The schema number of a synced profile row's body.
pub const SYNC_SCHEMA: u32 = 1;

/// One profile row: `kind` is `entry` or `proposal`, `key` the entry id or the proposal id, `body` the JSON
/// `{"kind", "key", "entry"}`. Times are Unix milliseconds.
#[derive(Debug, Clone)]
pub struct SyncRow {
    pub kind: &'static str,
    pub key: String,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub body: Vec<u8>,
}

/// An entry as JSON (`null` if it cannot be encoded).
pub fn entry_json(entry: &Entry) -> Value {
    serde_json::to_value(entry).unwrap_or(Value::Null)
}

/// The body of one row.
pub fn sync_body(kind: &str, key: &str, entry: &Entry) -> Vec<u8> {
    json!({ "kind": kind, "key": key, "entry": entry_json(entry) })
        .to_string()
        .into_bytes()
}

/// Every entry, then every proposal, as rows.
pub fn rows(state: &ProfileState) -> Vec<SyncRow> {
    let mut rows = Vec::new();
    for (key, entry) in &state.entries {
        rows.push(SyncRow {
            kind: "entry",
            key: key.clone(),
            created_ms: entry.created_at,
            updated_ms: entry.updated_at,
            body: sync_body("entry", key, entry),
        });
    }
    for (key, entry) in &state.proposals {
        rows.push(SyncRow {
            kind: "proposal",
            key: key.clone(),
            created_ms: entry.created_at,
            updated_ms: entry.updated_at,
            body: sync_body("proposal", key, entry),
        });
    }
    rows
}
