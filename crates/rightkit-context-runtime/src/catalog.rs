//! Central context catalog — the G3B storage lane.
//!
//! Strict storage boundary (locked 2026-07-12, dispatch §G3B):
//!
//! ```text
//! Cortex DB               durable memories only
//! <context-home>/catalog.db identities, handles, generations, scope grants,
//!                           retrieval events, capabilities, receipts
//! <context-home>/repos/...  separately attachable graph generations
//! ```
//!
//! The catalog NEVER reuses the Cortex durable-memory DB. It owns a
//! separate SQLite file, lives in a dedicated directory, creates its own
//! final-form schema (`PRAGMA user_version` = 1), and exposes a small,
//! planner-specific surface (issuance / lookup / revoke for scope grants, plus
//! append-only event recording for retrieval events and acceptance of planner
//! receipts). Content-free by construction — receipts are stored by id only,
//! never with raw prompt or repository text.
//!
//! The catalog is `failure-isolated` from the existing memory lanes: a planner
//! error on the catalog path must never bring down `get`/`put`. The serve
//! router enforces this by passing a `MemDb` and a `ContextCatalog` as
//! independent handles; the catalog connection is wrapped in its own mutex so a
//! poisoned/panicked helper degrades without poisoning the route's other DB.

use rightkit_context_protocol::ReadPathV1;
use rightkit_db::rusqlite::{self, params, Connection, OptionalExtension};
use rightkit_db::{MigrateOptions, Migration};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// Schema-version of the catalog: one final-form schema, created once.
pub const CATALOG_SCHEMA_VERSION: i64 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogAlternate {
    pub path: PathBuf,
    pub status: String,
    pub schema_version: Option<i64>,
    pub catalog_installation_id: Option<String>,
    pub duplicate_catalog_identity: bool,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogStartupReport {
    pub canonical_path: PathBuf,
    pub status: String,
    pub degradation_reason: Option<String>,
    pub journal_mode: String,
    pub schema_version: i64,
    pub catalog_installation_id: String,
    pub same_schema_alternate_count: usize,
    pub alternate_catalogs: Vec<CatalogAlternate>,
}

/// Typed resolver for one absolute catalog identity shared with provider shims.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CatalogPathError {
    #[error("catalog path binding {binding} must be absolute: {value}")]
    Relative {
        binding: &'static str,
        value: String,
    },
    #[error("catalog path is unbound: no catalog, context home, database or workspace binding")]
    MissingBinding,
}

fn absolute_binding(
    binding: &'static str,
    value: Option<std::ffi::OsString>,
) -> Result<Option<PathBuf>, CatalogPathError> {
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let path = PathBuf::from(&value);
    if !path.is_absolute() {
        return Err(CatalogPathError::Relative {
            binding,
            value: path.to_string_lossy().into_owned(),
        });
    }
    Ok(Some(path))
}

pub fn resolve_catalog_path_from(
    membrane_catalog: Option<std::ffi::OsString>,
    context_home: Option<std::ffi::OsString>,
    cortex_db: Option<std::ffi::OsString>,
    workspace_root: Option<std::ffi::OsString>,
) -> Result<PathBuf, CatalogPathError> {
    if let Some(path) = absolute_binding("catalog", membrane_catalog)? {
        return Ok(path);
    }
    if let Some(path) = absolute_binding("context home", context_home)? {
        return Ok(path.join("catalog.db"));
    }
    if let Some(path) = absolute_binding("cortex database", cortex_db)? {
        return Ok(path
            .parent()
            .expect("absolute database path has a parent")
            .join("catalog.db"));
    }
    if let Some(path) = absolute_binding("workspace root", workspace_root)? {
        return Ok(path.join("tools/.cache/memory/catalog.db"));
    }
    Err(CatalogPathError::MissingBinding)
}

/// Resolve one canonical catalog path without current-directory fallback.
pub fn default_catalog_path() -> Result<PathBuf, CatalogPathError> {
    // Installed CLI, MCP & Hub share this identity without Hub-populated env.
    if let Ok(exe) = std::env::current_exe() {
        if let Ok(runtime) = crate::service::runtime_from_installed_exe(&exe) {
            return Ok(runtime
                .db
                .parent()
                .expect("installed database parent")
                .join("catalog.db"));
        }
    }
    resolve_catalog_path_from(
        crate::qa_env::catalog(),
        crate::qa_env::context_home(),
        crate::qa_env::cortex_db(),
        crate::qa_env::workspace_root(),
    )
}

/// Handle to the central context catalog. Cheap to clone (Arc-shared); all
/// callers serialise through the inner mutex.
#[derive(Clone)]
pub struct ContextCatalog {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
    startup_report: CatalogStartupReport,
}

