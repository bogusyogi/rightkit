//! MemDb — the memory engine's own SQLite store (the `memories` table). A cloneable
//! `Arc<Mutex<Connection>>` handle, WAL, mirroring the interface `MemoryStore` needs. Self-contained
//! so Cortex is publishable without dragging in CodeRight's governance DB.

use crate::db::{open_store, open_store_read_only};
use rightkit_db::rusqlite::{self, Connection, OptionalExtension};
use rightkit_db::{DbError, MigrateOptions, Migration};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS memories (
    id           TEXT PRIMARY KEY,
    tier         TEXT NOT NULL,
    content      TEXT NOT NULL,
    keywords     TEXT NOT NULL,
    score        REAL NOT NULL,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL DEFAULT '',
    access_count INTEGER NOT NULL,
    embedding_q  BLOB,
    scope_id     TEXT NOT NULL DEFAULT 'global',
    inject_count INTEGER NOT NULL DEFAULT 0,
    content_hash TEXT,
    embed_model  TEXT,
    source_ids   TEXT NOT NULL DEFAULT '[]',
    artifact_family TEXT NOT NULL DEFAULT 'memory',
    producer     TEXT NOT NULL DEFAULT 'manual',
    record_type  TEXT NOT NULL DEFAULT 'memory',
    authority    TEXT NOT NULL DEFAULT 'A2',
    influence_class TEXT NOT NULL DEFAULT 'reference',
    lifecycle_state TEXT NOT NULL DEFAULT 'active',
    effective_from_ms INTEGER,
    effective_until_ms INTEGER,
    expires_at_ms INTEGER,
    review_after_ms INTEGER,
    superseded_by TEXT,
    priority_class TEXT NOT NULL DEFAULT 'normal',
    confidence REAL,
    confidence_basis TEXT,
    sensitivity TEXT NOT NULL DEFAULT 'unavailable_legacy',
    derivation TEXT NOT NULL DEFAULT 'unavailable_legacy'
);
CREATE TABLE IF NOT EXISTS membrane_temporal_fact (
    fact_id TEXT PRIMARY KEY,
    subject TEXT NOT NULL,
    predicate TEXT NOT NULL,
    object_json TEXT NOT NULL,
    scope_id TEXT NOT NULL,
    authority TEXT NOT NULL,
    veracity TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    valid_from TEXT NOT NULL,
    valid_until TEXT,
    expires_at TEXT,
    supersedes TEXT,
    payload_sha256 TEXT NOT NULL,
    transition_sha256 TEXT NOT NULL,
    valid_at TEXT,
    valid_at_unavailable_reason TEXT,
    recorded_at TEXT,
    recorded_at_unavailable_reason TEXT,
    invalid_at TEXT,
    superseded_by TEXT,
    revoked INTEGER NOT NULL DEFAULT 0,
    independently_verified INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS membrane_temporal_fact_asof
 ON membrane_temporal_fact(scope_id,subject,predicate,valid_from,valid_until);
CREATE TABLE IF NOT EXISTS recall_log (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    ts             TEXT NOT NULL,
    scope          TEXT,
    query_chars    INTEGER NOT NULL,
    hit_count      INTEGER NOT NULL,
    full_chars     INTEGER NOT NULL,  -- sum of full memory content for every hit returned
    injected_chars INTEGER NOT NULL,  -- sum of the skeleton text actually sent in the response
    source         TEXT NOT NULL DEFAULT 'serve', -- 'serve' (agent-facing) | 'cli' (human debug)
    query_preview  TEXT,              -- first ~120 chars of the query, so a ranking change can be
                                      -- verified by replaying REAL traffic (T2.1 step 1a)
    -- 2026-07-09 (add-now plan, Phase 1): client attribution + hook provenance.
    -- `client` distinguishes which agent/UI issued the recall (claude|claudemm|codex|manual|smoke|cli).
    -- `session_id`/`cwd_scope`/`hook_event`/`trace_id` let us follow a single agent turn end-to-end
    -- (which project, which hook fired, which request) without joining external logs. `client_visibility`
    -- records the cross-client visibility policy in force when the row was written, so a future
    -- switch to per-client isolation is auditable from the row itself.
    client         TEXT NOT NULL DEFAULT 'unknown',
    session_id     TEXT,
    cwd_scope      TEXT,
    hook_event     TEXT,
    trace_id       TEXT,
    client_visibility TEXT NOT NULL DEFAULT 'all',
    traffic_class  TEXT NOT NULL DEFAULT 'production',
    candidate_hits TEXT NOT NULL DEFAULT '[]',
    admitted_hits  TEXT NOT NULL DEFAULT '[]',
    event_uid            TEXT,
    installation_id      TEXT,
    service_instance_id  TEXT,
    turn_id              TEXT,
    identity_status      TEXT NOT NULL DEFAULT 'legacy_unattributed'
        CHECK (identity_status IN ('observed', 'legacy_unattributed'))
);
CREATE TABLE IF NOT EXISTS recall_log_smoke (
    id                     INTEGER PRIMARY KEY AUTOINCREMENT,
    ts                     TEXT NOT NULL,
    scope                  TEXT,
    query_chars            INTEGER NOT NULL,
    hit_count              INTEGER NOT NULL,
    full_chars             INTEGER NOT NULL,
    injected_chars         INTEGER NOT NULL,
    source                 TEXT NOT NULL DEFAULT 'serve',
    query_preview          TEXT,
    client                 TEXT NOT NULL DEFAULT 'unknown',
    session_id             TEXT,
    cwd_scope              TEXT,
    hook_event             TEXT,
    trace_id               TEXT,
    client_visibility      TEXT NOT NULL DEFAULT 'all',
    traffic_class          TEXT NOT NULL DEFAULT 'smoke' CHECK (traffic_class != 'production'),
    candidate_hits         TEXT NOT NULL DEFAULT '[]',
    admitted_hits          TEXT NOT NULL DEFAULT '[]',
    source_recall_id       INTEGER,
    original_traffic_class TEXT NOT NULL,
    isolated_at            TEXT NOT NULL,
    isolation_reason       TEXT NOT NULL,
    event_uid              TEXT,
    installation_id        TEXT,
    service_instance_id    TEXT,
    turn_id                TEXT,
    identity_status        TEXT NOT NULL DEFAULT 'legacy_unattributed'
        CHECK (identity_status IN ('observed', 'legacy_unattributed'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_recall_log_smoke_source
    ON recall_log_smoke(source_recall_id) WHERE source_recall_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_recall_event_uid
    ON recall_log(event_uid) WHERE event_uid IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_recall_decision
    ON recall_log(client, session_id, turn_id, trace_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_smoke_recall_event_uid
    ON recall_log_smoke(event_uid) WHERE event_uid IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_smoke_recall_decision
    ON recall_log_smoke(client, session_id, turn_id, trace_id);
CREATE TABLE IF NOT EXISTS deletions (
    id         TEXT PRIMARY KEY,     -- memory id (scope-qualified) whose row was deleted
    deleted_at TEXT NOT NULL         -- ISO; lets cross-machine sync propagate deletions
);
CREATE TABLE IF NOT EXISTS memory_event_log (
    event_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    ts         TEXT NOT NULL,
    event_kind TEXT NOT NULL,
    memory_id  TEXT,
    surface    TEXT NOT NULL DEFAULT 'unknown',
    session_id TEXT,
    trace_id   TEXT,
    scope_id   TEXT,
    quantity   INTEGER NOT NULL DEFAULT 1,
    meta       TEXT NOT NULL DEFAULT '{}',
    event_uid  TEXT,
    installation_id TEXT,
    client     TEXT,
    turn_id    TEXT,
    artifact_id TEXT,
    identity_status TEXT NOT NULL DEFAULT 'legacy_unattributed'
);
CREATE TABLE IF NOT EXISTS memory_identity (
    memory_id           TEXT PRIMARY KEY,
    artifact_id         TEXT NOT NULL UNIQUE,
    origin_event_uid    TEXT NOT NULL UNIQUE,
    installation_id     TEXT,
    client              TEXT NOT NULL,
    session_id          TEXT NOT NULL,
    turn_id             TEXT NOT NULL,
    trace_id            TEXT NOT NULL,
    identity_status     TEXT NOT NULL CHECK (identity_status IN ('observed', 'legacy_unattributed')),
    created_at          TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_memory_event_memory_ts
    ON memory_event_log(memory_id, ts);
CREATE INDEX IF NOT EXISTS idx_memory_event_surface_ts
    ON memory_event_log(surface, ts);
CREATE UNIQUE INDEX IF NOT EXISTS idx_memory_event_uid
    ON memory_event_log(event_uid) WHERE event_uid IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_memory_event_decision
    ON memory_event_log(client, session_id, turn_id, trace_id);
CREATE TABLE IF NOT EXISTS context_policy_assignment (
    session_id     TEXT NOT NULL,
    surface        TEXT NOT NULL,
    policy_version TEXT NOT NULL,
    cohort         TEXT NOT NULL CHECK (cohort IN ('control', 'candidate')),
    assigned_at    TEXT NOT NULL,
    task_class     TEXT NOT NULL DEFAULT 'unknown',
    PRIMARY KEY (session_id, surface, policy_version)
);
CREATE INDEX IF NOT EXISTS idx_context_policy_cohort_time
    ON context_policy_assignment(policy_version, cohort, assigned_at);
CREATE TABLE IF NOT EXISTS context_feedback (
    trace_id       TEXT NOT NULL,
    candidate_id   TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    outcome        TEXT NOT NULL,        -- used | ignored | contradicted
    verified       INTEGER NOT NULL,     -- 1 = observed action / cited verdict; 0 = advisory only
    verdict_ref    TEXT,
    scope_id       TEXT NOT NULL DEFAULT 'global',
    ts             TEXT NOT NULL,
    qualified_experiment_id TEXT,
    PRIMARY KEY (trace_id, candidate_id)
);
CREATE INDEX IF NOT EXISTS idx_context_feedback_candidate
    ON context_feedback(candidate_id);
CREATE TABLE IF NOT EXISTS links (
    src_id   TEXT NOT NULL,
    dst_slug TEXT NOT NULL,
    PRIMARY KEY (src_id, dst_slug)
);
CREATE INDEX IF NOT EXISTS idx_links_dst ON links(dst_slug);
CREATE TABLE IF NOT EXISTS skills (
    name        TEXT PRIMARY KEY,
    description TEXT NOT NULL DEFAULT '',
    body        TEXT NOT NULL,
    body_sha256 TEXT NOT NULL,
    resources   TEXT NOT NULL DEFAULT '[]',
    updated_at  TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS memory_quarantine (
    id             TEXT PRIMARY KEY,
    tier           TEXT NOT NULL,
    content        TEXT NOT NULL,
    keywords       TEXT NOT NULL,
    score          REAL NOT NULL,
    created_at     TEXT NOT NULL,
    updated_at     TEXT NOT NULL DEFAULT '',
    access_count   INTEGER NOT NULL,
    embedding_q    BLOB,
    scope_id       TEXT NOT NULL DEFAULT 'global',
    inject_count   INTEGER NOT NULL DEFAULT 0,
    content_hash   TEXT,
    embed_model    TEXT,
    source_ids     TEXT NOT NULL DEFAULT '[]',
    authority      TEXT NOT NULL DEFAULT 'A0',
    influence_class TEXT NOT NULL DEFAULT 'unknown',
    lifecycle_state TEXT NOT NULL DEFAULT 'active',
    effective_from_ms INTEGER,
    effective_until_ms INTEGER,
    expires_at_ms INTEGER,
    review_after_ms INTEGER,
    superseded_by TEXT,
    priority_class TEXT NOT NULL DEFAULT 'normal',
    confidence REAL,
    confidence_basis TEXT,
    sensitivity TEXT NOT NULL DEFAULT 'unavailable_legacy',
    derivation TEXT NOT NULL DEFAULT 'unavailable_legacy',
    quarantined_at TEXT NOT NULL,
    reason         TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS memory_batch_receipt (
    batch_id       TEXT PRIMARY KEY,
    request_sha256 TEXT NOT NULL,
    item_count     INTEGER NOT NULL,
    created_at     TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS memory_batch_item_receipt (
    batch_id       TEXT NOT NULL,
    ordinal        INTEGER NOT NULL,
    item_id        TEXT NOT NULL,
    memory_id      TEXT NOT NULL,
    disposition    TEXT NOT NULL,
    item_sha256    TEXT NOT NULL,
    artifact_family TEXT NOT NULL,
    producer       TEXT NOT NULL,
    record_type    TEXT NOT NULL,
    PRIMARY KEY (batch_id, item_id),
    UNIQUE (batch_id, ordinal),
    FOREIGN KEY (batch_id) REFERENCES memory_batch_receipt(batch_id)
        ON DELETE RESTRICT
);
CREATE TABLE IF NOT EXISTS membrane_event_outbox (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    batch_sha256 TEXT NOT NULL UNIQUE,
    batch_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS learning_experiment (
    experiment_id TEXT PRIMARY KEY,
    request_sha256 TEXT NOT NULL,
    policy_version TEXT NOT NULL,
    policy_activation_sha256 TEXT NOT NULL,
    task_class TEXT NOT NULL,
    observed_since TEXT NOT NULL,
    observed_through TEXT NOT NULL,
    min_per_cohort INTEGER NOT NULL CHECK(min_per_cohort > 0),
    min_effect_bps INTEGER NOT NULL CHECK(min_effect_bps BETWEEN -10000 AND 10000),
    created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS learning_update_target (
    experiment_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    memory_id TEXT NOT NULL,
    content_sha256 TEXT NOT NULL,
    delta_micros INTEGER NOT NULL CHECK(delta_micros BETWEEN -1000000 AND 1000000),
    PRIMARY KEY(experiment_id, ordinal),
    UNIQUE(experiment_id, memory_id),
    FOREIGN KEY(experiment_id) REFERENCES learning_experiment(experiment_id) ON DELETE RESTRICT
);
CREATE TABLE IF NOT EXISTS learning_promotion_receipt (
    experiment_id TEXT PRIMARY KEY,
    evidence_sha256 TEXT NOT NULL,
    control_count INTEGER NOT NULL,
    control_success INTEGER NOT NULL,
    candidate_count INTEGER NOT NULL,
    candidate_success INTEGER NOT NULL,
    effect_bps INTEGER NOT NULL,
    disposition TEXT NOT NULL CHECK(disposition IN ('qualified','rejected')),
    reason_code TEXT NOT NULL,
    applied_count INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY(experiment_id) REFERENCES learning_experiment(experiment_id) ON DELETE RESTRICT
);
CREATE INDEX IF NOT EXISTS idx_learning_experiment_policy ON learning_experiment(policy_version, task_class);
CREATE INDEX IF NOT EXISTS idx_learning_target_memory ON learning_update_target(memory_id);
CREATE INDEX IF NOT EXISTS idx_memories_authority_lifecycle_eligibility
    ON memories(authority, lifecycle_state, superseded_by, effective_from_ms);
";

/// CTX-017 closed relation vocabulary, mirroring
/// `rightkit_memory_core::graph::CANONICAL_RELATIONS`. cortex-store does not depend on
/// cortex-core, so the two lists are kept in lockstep by an equality test in
/// membrane-runtime (which depends on both) and by the `CHECK` constraint in
/// [`MEMORY_RELATION_SCHEMA`].
pub const CANONICAL_RELATIONS: &[&str] = &[
    "supports",
    "contradicts",
    "supersedes",
    "derived_from",
    "enriches",
];

/// True when `relation` is a member of the closed CTX-017 vocabulary.
pub fn is_canonical_relation(relation: &str) -> bool {
    CANONICAL_RELATIONS.contains(&relation)
}

/// An evidence relation offered for durable admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalRelationEdge {
    pub source_id: String,
    pub target_id: String,
    /// One of [`CANONICAL_RELATIONS`].
    pub relation: String,
    /// Who produced this edge (subsystem/producer identity).
    pub provenance_producer: String,
    /// The specific evidence the edge came from (event id, digest, doc ref).
    pub provenance_ref: String,
    pub created_at: String,
}

/// Outcome of a successful admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationAdmission {
    pub relation_id: String,
    /// Retained for wire compatibility; canonical admissions always resolve.
    pub target_resolved: bool,
}

/// Typed refusal of an admission attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationRejection {
    /// Relation outside the closed CTX-017 vocabulary.
    NonCanonicalRelation,
    /// Producer or reference missing: an edge cannot be provenance-free.
    MissingProvenance,
    /// The source record does not resolve; an edge from nowhere is not evidence.
    UnresolvedSource,
    /// The target record does not resolve; dangling edges are never durable evidence.
    UnresolvedTarget,
    Storage(String),
}

impl RelationRejection {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NonCanonicalRelation => "non_canonical_relation",
            Self::MissingProvenance => "missing_provenance",
            Self::UnresolvedSource => "unresolved_source",
            Self::UnresolvedTarget => "unresolved_target",
            Self::Storage(_) => "relation_storage_error",
        }
    }
}

/// Why a stored relation is not traversable as live evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationDiagnostic {
    /// The target id has no row in `memories`: forgotten, never ingested, or
    /// quarantined out of the live table.
    DanglingTarget,
    /// The target resolves but is not live evidence — `superseded`,
    /// otherwise non-active, or recall-suppressed.
    LifecycleIneligible(String),
}

/// A durable relation plus its read-time disposition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRelation {
    pub relation_id: String,
    pub edge: CanonicalRelationEdge,
    /// None when the edge is live traversable evidence.
    pub diagnostic: Option<RelationDiagnostic>,
}

impl StoredRelation {
    /// Traversable exactly when there is no diagnostic reason not to.
    pub fn traversable(&self) -> bool {
        self.diagnostic.is_none()
    }
}

fn table_present(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )
}

/// Read every stored relation on `source_id` and classify each one. Lifecycle
/// eligibility mirrors the recall predicate: the target must exist, be
/// `active` with no `superseded_by`, must not be quarantined, and must not be
/// recall-suppressed. `cortex_recall_suppression_v1` and `memory_quarantine`
/// are probed for existence first: `cortex_recall_suppression_v1` is created
/// by the lifecycle layer, not by this schema.
fn stored_relations(conn: &Connection, source_id: &str) -> Result<Vec<StoredRelation>, String> {
    let suppression_present =
        table_present(conn, "cortex_recall_suppression_v1").map_err(|e| e.to_string())?;
    let quarantine_present = table_present(conn, "memory_quarantine").map_err(|e| e.to_string())?;
    let suppressed_clause = if suppression_present {
        "EXISTS(SELECT 1 FROM cortex_recall_suppression_v1 s
                 WHERE s.memory_id = r.target_id AND s.suppressed = 1)"
    } else {
        "0"
    };
    let quarantined_clause = if quarantine_present {
        "EXISTS(SELECT 1 FROM memory_quarantine q WHERE q.id = r.target_id)"
    } else {
        "0"
    };
    let sql = format!(
        "SELECT r.relation_id, r.source_id, r.target_id, r.relation,
                r.provenance_producer, r.provenance_ref, r.created_at,
                EXISTS(SELECT 1 FROM memories m WHERE m.id = r.target_id) AS resolved,
                COALESCE((SELECT m.lifecycle_state FROM memories m WHERE m.id = r.target_id), '') AS state,
                COALESCE((SELECT m.superseded_by IS NOT NULL FROM memories m WHERE m.id = r.target_id), 0) AS superseded,
                {suppressed_clause} AS suppressed,
                {quarantined_clause} AS quarantined
           FROM memory_relation r
          WHERE r.source_id = ?1
          ORDER BY r.relation, r.target_id"
    );
    let mut statement = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = statement
        .query_map([source_id], |row| {
            let relation_id: String = row.get(0)?;
            let edge = CanonicalRelationEdge {
                source_id: row.get(1)?,
                target_id: row.get(2)?,
                relation: row.get(3)?,
                provenance_producer: row.get(4)?,
                provenance_ref: row.get(5)?,
                created_at: row.get(6)?,
            };
            let resolved: bool = row.get(7)?;
            let state: String = row.get(8)?;
            let superseded: bool = row.get(9)?;
            let suppressed: bool = row.get(10)?;
            let quarantined: bool = row.get(11)?;
            let diagnostic = if !resolved {
                Some(RelationDiagnostic::DanglingTarget)
            } else if quarantined {
                Some(RelationDiagnostic::LifecycleIneligible(
                    "quarantined".into(),
                ))
            } else if suppressed {
                Some(RelationDiagnostic::LifecycleIneligible(
                    "recall_suppressed".into(),
                ))
            } else if superseded {
                Some(RelationDiagnostic::LifecycleIneligible("superseded".into()))
            } else if state != "active" {
                Some(RelationDiagnostic::LifecycleIneligible(format!(
                    "lifecycle_state_{state}"
                )))
            } else {
                None
            };
            Ok(StoredRelation {
                relation_id,
                edge,
                diagnostic,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| e.to_string())
}

/// CTX-017 canonical evidence-relation storage (canonical relation table).
///
/// One row per admitted edge. `relation` is constrained to the closed
/// vocabulary; provenance is mandatory and non-empty, so no edge can exist
/// without a recorded origin. Endpoints are plain text, not foreign keys:
/// a dangling edge stays queryable as a diagnostic and is excluded from
/// traversal by [`MemDb::traversable_relations_from`].
pub const MEMORY_RELATION_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS memory_relation (
    relation_id         TEXT PRIMARY KEY,
    source_id           TEXT NOT NULL,
    target_id           TEXT NOT NULL,
    relation            TEXT NOT NULL CHECK(relation IN ('supports','contradicts','supersedes','derived_from','enriches')),
    provenance_producer TEXT NOT NULL CHECK(trim(provenance_producer) <> ''),
    provenance_ref      TEXT NOT NULL CHECK(trim(provenance_ref) <> ''),
    created_at          TEXT NOT NULL,
    UNIQUE(source_id, target_id, relation)
);
CREATE INDEX IF NOT EXISTS idx_memory_relation_source ON memory_relation(source_id, relation);
CREATE INDEX IF NOT EXISTS idx_memory_relation_target ON memory_relation(target_id, relation);
";

/// Current durable Cortex schema contract, folded into one CREATE script (`SCHEMA`).
pub const LATEST_SCHEMA_VERSION: i64 = 1;

/// CTX-004: the explicit marker used when sensitivity or derivation metadata
/// is not known for a record — any row a writer left unset. Never replaced by a
/// guess; a real value must be written explicitly to move a row off this marker.
pub const UNAVAILABLE_LEGACY: &str = "unavailable_legacy";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecallLogSink {
    Production,
    Smoke,
}

fn opaque_legacy_identity(prefix: &str, memory_id: &str) -> String {
    let digest = rightkit_fs::digest::sha256_bytes(memory_id.as_bytes());
    format!("{prefix}{}", &digest[..32])
}

fn schema_v1(tx: &Connection) -> rusqlite::Result<()> {
    tx.execute_batch(SCHEMA)?;
    tx.execute_batch(MEMORY_RELATION_SCHEMA)?;
    Ok(())
}

const MIGRATIONS: &[Migration] = &[Migration::code(1, "schema_v1", schema_v1)];

fn sqlite_error(error: DbError) -> rusqlite::Error {
    match error {
        DbError::Sqlite(source) | DbError::MigrationFailed { source, .. } => source,
        other => rusqlite::Error::ToSqlConversionFailure(Box::new(other)),
    }
}

fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    let options = MigrateOptions {
        backup: false,
        optimize_after: false,
        ..MigrateOptions::default()
    };
    rightkit_db::migrate(conn, None, MIGRATIONS, &options)
        .map(|_| ())
        .map_err(sqlite_error)
}

/// Store rule (MDP-047, R153; #482): a store whose schema version differs from this build's fails
/// closed and is replaced, never migrated or rewritten. Before the writer opens `path`, read its
/// `user_version` read-only; a non-zero version other than `LATEST_SCHEMA_VERSION` means an older
/// or newer build wrote it, so the file and its sidecars are set aside (`<db>.replaced-<unix-ms>`,
/// nothing deleted, read by nothing) and the open below creates the final-form store fresh. How the
/// set-aside file is kept or removed afterwards is open item C-M2 and is not decided here.
///
/// The replacement runs under a cross-process lock on the file `<db>.replace-lock` that the caller
/// keeps held through the open and the migration. The file itself persists after the lock is
/// released (it is never deleted), so its existence means nothing: an open of an empty store path
/// takes the lock path only when the store file is missing (`replacement_in_flight`: no store
/// file, and the lock file or a `-wal`/`-journal` beside it). The version is read again under the
/// lock: two openers that both saw the old store cannot set aside the fresh store the first one
/// created. A lock that
/// stays busy means no replacement (`lock_unavailable`); the normal open then refuses with the typed
/// schema error. If the set-aside fails after the main file moved and `path` no longer exists, the
/// open is refused with a typed error (`StoreReplaceError`) instead of creating an empty store next
/// to the stranded one; if `path` still exists the store is in place and the normal open refuses it.
/// The outcome is one structured stderr event (`cortex.store_replaced`, `cortex.store_replace_failed`)
/// with versions only, no path text, and a record next to the store (`<db>.store-replacement.json`)
/// that status reads from any process (`store_replacement_status`).
static LAST_STORE_REPLACEMENT: std::sync::Mutex<Option<(i64, &'static str, i64)>> =
    std::sync::Mutex::new(None);

const REPLACE_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(15);

/// A schema-mismatch replacement that moved the store away and could not finish: the store path is
/// empty and the old file is stranded under a set-aside name. The open is refused rather than
/// creating a fresh store over it.
#[derive(Debug)]
pub struct StoreReplaceError {
    found: i64,
    /// The replacement lock stayed busy while the store path was empty or half moved: the open
    /// is refused instead of running unlocked.
    locked: bool,
}

impl std::fmt::Display for StoreReplaceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.locked {
            return write!(
                f,
                "store replacement is in progress or unfinished and its lock stayed busy; refusing to open"
            );
        }
        if self.found == 0 {
            return write!(
                f,
                "the store file is missing but its journal remains (a stranded set-aside); refusing to open"
            );
        }
        write!(
            f,
            "store replacement stranded the schema {} store; refusing to open",
            self.found
        )
    }
}

impl std::error::Error for StoreReplaceError {}

fn replace_lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".replace-lock");
    PathBuf::from(name)
}

fn replacement_record_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".store-replacement.json");
    PathBuf::from(name)
}

