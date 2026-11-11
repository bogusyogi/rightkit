//! Governed Cortex proposal intake, independently signed review and bounded
//! recovery. The event-store proposal table remains the single pending queue;
//! canonical memory effects use the existing admission transaction.
//!
//! A wire caller's reviewer/authority strings never confer review permission.
//! Trust is loaded from an installation-owned file, never a request path.

use crate::store::{MemoryEventContext, MemoryLifecycleInputV1};
use crate::{
    digest::{digest_bytes, digest_str},
    MemoryStore,
};
use rightkit_adapt::multiwriter::{
    proposal_emission as multiwriter_proposal_emission,
    validate_proposal_emission as validate_multiwriter_emission, WriterRequestV1, MULTIWRITER_KIND,
};
use rightkit_db::rusqlite::{self, params, Connection, OptionalExtension, TransactionBehavior};
use rightkit_memory_core::MemoryTier;
use rightkit_memory_store::temporal::{
    validate_fact_proposal, TemporalInstantV1, TemporalValidityReceiptV1, TemporalValidityV1,
};
use rightkit_memory_store::TemporalFact;
use ring::signature::{UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::PathBuf,
};

pub const MAX_PAYLOAD_BYTES: usize = 65_536;
pub const REVIEW_POLICY: &str = "cortex-reviewed-effect-v1";
const SIGNING_DOMAIN: &[u8] = b"Membrane Cortex reviewed effect v1\0";
const MAX_REVIEW_LIFETIME_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct LifecycleError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}
type Result<T> = std::result::Result<T, LifecycleError>;
fn fail(code: &'static str, message: impl Into<String>) -> LifecycleError {
    LifecycleError {
        code,
        message: message.into(),
        retryable: false,
    }
}
fn storage(error: impl std::fmt::Display) -> LifecycleError {
    LifecycleError {
        code: "cortex_storage_unavailable",
        message: error.to_string(),
        retryable: true,
    }
}

/// Installation-owned public keys. Enrollment is deliberately NOT an MCP
/// operation. Protect this file like the existing installation grant registry.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewerTrustV1 {
    pub schema_version: u32,
    pub installation_id: String,
    pub cortex_store_id: String,
    pub reviewers: Vec<ReviewerKeyV1>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewerKeyV1 {
    pub key_id: String,
    pub public_key_hex: String,
    pub repository_id: String,
    pub scope_id: String,
    pub allowed_operations: Vec<String>,
    pub revoked: bool,
}

/// The complete effect is signed, including target version, exact caller
/// binding, store, expiry and policy. An approval is not a transferable role.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewedEffectV1 {
    pub schema_version: u32,
    pub policy_version: String,
    pub installation_id: String,
    pub cortex_store_id: String,
    pub repository_id: String,
    pub scope_id: String,
    pub operation: String,
    pub target_id: String,
    pub expected_content_hash: String,
    /// Suppression CAS token. The first decision requires "none"; subsequent
    /// decisions name the last decisionHash. Unused for immutable proposals.
    pub expected_control_revision: Option<String>,
    pub key_id: String,
    pub nonce: String,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub signature_hex: String,
}
impl ReviewedEffectV1 {
    /// Sign these exact domain-separated bytes. The empty signature field is
    /// included, preventing an ambiguous alternate JSON canonicalization.
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        let mut unsigned = self.clone();
        unsigned.signature_hex.clear();
        let mut bytes = SIGNING_DOMAIN.to_vec();
        bytes.extend(serde_json::to_vec(&unsigned).map_err(storage)?);
        Ok(bytes)
    }
}

pub fn trust_path(store: &MemoryStore) -> Result<PathBuf> {
    store
        .db()
        .event_db_path()
        .map(|p| p.with_file_name("cortex-review-trust.v1.json"))
        .ok_or_else(|| {
            fail(
                "cortex_review_unavailable",
                "installation-backed review trust is unavailable",
            )
        })
}
fn load_trust(store: &MemoryStore) -> Result<ReviewerTrustV1> {
    let path = trust_path(store)?;
    let meta = std::fs::symlink_metadata(&path).map_err(|_| {
        fail(
            "cortex_review_unavailable",
            "review trust has not been enrolled",
        )
    })?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_PAYLOAD_BYTES as u64 {
        return Err(fail(
            "cortex_review_unavailable",
            "invalid installation review trust file",
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(storage)?
        .take((MAX_PAYLOAD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(storage)?;
    if bytes.len() > MAX_PAYLOAD_BYTES {
        return Err(fail(
            "cortex_review_unavailable",
            "review trust exceeds limit",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| fail("cortex_review_unavailable", "malformed review trust"))
}
fn verify_review(
    store: &MemoryStore,
    review: &ReviewedEffectV1,
    repository: &str,
    scope: &str,
) -> Result<()> {
    verify_with_trust(
        store,
        review,
        repository,
        scope,
        &load_trust(store)?,
        crate::time::now_millis() as u64,
    )
}
fn verify_with_trust(
    store: &MemoryStore,
    review: &ReviewedEffectV1,
    repository: &str,
    scope: &str,
    trust: &ReviewerTrustV1,
    now: u64,
) -> Result<()> {
    if review.schema_version != 1
        || review.policy_version != REVIEW_POLICY
        || trust.schema_version != 1
        || trust.cortex_store_id != store.cortex_store_id()
        || review.installation_id != trust.installation_id
        || review.cortex_store_id != trust.cortex_store_id
        || review.repository_id != repository
        || review.scope_id != scope
    {
        return Err(fail(
            "cortex_review_binding_denied",
            "review does not match policy, installation, store or caller",
        ));
    }
    if !matches!(
        review.operation.as_str(),
        "approve" | "reject" | "retry" | "suppress" | "resume"
    ) || review.target_id.trim().is_empty()
        || review.target_id.len() > 1024
        || review.nonce.trim().is_empty()
        || review.nonce.len() > 160
        || review.expected_content_hash.len() != 71
        || !review.expected_content_hash.starts_with("sha256:")
        || !review.expected_content_hash[7..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        || review.issued_at_ms == 0
        || review.issued_at_ms > now
        || now >= review.expires_at_ms
        || review.expires_at_ms.saturating_sub(review.issued_at_ms) > MAX_REVIEW_LIFETIME_MS
    {
        return Err(fail(
            "cortex_review_invalid",
            "invalid or expired reviewed effect",
        ));
    }
    let mut ids = BTreeSet::new();
    if trust.reviewers.len() > 256 || trust.reviewers.iter().any(|k| !ids.insert(&k.key_id)) {
        return Err(fail(
            "cortex_review_unavailable",
            "ambiguous or oversized reviewer registry",
        ));
    }
    let key = trust
        .reviewers
        .iter()
        .find(|k| {
            !k.revoked
                && k.key_id == review.key_id
                && k.repository_id == repository
                && k.scope_id == scope
                && k.allowed_operations
                    .iter()
                    .any(|op| op == &review.operation)
        })
        .ok_or_else(|| {
            fail(
                "cortex_review_denied",
                "key is not authorized for this effect and scope",
            )
        })?;
    let key = rightkit_fs::digest::unhex(&key.public_key_hex)
        .ok_or_else(|| fail("cortex_review_denied", "invalid public key"))?;
    let signature = rightkit_fs::digest::unhex(&review.signature_hex)
        .ok_or_else(|| fail("cortex_review_denied", "invalid signature"))?;
    UnparsedPublicKey::new(&ED25519, key)
        .verify(&review.signing_bytes()?, &signature)
        .map_err(|_| fail("cortex_review_denied", "signature verification failed"))
}

pub(crate) fn ensure_memory_schema(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS cortex_admission_receipts_v1(
           request_id TEXT PRIMARY KEY, payload_hash TEXT NOT NULL, disposition_json TEXT NOT NULL) STRICT;
         CREATE TABLE IF NOT EXISTS cortex_recall_suppression_v1(
           memory_id TEXT PRIMARY KEY, scope_id TEXT NOT NULL, content_hash TEXT NOT NULL,
           suppressed INTEGER NOT NULL CHECK(suppressed IN (0,1)), decision_hash TEXT NOT NULL) STRICT;
         CREATE TABLE IF NOT EXISTS cortex_reviewed_controls_v1(
           nonce_key TEXT PRIMARY KEY, effect_hash TEXT NOT NULL, receipt_json TEXT NOT NULL) STRICT;
         -- CTX-010: Cortex-side intake for lifecycle review signals originated by
         -- another Membrane subsystem (Ledger document supersession, Blueprint
         -- re-anchoring, ...). Append-only and ENQUEUE-ONLY: rows here are read by
         -- `MemoryStore::lifecycle_reviews_due` and never rewrite `memories`,
         -- `lifecycle_state`, `superseded_by`, authority or content. The primary
         -- key is the caller's idempotency key, so a redelivered signal is a no-op.
         CREATE TABLE IF NOT EXISTS cortex_lifecycle_review_signals_v1(
           idempotency_key TEXT PRIMARY KEY,
           origin_subsystem TEXT NOT NULL,
           memory_id TEXT,
           scope_id TEXT,
           reason TEXT NOT NULL,
           observed_at_ms INTEGER NOT NULL,
           recorded_at_ms INTEGER NOT NULL,
           CHECK(memory_id IS NOT NULL OR scope_id IS NOT NULL)) STRICT;
         CREATE INDEX IF NOT EXISTS cortex_lifecycle_review_signals_v1_memory
           ON cortex_lifecycle_review_signals_v1(memory_id);
         CREATE INDEX IF NOT EXISTS cortex_lifecycle_review_signals_v1_scope
           ON cortex_lifecycle_review_signals_v1(scope_id);
         -- CTX-043 (decision 19): the bytes an agent submitted through
         -- `membrane_memory_push` are the immutable admission record. The
         -- canonical `memories` row the admission pipeline derived from them
         -- is only the lifecycle/retrieval projection of these bytes;
         -- downstream storage must never need them to re-derive what was
         -- pushed. Submission identity is (scope_id, request_id): a body
         -- that dedup-merges into an existing record still keeps its own
         -- immutable source row, and several submissions may bind one
         -- `memory_id`. `caller_input_json` retains the caller-declared
         -- optional fields (keywords/lifecycle) as submitted so the source
         -- record — not the derived projection — carries what the agent
         -- asked for.
         CREATE TABLE IF NOT EXISTS cortex_agent_memory_source_v1(
           memory_id TEXT NOT NULL,
           request_id TEXT NOT NULL,
           repository_id TEXT NOT NULL,
           scope_id TEXT NOT NULL,
           caller_id TEXT NOT NULL,
           raw_body BLOB NOT NULL,
           raw_sha256 TEXT NOT NULL,
           caller_input_json TEXT NOT NULL DEFAULT '{}',
           recorded_at_ms INTEGER NOT NULL,
           PRIMARY KEY(scope_id, request_id),
           FOREIGN KEY(memory_id) REFERENCES memories(id)) STRICT;
         CREATE INDEX IF NOT EXISTS cortex_agent_memory_source_memory_v1
           ON cortex_agent_memory_source_v1(memory_id);
         -- CTX-025: erasure is the only governed delete from the source
         -- table. `MemoryStore::hard_erase` inserts this row's permit inside
         -- its own immediate transaction and removes it again before commit;
         -- a permit exists only while that transaction is open, so no other
         -- writer can ever satisfy the delete trigger's gate.
         CREATE TABLE IF NOT EXISTS cortex_erasure_permit_v1(
           memory_id TEXT PRIMARY KEY, granted_at_ms INTEGER NOT NULL) STRICT;
         CREATE TRIGGER IF NOT EXISTS cortex_agent_memory_source_immutable_update_v1
           BEFORE UPDATE ON cortex_agent_memory_source_v1
           BEGIN SELECT RAISE(ABORT, 'immutable Cortex agent memory source'); END;
         CREATE TRIGGER IF NOT EXISTS cortex_agent_memory_source_immutable_delete_v1
           BEFORE DELETE ON cortex_agent_memory_source_v1
           WHEN NOT EXISTS (SELECT 1 FROM cortex_erasure_permit_v1 permit
                            WHERE permit.memory_id = OLD.memory_id)
           BEGIN SELECT RAISE(ABORT, 'immutable Cortex agent memory source'); END;"
    )
}

/// One immutable CTX-043 push-source row. Serialized inside the
/// `membrane.cortex-backup.v3` envelope (snake_case, matching the other
/// backup sections) so a sealed backup carries the admission record itself,
/// not a pointer to it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentMemorySourceRowV1 {
    pub memory_id: String,
    pub request_id: String,
    pub repository_id: String,
    pub scope_id: String,
    pub caller_id: String,
    pub raw_body: Vec<u8>,
    pub raw_sha256: String,
    pub caller_input_json: String,
    pub recorded_at_ms: i64,
}

fn agent_memory_source_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentMemorySourceRowV1> {
    Ok(AgentMemorySourceRowV1 {
        memory_id: row.get(0)?,
        request_id: row.get(1)?,
        repository_id: row.get(2)?,
        scope_id: row.get(3)?,
        caller_id: row.get(4)?,
        raw_body: row.get(5)?,
        raw_sha256: row.get(6)?,
        caller_input_json: row.get(7)?,
        recorded_at_ms: row.get(8)?,
    })
}

const AGENT_SOURCE_SELECT: &str =
    "SELECT memory_id, request_id, repository_id, scope_id, caller_id,
            raw_body, raw_sha256, caller_input_json, recorded_at_ms
       FROM cortex_agent_memory_source_v1";

/// Every immutable source row bound to one canonical memory, oldest request
/// id ordering for deterministic digests. Used by resolve (CTX-043 exact
/// recovery), hard erase and backup; absent table reads as no records.
pub(crate) fn agent_memory_sources_for_on(
    db: &Connection,
    memory_id: &str,
) -> rusqlite::Result<Vec<AgentMemorySourceRowV1>> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master
            WHERE type='table' AND name='cortex_agent_memory_source_v1')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(Vec::new());
    }
    db.prepare(&format!(
        "{AGENT_SOURCE_SELECT} WHERE memory_id = ?1 ORDER BY request_id"
    ))?
    .query_map([memory_id], agent_memory_source_row)?
    .collect()
}

/// Every immutable source row in the store, in a deterministic order, for
/// the sealed backup envelope.
pub(crate) fn agent_memory_sources_on(
    db: &Connection,
) -> rusqlite::Result<Vec<AgentMemorySourceRowV1>> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master
            WHERE type='table' AND name='cortex_agent_memory_source_v1')",
        [],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(Vec::new());
    }
    db.prepare(&format!(
        "{AGENT_SOURCE_SELECT} ORDER BY scope_id, request_id"
    ))?
    .query_map([], agent_memory_source_row)?
    .collect()
}