impl ContextCatalog {
    /// Open (or create) the catalog at `path`, WAL + busy timeout, run
    /// migrations. The path is recorded so health/output can echo it; the
    /// Cortex DB is never touched.
    pub fn open<P: AsRef<Path>>(path: P) -> rusqlite::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
        }
        // Durable catalog: rightkit-db writer (WAL with verified fallback, 5s busy
        // wait, Full durability, temp_store=MEMORY).
        let mut options = rightkit_db::DbOptions::new(&path);
        options.durability = rightkit_db::DurabilityProfile::Full;
        let conn = options.open_writer().map_err(catalog_open_error)?;
        migrate(&conn)?;
        let startup_report = startup_report(&conn, &path, &known_catalog_candidates(&path))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            path,
            startup_report,
        })
    }

    /// In-memory ephemeral catalog for tests. Mirrors MemDb::open_in_memory but
    /// lives in a separate connection so it can never accidentally write to
    /// the Cortex DB.
    pub fn open_in_memory() -> Self {
        let conn = rightkit_db::open_in_memory().expect("open in-memory catalog");
        migrate(&conn).expect("migrate in-memory catalog");
        let startup_report =
            startup_report(&conn, Path::new(":memory:"), &[]).expect("inventory in-memory catalog");
        Self {
            conn: Arc::new(Mutex::new(conn)),
            path: PathBuf::from(":memory:"),
            startup_report,
        }
    }

    /// Path to the catalog SQLite file. Surfaced in `/health` and tests.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn startup_report(&self) -> &CatalogStartupReport {
        &self.startup_report
    }

    /// Lock the connection for a unit of work. Poison-recovering so a panicked
    /// planner write cannot take down the whole service (mirrors the
    /// MemDb::lock contract).
    pub fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Server-issued monotonic timestamp seconds for fixtures.
    pub fn now_unix() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}

// ---- Schema ---------------------------------------------------------------
//
// The catalog is created once in its final form; it never rewrites the Cortex DB.

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS identities (
    client          TEXT PRIMARY KEY,
    first_seen_unix INTEGER NOT NULL,
    last_seen_unix  INTEGER NOT NULL,
    capability      TEXT NOT NULL DEFAULT 'standard'
);
CREATE TABLE IF NOT EXISTS handles (
    handle          TEXT PRIMARY KEY,
    client          TEXT NOT NULL,
    -- a stable per-session handle assigned by the gateway
    issued_at_unix  INTEGER NOT NULL,
    metadata        TEXT NOT NULL DEFAULT '{}'
);
CREATE TABLE IF NOT EXISTS generations (
    generation_id   TEXT PRIMARY KEY,
    repository_id   TEXT NOT NULL,
    source_root     TEXT NOT NULL,
    captured_at_unix INTEGER NOT NULL,
    manifest_digest TEXT NOT NULL,
    -- append-only lifecycle (no UPDATE allowed)
    status          TEXT NOT NULL CHECK (status IN ('fresh','stale','quarantined'))
                    DEFAULT 'fresh'
);
CREATE TABLE IF NOT EXISTS scope_grants (
    id              TEXT PRIMARY KEY,
    issuer          TEXT NOT NULL,
    client          TEXT NOT NULL,
    repository_ids  TEXT NOT NULL,
    permitted_edges TEXT NOT NULL,
    task_id         TEXT NOT NULL,
    session_id      TEXT NOT NULL,
    issued_at_unix  INTEGER NOT NULL,
    expires_at_unix INTEGER NOT NULL,
    status          TEXT NOT NULL CHECK (status IN ('active','revoked','expired'))
                    DEFAULT 'active',
    nonce           TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    revoked_at_unix INTEGER,
    read_paths_json TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS idx_scope_grants_client
    ON scope_grants(client, status);
CREATE TABLE IF NOT EXISTS retrieval_events (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_unix         INTEGER NOT NULL,
    trace_id        TEXT NOT NULL,
    client          TEXT NOT NULL,
    mode            TEXT NOT NULL,
    provider        TEXT NOT NULL,
    provider_status TEXT NOT NULL,
    fallback_mode   TEXT NOT NULL,
    degradation_reason TEXT NOT NULL,
    source_generation TEXT,
    candidate_count INTEGER NOT NULL,
    admitted_count  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_retrieval_events_trace
    ON retrieval_events(trace_id);
CREATE TABLE IF NOT EXISTS capabilities (
    capability      TEXT PRIMARY KEY,
    description     TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS receipts (
    receipt_id      TEXT PRIMARY KEY,
    trace_id        TEXT NOT NULL,
    candidate_id    TEXT NOT NULL,
    decision        TEXT NOT NULL,
    reason          TEXT NOT NULL,
    provider        TEXT NOT NULL,
    provider_status TEXT NOT NULL,
    fallback_mode   TEXT NOT NULL,
    degradation_reason TEXT NOT NULL,
    written_at_unix  INTEGER NOT NULL,
    -- receipt row is content-free: text is forbidden by schema, no column for it.
    bytes_sha256    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_receipts_trace
    ON receipts(trace_id);
CREATE TABLE IF NOT EXISTS delivery_acknowledgements (
    installation_id       TEXT NOT NULL,
    repository_id         TEXT NOT NULL,
    request_id            TEXT NOT NULL,
    trace_id              TEXT NOT NULL,
    task_id               TEXT NOT NULL DEFAULT '',
    session_id            TEXT NOT NULL DEFAULT '',
    context_epoch         TEXT NOT NULL,
    publication_id        TEXT NOT NULL,
    representation_digest TEXT NOT NULL,
    packet_digest         TEXT NOT NULL,
    host_serialized_digest TEXT NOT NULL,
    acknowledged_at_unix  INTEGER NOT NULL,
    PRIMARY KEY (installation_id, repository_id, request_id, trace_id,
                 context_epoch, publication_id, representation_digest, packet_digest)
);
CREATE INDEX IF NOT EXISTS idx_delivery_acknowledgements_lookup
    ON delivery_acknowledgements (installation_id, repository_id, request_id, trace_id);
CREATE TABLE IF NOT EXISTS pending_pull_publications (
    installation_id       TEXT NOT NULL,
    repository_id         TEXT NOT NULL,
    request_id            TEXT NOT NULL,
    trace_id              TEXT NOT NULL,
    task_id               TEXT NOT NULL,
    session_id            TEXT NOT NULL,
    context_epoch         INTEGER NOT NULL,
    publication_id        TEXT NOT NULL,
    representation_identity TEXT NOT NULL,
    source_ref            TEXT NOT NULL,
    representation_digest TEXT NOT NULL,
    packet_digest         TEXT NOT NULL,
    PRIMARY KEY (installation_id, repository_id, request_id, trace_id,
                 task_id, session_id, context_epoch, publication_id)
);
";

const CATALOG_METADATA_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS catalog_metadata (
    singleton               INTEGER PRIMARY KEY CHECK (singleton = 1),
    catalog_installation_id TEXT NOT NULL UNIQUE,
    created_at_unix         INTEGER NOT NULL
);
";

const MIGRATIONS: &[Migration] = &[Migration::code(1, "catalog_schema", create_catalog)];

fn create_catalog(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)?;
    conn.execute_batch(CATALOG_METADATA_SCHEMA)?;
    conn.execute(
        "INSERT INTO catalog_metadata
            (singleton, catalog_installation_id, created_at_unix)
         VALUES (1, ?1, ?2)",
        params![
            rightkit_logs::identity::random_uuid(),
            ContextCatalog::now_unix()
        ],
    )?;
    Ok(())
}

/// Create the catalog in its final schema. No backup and no marker file (the catalog is
/// rebuilt from its own sources).
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let options = MigrateOptions {
        backup: false,
        optimize_after: false,
        ..MigrateOptions::default()
    };
    rightkit_db::migrate(conn, None, MIGRATIONS, &options)
        .map(|_| ())
        .map_err(|error| match error {
            rightkit_db::DbError::MigrationFailed { source, .. } => source,
            other => catalog_open_error(other),
        })
}

fn catalog_installation_id(conn: &Connection) -> rusqlite::Result<String> {
    conn.query_row(
        "SELECT catalog_installation_id FROM catalog_metadata WHERE singleton = 1",
        [],
        |row| row.get(0),
    )
}

fn same_file(left: &Path, right: &Path) -> bool {
    left == right
        || match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        }
}

fn known_catalog_candidates(canonical: &Path) -> Vec<PathBuf> {
    let mut candidates = BTreeSet::new();
    if let Some(value) = crate::qa_env::catalog() {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            candidates.insert(path);
        }
    }
    if let Some(value) = crate::qa_env::context_home() {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            candidates.insert(path.join("catalog.db"));
        }
    }
    if let Some(value) = crate::qa_env::cortex_db() {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            if let Some(parent) = path.parent() {
                candidates.insert(parent.join("catalog.db"));
            }
        }
    }
    if let Some(value) = crate::qa_env::workspace_root() {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            candidates.insert(path.join("catalog.db"));
            candidates.insert(path.join("tools/.cache/memory/catalog.db"));
        }
    }
    for value in [std::env::var_os("HOME"), std::env::var_os("USERPROFILE")]
        .into_iter()
        .flatten()
    {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            candidates.insert(path.join(".claude/membrane/catalog.db"));
        }
    }
    candidates
        .into_iter()
        .filter(|path| !same_file(path, canonical))
        .collect()
}

