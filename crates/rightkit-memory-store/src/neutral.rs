//! Neutral memory export and additive import (RK-7).
//!
//! The neutral form is one JSON object per line, UTF-8, `\n` line ends, field order fixed by the
//! structs below. It carries each memory across store layouts: `export_neutral` reads a store
//! read-only, whatever layout it has, and `import_neutral` adds the lines to a store in the
//! current layout.
//!
//! Rules this module keeps:
//! - The body is carried as a JSON string and compared by SHA-256 over its UTF-8 bytes, so a
//!   viewer that normalises line ends or Unicode cannot change it unseen. A body with CRLF line
//!   ends, an emoji and trailing whitespace comes back identical.
//! - Export only reads. It never writes, creates a table, sets a file aside or opens a
//!   transaction that writes. [`export_neutral_from_path`] opens the file read-only and never
//!   creates it.
//! - Import only adds. It never updates or deletes a row, and it never calls the backup
//!   restore, which wipes first. An id already present with the same body is
//!   [`NeutralOutcome::AlreadyPresent`]; with a different body it is rejected, never overwritten.
//!   An id on the `deletions` tombstone list or in `memory_quarantine` is rejected, so a memory
//!   someone deleted or quarantined does not come back. Each line is its own transaction, so a
//!   rejected line leaves nothing behind.
//! - Embeddings are not carried. Imported rows have no vector (`embedding_q` and `embed_model`
//!   are NULL); re-embedding is the caller's job. The `cortex_fts5` lexical projection is
//!   written for each inserted row, inside the same transaction.
//! - The `links` table (wikilink edges derived from a body) is not written here: it is derived
//!   from content by the runtime and is rebuilt by the caller's reindex.
//!
//! Layout reading: export asks the connection which columns `memories` has and substitutes the
//! current schema's default for a column the layout lacks, so a past layout is read without the
//! build that wrote it. The v27 layout's extra `embedding` column is only counted, never read.

use crate::fts5::{Fts5Document, Fts5Projection};
use rightkit_db::rusqlite::{self, params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::Path;
use std::time::Duration;

/// Format name, written on every line so a line can be checked alone. It follows the crate
/// family's `rightkit.<area>.<thing>.vN` schema naming and stays distinct from
/// `membrane.cortex-backup.v3`.
pub const NEUTRAL_FORMAT: &str = "rightkit.memory.neutral.v1";

/// The draft name the Membrane dry-run export (`me2-memory-return.md`, section 4) wrote. The
/// body of the line is the same form, so import accepts it; export never writes it.
pub const NEUTRAL_FORMAT_DRAFT: &str = "membrane.memory-export.v0-draft";

/// Errors that stop an export or an import as a whole. A problem with one import line is a
/// [`NeutralOutcome::Rejected`], not an error.
#[derive(Debug, thiserror::Error)]
pub enum NeutralError {
    #[error("neutral memory store: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("neutral memory io: {0}")]
    Io(#[from] std::io::Error),
    #[error("neutral memory json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the store has no memories table")]
    NoMemoriesTable,
}

/// One `memory_identity` row, carried as found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeutralIdentity {
    pub memory_id: String,
    pub artifact_id: String,
    pub origin_event_uid: String,
    pub installation_id: Option<String>,
    pub client: String,
    pub session_id: String,
    pub turn_id: String,
    pub trace_id: String,
    pub identity_status: String,
    pub created_at: String,
}

/// One `cortex_agent_memory_source_v1` row without its body: the body of those rows equals the
/// memory's `content` byte for byte, and `raw_sha256` proves it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeutralAgentSource {
    pub request_id: String,
    pub repository_id: String,
    pub scope_id: String,
    pub caller_id: String,
    pub raw_sha256: String,
    pub recorded_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeutralProvenance {
    pub producer: String,
    pub authority: String,
    pub artifact_family: String,
    pub record_type: String,
    pub influence_class: String,
    pub derivation: String,
    pub sensitivity: String,
    /// The stored JSON array. A stored value that is not valid JSON is carried as a string.
    pub source_ids: Value,
    pub confidence: Option<f64>,
    pub confidence_basis: Option<String>,
    #[serde(default)]
    pub identity: Option<NeutralIdentity>,
    #[serde(default)]
    pub agent_sources: Vec<NeutralAgentSource>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeutralLifecycle {
    pub effective_from_ms: Option<i64>,
    pub effective_until_ms: Option<i64>,
    pub expires_at_ms: Option<i64>,
    pub review_after_ms: Option<i64>,
    pub superseded_by: Option<String>,
    pub priority_class: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeutralUsage {
    pub score: f64,
    pub access_count: i64,
    pub inject_count: i64,
}

/// What the source row held that is not carried: evidence only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeutralDerived {
    pub embed_model: Option<String>,
    pub embedding_q_bytes: Option<i64>,
    pub embedding_dropped: bool,
}

/// Where the lines came from. `file`, `file_sha256`, `machine` and `set_aside` are supplied by the
/// caller (this crate does not know them); `user_version` is read from the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeutralSourceStore {
    pub file: Option<String>,
    pub user_version: Option<i64>,
    pub file_sha256: Option<String>,
    pub machine: Option<String>,
    pub set_aside: Option<String>,
}

/// The caller-known part of `source_store`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NeutralSource {
    pub file: Option<String>,
    pub file_sha256: Option<String>,
    pub machine: Option<String>,
    pub set_aside: Option<String>,
}