/// Insert one source row inside an existing transaction (restore).
pub(crate) fn insert_agent_memory_source_on(
    tx: &rusqlite::Transaction<'_>,
    row: &AgentMemorySourceRowV1,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO cortex_agent_memory_source_v1
         (memory_id,request_id,repository_id,scope_id,caller_id,raw_body,raw_sha256,caller_input_json,recorded_at_ms)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            row.memory_id, row.request_id, row.repository_id, row.scope_id,
            row.caller_id, row.raw_body, row.raw_sha256, row.caller_input_json,
            row.recorded_at_ms
        ],
    )?;
    Ok(())
}

/// Insert a pending CTX-043 source row inside an open admission transaction,
/// bound to the memory id that transaction resolved. This is the ONLY write
/// path `agent_memory_push_with_input` uses for source rows — the
/// submitted-bytes record commits or rolls back with the canonical record it
/// binds, never one without the other.
pub(crate) fn insert_agent_memory_source_pending_on(
    tx: &rusqlite::Transaction<'_>,
    memory_id: &str,
    pending: crate::store::PendingAgentSourceRow<'_>,
) -> rusqlite::Result<()> {
    insert_agent_memory_source_on(
        tx,
        &AgentMemorySourceRowV1 {
            memory_id: memory_id.to_owned(),
            request_id: pending.request_id.to_owned(),
            repository_id: pending.repository_id.to_owned(),
            scope_id: pending.scope_id.to_owned(),
            caller_id: pending.caller_id.to_owned(),
            raw_body: pending.raw_body.to_vec(),
            raw_sha256: pending.raw_sha256.to_owned(),
            caller_input_json: pending.caller_input_json.to_owned(),
            recorded_at_ms: crate::time::now_millis() as i64,
        },
    )
}

/// CTX-025/CTX-043: the ONLY governed delete from the immutable source
/// table. The per-memory permit row is inserted, the rows are deleted, and
/// the permit is removed — all inside the caller's immediate transaction —
/// so the delete trigger's `WHEN` gate holds only while a governed erase is
/// open. Ordinary deletes still hit `immutable Cortex agent memory source`.
/// The `hard_erase` lifecycle event logged by the caller is the content-free
/// evidence that remains; nothing payload-bearing survives this call.
pub(crate) fn erase_agent_memory_sources_on(
    tx: &rusqlite::Transaction<'_>,
    memory_id: &str,
) -> rusqlite::Result<usize> {
    ensure_memory_schema(tx)?;
    tx.execute(
        "INSERT OR REPLACE INTO cortex_erasure_permit_v1(memory_id, granted_at_ms) VALUES(?1,?2)",
        params![memory_id, crate::time::now_millis() as i64],
    )?;
    let removed = tx.execute(
        "DELETE FROM cortex_agent_memory_source_v1 WHERE memory_id = ?1",
        params![memory_id],
    )?;
    tx.execute(
        "DELETE FROM cortex_erasure_permit_v1 WHERE memory_id = ?1",
        params![memory_id],
    )?;
    Ok(removed)
}

/// Restore-wipe variant: erase every source row under permits granted to
/// each bound memory, inside the caller's immediate transaction.
pub(crate) fn erase_all_agent_memory_sources_on(
    tx: &rusqlite::Transaction<'_>,
) -> rusqlite::Result<usize> {
    ensure_memory_schema(tx)?;
    tx.execute(
        "INSERT OR IGNORE INTO cortex_erasure_permit_v1(memory_id, granted_at_ms)
           SELECT DISTINCT memory_id, ?1 FROM cortex_agent_memory_source_v1",
        params![crate::time::now_millis() as i64],
    )?;
    let removed = tx.execute("DELETE FROM cortex_agent_memory_source_v1", [])?;
    tx.execute("DELETE FROM cortex_erasure_permit_v1", [])?;
    Ok(removed)
}

/// Caller-declared optional fields accepted by the push envelope. Both are
/// untrusted input — authority and influence class are always assigned by
/// Cortex — but they are retained in the immutable source record so the
/// admission record, not the derived projection, carries what the agent
/// actually submitted.
#[derive(Clone, Debug, Default)]
pub struct AgentPushInputV1 {
    /// Keyword hints. Normalization to the recall vocabulary (trim, lowercase,
    /// dedup) happens inside; the submitted forms are preserved verbatim in
    /// `caller_input_json`.
    pub keywords: Vec<String>,
    /// Scheduling-only lifecycle hints. `authority` and `influence_class`
    /// are stripped before validation and named in `callerFieldsStripped`.
    pub lifecycle: Option<MemoryLifecycleInputV1>,
    /// The caller's optional-fields JSON exactly as submitted, retained in
    /// the source row. `None` records the canonical serialization of the
    /// (possibly empty) `keywords`/`lifecycle` fields actually received.
    pub caller_input_json: Option<String>,
}

/// Admit one agent-directed memory while retaining submitted UTF-8 bytes in
/// an immutable Cortex source row. Request identity is scope-bound and
/// replay-safe; authority/lifecycle are assigned by Cortex and never copied
/// from request JSON.
pub fn agent_memory_push(
    store: &MemoryStore,
    repository: &str,
    scope: &str,
    request_id: &str,
    caller_id: &str,
    body: &str,
) -> Result<Value> {
    agent_memory_push_with_input(
        store,
        repository,
        scope,
        request_id,
        caller_id,
        body,
        &AgentPushInputV1::default(),
    )
}