fn inventory_catalog_alternates(
    canonical: &Path,
    canonical_id: &str,
    candidates: &[PathBuf],
) -> Vec<CatalogAlternate> {
    let mut seen = BTreeSet::new();
    let mut alternates = Vec::new();
    for path in candidates {
        if !path.is_absolute()
            || same_file(path, canonical)
            || !path.is_file()
            || !seen.insert(path.clone())
        {
            continue;
        }
        let alternate = match rightkit_db::open_read_only(path) {
            Ok(conn) => match rightkit_db::user_version(&conn).map_err(catalog_open_error) {
                Ok(schema_version) => {
                    let alternate_id = if schema_version >= 1 {
                        catalog_installation_id(&conn).ok()
                    } else {
                        None
                    };
                    let identity_unavailable =
                        schema_version == CATALOG_SCHEMA_VERSION && alternate_id.is_none();
                    CatalogAlternate {
                        path: path.clone(),
                        status: if identity_unavailable {
                            "unavailable".to_string()
                        } else if schema_version == CATALOG_SCHEMA_VERSION {
                            "same_schema".to_string()
                        } else {
                            "different_schema".to_string()
                        },
                        schema_version: Some(schema_version),
                        duplicate_catalog_identity: alternate_id.as_deref() == Some(canonical_id),
                        catalog_installation_id: alternate_id,
                        reason: identity_unavailable
                            .then(|| "catalog_identity_unavailable".to_string()),
                    }
                }
                Err(error) => CatalogAlternate {
                    path: path.clone(),
                    status: "unavailable".to_string(),
                    schema_version: None,
                    catalog_installation_id: None,
                    duplicate_catalog_identity: false,
                    reason: Some(format!("read_user_version: {error}")),
                },
            },
            Err(error) => CatalogAlternate {
                path: path.clone(),
                status: "unavailable".to_string(),
                schema_version: None,
                catalog_installation_id: None,
                duplicate_catalog_identity: false,
                reason: Some(format!("read_only_open: {error}")),
            },
        };
        alternates.push(alternate);
    }
    alternates.sort_by(|left, right| left.path.cmp(&right.path));
    alternates
}