/// One line of the neutral form. Field order is the order written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeutralMemory {
    pub format: String,
    pub id: String,
    pub scope: String,
    /// The decoded tier word (the store keeps it as a JSON string, quotes included).
    pub tier: String,
    pub name: String,
    /// `id_suffix_after_scope_prefix` when the id is `scope/name`, `id_unqualified_legacy` when
    /// the whole id is the name (the values the Membrane dry-run export wrote). Informational:
    /// import does not use it.
    pub name_source: String,
    pub content: String,
    pub content_bytes: u64,
    pub content_sha256: String,
    pub keywords: Value,
    pub created: String,
    /// ISO text as found; `null` when the store has none (an empty stored value is `null`).
    pub updated: Option<String>,
    pub state: String,
    pub provenance: NeutralProvenance,
    pub lifecycle: NeutralLifecycle,
    pub usage: NeutralUsage,
    pub derived_not_exported: NeutralDerived,
    #[serde(default)]
    pub source_store: Option<NeutralSourceStore>,
}

/// What happened to one import line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NeutralOutcome {
    /// The memory was added.
    Inserted,
    /// The id is present with the same body; nothing was written.
    AlreadyPresent,
    /// The line was refused; nothing was written for it.
    Rejected(NeutralRejection),
}

/// Why a line was refused: a stable `code` and a human-readable `detail`.
///
/// Codes: `not_utf8`, `bad_json`, `wrong_format`, `empty_id`, `empty_scope`, `bytes_mismatch`,
/// `hash_mismatch`, `id_conflict`, `tombstoned`, `quarantined`, `identity_mismatch`,
/// `identity_conflict`, `agent_source_mismatch`, `agent_source_conflict`, `storage_error`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeutralRejection {
    pub code: &'static str,
    pub detail: String,
}

/// The outcome of one input line, 1-based. Blank lines produce no outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeutralLineOutcome {
    pub line: usize,
    pub id: Option<String>,
    pub outcome: NeutralOutcome,
    /// Things that did not carry for an inserted or present line (for example agent-source rows
    /// skipped because the store has no such table).
    pub notes: Vec<String>,
}

fn reject(code: &'static str, detail: impl Into<String>) -> NeutralOutcome {
    NeutralOutcome::Rejected(NeutralRejection {
        code,
        detail: detail.into(),
    })
}

fn sha256_hex(text: &str) -> String {
    rightkit_fs::digest::sha256_bytes(text.as_bytes())
}

fn table_present(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )
}

fn table_columns(conn: &Connection, table: &str) -> rusqlite::Result<BTreeSet<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect()
}

fn decode_tier(raw: &str) -> String {
    serde_json::from_str::<String>(raw).unwrap_or_else(|_| raw.to_string())
}

fn text_to_value(raw: String) -> Value {
    serde_json::from_str::<Value>(&raw).unwrap_or(Value::String(raw))
}

fn value_to_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn name_of(id: &str, scope: &str) -> (String, &'static str) {
    match id
        .strip_prefix(scope)
        .and_then(|rest| rest.strip_prefix('/'))
    {
        Some(name) if !name.is_empty() => (name.to_string(), "id_suffix_after_scope_prefix"),
        _ => (id.to_string(), "id_unqualified_legacy"),
    }
}