/// The persisted outcome of the most recent schema-mismatch replacement of the store at `path`:
/// `{foundSchemaVersion, result, atMs}`, from this process or from the record another process left
/// next to the store. `None` when no replacement was ever recorded. Status surfaces it so a
/// replaced or refused store is visible without reading the log (#482).
pub fn store_replacement_status(path: &Path) -> Option<serde_json::Value> {
    let in_process = *LAST_STORE_REPLACEMENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((found, result, at_ms)) = in_process {
        return Some(
            serde_json::json!({"foundSchemaVersion": found, "result": result, "atMs": at_ms}),
        );
    }
    let bytes = std::fs::read(replacement_record_path(path)).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let found = value.get("foundSchemaVersion")?.as_i64()?;
    let result = value.get("result")?.as_str()?;
    let at_ms = value.get("atMs")?.as_i64()?;
    Some(serde_json::json!({"foundSchemaVersion": found, "result": result, "atMs": at_ms}))
}

/// The stored `user_version`, or `None` when the store is absent or cannot be probed read-only.
fn stored_schema_version(path: &Path) -> Option<i64> {
    if !path.exists() {
        return None;
    }
    let busy = std::time::Duration::from_millis(250);
    let probe = open_store_read_only(path, busy).ok()?;
    let found = probe
        .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
        .ok();
    drop(probe);
    found
}