/// Maps a rightkit-db open failure onto the `rusqlite::Result` the catalog API exposes.
/// SQLite errors pass through unchanged; bootstrap filesystem failures are wrapped.
fn catalog_open_error(error: rightkit_db::DbError) -> rusqlite::Error {
    match error {
        rightkit_db::DbError::Sqlite(error) => error,
        other => rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(other),
        ),
    }
}

fn startup_report(
    conn: &Connection,
    path: &Path,
    candidates: &[PathBuf],
) -> rusqlite::Result<CatalogStartupReport> {
    let journal_mode = rightkit_db::journal_mode(conn).map_err(catalog_open_error)?;
    let schema_version = rightkit_db::user_version(conn).map_err(catalog_open_error)?;
    let catalog_installation_id = catalog_installation_id(conn)?;
    let alternate_catalogs =
        inventory_catalog_alternates(path, &catalog_installation_id, candidates);
    let same_schema_alternate_count = alternate_catalogs
        .iter()
        .filter(|alternate| alternate.schema_version == Some(CATALOG_SCHEMA_VERSION))
        .count();
    let journal_is_expected = journal_mode == "wal" || path == Path::new(":memory:");
    let alternate_degradation = alternate_catalogs.iter().find_map(|alternate| {
        if alternate.duplicate_catalog_identity {
            Some("duplicate_catalog_identity")
        } else if alternate.status == "unavailable" {
            Some("alternate_catalog_unavailable")
        } else {
            None
        }
    });
    let degradation_reason = if !journal_is_expected {
        Some("journal_mode_not_wal".to_string())
    } else {
        alternate_degradation.map(str::to_string)
    };
    Ok(CatalogStartupReport {
        canonical_path: path.to_path_buf(),
        status: if degradation_reason.is_some() {
            "degraded"
        } else {
            "ok"
        }
        .to_string(),
        degradation_reason,
        journal_mode,
        schema_version,
        catalog_installation_id,
        same_schema_alternate_count,
        alternate_catalogs,
    })
}

// ---- Scope grants --------------------------------------------------------

/// Server-minted `ScopeGrantV1` mirror. The canonical contract is the JSON
/// schema in `tools/lib/context-contracts.schema.json`; the Rust mirror is the
/// authority for the lookup path in the resident service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopeGrant {
    pub id: String,
    pub issuer: String,
    pub client: String,
    pub repository_ids: Vec<String>,
    pub permitted_edge_types: Vec<String>,
    pub read_paths: Vec<ReadPathV1>,
    pub task_id: String,
    pub session_id: String,
    pub issued_at_unix: i64,
    pub expires_at_unix: i64,
    pub status: GrantStatus,
    pub nonce: String,
    pub manifest_digest: String,
    pub revoked_at_unix: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantStatus {
    Active,
    Revoked,
    Expired,
}

impl GrantStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            GrantStatus::Active => "active",
            GrantStatus::Revoked => "revoked",
            GrantStatus::Expired => "expired",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "revoked" => GrantStatus::Revoked,
            "expired" => GrantStatus::Expired,
            _ => GrantStatus::Active,
        }
    }
}

/// Issue a new scope grant. The caller provides the derived fields; the
/// catalog stores the row and returns it. Issued grants start `active` and
/// expire automatically on lookup if `expires_at_unix` has elapsed.
#[allow(clippy::too_many_arguments)]
pub fn issue_scope_grant(
    catalog: &ContextCatalog,
    id: &str,
    client: &str,
    repository_ids: &[String],
    permitted_edge_types: &[String],
    read_paths: &[ReadPathV1],
    task_id: &str,
    session_id: &str,
    ttl_seconds: i64,
    nonce: &str,
    manifest_digest: &str,
) -> rusqlite::Result<ScopeGrant> {
    let now = ContextCatalog::now_unix();
    let expires = now.saturating_add(ttl_seconds.max(1));
    let repo_csv = repository_ids.join(",");
    let edge_csv = permitted_edge_types.join(",");
    let read_paths_json = serde_json::to_string(read_paths)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let conn = catalog.lock();
    conn.execute(
        "INSERT INTO scope_grants
            (id, issuer, client, repository_ids, permitted_edges, read_paths_json,
             task_id, session_id, issued_at_unix, expires_at_unix,
             status, nonce, manifest_digest, revoked_at_unix)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, NULL)",
        params![
            id,
            "membrane-gateway",
            client,
            repo_csv,
            edge_csv,
            read_paths_json,
            task_id,
            session_id,
            now,
            expires,
            GrantStatus::Active.as_str(),
            nonce,
            manifest_digest,
        ],
    )?;
    Ok(ScopeGrant {
        id: id.to_string(),
        issuer: "membrane-gateway".into(),
        client: client.to_string(),
        repository_ids: repository_ids.to_vec(),
        permitted_edge_types: permitted_edge_types.to_vec(),
        read_paths: read_paths.to_vec(),
        task_id: task_id.to_string(),
        session_id: session_id.to_string(),
        issued_at_unix: now,
        expires_at_unix: expires,
        status: GrantStatus::Active,
        nonce: nonce.to_string(),
        manifest_digest: manifest_digest.to_string(),
        revoked_at_unix: None,
    })
}