// ─── Export ──────────────────────────────────────────────────────────────────

/// Write every memory in `conn` as one neutral line to `out`; returns the line count.
///
/// Read-only: only SELECTs run. Works on any layout whose `memories` table has at least `id`,
/// `tier` and `content` (see the module docs). Lines are ordered by scope, then id. Embeddings
/// are not exported; the line records only the model name, the byte length and that one was
/// dropped. `identity` and `agent_sources` are filled when those tables exist.
pub fn export_neutral(
    conn: &Connection,
    out: &mut dyn Write,
    source: &NeutralSource,
) -> Result<usize, NeutralError> {
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let cols = table_columns(conn, "memories")?;
    for required in ["id", "tier", "content"] {
        if !cols.contains(required) {
            return Err(NeutralError::NoMemoriesTable);
        }
    }
    let pick = |name: &str, default: &str| -> String {
        if cols.contains(name) {
            name.to_string()
        } else {
            default.to_string()
        }
    };
    let embedding_q_len = if cols.contains("embedding_q") {
        "length(embedding_q)"
    } else {
        "NULL"
    };
    let legacy_embedding_len = if cols.contains("embedding") {
        "length(embedding)"
    } else {
        "NULL"
    };
    let sql = format!(
        "SELECT id, {scope}, tier, content, {keywords}, {created}, {updated}, {state},
                {producer}, {authority}, {family}, {rtype}, {influence}, {derivation},
                {sensitivity}, {source_ids}, {confidence}, {confidence_basis},
                {eff_from}, {eff_until}, {expires}, {review}, {superseded}, {priority},
                {score}, {access}, {inject}, {embed_model}, {embedding_q_len}, {legacy_embedding_len}
           FROM memories ORDER BY 2, 1",
        scope = pick("scope_id", "'global'"),
        keywords = pick("keywords", "'[]'"),
        created = pick("created_at", "''"),
        updated = pick("updated_at", "''"),
        state = pick("lifecycle_state", "'active'"),
        producer = pick("producer", "'manual'"),
        authority = pick("authority", "'A2'"),
        family = pick("artifact_family", "'memory'"),
        rtype = pick("record_type", "'memory'"),
        influence = pick("influence_class", "'reference'"),
        derivation = pick("derivation", "'unavailable_legacy'"),
        sensitivity = pick("sensitivity", "'unavailable_legacy'"),
        source_ids = pick("source_ids", "'[]'"),
        confidence = pick("confidence", "NULL"),
        confidence_basis = pick("confidence_basis", "NULL"),
        eff_from = pick("effective_from_ms", "NULL"),
        eff_until = pick("effective_until_ms", "NULL"),
        expires = pick("expires_at_ms", "NULL"),
        review = pick("review_after_ms", "NULL"),
        superseded = pick("superseded_by", "NULL"),
        priority = pick("priority_class", "'normal'"),
        score = pick("score", "0.0"),
        access = pick("access_count", "0"),
        inject = pick("inject_count", "0"),
        embed_model = pick("embed_model", "NULL"),
    );

    let identity_present = table_present(conn, "memory_identity")?;
    let sources_present = table_present(conn, "cortex_agent_memory_source_v1")?;
    let mut identity_stmt = if identity_present {
        Some(conn.prepare(
            "SELECT memory_id, artifact_id, origin_event_uid, installation_id, client, session_id,
                    turn_id, trace_id, identity_status, created_at
               FROM memory_identity WHERE memory_id = ?1",
        )?)
    } else {
        None
    };
    let mut sources_stmt = if sources_present {
        Some(conn.prepare(
            "SELECT request_id, repository_id, scope_id, caller_id, raw_sha256, recorded_at_ms
               FROM cortex_agent_memory_source_v1 WHERE memory_id = ?1 ORDER BY scope_id, request_id",
        )?)
    } else {
        None
    };

    let source_store = NeutralSourceStore {
        file: source.file.clone(),
        user_version: Some(user_version),
        file_sha256: source.file_sha256.clone(),
        machine: source.machine.clone(),
        set_aside: source.set_aside.clone(),
    };

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    let mut count = 0usize;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let scope: String = row.get(1)?;
        let tier_raw: String = row.get(2)?;
        let content: String = row.get(3)?;
        let (name, name_source) = name_of(&id, &scope);
        let identity = match identity_stmt.as_mut() {
            Some(stmt) => stmt
                .query_row([&id], |r| {
                    Ok(NeutralIdentity {
                        memory_id: r.get(0)?,
                        artifact_id: r.get(1)?,
                        origin_event_uid: r.get(2)?,
                        installation_id: r.get(3)?,
                        client: r.get(4)?,
                        session_id: r.get(5)?,
                        turn_id: r.get(6)?,
                        trace_id: r.get(7)?,
                        identity_status: r.get(8)?,
                        created_at: r.get(9)?,
                    })
                })
                .optional()?,
            None => None,
        };
        let agent_sources = match sources_stmt.as_mut() {
            Some(stmt) => {
                let found = stmt.query_map([&id], |r| {
                    Ok(NeutralAgentSource {
                        request_id: r.get(0)?,
                        repository_id: r.get(1)?,
                        scope_id: r.get(2)?,
                        caller_id: r.get(3)?,
                        raw_sha256: r.get(4)?,
                        recorded_at_ms: r.get(5)?,
                    })
                })?;
                found.collect::<rusqlite::Result<Vec<_>>>()?
            }
            None => Vec::new(),
        };
        let embedding_q_bytes: Option<i64> = row.get(28)?;
        let legacy_embedding_bytes: Option<i64> = row.get(29)?;
        let line = NeutralMemory {
            format: NEUTRAL_FORMAT.to_string(),
            id,
            scope,
            tier: decode_tier(&tier_raw),
            name,
            name_source: name_source.to_string(),
            content_bytes: content.len() as u64,
            content_sha256: sha256_hex(&content),
            content,
            keywords: text_to_value(row.get(4)?),
            created: row.get(5)?,
            updated: row
                .get::<_, Option<String>>(6)?
                .filter(|text| !text.is_empty()),
            state: row.get(7)?,
            provenance: NeutralProvenance {
                producer: row.get(8)?,
                authority: row.get(9)?,
                artifact_family: row.get(10)?,
                record_type: row.get(11)?,
                influence_class: row.get(12)?,
                derivation: row.get(13)?,
                sensitivity: row.get(14)?,
                source_ids: text_to_value(row.get(15)?),
                confidence: row.get(16)?,
                confidence_basis: row.get(17)?,
                identity,
                agent_sources,
            },
            lifecycle: NeutralLifecycle {
                effective_from_ms: row.get(18)?,
                effective_until_ms: row.get(19)?,
                expires_at_ms: row.get(20)?,
                review_after_ms: row.get(21)?,
                superseded_by: row.get(22)?,
                priority_class: row.get(23)?,
            },
            usage: NeutralUsage {
                score: row.get(24)?,
                access_count: row.get(25)?,
                inject_count: row.get(26)?,
            },
            derived_not_exported: NeutralDerived {
                embed_model: row.get(27)?,
                embedding_q_bytes,
                embedding_dropped: embedding_q_bytes.is_some() || legacy_embedding_bytes.is_some(),
            },
            source_store: Some(source_store.clone()),
        };
        let mut text = serde_json::to_string(&line)?;
        text.push('\n');
        out.write_all(text.as_bytes())?;
        count += 1;
    }
    Ok(count)
}