fn is_mismatch(found: i64) -> bool {
    found != 0 && found != LATEST_SCHEMA_VERSION
}

/// Record and log one replacement outcome. The record is best-effort; `recorded` says whether it
/// reached disk.
fn record_replacement(
    path: &Path,
    found: i64,
    event: &'static str,
    level: rightkit_logs::Level,
    result: &'static str,
) {
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default();
    *LAST_STORE_REPLACEMENT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((found, result, ts_ms));
    let record_path = replacement_record_path(path);
    let body = serde_json::json!({"foundSchemaVersion": found, "result": result, "atMs": ts_ms});
    let mut temp_name = record_path.as_os_str().to_os_string();
    temp_name.push(".tmp");
    let temp = PathBuf::from(temp_name);
    let recorded = serde_json::to_vec(&body)
        .ok()
        .map(|bytes| {
            std::fs::write(&temp, bytes).is_ok() && std::fs::rename(&temp, &record_path).is_ok()
        })
        .unwrap_or(false);
    let record = rightkit_logs::EventRecord::new(ts_ms, level, event)
        .with_field("reason", serde_json::json!("schema_mismatch"))
        .with_field("foundSchemaVersion", serde_json::json!(found))
        .with_field(
            "supportedSchemaVersion",
            serde_json::json!(LATEST_SCHEMA_VERSION),
        )
        .with_field("result", serde_json::json!(result))
        .with_field("recorded", serde_json::json!(recorded));
    if let Ok(line) = serde_json::to_string(&record) {
        stderr_line!("{line}");
    }
}