fn grant_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScopeGrant> {
    let repo_csv: String = row.get("repository_ids")?;
    let edge_csv: String = row.get("permitted_edges")?;
    let status_str: String = row.get("status")?;
    let read_paths_json: String = row.get("read_paths_json")?;
    let read_paths =
        serde_json::from_str::<Vec<ReadPathV1>>(&read_paths_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
    Ok(ScopeGrant {
        id: row.get("id")?,
        issuer: row.get("issuer")?,
        client: row.get("client")?,
        repository_ids: repo_csv
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        permitted_edge_types: edge_csv
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        read_paths,
        task_id: row.get("task_id")?,
        session_id: row.get("session_id")?,
        issued_at_unix: row.get("issued_at_unix")?,
        expires_at_unix: row.get("expires_at_unix")?,
        status: GrantStatus::parse(&status_str),
        nonce: row.get("nonce")?,
        manifest_digest: row.get("manifest_digest")?,
        revoked_at_unix: row.get("revoked_at_unix")?,
    })
}

/// Look up a grant by id. If the grant expired since issue (or has been
/// revoked), returns the observed status — callers fail closed on
/// `Active` checks; expired/revoked permits are explicitly surfaced.
pub fn lookup_grant(catalog: &ContextCatalog, id: &str) -> rusqlite::Result<Option<ScopeGrant>> {
    let conn = catalog.lock();
    let mut stmt = conn.prepare("SELECT * FROM scope_grants WHERE id = ?1")?;
    let grant = stmt.query_row([id], grant_from_row).optional()?;
    if let Some(mut grant) = grant {
        if grant.status == GrantStatus::Active && is_expired(&grant) {
            grant.status = GrantStatus::Expired;
        }
        Ok(Some(grant))
    } else {
        Ok(None)
    }
}

fn is_expired(grant: &ScopeGrant) -> bool {
    ContextCatalog::now_unix() >= grant.expires_at_unix
}

/// Request-scoped read avoids waiting on another caller's catalog mutex or
/// running catalog migrations inside a federation deadline.
pub(crate) fn lookup_grant_until(
    path: &Path,
    id: &str,
    deadline: rightkit_context_federation::deadline::Deadline,
) -> Result<Option<ScopeGrant>, String> {
    let remaining = || {
        let remaining = deadline.remaining_at(std::time::Instant::now());
        if remaining.is_zero() {
            Err("federation deadline exhausted during owner binding".to_owned())
        } else {
            Ok(remaining.min(std::time::Duration::from_secs(5)))
        }
    };
    remaining()?;
    // Request-scoped busy wait: the read-only open itself honors the remaining deadline.
    let mut options = rightkit_db::DbOptions::new(path);
    options.busy_timeout = remaining()?;
    let conn = options
        .open_read_only()
        .map_err(|error| error.to_string())?;
    conn.pragma_update(None, "query_only", "ON")
        .map_err(|error| error.to_string())?;
    let schema_version = rightkit_db::user_version(&conn)
        .map_err(catalog_open_error)
        .map_err(|error| error.to_string())?;
    if schema_version != CATALOG_SCHEMA_VERSION {
        return Err("catalog schema version unavailable for read-only grant lookup".to_owned());
    }
    let mut stmt = conn
        .prepare("SELECT * FROM scope_grants WHERE id = ?1")
        .map_err(|error| error.to_string())?;
    conn.busy_timeout(remaining()?)
        .map_err(|error| error.to_string())?;
    let result = stmt.query_row([id], grant_from_row).optional();
    remaining()?;
    let mut grant = result.map_err(|error| error.to_string())?;
    if let Some(grant) = grant.as_mut() {
        if grant.status == GrantStatus::Active && is_expired(grant) {
            grant.status = GrantStatus::Expired;
        }
    }
    Ok(grant)
}

impl ScopeGrant {
    /// Whether a grant can authorise a planner call right now.
    pub fn permits(&self) -> bool {
        self.status == GrantStatus::Active && !is_expired(self)
    }
}

/// Revoke a grant. Idempotent — revoking an already-revoked grant leaves the
/// original revoke timestamp intact.
pub fn revoke_scope_grant(catalog: &ContextCatalog, id: &str) -> rusqlite::Result<bool> {
    let now = ContextCatalog::now_unix();
    let conn = catalog.lock();
    let changed = conn.execute(
        "UPDATE scope_grants
         SET status = ?1, revoked_at_unix = COALESCE(revoked_at_unix, ?2)
         WHERE id = ?3 AND status = 'active'",
        params![GrantStatus::Revoked.as_str(), now, id],
    )?;
    Ok(changed > 0)
}

// ---- Events + receipts (append-only) -------------------------------------

#[allow(clippy::too_many_arguments)]
pub fn record_retrieval_event(
    catalog: &ContextCatalog,
    trace_id: &str,
    client: &str,
    mode: &str,
    provider: &str,
    provider_status: &str,
    fallback_mode: &str,
    degradation_reason: &str,
    source_generation: Option<&str>,
    candidate_count: usize,
    admitted_count: usize,
) -> rusqlite::Result<()> {
    let conn = catalog.lock();
    conn.execute(
        "INSERT INTO retrieval_events
            (ts_unix, trace_id, client, mode, provider, provider_status,
             fallback_mode, degradation_reason, source_generation,
             candidate_count, admitted_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            ContextCatalog::now_unix(),
            trace_id,
            client,
            mode,
            provider,
            provider_status,
            fallback_mode,
            degradation_reason,
            source_generation,
            candidate_count as i64,
            admitted_count as i64
        ],
    )?;
    Ok(())
}

/// Append-only receipt persistence. `bytes_sha256` is the SHA-256 of the
/// serialised receipt payload (used for dedup + audit) — the raw JSON itself
/// stays the caller's responsibility. Content-free by construction.
#[allow(clippy::too_many_arguments)]
pub fn record_receipt(
    catalog: &ContextCatalog,
    receipt_id: &str,
    trace_id: &str,
    candidate_id: &str,
    decision: &str,
    reason: &str,
    provider: &str,
    provider_status: &str,
    fallback_mode: &str,
    degradation_reason: &str,
    bytes_sha256: &str,
) -> rusqlite::Result<()> {
    let conn = catalog.lock();
    conn.execute(
        "INSERT OR IGNORE INTO receipts
            (receipt_id, trace_id, candidate_id, decision, reason,
             provider, provider_status, fallback_mode, degradation_reason,
             written_at_unix, bytes_sha256)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            receipt_id,
            trace_id,
            candidate_id,
            decision,
            reason,
            provider,
            provider_status,
            fallback_mode,
            degradation_reason,
            ContextCatalog::now_unix(),
            bytes_sha256,
        ],
    )?;
    Ok(())
}