/// As [`export_neutral`] on the store file at `path`, opened read-only (never created, never
/// written).
pub fn export_neutral_from_path(
    path: &Path,
    out: &mut dyn Write,
    source: &NeutralSource,
) -> Result<usize, NeutralError> {
    let conn = crate::db::open_store_read_only(path, Duration::from_millis(250))?;
    export_neutral(&conn, out, source)
}

// ─── Import ──────────────────────────────────────────────────────────────────

/// Add the neutral lines in `input` to the store in `conn` (the current layout). Returns one
/// outcome per non-blank line, in order.
///
/// Additive and idempotent: running it twice on the same input inserts on the first run and
/// reports `AlreadyPresent` on the second. A memory is identified by id and body hash. Nothing is
/// updated or deleted. Imported rows carry no embedding; the caller re-embeds. A line that cannot
/// be read, fails its checks, or collides is `Rejected` and the rest still run. Only an I/O
/// failure reading `input` stops the import.
pub fn import_neutral(
    conn: &mut Connection,
    input: &mut dyn BufRead,
) -> Result<Vec<NeutralLineOutcome>, NeutralError> {
    let mut outcomes = Vec::new();
    let mut raw = Vec::new();
    let mut line_no = 0usize;
    loop {
        raw.clear();
        if input.read_until(b'\n', &mut raw)? == 0 {
            break;
        }
        line_no += 1;
        let text = match std::str::from_utf8(&raw) {
            Ok(text) => text.trim(),
            Err(error) => {
                outcomes.push(NeutralLineOutcome {
                    line: line_no,
                    id: None,
                    outcome: reject("not_utf8", error.to_string()),
                    notes: Vec::new(),
                });
                continue;
            }
        };
        if text.is_empty() {
            continue;
        }
        let record: NeutralMemory = match serde_json::from_str(text) {
            Ok(record) => record,
            Err(error) => {
                outcomes.push(NeutralLineOutcome {
                    line: line_no,
                    id: None,
                    outcome: reject("bad_json", error.to_string()),
                    notes: Vec::new(),
                });
                continue;
            }
        };
        let mut notes = Vec::new();
        let outcome = match import_one(conn, &record, &mut notes) {
            Ok(outcome) => outcome,
            Err(error) => reject("storage_error", error.to_string()),
        };
        outcomes.push(NeutralLineOutcome {
            line: line_no,
            id: Some(record.id.clone()),
            outcome,
            notes,
        });
    }
    Ok(outcomes)
}