/// CTX-043 push with the caller's optional fields. The submitted body is
/// still the only thing that enters the canonical `memories` content;
/// `input.keywords` union into the derived keyword projection and the
/// sanitized `input.lifecycle` schedules the record — both are recorded
/// verbatim in the immutable source row's `caller_input_json` so nothing the
/// caller asked for is silently dropped and nothing the caller claimed is
/// silently granted.
#[allow(clippy::too_many_arguments)]
pub fn agent_memory_push_with_input(
    store: &MemoryStore,
    repository: &str,
    scope: &str,
    request_id: &str,
    caller_id: &str,
    body: &str,
    input: &AgentPushInputV1,
) -> Result<Value> {
    if repository.trim().is_empty()
        || scope.trim().is_empty()
        || request_id.trim().is_empty()
        || caller_id.trim().is_empty()
        || body.is_empty()
    {
        return Err(fail(
            "memory_envelope_invalid",
            "repository, scope, requestId, callerId, and body are required",
        ));
    }
    if body.len() > rightkit_context_protocol::explicit::EXPLICIT_MAX_BYTES {
        return Err(fail(
            "memory_payload_too_large",
            "body exceeds 8388608 UTF-8 bytes",
        ));
    }
    // A4 runs before hashes, idempotency reads, admission events and CTX-043
    // source capture. Only the post-policy candidate may reach the store.
    let policy = pii_policy(scope)?;
    let mut pii = PiiEvidence::default();
    let mut sanitized_body = body.to_owned();
    let mut sanitized_input = input.clone();
    let input_scan = serde_json::to_string(&json!({
        "keywords":input.keywords,"lifecycle":input.lifecycle,
        "callerInput":input.caller_input_json,
    }))
    .map_err(storage)?;
    if let Some(class) = dlp_hit(body).or_else(|| dlp_hit(&input_scan)) {
        return Err(fail(
            "dlp_denied",
            format!("push carries denied secret material ({class})"),
        ));
    }
    if policy != PiiPolicy::Off {
        pii.scan(&mut sanitized_body, policy);
        for keyword in &mut sanitized_input.keywords {
            pii.scan(keyword, policy);
        }
        if let Some(lifecycle) = &mut sanitized_input.lifecycle {
            let mut value = serde_json::to_value(&*lifecycle).map_err(storage)?;
            pii.scan_value(&mut value, policy);
            *lifecycle = serde_json::from_value(value).map_err(storage)?;
        }
        if let Some(raw) = &mut sanitized_input.caller_input_json {
            if raw.len() > MAX_PAYLOAD_BYTES {
                return Err(fail(
                    "memory_payload_too_large",
                    "caller input record exceeds limit",
                ));
            }
            if !raw.is_empty() {
                let mut value: Value = serde_json::from_str(raw).map_err(|_| {
                    fail(
                        "memory_envelope_invalid",
                        "caller input record must be JSON for PII filtering",
                    )
                })?;
                pii.scan_value(&mut value, policy);
                *raw = serde_json::to_string(&value).map_err(storage)?;
            }
        }
        pii.enforce(policy)?;
    }
    let body = sanitized_body.as_str();
    let input = &sanitized_input;
    let pii_receipt = pii.receipt(policy);
    // Bounded, non-empty keyword hints; the immutable record keeps the
    // submitted forms, the projection gets the normalized vocabulary.
    if input.keywords.len() > 64
        || input
            .keywords
            .iter()
            .any(|keyword| keyword.trim().is_empty() || keyword.len() > 256)
    {
        return Err(fail(
            "memory_envelope_invalid",
            "keywords must be at most 64 non-empty entries of at most 256 bytes",
        ));
    }
    let mut keywords: Vec<String> = Vec::new();
    for keyword in &input.keywords {
        let normalized = keyword.trim().to_lowercase();
        if !keywords.iter().any(|existing| existing == &normalized) {
            keywords.push(normalized);
        }
    }
    // The caller's lifecycle may request scheduling only. `authority` and
    // `influence_class` are untrusted claims — Cortex assigns A2/reference —
    // and `supersedes` is an authority-bearing lifecycle mutation of ANOTHER
    // record (an A1 supersession event plus a canonical relation), never a
    // property of the submitted body. All three are stripped before
    // validation and named in the receipt.
    let mut lifecycle = input.lifecycle.clone().unwrap_or_default();
    let mut stripped: Vec<&str> = Vec::new();
    if lifecycle.authority.take().is_some() {
        stripped.push("authority");
    }
    if lifecycle.influence_class.take().is_some() {
        stripped.push("influenceClass");
    }
    if lifecycle.supersedes.take().is_some() {
        stripped.push("supersedes");
    }
    lifecycle
        .validate()
        .map_err(|message| fail("memory_envelope_invalid", message))?;
    let caller_input_json = input
        .caller_input_json
        .clone()
        .filter(|raw| !raw.is_empty())
        .unwrap_or_else(|| {
            serde_json::to_string(&json!({
                "keywords": input.keywords,
                "lifecycle": input.lifecycle,
            }))
            .unwrap_or_else(|_| "{}".into())
        });
    if caller_input_json.len() > MAX_PAYLOAD_BYTES {
        return Err(fail(
            "memory_payload_too_large",
            "caller input record exceeds limit",
        ));
    }
    let body_bytes = body.as_bytes();
    let body_hash = digest_bytes(body_bytes);
    let request_key = digest_str(&format!("agent-memory-push\0{scope}\0{request_id}"));
    let memory_name = format!("agent_push_{}", &request_key[7..39]);
    {
        let db = store.db().lock();
        ensure_memory_schema(&db).map_err(storage)?;
        let existing: Option<(String, String, Vec<u8>, String, String)> = db
            .query_row(
                "SELECT memory_id,raw_sha256,raw_body,repository_id,caller_id
               FROM cortex_agent_memory_source_v1 WHERE scope_id=?1 AND request_id=?2",
                rusqlite::params![scope, request_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(storage)?;
        if let Some((bound_id, hash, raw, stored_repository, stored_caller)) = existing {
            if hash != body_hash
                || raw != body_bytes
                || stored_repository != repository
                || stored_caller != caller_id
            {
                return Err(fail(
                    "memory_push_idempotency_conflict",
                    "requestId is already bound to different immutable source bytes or provenance",
                ));
            }
            let stored_content: Option<String> = db
                .query_row(
                    "SELECT content FROM memories WHERE id=?1",
                    [&bound_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(storage)?;
            let stored_hash = stored_content.as_deref().map(digest_str);
            return Ok(
                json!({"schemaVersion":1,"memoryId":bound_id,"contentHash":hash,
                "storedContentHash":stored_hash,"rawBodyBytes":raw.len(),"authority":"A2",
                "status":"replayed","recordPresent":stored_content.is_some(),
                "dedup":{"merged":stored_hash.as_deref() != Some(hash.as_str())},
                "provenance":{"repositoryId":repository,"scopeId":scope,"callerId":caller_id,
                    "requestId":request_id,"source":"cortex_agent_memory_source_v1",
                    "callerFieldsStripped":stripped},
                "pregate":{"pii":pii_receipt},
                "lifecycle":{"managedBy":"cortex","callerAuthority":"untrusted"}}),
            );
        }
    }
    let context = MemoryEventContext::new("agent")
        .with_session(scope)
        .with_trace(request_id);
    // The immutable source row and the canonical record it binds commit in
    // ONE admission transaction (`pending_source` inside
    // `try_admit_idempotent_observed`): an admitted push can never exist
    // without its submitted bytes, and a rolled-back admission leaves no
    // orphaned source row.
    let pending = crate::store::PendingAgentSourceRow {
        request_id,
        repository_id: repository,
        scope_id: scope,
        caller_id,
        raw_body: body_bytes,
        raw_sha256: body_hash.as_str(),
        caller_input_json: caller_input_json.as_str(),
    };
    let admitted = store.try_put_attributed_lifecycle_keywords_observed(
        &memory_name,
        body,
        scope,
        MemoryTier::Semantic,
        "memory",
        "agent",
        "agent_memory_push",
        &context,
        &lifecycle,
        &keywords,
        Some(pending),
    );
    let (memory_id, replayed) = match admitted {
        Ok(id) => (id, false),
        Err(error)
            if error.contains("UNIQUE") && error.contains("cortex_agent_memory_source_v1") =>
        {
            // A raced identical request converged inside its own admission
            // transaction and this one rolled back. Re-read the winner's
            // immutable row and apply the same replay comparison the fast
            // path uses — never report success for unrecorded bytes.
            let db = store.db().lock();
            ensure_memory_schema(&db).map_err(storage)?;
            let winner: Option<(String, String, Vec<u8>, String, String)> = db
                .query_row(
                    "SELECT memory_id,raw_sha256,raw_body,repository_id,caller_id
                   FROM cortex_agent_memory_source_v1 WHERE scope_id=?1 AND request_id=?2",
                    rusqlite::params![scope, request_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(storage)?;
            let Some((bound_id, hash, raw, stored_repository, stored_caller)) = winner else {
                return Err(storage("agent memory push raced with an identical request"));
            };
            if hash != body_hash
                || raw != body_bytes
                || stored_repository != repository
                || stored_caller != caller_id
            {
                return Err(fail(
                    "memory_push_idempotency_conflict",
                    "requestId is already bound to different immutable source bytes or provenance",
                ));
            }
            (bound_id, true)
        }
        Err(error) => return Err(storage(error)),
    };
    let stored_content: Option<String> = store
        .db()
        .lock()
        .query_row(
            "SELECT content FROM memories WHERE id=?1",
            [&memory_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    let Some(stored_content) = stored_content else {
        return Err(storage("admitted memory row is missing after push"));
    };
    let stored_hash = digest_str(&stored_content);
    // CTX-043 honest dedup reporting: when the admission pre-filter merged
    // this submission into an existing record the canonical content differs
    // from the submitted bytes — the immutable source row, committed in the
    // same transaction, is where the submitted bytes live.
    let merged = stored_hash != body_hash;
    let status = if replayed {
        "replayed"
    } else if merged {
        "deduplicated"
    } else {
        "stored"
    };
    Ok(
        json!({"schemaVersion":1,"memoryId":memory_id,"contentHash":body_hash,
        "storedContentHash":stored_hash,"rawBodyBytes":body_bytes.len(),"authority":"A2",
        "status":status,"recordPresent":true,
        "dedup":{"merged":merged},
        "provenance":{"repositoryId":repository,"scopeId":scope,"callerId":caller_id,
            "requestId":request_id,"source":"cortex_agent_memory_source_v1",
            "callerFieldsStripped":stripped},
        "pregate":{"pii":pii_receipt},
        "lifecycle":{"managedBy":"cortex","callerAuthority":"untrusted"}}),
    )
}
fn ensure_proposal_schema(db: &Connection) -> rusqlite::Result<()> {
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS membrane_knowledge_proposal(
           proposal_id TEXT PRIMARY KEY,repository_id TEXT NOT NULL,scope_id TEXT NOT NULL,
           emission_json TEXT NOT NULL,emission_sha256 TEXT NOT NULL,
           state TEXT NOT NULL CHECK(state IN ('pending','approved','rejected')),
           created_at TEXT NOT NULL,decided_at TEXT,reviewer TEXT) STRICT;
         CREATE TABLE IF NOT EXISTS cortex_proposal_admission_v1(
           proposal_id TEXT PRIMARY KEY, effect_json TEXT NOT NULL, effect_hash TEXT NOT NULL,
           state TEXT NOT NULL CHECK(state IN ('pending','completed','blocked')),
           attempts INTEGER NOT NULL DEFAULT 0, next_attempt_ms INTEGER NOT NULL DEFAULT 0,
           last_error TEXT, receipt_json TEXT) STRICT;
         CREATE TABLE IF NOT EXISTS cortex_proposal_reviews_v1(
           nonce_key TEXT PRIMARY KEY, effect_hash TEXT NOT NULL, proposal_id TEXT NOT NULL) STRICT;
         CREATE TABLE IF NOT EXISTS cortex_proposal_pii_receipts_v1(
           proposal_id TEXT PRIMARY KEY, receipt_json TEXT NOT NULL) STRICT;",
    )
}
fn bound(value: &Value) -> Result<String> {
    let text = serde_json::to_string(value).map_err(storage)?;
    if text.len() > MAX_PAYLOAD_BYTES {
        return Err(fail(
            "proposal_payload_too_large",
            "payload exceeds 65536 bytes",
        ));
    }
    Ok(text)
}

/// CTX-002 frozen ordered pre-gate vocabulary. Gate order is
/// schema -> scope -> producer -> DLP -> epistemic class -> stable identity,
/// matching the legacy Cortex pipeline order. Every dimension fails closed
/// with a typed code; missing producer or epistemic evidence is denied,
/// never defaulted.
pub const PREGATE_PRODUCERS: &[&str] = &[
    "manual",
    "agent",
    "harness",
    "adapt_native",
    "ingest_hook",
    "dream",
    "episodic",
    "import",
    "checkpoint",
    "system",
];
/// How the candidate claims to know. `observed` is first-hand tool/system
/// measurement; `reported` is an agent/user assertion; `inferred` is derived
/// from other durable records; `directive` is an explicit operator decision.
pub const PREGATE_EPISTEMIC_CLASSES: &[&str] = &["observed", "reported", "inferred", "directive"];

/// Deterministic secret-material scan for the DLP pre-gate dimension.
/// Secret classes always deny, independently of the PII policy.
fn dlp_hit(text: &str) -> Option<&'static str> {
    const KEY_HEADERS: &[&str] = &[
        concat!("-----BEGIN ", "PRIVATE KEY-----"),
        concat!("-----BEGIN ", "RSA PRIVATE KEY-----"),
        concat!("-----BEGIN ", "OPENSSH PRIVATE KEY-----"),
        concat!("-----BEGIN ", "EC PRIVATE KEY-----"),
    ];
    if KEY_HEADERS.iter().any(|header| text.contains(header)) {
        return Some("secret_key_material");
    }
    fn keyed(text: &str, prefix: &str, min_tail: usize) -> bool {
        for (pos, _) in text.match_indices(prefix) {
            let tail = &text[pos + prefix.len()..];
            let run = tail
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .count();
            if run >= min_tail {
                return true;
            }
        }
        false
    }
    if keyed(text, "AKIA", 16) {
        return Some("aws_access_key");
    }
    if keyed(text, "ghp_", 10) || keyed(text, "gho_", 10) || keyed(text, "github_pat_", 10) {
        return Some("github_token");
    }
    if keyed(text, "sk-live-", 8) || keyed(text, "sk-test-", 8) {
        return Some("api_secret");
    }
    if keyed(text, "xoxb-", 8)
        || keyed(text, "xoxp-", 8)
        || keyed(text, "xoxa-", 8)
        || keyed(text, "xoxs-", 8)
    {
        return Some("chat_token");
    }
    if keyed(text, "AIza", 10) {
        return Some("api_key");
    }
    None
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PiiPolicy {
    #[default]
    Off,
    Redact,
    Deny,
}

/// Runtime environment configuration, like the existing Cortex feature flags.
/// No pre-existing per-scope pre-gate configuration exists. A scope override is
/// selected only from this host-owned JSON map, never from candidate text.
/// RIGHTKIT_CONTEXT_CORTEX_PII_POLICY=off|redact|deny (default off)
/// RIGHTKIT_CONTEXT_CORTEX_PII_SCOPES={"scope-id":"redact"}
fn pii_policy(scope: &str) -> Result<PiiPolicy> {
    let read = |name| match std::env::var(name) {
        Ok(value) if value.len() <= MAX_PAYLOAD_BYTES => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        _ => Err(fail(
            "dlp_policy_invalid",
            "invalid Cortex PII configuration",
        )),
    };
    let default = read("RIGHTKIT_CONTEXT_CORTEX_PII_POLICY")?
        .map(|value| serde_json::from_value::<PiiPolicy>(json!(value)))
        .transpose()
        .map_err(|_| {
            fail(
                "dlp_policy_invalid",
                "PII policy must be off, redact or deny",
            )
        })?
        .unwrap_or_default();
    let scopes = read("RIGHTKIT_CONTEXT_CORTEX_PII_SCOPES")?
        .map(|value| serde_json::from_str::<BTreeMap<String, PiiPolicy>>(&value))
        .transpose()
        .map_err(|_| {
            fail(
                "dlp_policy_invalid",
                "PII scope policies must be a JSON policy map",
            )
        })?
        .unwrap_or_default();
    Ok(scopes.get(scope).copied().unwrap_or(default))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PiiMatch {
    start: usize,
    end: usize,
    class: &'static str,
}

fn token_boundary(bytes: &[u8], start: usize, end: usize) -> bool {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 128;
    (start == 0 || !word(bytes[start - 1])) && (end == bytes.len() || !word(bytes[end]))
}

fn email_matches(text: &str) -> Vec<PiiMatch> {
    let bytes = text.as_bytes();
    let local = |b: u8| b.is_ascii_alphanumeric() || b".!#$%&'*+/=?^_`{|}~-".contains(&b);
    let mut matches = Vec::new();
    for (at, _) in text.match_indices('@') {
        let mut start = at;
        while start > 0 && local(bytes[start - 1]) {
            start -= 1;
        }
        let mut end = at + 1;
        while end < bytes.len()
            && (bytes[end].is_ascii_alphanumeric() || b".-".contains(&bytes[end]))
        {
            end += 1;
        }
        while end > at + 1 && bytes[end - 1] == b'.' {
            end -= 1;
        }
        let user = &text[start..at];
        let domain = &text[at + 1..end];
        let labels: Vec<_> = domain.split('.').collect();
        if !user.is_empty()
            && user.len() <= 64
            && !user.starts_with('.')
            && !user.ends_with('.')
            && !user.contains("..")
            && domain.len() <= 253
            && labels.len() >= 2
            && labels.iter().all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
            })
            && labels.last().is_some_and(|label| {
                label.len() >= 2 && label.bytes().all(|b| b.is_ascii_alphabetic())
            })
            && token_boundary(bytes, start, end)
        {
            matches.push(PiiMatch {
                start,
                end,
                class: "email",
            });
        }
    }
    matches
}

fn luhn_valid(digits: &[u8]) -> bool {
    if !(13..=19).contains(&digits.len()) || digits.iter().all(|d| *d == digits[0]) {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| {
            let value = (*d - b'0') as u32 * if i % 2 == 1 { 2 } else { 1 };
            if value > 9 {
                value - 9
            } else {
                value
            }
        })
        .sum();
    sum.is_multiple_of(10)
}

fn iban_valid(compact: &str) -> bool {
    let bytes = compact.as_bytes();
    if !(15..=34).contains(&bytes.len())
        || !bytes[..2].iter().all(u8::is_ascii_uppercase)
        || !bytes[2..4].iter().all(u8::is_ascii_digit)
        || !bytes[4..].iter().all(u8::is_ascii_alphanumeric)
    {
        return false;
    }
    // Common country lengths also reject checksum-valid prefixes of a
    // longer, conventionally spaced account; other shapes still require
    // the bounded IBAN envelope and mod-97 check below.
    let expected_length = match &compact[..2] {
        "GB" | "IE" | "DE" => 22,
        "FR" | "IT" | "GR" => 27,
        "ES" | "CZ" | "SE" | "SK" => 24,
        "NL" | "DK" | "FI" => 18,
        "BE" => 16,
        "NO" => 15,
        "AT" | "LT" | "LU" | "EE" => 20,
        "CH" | "LI" | "LV" | "HR" => 21,
        _ => bytes.len(),
    };
    if bytes.len() != expected_length {
        return false;
    }
    let mut remainder = 0u32;
    for &b in bytes[4..].iter().chain(&bytes[..4]) {
        if b.is_ascii_digit() {
            remainder = (remainder * 10 + (b - b'0') as u32) % 97;
        } else if b.is_ascii_uppercase() {
            remainder = (remainder * 100 + (b - b'A' + 10) as u32) % 97;
        } else {
            return false;
        }
    }
    remainder == 1
}

fn government_id_matches(text: &str) -> Vec<PiiMatch> {
    let bytes = text.as_bytes();
    let mut matches = Vec::new();
    for start in 0..bytes.len() {
        if let Some(ssn) = bytes.get(start..start + 11) {
            if ssn[3] == b'-'
                && ssn[6] == b'-'
                && ssn
                    .iter()
                    .enumerate()
                    .all(|(i, b)| i == 3 || i == 6 || b.is_ascii_digit())
                && &ssn[..3] != b"000"
                && &ssn[..3] != b"666"
                && ssn[0] != b'9'
                && &ssn[4..6] != b"00"
                && &ssn[7..] != b"0000"
                && token_boundary(bytes, start, start + 11)
            {
                matches.push(PiiMatch {
                    start,
                    end: start + 11,
                    class: "government_id",
                });
            }
        }
        // NINO: two permitted prefix letters, six digits, suffix A-D;
        // allow the conventional spaces between digit pairs.
        if let Some(prefix) = bytes.get(start..start + 2) {
            let prefix = [
                prefix[0].to_ascii_uppercase(),
                prefix[1].to_ascii_uppercase(),
            ];
            if prefix.iter().all(u8::is_ascii_uppercase)
                && !b"DFIQUV".contains(&prefix[0])
                && !b"DFIOQUV".contains(&prefix[1])
                && ![b"BG", b"GB", b"KN", b"NK", b"NT", b"TN", b"ZZ"].contains(&&prefix)
            {
                let mut end = start + 2;
                let mut digits = 0;
                while end < bytes.len() && digits < 6 {
                    if bytes[end] == b' ' && digits % 2 == 0 {
                        end += 1;
                    } else if bytes[end].is_ascii_digit() {
                        digits += 1;
                        end += 1;
                    } else {
                        break;
                    }
                }
                if bytes.get(end) == Some(&b' ') {
                    end += 1;
                }
                if digits == 6
                    && bytes
                        .get(end)
                        .is_some_and(|b| b"ABCD".contains(&b.to_ascii_uppercase()))
                    && token_boundary(bytes, start, end + 1)
                {
                    matches.push(PiiMatch {
                        start,
                        end: end + 1,
                        class: "government_id",
                    });
                }
            }
        }
        if bytes.get(start..start + 4).is_some_and(|head| {
            head[..2].iter().all(u8::is_ascii_alphabetic)
                && head[2..].iter().all(u8::is_ascii_digit)
        }) && (start == 0 || !bytes[start - 1].is_ascii_alphanumeric())
        {
            let mut compact = String::new();
            let mut end = start;
            while end < bytes.len() && compact.len() < 34 {
                let b = bytes[end];
                if b.is_ascii_alphanumeric() {
                    compact.push(char::from(b.to_ascii_uppercase()));
                    end += 1;
                    if compact.len() >= 15
                        && token_boundary(bytes, start, end)
                        && iban_valid(&compact)
                    {
                        matches.push(PiiMatch {
                            start,
                            end,
                            class: "government_id",
                        });
                        break;
                    }
                } else if b == b' ' && compact.len().is_multiple_of(4) {
                    end += 1;
                } else {
                    break;
                }
            }
        }
    }
    matches
}

fn numeric_pii_matches(text: &str) -> Vec<PiiMatch> {
    let bytes = text.as_bytes();
    let mut matches = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        if !(bytes[start].is_ascii_digit() || bytes[start] == b'+' || bytes[start] == b'(') {
            start += 1;
            continue;
        }
        let mut end = start + 1;
        let mut digit_run = usize::from(bytes[start].is_ascii_digit());
        while end < bytes.len() && (bytes[end].is_ascii_digit() || b" -().".contains(&bytes[end])) {
            if bytes[end] == b' ' && digit_run >= 10 {
                let next = bytes[end..].iter().take_while(|b| **b == b' ').count() + end;
                let next_run = bytes[next..]
                    .iter()
                    .take_while(|b| b.is_ascii_digit())
                    .count();
                if next_run >= 10 {
                    break;
                }
            }
            digit_run = if bytes[end].is_ascii_digit() {
                digit_run + 1
            } else {
                0
            };
            end += 1;
        }
        while end > start && b" -.".contains(&bytes[end - 1]) {
            end -= 1;
        }
        let candidate = &bytes[start..end];
        let digits: Vec<_> = candidate
            .iter()
            .copied()
            .filter(u8::is_ascii_digit)
            .collect();
        let groups: Vec<_> = candidate
            .split(|b| !b.is_ascii_digit())
            .filter(|g| !g.is_empty())
            .map(|g| g.len())
            .collect();
        let label = text[..start]
            .split_whitespace()
            .next_back()
            .unwrap_or("")
            .trim_end_matches([':', '='])
            .to_ascii_lowercase();
        let non_phone_label = matches!(
            label.as_str(),
            "version" | "ver" | "date" | "hash" | "sha256" | "sha1" | "md5"
        );
        let boundary = token_boundary(bytes, start, end)
            && (start == 0 || !b"./-".contains(&bytes[start - 1]))
            && (end == bytes.len() || !b"/_".contains(&bytes[end]));
        let card = boundary
            && candidate
                .iter()
                .all(|b| b.is_ascii_digit() || b" -".contains(b))
            && luhn_valid(&digits);
        // Card-length numbers, valid or invalid, never fall back to phone.
        // National grouping is deliberately narrow; dotted versions, ISO
        // dates, numeric/hex hashes and identifier fragments do not qualify.
        let phone_shape = groups.len() == 1
            || matches!(
                groups.as_slice(),
                [3, 3, 4]
                    | [3, 7]
                    | [1, 3, 3, 4]
                    | [1, 3, 7]
                    | [5, 5]
                    | [5, 6]
                    | [4, 3, 3]
                    | [4, 3, 4]
                    | [2, 4, 4]
                    | [3, 4, 4]
            )
            || (candidate[0] == b'+'
                && groups.len() <= 5
                && groups.iter().skip(1).all(|n| (2..=6).contains(n)));
        let phone = boundary
            && !non_phone_label
            && (10..=15).contains(&digits.len())
            && phone_shape
            && digits.iter().any(|d| *d != b'0')
            && (candidate[0] != b'+' || digits[0] != b'0')
            && (digits.len() < 13 || candidate[0] == b'+' || candidate.contains(&b'('))
            && candidate.iter().filter(|b| **b == b'(').count()
                == candidate.iter().filter(|b| **b == b')').count();
        if card || phone {
            matches.push(PiiMatch {
                start,
                end,
                class: if card { "payment_card" } else { "phone" },
            });
        }
        start = end.max(start + 1);
    }
    matches
}

/// Byte ranges refer only to ASCII detector characters, preserving UTF-8.
/// Prefer specific classes over an overlapping phone-shaped identifier.
fn pii_matches(text: &str) -> Vec<PiiMatch> {
    let mut matches = email_matches(text);
    matches.extend(government_id_matches(text));
    matches.extend(numeric_pii_matches(text));
    matches.sort_by_key(|m| {
        (
            m.start,
            match m.class {
                "email" => 0,
                "government_id" => 1,
                "payment_card" => 2,
                _ => 3,
            },
        )
    });
    let mut selected: Vec<PiiMatch> = Vec::new();
    for found in matches {
        if selected
            .last()
            .is_none_or(|previous| previous.end <= found.start)
        {
            selected.push(found);
        }
    }
    selected
}

fn redact_pii(text: &str, matches: &[PiiMatch]) -> String {
    let mut redacted = String::new();
    let mut cursor = 0;
    for found in matches {
        redacted.push_str(&text[cursor..found.start]);
        redacted.push_str(&format!("[REDACTED:{}]", found.class));
        cursor = found.end;
    }
    redacted.push_str(&text[cursor..]);
    redacted
}

#[derive(Default)]
struct PiiEvidence(BTreeMap<&'static str, usize>);
impl PiiEvidence {
    fn receipt(&self, policy: PiiPolicy) -> Value {
        json!({"policy":policy,"redacted":policy == PiiPolicy::Redact && !self.0.is_empty(),
            "classes":self.0.iter().map(|(class, count)| json!({"class":class,"count":count})).collect::<Vec<_>>()})
    }
    fn scan(&mut self, text: &mut String, policy: PiiPolicy) {
        let matches = pii_matches(text);
        for found in &matches {
            *self.0.entry(found.class).or_default() += 1;
        }
        if policy == PiiPolicy::Redact && !matches.is_empty() {
            *text = redact_pii(text, &matches);
        }
    }
    fn scan_value(&mut self, value: &mut Value, policy: PiiPolicy) {
        match value {
            Value::String(text) => self.scan(text, policy),
            Value::Array(values) => {
                for value in values {
                    self.scan_value(value, policy);
                }
            }
            Value::Object(object) => {
                let mut sanitized = serde_json::Map::new();
                for (mut key, mut value) in std::mem::take(object) {
                    self.scan(&mut key, policy);
                    self.scan_value(&mut value, policy);
                    sanitized.insert(key, value);
                }
                *object = sanitized;
            }
            _ => {}
        }
    }
    fn enforce(&self, policy: PiiPolicy) -> Result<()> {
        if policy == PiiPolicy::Deny && !self.0.is_empty() {
            return Err(fail(
                "dlp_denied",
                format!("PII pre-gate denied: {}", self.receipt(policy)),
            ));
        }
        Ok(())
    }
}

/// Store a proposal only. Scope is the verified caller binding, not a claim in
/// emission JSON.
pub fn propose(
    store: &MemoryStore,
    repository: &str,
    scope: &str,
    emission: &Value,
) -> Result<Value> {
    let mut emission = emission.as_object().cloned().ok_or_else(|| {
        fail(
            "proposal_emission_text_required",
            "emission must be an object",
        )
    })?;
    if emission.get("kind").and_then(Value::as_str) == Some(MULTIWRITER_KIND) {
        validate_multiwriter_emission(repository, scope, &Value::Object(emission.clone()))
            .map_err(|code| fail("multiwriter_emission_invalid", code))?;
    }
    let text = emission
        .get("text")
        .or_else(|| emission.get("content"))
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            fail(
                "proposal_emission_text_required",
                "emission text is required",
            )
        })?;
    if text.len() > MAX_PAYLOAD_BYTES {
        return Err(fail("proposal_payload_too_large", "emission exceeds limit"));
    }
    if emission
        .get("scopeId")
        .is_some_and(|v| v.as_str() != Some(scope))
    {
        return Err(fail(
            "proposal_scope_denied",
            "emission scope does not match caller",
        ));
    }
    emission.insert("scopeId".into(), json!(scope));
    // CTX-002 ordered pre-gate, dimensions 3-6. Dimensions 1-2 (schema text,
    // caller scope) are enforced above; every check below fails closed before
    // any semantic admission or durable write.
    let producer = emission
        .get("producer")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !PREGATE_PRODUCERS.contains(&producer) {
        return Err(fail(
            "producer_denied",
            "emission producer is missing or not an admitted producer",
        ));
    }
    // The DLP scan must inspect the content that will actually be stored, not
    // just the outer text/content string: a nested durable payload (e.g.
    // temporalFact) can carry secret material the outer text omits.
    let full_emission_scan = serde_json::to_string(&emission).map_err(storage)?;
    if let Some(class) = dlp_hit(&text).or_else(|| dlp_hit(&full_emission_scan)) {
        return Err(fail(
            "dlp_denied",
            format!("emission carries denied secret material ({class})"),
        ));
    }
    let policy = pii_policy(scope)?;
    let mut pii = PiiEvidence::default();
    if policy != PiiPolicy::Off {
        let mut candidate = Value::Object(emission);
        pii.scan_value(&mut candidate, policy);
        pii.enforce(policy)?;
        emission = candidate
            .as_object()
            .cloned()
            .expect("object preserved by PII scan");
        emission.insert("scopeId".into(), json!(scope));
        if policy == PiiPolicy::Redact
            && !pii.0.is_empty()
            && emission.get("kind").and_then(Value::as_str) == Some(MULTIWRITER_KIND)
        {
            // Multiwriter text/convergence are canonical projections of its
            // records. Re-derive them from filtered records, preserving its
            // strict restart validation instead of keeping a stale projection.
            let requests: Vec<WriterRequestV1> =
                serde_json::from_value(emission["records"].clone()).map_err(|_| {
                    fail(
                        "multiwriter_emission_invalid",
                        "invalid filtered writer records",
                    )
                })?;
            emission = multiwriter_proposal_emission(repository, scope, &requests)
                .map_err(|code| fail("multiwriter_emission_invalid", code))?
                .as_object()
                .cloned()
                .expect("multiwriter emission is an object");
        }
    }
    let epistemic = emission
        .get("epistemicClass")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if !PREGATE_EPISTEMIC_CLASSES.contains(&epistemic) {
        return Err(fail(
            "epistemic_denied",
            "emission epistemicClass is missing or not admitted",
        ));
    }
    let claimed = emission
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    // The identity claim is evidence about identity, not content: it is
    // checked against the derived identity, never hashed into it.
    emission.remove("id");
    let emission_json = bound(&Value::Object(emission))?;
    let hash = digest_str(&emission_json);
    let id = digest_str(&serde_json::to_string(&(repository, scope, &hash)).map_err(storage)?);
    if claimed.as_deref().is_some_and(|value| value != id) {
        return Err(fail(
            "identity_conflict",
            "emission id claim does not match derived stable identity",
        ));
    }
    {
        let mut db = store.db().lock_events();
        ensure_proposal_schema(&db).map_err(storage)?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        tx.execute(
            "INSERT OR IGNORE INTO membrane_knowledge_proposal
            (proposal_id,repository_id,scope_id,emission_json,emission_sha256,state,created_at)
            VALUES(?1,?2,?3,?4,?5,'pending',?6)",
            params![
                id,
                repository,
                scope,
                emission_json,
                hash,
                crate::time::now_iso()
            ],
        )
        .map_err(storage)?;
        // Receipt metadata stays outside canonical emission JSON: exact
        // validators and signed proposal hashes keep their existing shape.
        tx.execute(
            "INSERT OR IGNORE INTO cortex_proposal_pii_receipts_v1(proposal_id,receipt_json) VALUES(?1,?2)",
            params![id, serde_json::to_string(&pii.receipt(policy)).map_err(storage)?],
        ).map_err(storage)?;
        tx.commit().map_err(storage)?;
    }
    proposal_status(store, repository, scope, &id)
}

/// Proposal-only Adapt multiwriter intake.  The merge is computed from a
/// canonical, identity-bound request set, then handed to Cortex's ordinary
/// proposal queue.  No Adapt-owned durable state or direct Cortex truth write
/// is possible from this seam.
pub fn propose_multiwriter(
    store: &MemoryStore,
    repository: &str,
    scope: &str,
    requests: &[WriterRequestV1],
) -> Result<Value> {
    let emission = multiwriter_proposal_emission(repository, scope, requests)
        .map_err(|code| fail("multiwriter_request_invalid", code))?;
    let mut receipt = propose(store, repository, scope, &emission)?;
    receipt["multiwriter"] = json!({
        "schemaVersion": 1,
        "kind": MULTIWRITER_KIND,
        "requestCount": requests.len(),
        "convergence": emission["convergence"].clone(),
        "proposalOnly": true,
    });
    if pii_policy(scope)? == PiiPolicy::Redact {
        PiiEvidence::default().scan_value(&mut receipt, PiiPolicy::Redact);
    }
    Ok(receipt)
}

/// Temporal MCP record is proposal-only. Caller authority & veracity are
/// discarded before validation or persistence; predicate cardinality is an
/// explicit deterministic policy input carried into signed review.
pub fn propose_temporal(
    store: &MemoryStore,
    repository: &str,
    scope: &str,
    fact: &TemporalFact,
    single_valued_predicates: &[String],
) -> Result<Value> {
    if single_valued_predicates.len() > 64
        || single_valued_predicates
            .iter()
            .any(|value| value.trim().is_empty() || value.len() > 512)
    {
        return Err(fail(
            "temporal_fact_envelope_invalid",
            "singleValuedPredicates must be a bounded list of predicate names",
        ));
    }
    let unique = single_valued_predicates.iter().collect::<BTreeSet<_>>();
    if unique.len() != single_valued_predicates.len() {
        return Err(fail(
            "temporal_fact_envelope_invalid",
            "singleValuedPredicates must be unique",
        ));
    }
    let mut normalized = fact.clone();
    normalized.scope_id = scope.to_owned();
    normalized.authority = "A0".into();
    normalized.veracity = "unverified".into();
    normalized.supersedes = None;
    validate_fact_proposal(&normalized)
        .map_err(|message| fail("temporal_fact_invalid", message))?;
    let single_valued = unique.iter().any(|value| value.as_str() == fact.predicate);
    let text = serde_json::to_string(&json!({
        "subject": normalized.subject,
        "predicate": normalized.predicate,
        "object": normalized.object,
        "validFrom": normalized.valid_from,
        "validUntil": normalized.valid_until,
        "expiresAt": normalized.expires_at
    }))
    .map_err(storage)?;
    let mut receipt = propose(
        store,
        repository,
        scope,
        &json!({
            "kind":"temporal",
            "text":text,
            "producer":"agent",
            "epistemicClass":"observed",
            "temporalFact":normalized,
            "callerFieldsStripped":["authority","veracity","supersedes"],
            "cardinalityPolicy":{
                "policyVersion":"cortex.temporal-cardinality.v1",
                "predicate":fact.predicate,
                "cardinality":if single_valued {"single"} else {"multi"},
                "source":"explicit_single_valued_predicates"
            }
        }),
    )?;
    receipt["provenance"]["callerFieldsStripped"] = json!(["authority", "veracity", "supersedes"]);
    receipt["provenance"]["cardinalityPolicy"] = json!({
        "policyVersion":"cortex.temporal-cardinality.v1",
        "predicate":fact.predicate,
        "cardinality":if single_valued {"single"} else {"multi"},
        "source":"explicit_single_valued_predicates"
    });
    if pii_policy(scope)? == PiiPolicy::Redact {
        PiiEvidence::default().scan_value(&mut receipt, PiiPolicy::Redact);
    }
    Ok(receipt)
}

pub fn proposal_status(
    store: &MemoryStore,
    repository: &str,
    scope: &str,
    id: &str,
) -> Result<Value> {
    let db = store.db().lock_events();
    ensure_proposal_schema(&db).map_err(storage)?;
    let row = db
        .query_row(
            "SELECT emission_sha256,state,created_at,emission_json FROM membrane_knowledge_proposal
        WHERE proposal_id=?1 AND repository_id=?2 AND scope_id=?3",
            params![id, repository, scope],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            },
        )
        .optional()
        .map_err(storage)?
        .ok_or_else(|| {
            fail(
                "proposal_review_unknown",
                "proposal unavailable in caller scope",
            )
        })?;
    let stored: Value = serde_json::from_str(&row.3).map_err(storage)?;
    let pii_receipt: Option<String> = db
        .query_row(
            "SELECT receipt_json FROM cortex_proposal_pii_receipts_v1 WHERE proposal_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()
        .map_err(storage)?;
    let pii_receipt = pii_receipt
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
        .map_err(storage)?
        .unwrap_or_else(|| PiiEvidence::default().receipt(PiiPolicy::Off));
    // Identity claims are checked against the derived identity at intake and
    // never stored, so every durable proposal carries derived stable identity.
    let pregate = json!({
        "producer": stored.get("producer"),
        "epistemicClass": stored.get("epistemicClass"),
        "dlp": "pass",
        "pii": pii_receipt,
        "stableIdentity": "derived",
    });
    let admission: Option<(String, Option<String>, Option<String>, String)> = db.query_row(
        "SELECT state,last_error,receipt_json,effect_hash FROM cortex_proposal_admission_v1 WHERE proposal_id=?1", [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).optional().map_err(storage)?;
    let admission_state = admission
        .as_ref()
        .map(|r| r.0.as_str())
        .unwrap_or("not_requested");
    let receipt = admission
        .as_ref()
        .and_then(|r| r.2.as_deref())
        .map(serde_json::from_str::<Value>)
        .transpose()
        .map_err(storage)?;
    Ok(
        json!({"status":if row.1 == "pending" { "needs_review" } else { "reviewed" },
        "durable":true,"proposalId":id,"durableId":id,"reviewState":row.1,
        "emissionHash":row.0,"admissionState":admission_state,"admission":receipt,
        "lastError":admission.as_ref().and_then(|r| r.1.clone()),
        "admissionRevision":admission.as_ref().map(|r|r.3.clone()).unwrap_or_default(),
        "pregate":pregate,
        "lifecycleReceipt":{"schema":"membrane.lifecycle-receipt.v1","operation":"knowledge_propose",
        "status":row.1,"durableId":id,"eventId":digest_str(&format!("proposal:{id}")),
        "readbackDigest":row.0,"recordedAt":row.2},
        "provenance":{"repositoryId":repository,"scopeId":scope,"authority":"proposal_only"}}),
    )
}

pub fn review(store: &MemoryStore, repository: &str, scope: &str, value: &Value) -> Result<Value> {
    bound(value)?;
    let effect: ReviewedEffectV1 = serde_json::from_value(value.clone())
        .map_err(|_| fail("cortex_review_invalid", "invalid reviewed-effect envelope"))?;
    verify_review(store, &effect, repository, scope)?;
    if matches!(effect.operation.as_str(), "suppress" | "resume") {
        return apply_suppression(store, &effect);
    }
    queue_review(store, &effect)?;
    if matches!(effect.operation.as_str(), "approve" | "retry") {
        // A failed admission is a durable pending/blocked job, not a false
        // success or an approval stranded by a process death.
        let _ = recover_one(store, &effect.target_id);
    }
    proposal_status(store, repository, scope, &effect.target_id)
}
fn queue_review(store: &MemoryStore, effect: &ReviewedEffectV1) -> Result<()> {
    let effect_json = serde_json::to_string(effect).map_err(storage)?;
    let effect_hash = digest_str(&effect_json);
    let nonce_key = digest_str(
        &serde_json::to_string(&(
            &effect.key_id,
            &effect.repository_id,
            &effect.scope_id,
            &effect.nonce,
        ))
        .map_err(storage)?,
    );
    let mut db = store.db().lock_events();
    ensure_proposal_schema(&db).map_err(storage)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    let previous: Option<String> = tx
        .query_row(
            "SELECT effect_hash FROM cortex_proposal_reviews_v1 WHERE nonce_key=?1",
            [&nonce_key],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    if let Some(previous) = previous {
        if previous != effect_hash {
            return Err(fail(
                "cortex_review_replay_conflict",
                "nonce already binds another effect",
            ));
        }
        return Ok(());
    }
    let row: Option<(String, String, String)> = tx
        .query_row(
            "SELECT emission_sha256,emission_json,state FROM membrane_knowledge_proposal
         WHERE proposal_id=?1 AND repository_id=?2 AND scope_id=?3",
            params![effect.target_id, effect.repository_id, effect.scope_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(storage)?;
    let Some((hash, payload, state)) = row else {
        return Err(fail(
            "proposal_review_unknown",
            "proposal unavailable in caller scope",
        ));
    };
    if hash != effect.expected_content_hash || digest_str(&payload) != hash {
        return Err(fail(
            "cortex_review_version_conflict",
            "proposal content changed",
        ));
    }
    let retry = effect.operation == "retry";
    if retry {
        let job: Option<(String, String)> = tx
            .query_row(
                "SELECT state,effect_hash FROM cortex_proposal_admission_v1 WHERE proposal_id=?1",
                [&effect.target_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(storage)?;
        let eligible =
            state == "approved" && job.as_ref().is_some_and(|(state, _)| state == "blocked");
        let revision = job.as_ref().map(|(_, hash)| hash.as_str());
        if !eligible || effect.expected_control_revision.as_deref() != revision {
            return Err(fail(
                "cortex_control_version_conflict",
                "retry must name the current blocked approval",
            ));
        }
    } else if state != "pending" {
        return Err(fail(
            "proposal_already_decided",
            "proposal has already been decided",
        ));
    }
    tx.execute(
        "INSERT INTO cortex_proposal_reviews_v1 VALUES(?1,?2,?3)",
        params![nonce_key, effect_hash, effect.target_id],
    )
    .map_err(storage)?;
    tx.execute(
        "UPDATE membrane_knowledge_proposal SET state=?2,decided_at=?3,reviewer=?4
        WHERE proposal_id=?1 AND state='pending'",
        params![
            effect.target_id,
            if effect.operation == "approve" {
                "approved"
            } else {
                "rejected"
            },
            crate::time::now_iso(),
            effect.key_id
        ],
    )
    .map_err(storage)?;
    if effect.operation == "approve" || retry {
        tx.execute(
            "INSERT INTO cortex_proposal_admission_v1(proposal_id,effect_json,effect_hash,state)
            VALUES(?1,?2,?3,'pending') ON CONFLICT(proposal_id) DO UPDATE SET
            effect_json=excluded.effect_json,effect_hash=excluded.effect_hash,state='pending',
            attempts=0,next_attempt_ms=0,last_error=NULL,receipt_json=NULL",
            params![effect.target_id, effect_json, effect_hash],
        )
        .map_err(storage)?;
    }
    tx.commit().map_err(storage)
}

/// Bounded reconciliation, called by the tray-owned maintenance loop and on
/// startup. It never promotes unsigned approvals or fabricates trust.
/// A crash-recoverable `admission_pending` job is a row in
/// `cortex_proposal_admission_v1` with `state='pending'`; convergence runs the
/// approved payload through normal Cortex admission idempotently.
pub fn recover_pending(store: &MemoryStore, limit: usize) -> Result<Value> {
    let ids = {
        let db = store.db().lock_events();
        ensure_proposal_schema(&db).map_err(storage)?;
        let mut stmt = db
            .prepare(
                "SELECT proposal_id FROM cortex_proposal_admission_v1
            WHERE state='pending' AND next_attempt_ms<=?1 ORDER BY proposal_id LIMIT ?2",
            )
            .map_err(storage)?;
        let rows = stmt
            .query_map(
                params![crate::time::now_millis() as i64, limit.min(32) as i64],
                |r| r.get::<_, String>(0),
            )
            .map_err(storage)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage)?;
        rows
    };
    let mut completed = 0;
    let mut failed = 0;
    for id in &ids {
        if recover_one(store, id).is_ok() {
            completed += 1;
        } else {
            failed += 1;
        }
    }
    Ok(
        json!({"schemaVersion":1,"operation":"cortex_admission_recovery","considered":ids.len(),
        "completed":completed,"deferredOrBlocked":failed,"complete":ids.len() < limit.min(32)}),
    )
}
fn recover_one(store: &MemoryStore, id: &str) -> Result<()> {
    let row = {
        let db = store.db().lock_events();
        ensure_proposal_schema(&db).map_err(storage)?;
        db.query_row(
            "SELECT a.effect_json,a.effect_hash,p.emission_json,p.repository_id,p.scope_id,p.emission_sha256,a.attempts
             FROM cortex_proposal_admission_v1 a JOIN membrane_knowledge_proposal p USING(proposal_id)
             WHERE a.proposal_id=?1 AND a.state='pending' AND p.state='approved'", [id],
            |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,
                r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,u32>(6)?)))
            .optional().map_err(storage)?
    };
    let Some((raw, effect_hash, payload, repository, scope, hash, attempts)) = row else {
        return Ok(());
    };
    let outcome = (|| {
        if digest_str(&raw) != effect_hash || digest_str(&payload) != hash {
            return Err(fail(
                "cortex_review_version_conflict",
                "persisted approval or proposal changed",
            ));
        }
        let effect: ReviewedEffectV1 = serde_json::from_str(&raw).map_err(|_| {
            fail(
                "cortex_review_invalid",
                "stored reviewed effect is malformed",
            )
        })?;
        if effect.target_id != id
            || effect.expected_content_hash != hash
            || !matches!(effect.operation.as_str(), "approve" | "retry")
            || effect.repository_id != repository
            || effect.scope_id != scope
        {
            return Err(fail(
                "cortex_review_binding_denied",
                "stored job does not match approved effect",
            ));
        }
        let mut payload: Value = serde_json::from_str(&payload).map_err(storage)?;
        if payload.get("kind").and_then(Value::as_str) == Some(MULTIWRITER_KIND) {
            // Revalidate every repository/scope/record/version/writer/clock/
            // payload binding on restart, before either replay lookup or the
            // normal Cortex admission transaction.
            validate_multiwriter_emission(&repository, &scope, &payload)
                .map_err(|code| fail("multiwriter_emission_invalid", code))?;
        }
        payload["scopeId"] = json!(scope);
        // A committed effect is an observed fact, not a new request to execute
        // an expired approval. Reconcile the acknowledgement before checking
        // present-day permission to execute an unfinished effect.
        if payload.get("kind").and_then(Value::as_str) == Some("temporal") {
            if let Some(receipt) = temporal_admission_receipt(store, &scope, &payload)? {
                return Ok(receipt);
            }
        } else if let Some(receipt) = store
            .reviewed_admission_receipt(id, &hash, &payload)
            .map_err(storage)?
        {
            return Ok(receipt);
        }
        verify_review(store, &effect, &repository, &scope)?;
        if payload.get("kind").and_then(Value::as_str) == Some("temporal") {
            return admit_temporal(store, id, &scope, &payload);
        }
        store
            .admit_reviewed_proposal(id, &hash, &payload, &scope)
            .map_err(storage)
    })();
    let db = store.db().lock_events();
    match outcome {
        Ok(receipt) => {
            db.execute(
                "UPDATE cortex_proposal_admission_v1 SET state='completed',receipt_json=?2,
                last_error=NULL,attempts=attempts+1 WHERE proposal_id=?1 AND state='pending'",
                params![id, serde_json::to_string(&receipt).map_err(storage)?],
            )
            .map_err(storage)?;
            Ok(())
        }
        Err(error) => {
            let retry = error.retryable && attempts < 7;
            let delay = 30_000i64.saturating_mul(1i64 << attempts.min(5));
            db.execute(
                "UPDATE cortex_proposal_admission_v1 SET state=?2,attempts=attempts+1,
                next_attempt_ms=?3,last_error=?4 WHERE proposal_id=?1 AND state='pending'",
                params![
                    id,
                    if retry { "pending" } else { "blocked" },
                    (crate::time::now_millis() as i64).saturating_add(delay),
                    error.code
                ],
            )
            .map_err(storage)?;
            Err(error)
        }
    }
}

/// A nested `temporalFact.scopeId` is caller-supplied payload, never an
/// independently verified binding. The durable record and every readback of
/// it are bound to `scope` (the caller/proposal scope carried by the signed
/// review), and a non-empty nested claim that disagrees is a fail-closed
/// binding denial rather than a silent overwrite that would hide an
/// attempted cross-scope escalation.
fn bind_temporal_scope(fact: &TemporalFact, scope: &str) -> Result<()> {
    if !fact.scope_id.is_empty() && fact.scope_id != scope {
        return Err(fail(
            "cortex_review_binding_denied",
            "temporal fact scope does not match the verified caller scope",
        ));
    }
    Ok(())
}

fn temporal_admission_receipt(
    store: &MemoryStore,
    scope: &str,
    payload: &Value,
) -> Result<Option<Value>> {
    let fact: TemporalFact =
        serde_json::from_value(payload["temporalFact"].clone()).map_err(|_| {
            fail(
                "temporal_fact_invalid",
                "stored temporal proposal is malformed",
            )
        })?;
    bind_temporal_scope(&fact, scope)?;
    let Some(receipt) = store
        .temporal_facts()
        .validity_receipt(&fact.fact_id)
        .map_err(storage)?
    else {
        return Ok(None);
    };
    Ok(Some(temporal_receipt_json(store, payload, &receipt)?))
}

fn temporal_receipt_json(
    store: &MemoryStore,
    payload: &Value,
    receipt: &TemporalValidityReceiptV1,
) -> Result<Value> {
    let utility = MemoryStore::admission_utility_eligibility(payload);
    Ok(json!({
        "outcome":"temporal_admitted",
        "factId":receipt.record_id,
        "payloadHash":receipt.payload_sha256,
        "authority":"A4",
        "independentlyVerified":true,
        "callerFieldsStripped":payload["callerFieldsStripped"],
        "cardinalityPolicy":payload["cardinalityPolicy"],
        "transitions":receipt.transitions,
        "cortexStoreId":store.cortex_store_id(),
        "utilityEligibility":utility
    }))
}

fn admit_temporal(
    store: &MemoryStore,
    proposal_id: &str,
    scope: &str,
    payload: &Value,
) -> Result<Value> {
    let utility = MemoryStore::admission_utility_eligibility(payload);
    if utility["eligible"] != true {
        return Ok(json!({
            "outcome":"utility_ineligible",
            "proposalId":proposal_id,
            "canonicalMutation":false,
            "utilityEligibility":utility
        }));
    }
    let mut fact: TemporalFact =
        serde_json::from_value(payload["temporalFact"].clone()).map_err(|_| {
            fail(
                "temporal_fact_invalid",
                "stored temporal proposal is malformed",
            )
        })?;
    bind_temporal_scope(&fact, scope)?;
    // A non-empty mismatch is already rejected above; once bound, the durable
    // record and its validation use the independently verified scope, never
    // a value the caller could omit or forge.
    fact.scope_id = scope.to_owned();
    validate_fact_proposal(&fact).map_err(|message| fail("temporal_fact_invalid", message))?;
    let policy = &payload["cardinalityPolicy"];
    let cardinality = policy.get("cardinality").and_then(Value::as_str);
    if policy.get("policyVersion").and_then(Value::as_str) != Some("cortex.temporal-cardinality.v1")
        || policy.get("predicate").and_then(Value::as_str) != Some(fact.predicate.as_str())
        || policy.get("source").and_then(Value::as_str) != Some("explicit_single_valued_predicates")
        || !matches!(cardinality, Some("single" | "multi"))
    {
        return Err(fail(
            "temporal_fact_invalid",
            "temporal cardinality policy is missing or inconsistent",
        ));
    }
    // CTX-009's fourth dimension. `TemporalValidityV1` carries only valid and
    // recorded time, so the observation instant has to be handed to the store
    // separately or it is lost at admission.
    let observed_at = fact.observed_at.clone();
    let record = TemporalValidityV1 {
        record_id: fact.fact_id,
        subject: fact.subject,
        predicate: fact.predicate,
        object: fact.object,
        // fact.scope_id was already normalized to the independently verified
        // caller/proposal scope above; a mismatching nested claim is rejected
        // before this point, never silently overwritten.
        scope_id: fact.scope_id,
        authority: "A4".into(),
        valid_at: TemporalInstantV1::known(fact.valid_from),
        // CTX-009: recorded_at is admission-time (when Cortex durably wrote
        // the record), distinct from fact.observed_at (when the fact was
        // first witnessed). Reuse this module's existing admission clock
        // idiom rather than introducing a new time source.
        recorded_at: TemporalInstantV1::known(crate::time::now_iso()),
        invalid_at: fact.valid_until,
        superseded_by: None,
        revoked: false,
        independently_verified: true,
        expires_at: fact.expires_at,
    };
    store
        .temporal_facts()
        .record_validity_observed(record, cardinality == Some("single"), Some(&observed_at))
        .map_err(storage)?;
    temporal_admission_receipt(store, scope, payload)?
        .ok_or_else(|| storage("temporal admission committed without readable canonical receipt"))
}

type MemoryEffectRecord = (
    String,
    String,
    String,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

fn apply_suppression(store: &MemoryStore, effect: &ReviewedEffectV1) -> Result<Value> {
    let hash = digest_str(&serde_json::to_string(effect).map_err(storage)?);
    let nonce_key = digest_str(
        &serde_json::to_string(&(
            &effect.key_id,
            &effect.repository_id,
            &effect.scope_id,
            &effect.nonce,
        ))
        .map_err(storage)?,
    );
    let mut db = store.db().lock();
    ensure_memory_schema(&db).map_err(storage)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    let previous: Option<(String, String)> = tx
        .query_row(
            "SELECT effect_hash,receipt_json FROM cortex_reviewed_controls_v1 WHERE nonce_key=?1",
            [&nonce_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(storage)?;
    if let Some((previous, receipt)) = previous {
        if previous != hash {
            return Err(fail(
                "cortex_review_replay_conflict",
                "nonce already binds another effect",
            ));
        }
        return serde_json::from_str(&receipt).map_err(storage);
    }
    let record: Option<MemoryEffectRecord> = tx.query_row(
        "SELECT content,authority,lifecycle_state,superseded_by,effective_from_ms,effective_until_ms,expires_at_ms
         FROM memories WHERE id=?1 AND scope_id=?2",
        params![effect.target_id,effect.scope_id],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))
        .optional().map_err(storage)?;
    let Some((content, authority, lifecycle, superseded, from, until, expiry)) = record else {
        return Err(fail(
            "memory_unavailable",
            "record unavailable in caller scope",
        ));
    };
    if digest_str(&content) != effect.expected_content_hash {
        return Err(fail("memory_version_conflict", "record content changed"));
    }
    if effect.operation == "resume" {
        let now = crate::time::now_millis() as i64;
        if authority == "A0"
            || lifecycle != "active"
            || superseded.is_some()
            || from.is_some_and(|t| now < t)
            || until.is_some_and(|t| now >= t)
            || expiry.is_some_and(|t| now >= t)
        {
            return Err(fail(
                "memory_ineligible",
                "record is not currently eligible for suppression reversal",
            ));
        }
    }
    let previous_revision: Option<String> = tx
        .query_row(
            "SELECT decision_hash FROM cortex_recall_suppression_v1 WHERE memory_id=?1",
            [&effect.target_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage)?;
    if effect.expected_control_revision.as_deref()
        != Some(previous_revision.as_deref().unwrap_or("none"))
    {
        return Err(fail(
            "cortex_control_version_conflict",
            "suppression decision changed; fresh authorization is required",
        ));
    }
    let suppressed = effect.operation == "suppress";
    tx.execute("INSERT INTO cortex_recall_suppression_v1 VALUES(?1,?2,?3,?4,?5)
        ON CONFLICT(memory_id) DO UPDATE SET scope_id=excluded.scope_id,content_hash=excluded.content_hash,
        suppressed=excluded.suppressed,decision_hash=excluded.decision_hash",
        params![effect.target_id,effect.scope_id,effect.expected_content_hash,suppressed as i64,hash]).map_err(storage)?;
    let receipt = json!({"schemaVersion":1,"operation":effect.operation,"memoryId":effect.target_id,
        "contentHash":effect.expected_content_hash,"suppressed":suppressed,"decisionHash":hash,
        "cortexStoreId":store.cortex_store_id(),"authorityChanged":false,"payloadErased":false});
    tx.execute(
        "INSERT INTO cortex_reviewed_controls_v1 VALUES(?1,?2,?3)",
        params![
            nonce_key,
            hash,
            serde_json::to_string(&receipt).map_err(storage)?
        ],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)?;
    Ok(receipt)
}

/// Exact, bounded native record resolution. The content hash binds the whole
/// body; pagination never silently truncates a full-record claim. This reads
/// canonical state, not the potentially stale in-memory registry.
pub fn resolve_memory(
    store: &MemoryStore,
    scope: &str,
    id: &str,
    expected: &str,
    offset: usize,
    max_chars: usize,
) -> Result<Value> {
    if id.is_empty() || max_chars == 0 || max_chars > 12_000 {
        return Err(fail(
            "memory_envelope_invalid",
            "id and a bounded character limit are required",
        ));
    }
    let mut db = store.db().lock();
    ensure_memory_schema(&db).map_err(storage)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Deferred)
        .map_err(storage)?;
    #[allow(clippy::type_complexity)]
    let row: Option<(String,String,String,String,Option<i64>,Option<i64>,Option<i64>,Option<String>,String,String,String,String)> = tx.query_row(
        "SELECT content,authority,lifecycle_state,source_ids,effective_from_ms,effective_until_ms,expires_at_ms,
          superseded_by,record_type,producer,sensitivity,derivation FROM memories WHERE id=?1 AND scope_id=?2", params![id,scope],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?,r.get(11)?)))
        .optional().map_err(storage)?;
    let Some((
        content,
        authority,
        lifecycle,
        sources,
        from,
        until,
        expiry,
        superseded,
        kind,
        producer,
        sensitivity,
        derivation_column,
    )) = row
    else {
        return Err(fail(
            "memory_unavailable",
            "record unavailable in caller scope",
        ));
    };
    let suppressed: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM cortex_recall_suppression_v1 WHERE memory_id=?1 AND suppressed=1)", [id], |r| r.get(0)).map_err(storage)?;
    let now = crate::time::now_millis() as i64;
    if authority == "A0"
        || lifecycle != "active"
        || superseded.is_some()
        || suppressed
        || from.is_some_and(|t| now < t)
        || until.is_some_and(|t| now >= t)
        || expiry.is_some_and(|t| now >= t)
    {
        return Err(fail(
            "memory_ineligible",
            "record is not currently eligible for agent resolution",
        ));
    }
    let hash = digest_str(&content);
    let expected_norm = expected.strip_prefix("sha256:").unwrap_or(expected);
    // CTX-043: the caller's expected hash also binds the immutable push-source
    // rows recorded against this memory. A pushed body that dedup-merged into
    // an existing canonical record resolves by its own `raw_sha256` to the
    // preserved submitted bytes — the admission record, not the derived
    // canonical content.
    let source_rows = agent_memory_sources_for_on(&tx, id).map_err(storage)?;
    let matched_source = source_rows.iter().find(|row| {
        row.raw_sha256
            .strip_prefix("sha256:")
            .unwrap_or(&row.raw_sha256)
            == expected_norm
    });
    if hash.strip_prefix("sha256:").unwrap_or(&hash) != expected_norm && matched_source.is_none() {
        return Err(fail(
            "memory_version_conflict",
            "expected full-content hash no longer matches",
        ));
    }
    let (served, served_hash, served_from): (String, String, &str) =
        if hash.strip_prefix("sha256:").unwrap_or(&hash) == expected_norm {
            (content, hash, "canonical")
        } else {
            let source = matched_source.expect("matched source row");
            let text = String::from_utf8(source.raw_body.clone()).map_err(|_| {
                fail(
                    "memory_unavailable",
                    "immutable source bytes are not valid UTF-8",
                )
            })?;
            (text, source.raw_sha256.clone(), "immutableSource")
        };
    let matched_request_id = matched_source.map(|row| row.request_id.clone());
    let total = served.chars().count();
    if offset > total {
        return Err(fail(
            "memory_envelope_invalid",
            "offset is beyond the record",
        ));
    }
    let body = served
        .chars()
        .skip(offset)
        .take(max_chars)
        .collect::<String>();
    let end = offset + body.chars().count();
    // A persisted observed read is not verified helped and does not reinforce
    // usefulness. Scope and content are bound before this counter changes.
    tx.execute(
        "UPDATE memories SET access_count=access_count+1 WHERE id=?1",
        [id],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)?;
    Ok(
        json!({"schemaVersion":1,"id":id,"cortexStoreId":store.cortex_store_id(),
        "contentHash":served_hash,"content":body,"offset":offset,"nextOffset":if end < total {Some(end)} else {None},
        "totalChars":total,"complete":offset==0 && end==total,"pageComplete":true,"authority":authority,"lifecycle":lifecycle,
        // CTX-043: whether the served bytes are the canonical record or an
        // immutable push-source row bound to it is explicit data, never an
        // implicit guess from which hash matched.
        "sourceRecord":{"present":!source_rows.is_empty(),
            "requestIds":source_rows.iter().map(|row| row.request_id.clone()).collect::<Vec<_>>(),
            "matchedRequestId":matched_request_id,"served":served_from},
        "scopeId":scope,"recordType":kind,"sourceRefs":serde_json::from_str::<Value>(&sources).map_err(storage)?,
        // Derived support: distinct unioned provenance refs (paraphrase and
        // duplicate admissions union into the surviving record).
        "supportCount":crate::store::provenance_support_count(
            &serde_json::from_str::<Vec<Value>>(&sources).unwrap_or_default()
                .iter().map(Value::to_string).collect::<Vec<_>>()),
        // CTX-004: provenance availability is explicit data, never guessed.
        // Legacy rows without recorded sources report unavailable_legacy while
        // authority, origin, lifecycle and derivation stay distinct fields.
        "provenanceAvailability":if sources.trim() != "[]" && !sources.trim().is_empty() {"explicit"} else {"unavailable_legacy"},
        "originProducer":producer,
        // CTX-004: sensitivity is a durable column bound at write time; a row
        // a writer never classified reports the explicit "unavailable_legacy"
        // marker rather than a guessed classification.
        "sensitivity":sensitivity,
        // Derivation prefers an explicit durable `derivation` value when a
        // writer recorded one; failing that, known supersession lineage
        // (superseded_by is never inferred, so this is still explicit, not
        // guessed); failing that, the row's derivation is bound to the
        // explicit "unavailable_legacy" marker, never Null/guessed.
        "derivation":if derivation_column != rightkit_memory_store::memdb::UNAVAILABLE_LEGACY {
            serde_json::from_str::<Value>(&derivation_column).unwrap_or(Value::String(derivation_column))
        } else if let Some(parent) = superseded.as_deref() {
            json!({"supersededBy":parent})
        } else {
            Value::String(rightkit_memory_store::memdb::UNAVAILABLE_LEGACY.to_string())
        },
        "observedResolution":true,"verifiedHelped":false}),
    )
}

/// CTX-040 minimal named/versioned retrieval recipe. Recipe selection changes
/// retrieval only; fallback must be explicit & every bound/projection is
/// echoed in receipt.
pub fn recall_recipe(store: &MemoryStore, scope: &str, args: &Value) -> Result<Value> {
    let query = args
        .get("query")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| fail("memory_recipe_invalid", "query is required"))?;
    let recipe = args
        .get("recipe")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            fail(
                "memory_recipe_invalid",
                "recipe name and version are required",
            )
        })?;
    let requested_name = recipe.get("name").and_then(Value::as_str).unwrap_or("");
    let requested_version = recipe.get("version").and_then(Value::as_u64).unwrap_or(0);
    if requested_name.is_empty() || requested_version == 0 {
        return Err(fail(
            "memory_recipe_invalid",
            "recipe name and positive version are required",
        ));
    }
    let supported = requested_name == "cortex.hybrid" && requested_version == 1;
    let fallback_allowed = recipe
        .get("allowFallback")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !supported && !fallback_allowed {
        return Err(fail(
            "memory_recipe_unsupported",
            format!("unsupported recall recipe {requested_name}@{requested_version}"),
        ));
    }
    let bounds = args.get("bounds").and_then(Value::as_object);
    let requested_items = bounds
        .and_then(|value| value.get("maxItems"))
        .and_then(Value::as_u64)
        .unwrap_or(8);
    let requested_preview = bounds
        .and_then(|value| value.get("maxPreviewChars"))
        .and_then(Value::as_u64)
        .unwrap_or(320);
    let max_items = requested_items.clamp(1, 32) as usize;
    let max_preview = requested_preview.min(2_000) as usize;
    let projection = args
        .get("projection")
        .and_then(Value::as_str)
        .unwrap_or("preview");
    if !matches!(projection, "metadata" | "preview") {
        return Err(fail(
            "memory_recipe_invalid",
            "projection must be metadata or preview",
        ));
    }
    let scopes = vec![scope.to_owned()];
    // A named recipe resolves to a frozen configuration of existing lanes. Keep
    // relationship expansion off here: CTX-040 may select lexical/vector recall,
    // but it cannot implicitly enable the CTX-039 evidence-path capability.
    let cancellation = tokio_util::sync::CancellationToken::new();
    // The store's own completeness envelope is the single source of truth for
    // considered/returned/dropped counts and drop causes (cancellation, tail
    // truncation, scope narrowing, etc). Recomputing those counts here from
    // `hits.len()` alone would let this surface's envelope silently disagree
    // with the store's — thread the store's envelope through unchanged.
    let (hits, _, completeness) = store.recall_scored_detailed_timed_cancellable(
        query,
        max_items,
        &scopes,
        false,
        &cancellation,
    );
    let actual_recipe = json!({
        "name":"cortex.hybrid",
        "version":1,
        "lanes":["lexical","vector"],
        "graphTraversal":false,
        "maxItems":max_items,
        "maxPreviewChars":max_preview,
        "projection":projection
    });
    let recipe_digest = digest_str(&serde_json::to_string(&actual_recipe).map_err(storage)?);
    let items = hits
        .into_iter()
        .map(|hit| {
            let entry = hit.entry;
            let score = hit.score;
            let content_hash = digest_str(&entry.content);
            let resolver = format!(
                "membrane_memory_read:membrane_memory:get:{}:{}",
                entry.id, content_hash
            );
            let mut item = json!({
                "id":entry.id,
                "scopeId":entry.scope_id,
                "tier":entry.tier,
                "score":score,
                "contentHash":content_hash,
                "resolver":resolver
            });
            if projection == "preview" {
                item["preview"] =
                    json!(entry.content.chars().take(max_preview).collect::<String>());
            }
            item
        })
        .collect::<Vec<_>>();
    // CTX-038: the agent-visible recall surface must carry an honest
    // completeness envelope, not just items + recipe metadata. Reuse the
    // canonical Cortex envelope vocabulary (CortexCompletenessV1) so this
    // surface cannot silently disagree with the lower-level store envelope
    // it is built on.
    let completeness = serde_json::to_value(&completeness).map_err(storage)?;
    Ok(json!({
        "schemaVersion":1,
        "operation":"recall",
        "items":items,
        "completeness":completeness,
        "receipt":{
            "schemaVersion":1,
            "policyVersion":"cortex.recall-recipe.v1",
            "recipeDigest":recipe_digest,
            "requested":{"name":requested_name,"version":requested_version},
            "actual":{
                "name":"cortex.hybrid",
                "version":1,
                "lanes":["lexical","vector"],
                "graphTraversal":false
            },
            "fallback":{
                "used":!supported,
                "reason":if supported {Value::Null} else {json!("unsupported_requested_recipe")}
            },
            "bounds":{
                "requested":{"maxItems":requested_items,"maxPreviewChars":requested_preview},
                "actual":{"maxItems":max_items,"maxPreviewChars":max_preview}
            },
            "projection":{"requested":projection,"actual":projection},
            "retrievalOnly":true,
            "scopeId":scope
        }
    }))
}

pub fn promote_checkpoint(
    store: &MemoryStore,
    repository: &str,
    scope: &str,
    id: &str,
) -> Result<Value> {
    let checkpoint = store
        .load_checkpoint(id, crate::time::now_millis() as i64)
        .map_err(storage)?;
    if checkpoint.repository_id != repository || checkpoint.scope_id != scope {
        return Err(fail(
            "checkpoint_scope_denied",
            "checkpoint does not match caller",
        ));
    }
    propose(
        store,
        repository,
        scope,
        &json!({"text":checkpoint.summary,"kind":"episodic",
        "producer":"checkpoint","epistemicClass":"reported",
        "scopeId":scope,"checkpointId":checkpoint.checkpoint_id,"sessionId":checkpoint.session_id,
        "sourceRefs":checkpoint.source_refs,"worktreeRevision":checkpoint.worktree_rev}),
    )
}

/// CTX-023/CTX-024 Cortex-specific proposal sink: drain installation-local
/// background-review JSONL records into the governed proposal queue.
///
/// Each record is validated and submitted through [`propose`], so the ordered
/// pre-gate, pending-only queueing, and signed-review requirements apply
/// unchanged. The drain preserves the provider-attested record scope as the
/// caller binding; forging that scope requires write access to the
/// installation-local file, which already implies operator trust. Draining is
/// at-least-once and safe to retry: proposal identities are deterministic, so
/// a re-drain is a typed idempotent no-op, never a second record.
///
/// Queue progress is tracked by an advancing, crash-safe line cursor
/// persisted in a sibling `<queue>.cursor` file. A missing or corrupt cursor
/// starts from 0; a cursor beyond the current line count means the queue was
/// truncated or rotated underneath us, so it also resets to 0 rather than
/// silently skipping the whole (new, shorter) file. The new offset is written
/// only after this batch's proposals are durably recorded, so a crash between
/// admission and the cursor write simply re-processes the batch (safe: see
/// idempotency above), never skips it.
pub fn drain_background_proposals(
    store: &MemoryStore,
    repository: &str,
    path: &std::path::Path,
    limit: usize,
) -> Result<Value> {
    use rightkit_memory_core::review::{
        validate_semantic_curation_proposals, MemoryCandidateV1, SemanticCurationProposalV1,
    };
    if limit == 0 {
        return Err(fail(
            "memory_envelope_invalid",
            "drain limit must be greater than zero",
        ));
    }
    let body = std::fs::read_to_string(path).map_err(|_| {
        fail(
            "cortex_storage_unavailable",
            "background proposal file is unavailable",
        )
    })?;
    let lines: Vec<&str> = body.lines().collect();
    let total_lines = lines.len();
    let cursor_path = {
        let mut name = path.as_os_str().to_owned();
        name.push(".cursor");
        std::path::PathBuf::from(name)
    };
    let mut cursor_start = std::fs::read_to_string(&cursor_path)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if cursor_start > total_lines {
        // The queue was truncated or rotated underneath us; a stale cursor
        // must not skip the entire (now shorter) file.
        cursor_start = 0;
    }
    let mut considered = 0usize;
    let mut proposed = 0usize;
    let mut proposal_ids = Vec::new();
    let mut failures = Vec::new();
    let mut consumed = cursor_start;
    for (index, line) in lines.iter().enumerate().skip(cursor_start) {
        if considered >= limit {
            break;
        }
        consumed = index + 1;
        let line_no = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        considered += 1;
        let record: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(_) => {
                failures.push(json!({"line":line_no,"code":"proposal_emission_text_required"}));
                continue;
            }
        };
        let outcome = (|| {
            let kind = record.get("kind").and_then(Value::as_str).unwrap_or("");
            let proposal = record.get("proposal").cloned().unwrap_or(Value::Null);
            match kind {
                "semantic_curation" => {
                    let curation: SemanticCurationProposalV1 = serde_json::from_value(proposal)
                        .map_err(|_| {
                            fail(
                                "proposal_emission_text_required",
                                "malformed curation record",
                            )
                        })?;
                    validate_semantic_curation_proposals(std::slice::from_ref(&curation)).map_err(
                        |_| fail("proposal_emission_text_required", "invalid curation record"),
                    )?;
                    let evidence: Vec<String> = curation
                        .evidence
                        .iter()
                        .flat_map(|item| {
                            item.memory_ids
                                .iter()
                                .chain(item.source_event_ids.iter())
                                .cloned()
                        })
                        .take(64)
                        .collect();
                    let text = format!(
                        "Stage 1 {:?} review for memories [{}] per evidence [{}] (curation {})",
                        curation.kind,
                        curation.target_memory_ids.join(","),
                        evidence.join(","),
                        curation.proposal_id,
                    );
                    propose(
                        store,
                        repository,
                        &curation.scope_id,
                        &json!({
                            "text": text,
                            "kind": "semantic_curation",
                            "producer": "dream",
                            "epistemicClass": "inferred",
                            "curationId": curation.proposal_id,
                            "curationKind": format!("{:?}", curation.kind),
                            "targetMemoryIds": curation.target_memory_ids,
                            "jobId": record.get("jobId"),
                        }),
                    )
                }
                "memory_candidate" => {
                    let candidate: MemoryCandidateV1 =
                        serde_json::from_value(proposal).map_err(|_| {
                            fail(
                                "proposal_emission_text_required",
                                "malformed candidate record",
                            )
                        })?;
                    if candidate.content.trim().is_empty() || candidate.scope_id.trim().is_empty() {
                        return Err(fail(
                            "proposal_emission_text_required",
                            "candidate carries no content or scope",
                        ));
                    }
                    propose(
                        store,
                        repository,
                        &candidate.scope_id,
                        &json!({
                            "text": candidate.content,
                            "kind": "episodic",
                            "producer": "episodic",
                            "epistemicClass": "reported",
                            "candidateId": candidate.candidate_id,
                            "sessionId": candidate.session_id,
                            "fromSeq": candidate.from_seq,
                            "toSeq": candidate.to_seq,
                            "sourceEventIds": candidate.source_event_ids,
                            "sourceContentHashes": candidate.source_content_hashes,
                            "jobId": record.get("jobId"),
                        }),
                    )
                }
                "adapt_proposal" => {
                    // ADP-029/035: Adapt learner output enters the same
                    // daemon-owned admission boundary — pre-gates, pending
                    // review, and idempotency stay intact.
                    let adapt: rightkit_adapt::learner::AdaptLearnerProposalV1 =
                        serde_json::from_value(proposal).map_err(|_| {
                            fail(
                                "proposal_emission_text_required",
                                "malformed adapt proposal record",
                            )
                        })?;
                    adapt.validate().map_err(|_| {
                        fail(
                            "proposal_emission_text_required",
                            "invalid adapt proposal record",
                        )
                    })?;
                    let scope = crate::scope::normalize_scope(&adapt.scope_id);
                    propose(
                        store,
                        repository,
                        &scope,
                        &json!({
                            "text": adapt.summary,
                            "kind": "adapt_review",
                            "producer": "adapt_native",
                            "epistemicClass": "inferred",
                            "adaptProposalId": adapt.proposal_id,
                            "findingClass": adapt.finding_class.as_str(),
                            "sessionId": adapt.session_id,
                            "fromSeq": adapt.first_seq,
                            "toSeq": adapt.last_seq.saturating_add(1),
                            "sourceEventIds": adapt.evidence.source_event_ids,
                            "sourceContentHashes": adapt.evidence.source_content_hashes,
                            "jobId": record.get("jobId"),
                        }),
                    )
                }
                _ => Err(fail(
                    "proposal_emission_text_required",
                    "unknown background proposal kind",
                )),
            }
        })();
        match outcome {
            Ok(receipt) => {
                proposed += 1;
                if let Some(id) = receipt.get("proposalId").and_then(Value::as_str) {
                    proposal_ids.push(id.to_owned());
                }
            }
            Err(error) => failures.push(json!({"line":line_no,"code":error.code})),
        }
    }
    // Persist the advancing cursor only now that every proposal in this batch
    // is durably recorded (propose() commits synchronously above), so a crash
    // before this write simply re-drains the same batch rather than skipping
    // unconsumed records.
    std::fs::write(&cursor_path, consumed.to_string()).map_err(storage)?;
    Ok(
        json!({"schemaVersion":1,"operation":"drain_background_proposals",
        "considered":considered,"proposed":proposed,"failed":failures.len(),
        "proposalIds":proposal_ids,"failures":failures,
        "complete":consumed >= total_lines}),
    )
}

pub(crate) fn drain_installation_background_proposals(
    store: &MemoryStore,
    limit: usize,
) -> Result<Value> {
    if limit == 0 {
        return Err(fail(
            "memory_envelope_invalid",
            "drain limit must be greater than zero",
        ));
    }
    let Some(root) = crate::adapt_service::learner_workspace_root(store) else {
        return Err(fail(
            "background_proposal_root_unavailable",
            "background proposal workspace root is unavailable",
        ));
    };
    let Some(path) = crate::adapt_service::learner_proposals_path(store) else {
        return Err(fail(
            "background_proposal_root_unavailable",
            "background proposal workspace root is unavailable",
        ));
    };
    if !path.is_file() {
        return Err(fail(
            "background_proposal_queue_absent",
            "background proposal queue is absent",
        ));
    }
    let repository = rightkit_context_federation::root::canonical_repository_id(&root);
    drain_background_proposals(store, &repository, &path, limit)
}

/// Supervised by serve::run, never a standalone service. The owner supplies the
/// same resident store handle used by MCP. A stop wakes the idle wait; work is
/// bounded to four journals per tick and seven attempts per journal.
pub(crate) struct AdmissionRecoveryWorker {
    stop: std::sync::mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl AdmissionRecoveryWorker {
    pub(crate) fn start(store: MemoryStore) -> std::result::Result<Self, String> {
        let (stop, stopped) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new().name("cortex-admission-recovery".into()).spawn(move || {
            let mut previously_unavailable = false;
            let mut previously_drain_unavailable = false;
            loop {
                if stopped.try_recv().is_ok() { break; }
                match drain_installation_background_proposals(&store, 8) {
                    Ok(summary) => {
                        if summary["considered"].as_u64().unwrap_or(0) > 0 || previously_drain_unavailable {
                            stderr_line!("cortex-lifecycle {summary}");
                        }
                        previously_drain_unavailable = false;
                    }
                    Err(error) => {
                        if !previously_drain_unavailable {
                            stderr_line!("cortex-lifecycle {}", json!({"operation":"background_proposal_drain","status":"unavailable","code":error.code}));
                        }
                        previously_drain_unavailable = true;
                    }
                }
                match recover_pending(&store, 4) {
                    Ok(summary) => {
                        if summary["considered"].as_u64().unwrap_or(0) > 0 || previously_unavailable {
                            stderr_line!("cortex-lifecycle {summary}");
                        }
                        previously_unavailable = false;
                    }
                    Err(error) => {
                        if !previously_unavailable {
                            // No payloads, prompts, signatures or storage paths in telemetry.
                            stderr_line!("cortex-lifecycle {}", json!({"operation":"cortex_admission_recovery","status":"unavailable","code":error.code}));
                        }
                        previously_unavailable = true;
                    }
                }
                match stopped.recv_timeout(std::time::Duration::from_secs(30)) {
                    Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
            }
        }).map_err(|error| error.to_string())?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for AdmissionRecoveryWorker {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