pub fn count_receipts(catalog: &ContextCatalog) -> rusqlite::Result<i64> {
    let conn = catalog.lock();
    conn.query_row("SELECT COUNT(*) FROM receipts", [], |row| row.get(0))
}

pub fn count_receipts_for_trace(catalog: &ContextCatalog, trace_id: &str) -> rusqlite::Result<i64> {
    let conn = catalog.lock();
    conn.query_row(
        "SELECT COUNT(*) FROM receipts WHERE trace_id = ?1",
        [trace_id],
        |row| row.get(0),
    )
}

pub fn count_events(catalog: &ContextCatalog) -> rusqlite::Result<i64> {
    let conn = catalog.lock();
    conn.query_row("SELECT COUNT(*) FROM retrieval_events", [], |row| {
        row.get(0)
    })
}

/// Content-free acknowledgement binding for one delivered Pull publication.
/// Every identity is exact: partial or process-local acknowledgement state is
/// intentionally insufficient for suppression.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryAcknowledgementRecordV1 {
    pub repository_id: String,
    pub request_id: String,
    pub trace_id: String,
    pub task_id: String,
    pub session_id: String,
    pub context_epoch: String,
    pub publication_id: String,
    pub representation_digest: String,
    pub packet_digest: String,
    pub host_serialized_digest: String,
}

impl DeliveryAcknowledgementRecordV1 {
    pub fn is_complete(&self) -> bool {
        [
            &self.repository_id,
            &self.request_id,
            &self.trace_id,
            &self.task_id,
            &self.session_id,
            &self.context_epoch,
            &self.publication_id,
            &self.representation_digest,
            &self.packet_digest,
            &self.host_serialized_digest,
        ]
        .iter()
        .all(|value| !value.trim().is_empty())
    }
}

/// Pull-owned pending-publication evidence. This records publication identity
/// before a host acknowledgement; it is not an acknowledgement authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingPullPublicationV1 {
    pub repository_id: String,
    pub request_id: String,
    pub trace_id: String,
    pub task_id: String,
    pub session_id: String,
    pub context_epoch: u64,
    pub publication_id: String,
    pub representation_identity: String,
    pub source_ref: String,
    pub representation_digest: String,
    pub packet_digest: String,
}

impl PendingPullPublicationV1 {
    pub fn is_complete(&self) -> bool {
        [
            &self.repository_id,
            &self.request_id,
            &self.trace_id,
            &self.task_id,
            &self.session_id,
            &self.publication_id,
            &self.representation_identity,
            &self.source_ref,
            &self.representation_digest,
            &self.packet_digest,
        ]
        .iter()
        .all(|value| !value.trim().is_empty())
    }
}

pub fn record_pending_pull_publication(
    catalog: &ContextCatalog,
    publication: &PendingPullPublicationV1,
) -> rusqlite::Result<bool> {
    if !publication.is_complete() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let installation_id = &catalog.startup_report.catalog_installation_id;
    let conn = catalog.lock();
    insert_pending_pull_publication_sql(&conn, installation_id, publication)
}