/// A store that opened and migrated after an earlier failed replacement is no longer stranded:
/// the failure state (`stranded_sidecar`, `set_aside_stranded`, `lock_unavailable`) is marked
/// `superseded` in the in-process state and in `<db>.store-replacement.json` (found version and
/// time kept), so `status` stops reporting a refusal that no longer holds. A completed
/// replacement (`set_aside`) and an already superseded record stay as they are. Best-effort.
fn supersede_replacement_failure(path: &Path) {
    let failed = |result: &str| !matches!(result, "set_aside" | "superseded");
    {
        let mut last = LAST_STORE_REPLACEMENT
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((found, result, at_ms)) = *last {
            if failed(result) {
                *last = Some((found, "superseded", at_ms));
            }
        }
    }
    let record_path = replacement_record_path(path);
    let Ok(bytes) = std::fs::read(&record_path) else {
        return;
    };
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };
    let Some(result) = value.get("result").and_then(|r| r.as_str()) else {
        return;
    };
    if !failed(result) {
        return;
    }
    value["result"] = serde_json::json!("superseded");
    let mut temp_name = record_path.as_os_str().to_os_string();
    temp_name.push(".tmp");
    let temp = PathBuf::from(temp_name);
    let recorded = serde_json::to_vec(&value).ok().is_some_and(|bytes| {
        std::fs::write(&temp, bytes).is_ok() && std::fs::rename(&temp, &record_path).is_ok()
    });
    log_replacement_event(
        "cortex.store_replace_superseded",
        rightkit_logs::Level::Info,
        if recorded {
            "superseded"
        } else {
            "superseded_unrecorded"
        },
    );
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// The store path is empty but a replacement may be mid-move or unfinished: the lock file, or a
/// journal or write-ahead log of the moved store, is still beside the path.
fn replacement_in_flight(path: &Path) -> bool {
    !path.exists()
        && (replace_lock_path(path).exists()
            || sidecar_path(path, "-wal").exists()
            || sidecar_path(path, "-journal").exists())
}