fn import_one(
    conn: &mut Connection,
    rec: &NeutralMemory,
    notes: &mut Vec<String>,
) -> rusqlite::Result<NeutralOutcome> {
    if rec.format != NEUTRAL_FORMAT && rec.format != NEUTRAL_FORMAT_DRAFT {
        return Ok(reject(
            "wrong_format",
            format!(
                "expected {NEUTRAL_FORMAT} (or the draft {NEUTRAL_FORMAT_DRAFT}), got {}",
                rec.format
            ),
        ));
    }
    if rec.id.is_empty() {
        return Ok(reject("empty_id", "id is empty"));
    }
    if rec.scope.is_empty() {
        return Ok(reject("empty_scope", "scope is empty"));
    }
    if rec.content_bytes != rec.content.len() as u64 {
        return Ok(reject(
            "bytes_mismatch",
            format!(
                "content_bytes {} but the body is {} bytes",
                rec.content_bytes,
                rec.content.len()
            ),
        ));
    }
    let body_sha = sha256_hex(&rec.content);
    if !body_sha.eq_ignore_ascii_case(&rec.content_sha256) {
        return Ok(reject(
            "hash_mismatch",
            "content_sha256 does not match the body",
        ));
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    let existing: Option<String> = tx
        .query_row(
            "SELECT content FROM memories WHERE id = ?1",
            [&rec.id],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(stored) = existing {
        return Ok(if sha256_hex(&stored) == body_sha {
            NeutralOutcome::AlreadyPresent
        } else {
            reject(
                "id_conflict",
                "the id is present with a different body; not overwritten",
            )
        });
    }
    if table_present(&tx, "deletions")?
        && tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM deletions WHERE id = ?1)",
            [&rec.id],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Ok(reject(
            "tombstoned",
            "the id was deleted from this store; it is not brought back",
        ));
    }
    if table_present(&tx, "memory_quarantine")?
        && tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM memory_quarantine WHERE id = ?1)",
            [&rec.id],
            |row| row.get::<_, bool>(0),
        )?
    {
        return Ok(reject(
            "quarantined",
            "the id is quarantined in this store; it is not brought back",
        ));
    }

    if let Some(identity) = &rec.provenance.identity {
        if identity.memory_id != rec.id {
            return Ok(reject(
                "identity_mismatch",
                "identity.memory_id is not the line's id",
            ));
        }
        if table_present(&tx, "memory_identity")?
            && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM memory_identity
                  WHERE memory_id = ?1 OR artifact_id = ?2 OR origin_event_uid = ?3)",
                params![
                    identity.memory_id,
                    identity.artifact_id,
                    identity.origin_event_uid
                ],
                |row| row.get::<_, bool>(0),
            )?
        {
            return Ok(reject(
                "identity_conflict",
                "a memory_identity row already holds this memory id, artifact id or origin event",
            ));
        }
    }

    let sources_table = table_present(&tx, "cortex_agent_memory_source_v1")?;
    for source in &rec.provenance.agent_sources {
        if !source.raw_sha256.eq_ignore_ascii_case(&body_sha) {
            return Ok(reject(
                "agent_source_mismatch",
                format!(
                    "agent source {} does not hash to the body",
                    source.request_id
                ),
            ));
        }
        if sources_table
            && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM cortex_agent_memory_source_v1
                  WHERE scope_id = ?1 AND request_id = ?2)",
                params![source.scope_id, source.request_id],
                |row| row.get::<_, bool>(0),
            )?
        {
            return Ok(reject(
                "agent_source_conflict",
                format!(
                    "agent source {}/{} already exists",
                    source.scope_id, source.request_id
                ),
            ));
        }
    }

    let tier = serde_json::to_string(&rec.tier).unwrap_or_else(|_| "\"Episodic\"".into());
    tx.execute(
        "INSERT INTO memories
             (id, tier, content, keywords, score, created_at, updated_at, access_count,
              embedding_q, scope_id, inject_count, content_hash, embed_model, source_ids,
              artifact_family, producer, record_type, authority, influence_class,
              lifecycle_state, effective_from_ms, effective_until_ms, expires_at_ms,
              review_after_ms, superseded_by, priority_class, confidence, confidence_basis,
              sensitivity, derivation)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, ?11, NULL, ?12,
                 ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28)",
        params![
            rec.id,
            tier,
            rec.content,
            value_to_text(&rec.keywords),
            rec.usage.score,
            rec.created,
            rec.updated.as_deref().unwrap_or(""),
            rec.usage.access_count,
            rec.scope,
            rec.usage.inject_count,
            body_sha,
            value_to_text(&rec.provenance.source_ids),
            rec.provenance.artifact_family,
            rec.provenance.producer,
            rec.provenance.record_type,
            rec.provenance.authority,
            rec.provenance.influence_class,
            rec.state,
            rec.lifecycle.effective_from_ms,
            rec.lifecycle.effective_until_ms,
            rec.lifecycle.expires_at_ms,
            rec.lifecycle.review_after_ms,
            rec.lifecycle.superseded_by,
            rec.lifecycle.priority_class,
            rec.provenance.confidence,
            rec.provenance.confidence_basis,
            rec.provenance.sensitivity,
            rec.provenance.derivation,
        ],
    )?;

    if let Some(identity) = &rec.provenance.identity {
        if table_present(&tx, "memory_identity")? {
            tx.execute(
                "INSERT INTO memory_identity
                     (memory_id, artifact_id, origin_event_uid, installation_id, client,
                      session_id, turn_id, trace_id, identity_status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    identity.memory_id,
                    identity.artifact_id,
                    identity.origin_event_uid,
                    identity.installation_id,
                    identity.client,
                    identity.session_id,
                    identity.turn_id,
                    identity.trace_id,
                    identity.identity_status,
                    identity.created_at,
                ],
            )?;
        } else {
            notes.push("memory_identity table absent: identity not written".to_string());
        }
    }
    if !rec.provenance.agent_sources.is_empty() {
        if sources_table {
            for source in &rec.provenance.agent_sources {
                // The body of an agent source equals the memory body (checked above against
                // raw_sha256). The caller's original input JSON is not in the neutral form.
                tx.execute(
                    "INSERT INTO cortex_agent_memory_source_v1
                         (memory_id, request_id, repository_id, scope_id, caller_id, raw_body,
                          raw_sha256, caller_input_json, recorded_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, '{}', ?8)",
                    params![
                        rec.id,
                        source.request_id,
                        source.repository_id,
                        source.scope_id,
                        source.caller_id,
                        rec.content.as_bytes(),
                        body_sha,
                        source.recorded_at_ms,
                    ],
                )?;
            }
        } else {
            notes.push(
                "cortex_agent_memory_source_v1 table absent: agent sources not written".to_string(),
            );
        }
    }

    Fts5Projection::new(&tx)
        .upsert_within(&Fts5Document {
            record_id: rec.id.clone(),
            record_type: rec.provenance.record_type.clone(),
            session_id: None,
            scope_id: rec.scope.clone(),
            lifecycle: rec.state.clone(),
            authority: rec.provenance.authority.clone(),
            content: rec.content.clone(),
            keywords: value_to_text(&rec.keywords),
        })
        .map_err(|error| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
                error.to_string(),
            )))
        })?;
    tx.commit()?;
    Ok(NeutralOutcome::Inserted)
}