fn insert_pending_pull_publication_sql(
    conn: &Connection,
    installation_id: &str,
    publication: &PendingPullPublicationV1,
) -> rusqlite::Result<bool> {
    Ok(conn.execute(
        "INSERT OR IGNORE INTO pending_pull_publications
          (installation_id, repository_id, request_id, trace_id, task_id, session_id,
           context_epoch, publication_id, representation_identity, source_ref,
           representation_digest, packet_digest)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            installation_id,
            publication.repository_id.as_str(),
            publication.request_id.as_str(),
            publication.trace_id.as_str(),
            publication.task_id.as_str(),
            publication.session_id.as_str(),
            publication.context_epoch as i64,
            publication.publication_id.as_str(),
            publication.representation_identity.as_str(),
            publication.source_ref.as_str(),
            publication.representation_digest.as_str(),
            publication.packet_digest.as_str()
        ],
    )? > 0)
}

/// Control-aware Pull publication recording. The resident catalog mutex and
/// SQLite busy wait are both bounded by request control. A transaction keeps
/// an insert out of the catalog when cancellation arrives before commit.
pub(crate) fn record_pending_pull_publication_with_control(
    catalog: &ContextCatalog,
    publication: &PendingPullPublicationV1,
    control: &crate::serve::PushRequestControl,
) -> Result<bool, String> {
    if !publication.is_complete() {
        return Err("invalid pending pull publication".to_owned());
    }
    let ready = || {
        if control.cancellation.is_cancelled() {
            Err("pull request cancelled during catalog publication".to_owned())
        } else if control.deadline.is_exhausted_at(std::time::Instant::now()) {
            Err("pull request deadline exhausted during catalog publication".to_owned())
        } else {
            Ok(control.deadline.remaining_at(std::time::Instant::now()))
        }
    };

    let mut conn = loop {
        let remaining = ready()?;
        match catalog.conn.try_lock() {
            Ok(conn) => break conn,
            Err(std::sync::TryLockError::Poisoned(error)) => break error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(remaining.min(std::time::Duration::from_millis(2)));
            }
        }
    };
    let installation_id = catalog.startup_report.catalog_installation_id.clone();

    loop {
        let remaining = ready()?;
        // Use short busy-handler slices so cancellation is observed while a
        // different SQLite connection holds the database write lock. Restore
        // the catalog's normal timeout after each attempt.
        conn.busy_timeout(remaining.min(std::time::Duration::from_millis(25)))
            .map_err(|error| error.to_string())?;
        let attempt = {
            match conn.transaction() {
                Err(error) => Err(error.to_string()),
                Ok(tx) => {
                    if let Err(error) = ready() {
                        drop(tx);
                        Err(error)
                    } else {
                        let result =
                            insert_pending_pull_publication_sql(&tx, &installation_id, publication)
                                .map_err(|error| error.to_string());
                        match result {
                            Ok(changed) => match ready() {
                                Ok(_) => tx
                                    .commit()
                                    .map(|()| changed)
                                    .map_err(|error| error.to_string()),
                                Err(error) => {
                                    drop(tx);
                                    Err(error)
                                }
                            },
                            Err(error) => {
                                drop(tx);
                                Err(error)
                            }
                        }
                    }
                }
            }
        };
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|error| error.to_string())?;
        match attempt {
            Ok(changed) => return Ok(changed),
            Err(error)
                if error.contains("database is locked")
                    || error.contains("database table is locked") =>
            {
                ready()?;
            }
            Err(error) => return Err(error),
        }
    }
}

pub fn has_pending_pull_publication(
    catalog: &ContextCatalog,
    publication: &PendingPullPublicationV1,
) -> rusqlite::Result<bool> {
    if !publication.is_complete() {
        return Ok(false);
    }
    let installation_id = &catalog.startup_report.catalog_installation_id;
    let conn = catalog.lock();
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pending_pull_publications
          WHERE installation_id=?1 AND repository_id=?2 AND request_id=?3 AND trace_id=?4
            AND task_id=?5 AND session_id=?6 AND context_epoch=?7 AND publication_id=?8
            AND representation_identity=?9 AND source_ref=?10 AND representation_digest=?11
            AND packet_digest=?12)",
        params![
            installation_id,
            publication.repository_id.as_str(),
            publication.request_id.as_str(),
            publication.trace_id.as_str(),
            publication.task_id.as_str(),
            publication.session_id.as_str(),
            publication.context_epoch as i64,
            publication.publication_id.as_str(),
            publication.representation_identity.as_str(),
            publication.source_ref.as_str(),
            publication.representation_digest.as_str(),
            publication.packet_digest.as_str()
        ],
        |row| row.get::<_, bool>(0),
    )
}

/// Resolve one server-issued pending-publication id within this installation.
/// This is content-free bridge lookup only; it neither accepts an
/// acknowledgement nor changes suppression eligibility.
pub(crate) fn pending_pull_publication(
    catalog: &ContextCatalog,
    publication_id: &str,
) -> rusqlite::Result<Option<PendingPullPublicationV1>> {
    if publication_id.trim().is_empty() {
        return Ok(None);
    }
    let installation_id = &catalog.startup_report.catalog_installation_id;
    let conn = catalog.lock();
    conn.query_row(
        "SELECT repository_id, request_id, trace_id, task_id, session_id, context_epoch,
                publication_id, representation_identity, source_ref, representation_digest,
                packet_digest
         FROM pending_pull_publications
         WHERE installation_id = ?1 AND publication_id = ?2",
        params![installation_id, publication_id],
        |row| {
            let epoch: i64 = row.get("context_epoch")?;
            let context_epoch = u64::try_from(epoch)
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(5, epoch))?;
            Ok(PendingPullPublicationV1 {
                repository_id: row.get("repository_id")?,
                request_id: row.get("request_id")?,
                trace_id: row.get("trace_id")?,
                task_id: row.get("task_id")?,
                session_id: row.get("session_id")?,
                context_epoch,
                publication_id: row.get("publication_id")?,
                representation_identity: row.get("representation_identity")?,
                source_ref: row.get("source_ref")?,
                representation_digest: row.get("representation_digest")?,
                packet_digest: row.get("packet_digest")?,
            })
        },
    )
    .optional()
}