/// One structured stderr event without a replacement record (versions unknown or unchanged).
fn log_replacement_event(event: &'static str, level: rightkit_logs::Level, result: &'static str) {
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default();
    let record = rightkit_logs::EventRecord::new(ts_ms, level, event)
        .with_field("reason", serde_json::json!("schema_mismatch"))
        .with_field("result", serde_json::json!(result));
    if let Ok(line) = serde_json::to_string(&record) {
        stderr_line!("{line}");
    }
}

/// Replace a schema-mismatched store under the cross-process lock. Returns the lock guard when one
/// was taken; the caller holds it through the open and the migration. A path that is absent while
/// a replacement may be in flight (`replacement_in_flight`) also takes the lock, and an open whose
/// lock stays busy there is refused (`StoreReplaceError`), never run unlocked into a half-moved
/// store.
fn replace_mismatched_store(path: &Path) -> rusqlite::Result<Option<rightkit_fs::lock::FileLock>> {
    let first = stored_schema_version(path);
    let in_flight = first.is_none() && replacement_in_flight(path);
    let first = match first {
        Some(first) if is_mismatch(first) => first,
        None if in_flight => 0,
        _ => return Ok(None),
    };
    let options = rightkit_fs::lock::LockOptions::wait(REPLACE_LOCK_WAIT).labeled("store-replace");
    let guard = match rightkit_fs::lock::FileLock::acquire(&replace_lock_path(path), &options) {
        Ok(guard) => guard,
        Err(_) => {
            // The replacer that held the lock may have finished: a current store is not a failure,
            // and its own record must not be overwritten.
            let current = matches!(stored_schema_version(path), Some(now) if !is_mismatch(now));
            if current {
                return Ok(None);
            }
            if in_flight && !path.exists() {
                log_replacement_event(
                    "cortex.store_replace_failed",
                    rightkit_logs::Level::Error,
                    "lock_unavailable_refused",
                );
                return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                    StoreReplaceError {
                        found: 0,
                        locked: true,
                    },
                )));
            }
            record_replacement(
                path,
                first,
                "cortex.store_replace_failed",
                rightkit_logs::Level::Error,
                "lock_unavailable",
            );
            return Ok(None);
        }
    };
    if in_flight {
        // The lock is ours: nothing else is moving the store. A path still empty with the moved
        // store's journal or log beside it is stranded; never create an empty store next to it.
        if !path.exists()
            && (sidecar_path(path, "-wal").exists() || sidecar_path(path, "-journal").exists())
        {
            // A record (not only a log line) so `status --json` `store_replacement` shows the
            // refusal; the stored version is unknown here, so it is recorded as 0.
            record_replacement(
                path,
                0,
                "cortex.store_replace_failed",
                rightkit_logs::Level::Error,
                "stranded_sidecar",
            );
            return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                StoreReplaceError {
                    found: 0,
                    locked: false,
                },
            )));
        }
    }
    // Another opener may have replaced the store while this one waited: read it again.
    let found = match stored_schema_version(path) {
        Some(found) if is_mismatch(found) => found,
        _ => return Ok(Some(guard)),
    };
    let options = rightkit_db::QuarantineOptions {
        name: rightkit_db::QuarantineStamp::Custom(|at| {
            let millis = at
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_millis())
                .unwrap_or_default();
            format!(".replaced-{millis}")
        }),
        ..rightkit_db::QuarantineOptions::default()
    };
    match rightkit_db::quarantine_database_opts(path, &options) {
        Ok(_) => {
            record_replacement(
                path,
                found,
                "cortex.store_replaced",
                rightkit_logs::Level::Warn,
                "set_aside",
            );
            Ok(Some(guard))
        }
        Err(_) if !path.exists() => {
            // The main file moved and the move could not be undone (stranded set-aside): never
            // create an empty store beside it.
            record_replacement(
                path,
                found,
                "cortex.store_replace_failed",
                rightkit_logs::Level::Error,
                "set_aside_stranded",
            );
            Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                StoreReplaceError {
                    found,
                    locked: false,
                },
            )))
        }
        Err(_) => {
            // Nothing moved (or the undo succeeded): the store is in place and the normal open
            // refuses it with the typed schema error.
            record_replacement(
                path,
                found,
                "cortex.store_replace_failed",
                rightkit_logs::Level::Error,
                "set_aside_failed",
            );
            Ok(Some(guard))
        }
    }
}

/// A cloneable handle to the memory database; clones share one connection via the inner mutex.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MemDbProbe {
    Ok,
    Busy,
    Error,
}

/// Content-free startup observation for one SQLite database owned by MemDb.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupWalReport {
    pub status: String,
    pub degradation_reason: Option<String>,
    pub path: Option<PathBuf>,
    pub effective_journal_mode: Option<String>,
}

/// Classifies a journal-mode result without requiring a live SQLite connection.
pub fn classify_wal_mode(mode: Option<&str>) -> (&'static str, Option<&'static str>) {
    if mode.is_some_and(|value| value.eq_ignore_ascii_case("wal")) {
        ("ok", None)
    } else {
        ("degraded", Some("journal_mode_not_wal"))
    }
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

fn startup_wal_report(conn: &Connection, path: Option<&Path>) -> StartupWalReport {
    let effective_journal_mode = rightkit_db::journal_mode(conn).ok();
    let (status, degradation_reason) = classify_wal_mode(effective_journal_mode.as_deref());
    StartupWalReport {
        status: status.to_owned(),
        degradation_reason: degradation_reason.map(str::to_owned),
        path: path.map(absolute_path),
        effective_journal_mode,
    }
}

fn in_memory_startup_wal_reports() -> Arc<[StartupWalReport; 2]> {
    Arc::new([
        StartupWalReport {
            status: "ok".to_owned(),
            degradation_reason: None,
            path: None,
            effective_journal_mode: None,
        },
        StartupWalReport {
            status: "ok".to_owned(),
            degradation_reason: None,
            path: None,
            effective_journal_mode: None,
        },
    ])
}

impl MemDbProbe {
    pub fn status(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Busy => "busy",
            Self::Error => "error",
        }
    }
}

fn resolve_event_db_path(memory_path: &Path) -> PathBuf {
    if let Some(path) = crate::qa_env::event_db() {
        return PathBuf::from(path);
    }
    let stem = memory_path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("cortex");
    memory_path.with_file_name(format!("{stem}.membrane-events.sqlite3"))
}

fn refuse_shared_event_file(memory_path: &Path, event_path: &Path) -> rusqlite::Result<()> {
    let resolve = |path: &Path| -> PathBuf {
        std::fs::canonicalize(path).unwrap_or_else(|_| match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => std::fs::canonicalize(parent)
                .map(|dir| dir.join(name))
                .unwrap_or_else(|_| path.to_path_buf()),
            _ => path.to_path_buf(),
        })
    };
    // The same file under another name (a hard link, or a path through a link): the same file
    // identity (device and inode on Unix, volume and file index on Windows).
    let same_identity = match (
        rightkit_fs::FileKey::of_path(memory_path),
        rightkit_fs::FileKey::of_path(event_path),
    ) {
        (Ok(memory), Ok(event)) => memory == event,
        _ => false,
    };
    if same_identity || resolve(memory_path) == resolve(event_path) {
        return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
            std::io::Error::other(format!(
                "the event database {} is the memory database itself; use a different file for the event database",
                event_path.display()
            )),
        )));
    }
    Ok(())
}

/// Opens the event database and ensures its context ledger schema.
fn open_event_ledger(event_path: &Path) -> rusqlite::Result<Connection> {
    let event_conn = open_store(event_path)?;
    event_conn.execute_batch(crate::context_telemetry::CONTEXT_LEDGER_SCHEMA)?;
    Ok(event_conn)
}

#[derive(Clone)]
pub struct MemDb {
    conn: Arc<Mutex<Connection>>,
    event_conn: Arc<Mutex<Connection>>,
    event_db_path: Option<Arc<PathBuf>>,
    startup_wal: Arc<[StartupWalReport; 2]>,
}

