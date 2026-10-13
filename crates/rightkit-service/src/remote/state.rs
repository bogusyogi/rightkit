//! Durable state. `state.json` holds devices, enrolment codes (hashed), replay
//! nonces, jobs, idempotency records and webhook dedupe/inbox; each upload
//! session lives in its own `uploads/<id>.json` so chunk acknowledgements do
//! not rewrite everything.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub devices: BTreeMap<String, Device>,
    /// sha256(code) hex -> code record. Codes are never stored in clear.
    pub enrol_codes: BTreeMap<String, EnrolCode>,
    /// device id -> nonce -> accepted-at seconds.
    pub nonces: BTreeMap<String, BTreeMap<String, u64>>,
    pub jobs: BTreeMap<String, Job>,
    /// "device:key" -> record.
    pub idempotency: BTreeMap<String, IdemRecord>,
    /// provider + signature digest -> accepted-at seconds.
    pub webhook_seen: BTreeMap<String, u64>,
    pub webhook_inbox: Vec<InboxItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub public_key: String,
    pub label: Option<String>,
    pub scopes: Vec<String>,
    pub enrolled_at: u64,
    pub revoked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrolCode {
    pub scopes: Vec<String>,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub revision: u64,
    pub fields: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdemRecord {
    pub request_hash: String,
    pub status: u16,
    pub body: Value,
    pub at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxItem {
    pub id: String,
    pub provider: String,
    pub received_at: u64,
    pub body_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Upload {
    pub id: String,
    pub device_id: String,
    pub size: u64,
    pub chunk_bytes: u64,
    pub chunk_count: u64,
    /// Per-session blake3 key, hex. Returned once to the owning device at init.
    pub key_hex: String,
    pub created_at: u64,
    pub expires_at: u64,
    /// chunk index -> keyed blake3 of that chunk, hex.
    pub received: BTreeMap<u64, String>,
    pub complete: Option<UploadDone>,
    /// Set while the final hash runs outside the state lock; not persisted.
    #[serde(skip)]
    pub finalizing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadDone {
    pub digest: String,
    pub completed_at: u64,
}