/// Internal persistence primitive. Public Pull callers must enter through
/// `pull::delivery_acknowledgement::acknowledge_delivery`, which validates
/// H10, pending-publication identity, & H9 retention before this write.
pub(crate) fn record_delivery_acknowledgement(
    catalog: &ContextCatalog,
    acknowledgement: &DeliveryAcknowledgementRecordV1,
) -> rusqlite::Result<bool> {
    if !acknowledgement.is_complete() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let installation_id = &catalog.startup_report.catalog_installation_id;
    let conn = catalog.lock();
    let changed = conn.execute(
        "INSERT OR IGNORE INTO delivery_acknowledgements
            (installation_id, repository_id, request_id, trace_id, task_id, session_id, context_epoch,
             publication_id, representation_digest, packet_digest, host_serialized_digest, acknowledged_at_unix)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            installation_id, acknowledgement.repository_id.as_str(), acknowledgement.request_id.as_str(),
            acknowledgement.trace_id.as_str(), acknowledgement.task_id.as_str(), acknowledgement.session_id.as_str(),
            acknowledgement.context_epoch.as_str(), acknowledgement.publication_id.as_str(), acknowledgement.representation_digest.as_str(),
            acknowledgement.packet_digest.as_str(), acknowledgement.host_serialized_digest.as_str(), ContextCatalog::now_unix(),
        ],
    )?;
    Ok(changed > 0)
}

pub fn has_delivery_acknowledgement(
    catalog: &ContextCatalog,
    acknowledgement: &DeliveryAcknowledgementRecordV1,
) -> rusqlite::Result<bool> {
    if !acknowledgement.is_complete() {
        return Ok(false);
    }
    let installation_id = &catalog.startup_report.catalog_installation_id;
    let conn = catalog.lock();
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM delivery_acknowledgements
          WHERE installation_id = ?1 AND repository_id = ?2 AND request_id = ?3
            AND trace_id = ?4 AND task_id = ?5 AND session_id = ?6 AND context_epoch = ?7 AND publication_id = ?8
            AND representation_digest = ?9 AND packet_digest = ?10 AND host_serialized_digest = ?11)",
        params![
            installation_id, acknowledgement.repository_id.as_str(), acknowledgement.request_id.as_str(),
            acknowledgement.trace_id.as_str(), acknowledgement.task_id.as_str(), acknowledgement.session_id.as_str(),
            acknowledgement.context_epoch.as_str(), acknowledgement.publication_id.as_str(), acknowledgement.representation_digest.as_str(),
            acknowledgement.packet_digest.as_str(),
            acknowledgement.host_serialized_digest.as_str(),
        ],
        |row| row.get::<_, bool>(0),
    )
}

// ---- Health snapshot -----------------------------------------------------

/// Snapshot the catalog reports into `/health`. Content-free and nonblocking by construction.
pub fn health_snapshot(catalog: &ContextCatalog) -> serde_json::Value {
    let path_str = catalog.path().to_string_lossy().into_owned();
    let startup_report = catalog.startup_report();
    let conn = match catalog.conn.try_lock() {
        Ok(conn) => conn,
        Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => {
            return serde_json::json!({
                "schemaVersion": CATALOG_SCHEMA_VERSION,
                "path": path_str,
                "status": "busy",
                "receipts": serde_json::Value::Null,
                "retrievalEvents": serde_json::Value::Null,
                "activeGrants": serde_json::Value::Null,
                "cortexDbUntouched": true,
                "startup": startup_report,
            });
        }
    };
    let counts = conn.query_row(
        "SELECT
                (SELECT COUNT(*) FROM receipts),
                (SELECT COUNT(*) FROM retrieval_events),
                (SELECT COUNT(*) FROM scope_grants WHERE status = 'active')",
        [],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    );
    match counts {
        Ok((receipts, events, active_grants)) => serde_json::json!({
            "schemaVersion": CATALOG_SCHEMA_VERSION,
            "path": path_str,
            "status": "ok",
            "receipts": receipts,
            "retrievalEvents": events,
            "activeGrants": active_grants,
            "cortexDbUntouched": true,
            "startup": startup_report,
        }),
        Err(_) => serde_json::json!({
            "schemaVersion": CATALOG_SCHEMA_VERSION,
            "path": path_str,
            "status": "error",
            "receipts": serde_json::Value::Null,
            "retrievalEvents": serde_json::Value::Null,
            "activeGrants": serde_json::Value::Null,
            "cortexDbUntouched": true,
            "startup": startup_report,
        }),
    }
}

// ---- Tests ---------------------------------------------------------------