impl MemDb {
    /// Open (or create) the DB at `path`, WAL + busy timeout, create the schema.
    pub fn open<P: AsRef<Path>>(path: P) -> rusqlite::Result<Self> {
        let path = path.as_ref();
        Self::open_with_event_path(path, resolve_event_db_path(path))
    }

    /// `open` with the event database path already resolved (`RIGHTKIT_CONTEXT_EVENT_DB` or the default).
    fn open_with_event_path(path: &Path, event_path: PathBuf) -> rusqlite::Result<Self> {
        // Held until this function returns: the replace lock covers read, set-aside, open and migrate.
        let _replace_guard = replace_mismatched_store(path)?;
        let mut conn = open_store(path)?;
        migrate(&mut conn)?;
        supersede_replacement_failure(path);
        refuse_shared_event_file(path, &event_path)?;
        let event_conn = open_event_ledger(&event_path)?;
        let startup_wal = Arc::new([
            startup_wal_report(&conn, Some(path)),
            startup_wal_report(&event_conn, Some(&event_path)),
        ]);
        let db = Self {
            conn: Arc::new(Mutex::new(conn)),
            event_conn: Arc::new(Mutex::new(event_conn)),
            event_db_path: Some(Arc::new(event_path)),
            startup_wal,
        };
        db.flush_context_event_outbox()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        Ok(db)
    }

    /// Open an existing store read-only without schema creation, event database
    /// setup, or outbox flushing. Used by bounded foreground retrieval.
    pub fn open_read_only<P: AsRef<Path>>(path: P) -> rusqlite::Result<Self> {
        let path = path.as_ref();
        // query_only stays explicit: read-only main DBs still permit SQLite TEMP tables.
        let busy = std::time::Duration::from_millis(250);
        let conn = open_store_read_only(path, busy)?;
        conn.execute_batch("PRAGMA query_only=ON;")?;
        let event_path = resolve_event_db_path(path);
        let event_conn = open_store_read_only(&event_path, busy)?;
        event_conn.execute_batch("PRAGMA query_only=ON;")?;
        let startup_wal = Arc::new([
            startup_wal_report(&conn, Some(path)),
            startup_wal_report(&event_conn, Some(&event_path)),
        ]);
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            event_conn: Arc::new(Mutex::new(event_conn)),
            event_db_path: Some(Arc::new(event_path)),
            startup_wal,
        })
    }

    /// Ephemeral in-memory DB (tests / no path).
    pub fn open_in_memory() -> Self {
        let mut conn = rightkit_db::open_in_memory().expect("open in-memory sqlite");
        migrate(&mut conn).expect("migrate in-memory memdb");
        conn.execute_batch(crate::context_telemetry::CONTEXT_LEDGER_SCHEMA)
            .expect("context ledger schema in-memory memdb");
        let conn = Arc::new(Mutex::new(conn));
        Self {
            conn: conn.clone(),
            event_conn: conn,
            event_db_path: None,
            startup_wal: in_memory_startup_wal_reports(),
        }
    }

    /// Lock the connection for a unit of work. Poison-recovering: a worker thread that panics
    /// mid-statement must NOT wedge every other worker's DB access into a serve-wide outage
    /// (the serve loop runs `route` under `catch_unwind`, so a panic is contained to one request;
    /// the mutex must not amplify it back to the whole service).
    pub fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn lock_events(&self) -> MutexGuard<'_, Connection> {
        self.event_conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// CTX-017: admit one canonical evidence relation.
    ///
    /// Admission requires a closed-vocabulary relation, resolved endpoints,
    /// and non-empty provenance. Re-recording the same
    /// (source, target, relation) refreshes provenance rather than duplicating.
    pub fn record_canonical_relation(
        &self,
        edge: &CanonicalRelationEdge,
    ) -> Result<RelationAdmission, RelationRejection> {
        if !is_canonical_relation(&edge.relation) {
            return Err(RelationRejection::NonCanonicalRelation);
        }
        if edge.provenance_producer.trim().is_empty() || edge.provenance_ref.trim().is_empty() {
            return Err(RelationRejection::MissingProvenance);
        }
        if edge.source_id.trim().is_empty() || edge.target_id.trim().is_empty() {
            return Err(RelationRejection::UnresolvedSource);
        }
        let conn = self.lock();
        Self::record_canonical_relation_on(&conn, edge)
    }

    /// Same admission as `record_canonical_relation`, but on a caller-held
    /// connection or transaction.
    ///
    /// The production writers of these relations are already inside the
    /// transaction that creates the fact the relation describes — a
    /// supersession updates `memories` and records the `supersedes` edge as one
    /// act. Recording the edge afterwards on a fresh lock would let a crash
    /// between the two commit a supersession with no evidence relation, which
    /// is precisely the divergence this table exists to prevent. SQLite has no
    /// nested transactions, so a caller holding one must use this.
    pub fn record_canonical_relation_on(
        conn: &Connection,
        edge: &CanonicalRelationEdge,
    ) -> Result<RelationAdmission, RelationRejection> {
        if !is_canonical_relation(&edge.relation) {
            return Err(RelationRejection::NonCanonicalRelation);
        }
        if edge.provenance_producer.trim().is_empty() || edge.provenance_ref.trim().is_empty() {
            return Err(RelationRejection::MissingProvenance);
        }
        if edge.source_id.trim().is_empty() || edge.target_id.trim().is_empty() {
            return Err(RelationRejection::UnresolvedSource);
        }
        let source_resolved: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM memories WHERE id=?1)",
                [&edge.source_id],
                |row| row.get(0),
            )
            .map_err(|error| RelationRejection::Storage(error.to_string()))?;
        if !source_resolved {
            return Err(RelationRejection::UnresolvedSource);
        }
        let target_resolved: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM memories WHERE id=?1)",
                [&edge.target_id],
                |row| row.get(0),
            )
            .map_err(|error| RelationRejection::Storage(error.to_string()))?;
        if !target_resolved {
            return Err(RelationRejection::UnresolvedTarget);
        }
        let relation_id = format!(
            "rel.{}",
            rightkit_fs::digest::sha256_bytes(
                format!("{}|{}|{}", edge.source_id, edge.relation, edge.target_id).as_bytes()
            )
        );
        conn.execute(
            "INSERT INTO memory_relation
                (relation_id, source_id, target_id, relation, provenance_producer,
                 provenance_ref, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(source_id, target_id, relation) DO UPDATE SET
                provenance_producer=excluded.provenance_producer,
                provenance_ref=excluded.provenance_ref,
                created_at=excluded.created_at",
            rusqlite::params![
                relation_id,
                edge.source_id,
                edge.target_id,
                edge.relation,
                edge.provenance_producer,
                edge.provenance_ref,
                edge.created_at,
            ],
        )
        .map_err(|error| RelationRejection::Storage(error.to_string()))?;
        Ok(RelationAdmission {
            relation_id,
            target_resolved,
        })
    }

    /// Every stored relation on `source_id`, with its diagnostic disposition.
    /// This is the diagnostic view: dangling and lifecycle-ineligible edges
    /// are present here and only here.
    pub fn canonical_relations_from(&self, source_id: &str) -> Result<Vec<StoredRelation>, String> {
        let conn = self.lock();
        stored_relations(&conn, source_id)
    }

    /// Only relations that may be traversed as live evidence: canonical
    /// relation, resolved target, and a target that is lifecycle-eligible
    /// (active, not superseded, not quarantined, not recall-suppressed).
    pub fn traversable_relations_from(
        &self,
        source_id: &str,
    ) -> Result<Vec<StoredRelation>, String> {
        Ok(self
            .canonical_relations_from(source_id)?
            .into_iter()
            .filter(|relation| relation.traversable())
            .collect())
    }

    pub fn event_db_path(&self) -> Option<&Path> {
        self.event_db_path.as_deref().map(PathBuf::as_path)
    }

    /// Startup-only WAL observations for main Cortex storage then physical event ledger.
    pub fn startup_wal_reports(&self) -> &[StartupWalReport; 2] {
        &self.startup_wal
    }

    /// Probe the live SQLite connection without waiting behind another request. The forge query
    /// verifies both connection execution and the required primary table; an empty table is healthy.
    pub fn health_probe(&self) -> MemDbProbe {
        let (conn, recovered_poison) = match self.conn.try_lock() {
            Ok(conn) => (conn, false),
            Err(std::sync::TryLockError::WouldBlock) => return MemDbProbe::Busy,
            Err(std::sync::TryLockError::Poisoned(error)) => (error.into_inner(), true),
        };
        match conn
            .query_row("SELECT 1 FROM memories LIMIT 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .optional()
        {
            Ok(_) => {
                if recovered_poison {
                    self.conn.clear_poison();
                }
                MemDbProbe::Ok
            }
            Err(_) => MemDbProbe::Error,
        }
    }

    /// Record one recall's full-corpus-vs-injected-skeleton size, so savings are measurable instead
    /// of assumed. `source` separates agent-facing serve recalls from human CLI debugging (the
    /// effectiveness numbers read serve rows only); `query_preview` (first ~120 chars) makes
    /// ranking changes verifiable by replaying real traffic. Best-effort: a logging failure must
    /// never break a recall response.
    ///
    /// 2026-07-09 (add-now plan, Phase 1): `client`/`session_id`/`cwd_scope`/`hook_event`/
    /// `trace_id`/`client_visibility` add cross-client observability. All optional; absent values
    /// default to NULL/`unknown`/`all`.
    #[allow(clippy::too_many_arguments)]
    pub fn log_recall(
        &self,
        ts: &str,
        scope: Option<&str>,
        query_chars: usize,
        hit_count: usize,
        full_chars: usize,
        injected_chars: usize,
        source: &str,
        query_preview: Option<&str>,
        client: Option<&str>,
        session_id: Option<&str>,
        cwd_scope: Option<&str>,
        hook_event: Option<&str>,
        trace_id: Option<&str>,
        client_visibility: Option<&str>,
    ) {
        self.log_recall_with_replay(
            ts,
            scope,
            query_chars,
            hit_count,
            full_chars,
            injected_chars,
            source,
            query_preview,
            client,
            session_id,
            cwd_scope,
            hook_event,
            trace_id,
            client_visibility,
            "production",
            "[]",
            "[]",
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn log_recall_with_replay(
        &self,
        ts: &str,
        scope: Option<&str>,
        query_chars: usize,
        hit_count: usize,
        full_chars: usize,
        injected_chars: usize,
        source: &str,
        query_preview: Option<&str>,
        client: Option<&str>,
        session_id: Option<&str>,
        cwd_scope: Option<&str>,
        hook_event: Option<&str>,
        trace_id: Option<&str>,
        client_visibility: Option<&str>,
        traffic_class: &str,
        candidate_hits: &str,
        admitted_hits: &str,
    ) {
        let _ = self.log_recall_with_replay_to(
            RecallLogSink::Production,
            ts,
            scope,
            query_chars,
            hit_count,
            full_chars,
            injected_chars,
            source,
            query_preview,
            client,
            session_id,
            cwd_scope,
            hook_event,
            trace_id,
            client_visibility,
            traffic_class,
            candidate_hits,
            admitted_hits,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn log_recall_with_replay_to(
        &self,
        sink: RecallLogSink,
        ts: &str,
        scope: Option<&str>,
        query_chars: usize,
        hit_count: usize,
        full_chars: usize,
        injected_chars: usize,
        source: &str,
        query_preview: Option<&str>,
        client: Option<&str>,
        session_id: Option<&str>,
        cwd_scope: Option<&str>,
        hook_event: Option<&str>,
        trace_id: Option<&str>,
        client_visibility: Option<&str>,
        traffic_class: &str,
        candidate_hits: &str,
        admitted_hits: &str,
    ) -> Result<(), String> {
        self.log_recall_with_replay_to_identity(
            sink,
            ts,
            scope,
            query_chars,
            hit_count,
            full_chars,
            injected_chars,
            source,
            query_preview,
            client,
            session_id,
            cwd_scope,
            hook_event,
            trace_id,
            client_visibility,
            traffic_class,
            candidate_hits,
            admitted_hits,
            None,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn log_recall_with_replay_to_identity(
        &self,
        sink: RecallLogSink,
        ts: &str,
        scope: Option<&str>,
        query_chars: usize,
        hit_count: usize,
        full_chars: usize,
        injected_chars: usize,
        source: &str,
        query_preview: Option<&str>,
        client: Option<&str>,
        session_id: Option<&str>,
        cwd_scope: Option<&str>,
        hook_event: Option<&str>,
        trace_id: Option<&str>,
        client_visibility: Option<&str>,
        traffic_class: &str,
        candidate_hits: &str,
        admitted_hits: &str,
        installation_id: Option<&str>,
        service_instance_id: Option<&str>,
        turn_id: Option<&str>,
    ) -> Result<(), String> {
        let event_uid = format!("event.{}", rightkit_logs::identity::random_uuid());
        let identity_status = if installation_id.is_some()
            && service_instance_id.is_some()
            && session_id.is_some()
            && turn_id.is_some()
            && trace_id.is_some()
        {
            "observed"
        } else {
            "legacy_unattributed"
        };
        let fallback_session = opaque_legacy_identity("session-", &event_uid);
        let fallback_turn = opaque_legacy_identity("turn-", &event_uid);
        let fallback_trace = opaque_legacy_identity("trace-", &event_uid);
        let conn = self.lock();
        match sink {
            RecallLogSink::Production => conn.execute(
                "INSERT INTO recall_log
                    (ts, scope, query_chars, hit_count, full_chars, injected_chars, source,
                     query_preview, client, session_id, cwd_scope, hook_event, trace_id,
                     client_visibility, traffic_class, candidate_hits, admitted_hits,
                     event_uid, installation_id, service_instance_id, turn_id, identity_status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                         ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
                rusqlite::params![
                    ts,
                    scope,
                    query_chars as i64,
                    hit_count as i64,
                    full_chars as i64,
                    injected_chars as i64,
                    source,
                    query_preview,
                    client.unwrap_or("unknown"),
                    session_id.unwrap_or(&fallback_session),
                    cwd_scope,
                    hook_event,
                    trace_id.unwrap_or(&fallback_trace),
                    client_visibility.unwrap_or("all"),
                    traffic_class,
                    candidate_hits,
                    admitted_hits,
                    event_uid,
                    installation_id,
                    service_instance_id,
                    turn_id.unwrap_or(&fallback_turn),
                    identity_status,
                ],
            ),
            RecallLogSink::Smoke => conn.execute(
                "INSERT INTO recall_log_smoke
                    (ts, scope, query_chars, hit_count, full_chars, injected_chars, source,
                     query_preview, client, session_id, cwd_scope, hook_event, trace_id,
                     client_visibility, traffic_class, candidate_hits, admitted_hits,
                     source_recall_id, original_traffic_class, isolated_at, isolation_reason,
                     event_uid, installation_id, service_instance_id, turn_id, identity_status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                         'smoke', ?16, ?17, NULL, ?15, ?1, 'prospective_smoke',
                         ?18, ?19, ?20, ?21, ?22)",
                rusqlite::params![
                    ts,
                    scope,
                    query_chars as i64,
                    hit_count as i64,
                    full_chars as i64,
                    injected_chars as i64,
                    source,
                    query_preview,
                    client.unwrap_or("unknown"),
                    session_id.unwrap_or(&fallback_session),
                    cwd_scope,
                    hook_event,
                    trace_id.unwrap_or(&fallback_trace),
                    client_visibility.unwrap_or("all"),
                    traffic_class,
                    candidate_hits,
                    admitted_hits,
                    event_uid,
                    installation_id,
                    service_instance_id,
                    turn_id.unwrap_or(&fallback_turn),
                    identity_status,
                ],
            ),
        }
        .map(|_| ())
        .map_err(|error| error.to_string())
    }

    /// Effectiveness snapshot over the memories table: corpus size, distinct entries ever
    /// injected (serve path bumps inject_count), and distinct injected entries later fetched in
    /// full (`get` bumps access_count). The fetch rate is a LOWER BOUND on usefulness — preview
    /// sufficiency and fetch friction both suppress it (§10.2 paired-metric protocol).
    pub fn effectiveness_metrics(&self) -> (u64, u64, u64) {
        let conn = self.lock();
        conn.query_row(
            "SELECT (SELECT COUNT(*) FROM memories), \
                    (SELECT COUNT(*) FROM memories WHERE inject_count > 0), \
                    (SELECT COUNT(*) FROM memories WHERE inject_count > 0 AND access_count > 0)",
            [],
            |r| {
                Ok((
                    r.get::<_, i64>(0)? as u64,
                    r.get::<_, i64>(1)? as u64,
                    r.get::<_, i64>(2)? as u64,
                ))
            },
        )
        .unwrap_or((0, 0, 0))
    }

    /// Most recent recall-log rows (live-usage feed for the dashboard), newest first.
    pub fn recent_recalls(&self, n: usize) -> Vec<RecentRecall> {
        let conn = self.lock();
        conn.prepare(
            "SELECT ts, COALESCE(scope,''), query_chars, hit_count, injected_chars,
                    traffic_class, candidate_hits, admitted_hits \
             FROM recall_log ORDER BY ts DESC LIMIT ?1",
        )
        .and_then(|mut st| {
            st.query_map([n as i64], |r| {
                Ok(RecentRecall {
                    ts: r.get(0)?,
                    scope: r.get(1)?,
                    query_chars: r.get(2)?,
                    hit_count: r.get(3)?,
                    injected_chars: r.get(4)?,
                    traffic_class: r.get(5)?,
                    candidate_hits: r.get(6)?,
                    admitted_hits: r.get(7)?,
                })
            })
            .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default()
    }

    /// Aggregate the recall log into a savings summary. `None` if no recalls have been logged yet.
    /// Serve rows only: CLI debugging recalls (`source='cli'`) never skew the agent-facing numbers.
    pub fn recall_metrics(&self) -> Option<RecallMetrics> {
        let conn = self.lock();
        conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(hit_count),0), COALESCE(SUM(full_chars),0), \
             COALESCE(SUM(injected_chars),0), MIN(ts), MAX(ts) FROM recall_log \
             WHERE source = 'serve' AND query_chars > 0 AND traffic_class = 'production'",
            [],
            |row| {
                let recalls: i64 = row.get(0)?;
                if recalls == 0 {
                    return Ok(None);
                }
                Ok(Some(RecallMetrics {
                    recalls: recalls as u64,
                    hits: row.get::<_, i64>(1)? as u64,
                    full_chars: row.get::<_, i64>(2)? as u64,
                    injected_chars: row.get::<_, i64>(3)? as u64,
                    first_ts: row.get(4)?,
                    last_ts: row.get(5)?,
                }))
            },
        )
        .ok()
        .flatten()
    }

    /// 2026-07-09 (add-now plan, Phase 1): per-client recall counts over serve rows.
    /// Returns (client, count) ordered by count desc, then client asc for stable output.
    /// `client = 'unknown'` is the value for hooks that never set the field — kept as a separate bucket so we can
    /// tell "no client claim" from "zero recalls".
    pub fn client_recall_counts(&self) -> Vec<(String, u64)> {
        let conn = self.lock();
        conn.prepare(
            "SELECT client, COUNT(*) FROM recall_log WHERE source = 'serve' \
             AND query_chars > 0 AND traffic_class = 'production' \
             AND (client != 'unknown' OR query_preview IS NOT NULL) \
             GROUP BY client ORDER BY COUNT(*) DESC, client ASC",
        )
        .and_then(|mut st| {
            st.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
            })
            .map(|rows| rows.flatten().collect())
        })
        .unwrap_or_default()
    }

    /// Attribution quality for interpreting `client='unknown'`.
    ///
    /// Rows without `query_preview` or client metadata stay visible as unattributed rather than
    /// being treated as active pollution.
    /// Empty-query rows are excluded from primary metrics, but kept here as a raw hygiene count.
    pub fn attribution_quality(&self) -> AttributionQuality {
        let conn = self.lock();
        conn.query_row(
            "SELECT \
               SUM(CASE WHEN source='serve' AND query_chars > 0 AND client!='unknown' THEN 1 ELSE 0 END), \
               SUM(CASE WHEN source='serve' AND client='unknown' THEN 1 ELSE 0 END), \
               SUM(CASE WHEN source='serve' AND client='unknown' AND query_chars > 0 AND query_preview IS NULL THEN 1 ELSE 0 END), \
               SUM(CASE WHEN source='serve' AND client='unknown' AND query_chars = 0 THEN 1 ELSE 0 END), \
               SUM(CASE WHEN source='serve' AND client='unknown' AND query_chars > 0 AND query_preview IS NOT NULL THEN 1 ELSE 0 END), \
               SUM(CASE WHEN source='serve' AND client='unknown' AND query_chars > 0 \
                         AND query_preview IS NOT NULL \
                         AND identity_status='legacy_unattributed' \
                         AND cwd_scope IS NULL AND hook_event IS NULL \
                    THEN 1 ELSE 0 END), \
               MIN(CASE WHEN source='serve' AND client='unknown' THEN ts ELSE NULL END), \
               MAX(CASE WHEN source='serve' AND client='unknown' THEN ts ELSE NULL END) \
             FROM recall_log WHERE traffic_class = 'production'",
            [],
            |r| {
                Ok(AttributionQuality {
                    attributed_meaningful: r.get::<_, Option<i64>>(0)?.unwrap_or(0) as u64,
                    unknown_total_raw: r.get::<_, Option<i64>>(1)?.unwrap_or(0) as u64,
                    unknown_legacy_no_preview: r.get::<_, Option<i64>>(2)?.unwrap_or(0) as u64,
                    unknown_empty_query: r.get::<_, Option<i64>>(3)?.unwrap_or(0) as u64,
                    unknown_current_meaningful: r.get::<_, Option<i64>>(4)?.unwrap_or(0) as u64,
                    unknown_current_no_metadata: r.get::<_, Option<i64>>(5)?.unwrap_or(0) as u64,
                    unknown_first_ts: r.get(6)?,
                    unknown_last_ts: r.get(7)?,
                })
            },
        )
        .unwrap_or_default()
    }
}

/// Aggregate token-savings report: what would have been sent if every recall dumped full memory
/// content, vs what was actually injected (the skeleton).
pub struct RecentRecall {
    pub ts: String,
    pub scope: String,
    pub query_chars: i64,
    pub hit_count: i64,
    pub injected_chars: i64,
    pub traffic_class: String,
    pub candidate_hits: String,
    pub admitted_hits: String,
}

pub struct RecallMetrics {
    pub recalls: u64,
    pub hits: u64,
    pub full_chars: u64,
    pub injected_chars: u64,
    pub first_ts: String,
    pub last_ts: String,
}

#[derive(Default)]
pub struct AttributionQuality {
    pub attributed_meaningful: u64,
    pub unknown_total_raw: u64,
    pub unknown_legacy_no_preview: u64,
    pub unknown_empty_query: u64,
    pub unknown_current_meaningful: u64,
    pub unknown_current_no_metadata: u64,
    pub unknown_first_ts: Option<String>,
    pub unknown_last_ts: Option<String>,
}

impl RecallMetrics {
    /// Chars saved per recall by injecting a skeleton instead of full content.
    pub fn chars_saved(&self) -> i64 {
        self.full_chars as i64 - self.injected_chars as i64
    }

    /// Rough token estimate at ~4 chars/token (no tokenizer dependency, intentionally approximate).
    pub fn tokens_saved(&self) -> i64 {
        self.chars_saved() / 4
    }
}
