//! Transient Push originals. Never Cortex knowledge. Publication is one SQLite
//! transaction; a digest is not authorization and reads never renew a lease.
use rightkit_context_federation::deadline::{Deadline, SystemClock};
use rightkit_db::rusqlite::{self, params, Connection, OptionalExtension, TransactionBehavior};
use rightkit_fs::digest::sha256_bytes;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub const MAX_ARTIFACT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_RESTORE_BYTES: usize = 256 * 1024;
pub const DEFAULT_RESTORE_BYTES: usize = 64 * 1024;
const MAX_STORE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SCOPE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ARTIFACTS: u64 = 4096;
const MAX_TOMBSTONES: u64 = 8192;
const MAX_TTL_MS: u64 = 7 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RecoveryError {
    #[error("push_invalid_anchor")]
    InvalidAnchor,
    #[error("push_scope_denied")]
    Denied,
    #[error("push_artifact_not_found")]
    NotFound,
    #[error("push_artifact_expired")]
    Expired,
    #[error("push_artifact_invalidated")]
    Invalidated,
    #[error("push_artifact_corrupt")]
    Corrupt,
    #[error("push_store_unavailable")]
    Unavailable,
    #[error("push_resource_limit")]
    Limit,
    #[error("push_invalid_selector")]
    InvalidSelector,
    #[error("push_selector_miss")]
    SelectorMiss,
    #[error("push_cancelled")]
    Cancelled,
}
impl RecoveryError {
    pub fn status(self) -> u16 {
        match self {
            Self::Denied => 403,
            Self::NotFound | Self::SelectorMiss => 404,
            Self::Expired | Self::Invalidated => 410,
            Self::Limit => 413,
            Self::Unavailable => 503,
            Self::Corrupt => 409,
            _ => 400,
        }
    }
}
fn db_error(error: rusqlite::Error) -> RecoveryError {
    match error {
        rusqlite::Error::IntegralValueOutOfRange(..) => RecoveryError::Corrupt,
        rusqlite::Error::SqliteFailure(ref e, _) if e.code == rusqlite::ErrorCode::DiskFull => {
            RecoveryError::Limit
        }
        _ => RecoveryError::Unavailable,
    }
}
/// rightkit-db open failures: SQLite errors keep their `db_error` mapping; a symlinked store
/// path is denied; other bootstrap filesystem hardening failures are store-unavailable.
fn open_error(error: rightkit_db::DbError) -> RecoveryError {
    match error {
        rightkit_db::DbError::Sqlite(error)
        | rightkit_db::DbError::MigrationFailed { source: error, .. } => db_error(error),
        rightkit_db::DbError::Fs(rightkit_fs::FsError::Symlink(_)) => RecoveryError::Denied,
        _ => RecoveryError::Unavailable,
    }
}

/// Final SQLite layout, created once. New rows use independent opaque handle generations
/// so reclaimed rows never resurrect.
const PUSH_STORE_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS push_store (id INTEGER PRIMARY KEY CHECK(id=1), identity TEXT NOT NULL);
    INSERT OR IGNORE INTO push_store VALUES(1, lower(hex(randomblob(32))));
    CREATE TABLE IF NOT EXISTS push_originals (
      scope TEXT NOT NULL, digest TEXT NOT NULL, handle_digest TEXT,
      content BLOB NOT NULL, size INTEGER NOT NULL, created INTEGER NOT NULL,
      expires INTEGER NOT NULL, invalidated INTEGER NOT NULL DEFAULT 0,
      PRIMARY KEY(scope,digest));
    CREATE UNIQUE INDEX IF NOT EXISTS push_handle ON push_originals(scope,handle_digest);
    CREATE INDEX IF NOT EXISTS push_expiry ON push_originals(expires);
    CREATE TABLE IF NOT EXISTS push_tombstones (
      scope TEXT NOT NULL, handle_digest TEXT NOT NULL, state TEXT NOT NULL,
      until INTEGER NOT NULL, PRIMARY KEY(scope,handle_digest));
    CREATE INDEX IF NOT EXISTS push_tombstone_until ON push_tombstones(until);";

const PUSH_STORE_MIGRATIONS: &[rightkit_db::Migration] = &[rightkit_db::Migration::sql(
    1,
    "push_store",
    PUSH_STORE_SCHEMA,
)];
fn sql_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
fn sql_size(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<usize> {
    let value: i64 = row.get(index)?;
    usize::try_from(value).map_err(|_| rusqlite::Error::IntegralValueOutOfRange(index, value))
}
pub fn digest(bytes: &[u8]) -> String {
    sha256_bytes(bytes)
}
pub fn now_ms() -> u64 {
    crate::time::now_millis().min(u64::MAX as u128) as u64
}

/// Construct only after the transport's normal repository authorization gate.
/// Canonical root and session are both part of the storage namespace.
#[derive(Debug, Clone)]
pub struct RecoveryScope {
    id: String,
    task_id: String,
    session_id: String,
    session_scope_id: String,
}
impl RecoveryScope {
    pub fn new(root: &Path, session: &str) -> Result<Self, RecoveryError> {
        Self::new_bound(root, "", session)
    }
    /// Construct a recovery namespace bound to one exact task and session.
    /// `new` binds only root and session, with no task.
    pub fn new_for_task(root: &Path, task: &str, session: &str) -> Result<Self, RecoveryError> {
        if task.is_empty() || task.len() > 256 {
            return Err(RecoveryError::Denied);
        }
        Self::new_bound(root, task, session)
    }
    fn new_bound(root: &Path, task: &str, session: &str) -> Result<Self, RecoveryError> {
        if session.is_empty() || session.len() > 256 {
            return Err(RecoveryError::Denied);
        }
        let root = root.canonicalize().map_err(|_| RecoveryError::Denied)?;
        if !root.is_dir() {
            return Err(RecoveryError::Denied);
        }
        let identity = serde_json::to_vec(&(root.to_string_lossy(), task, session))
            .map_err(|_| RecoveryError::Denied)?;
        // Independent of `task`: the resolver-consumer-proof namespace. A
        // proof is minted through the `probe` operation before the caller
        // necessarily has task context (it just needs an authorized
        // root+session), and must still authorize the same caller's later
        // task-bound `prepare`/`resolve` calls within that session. Content
        // storage stays keyed by the full (root, task, session) `id` above;
        // only the resolver-proof handshake uses this coarser identity.
        let session_identity = serde_json::to_vec(&(root.to_string_lossy(), session))
            .map_err(|_| RecoveryError::Denied)?;
        Ok(Self {
            id: digest(&identity),
            task_id: task.into(),
            session_id: session.into(),
            session_scope_id: digest(&session_identity),
        })
    }
    pub fn binding(&self) -> &str {
        &self.id
    }
    /// The coarser root+session identity used to authorize resolver
    /// consumer proofs regardless of task binding. See `new_bound`.
    pub fn consumer_proof_scope(&self) -> &str {
        &self.session_scope_id
    }
    pub fn task_id(&self) -> Option<&str> {
        (!self.task_id.is_empty()).then_some(self.task_id.as_str())
    }
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    pub fn local() -> Result<Self, RecoveryError> {
        let session =
            std::env::var("RIGHTKIT_CONTEXT_PUSH_SESSION").unwrap_or_else(|_| "local".into());
        Self::new(&workspace_root(), &session)
    }
}
pub fn workspace_root() -> PathBuf {
    if let Some(root) = crate::qa_env::repo_root().or_else(crate::qa_env::workspace_root) {
        return root.into();
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    cwd.ancestors()
        .find(|p| p.join(".git").exists())
        .unwrap_or(&cwd)
        .to_path_buf()
}
/// File-bound inputs cannot allocate beyond the same original-size ceiling.
pub fn read_file_bounded(path: &Path) -> Result<Vec<u8>, RecoveryError> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|_| RecoveryError::Unavailable)?;
    let metadata = file.metadata().map_err(|_| RecoveryError::Unavailable)?;
    if !metadata.is_file() || metadata.len() > MAX_ARTIFACT_BYTES as u64 {
        return Err(RecoveryError::Limit);
    }
    let mut bytes = Vec::new();
    file.take((MAX_ARTIFACT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| RecoveryError::Unavailable)?;
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(RecoveryError::Limit);
    }
    Ok(bytes)
}
pub fn read_text_bounded(path: &Path) -> Result<String, RecoveryError> {
    String::from_utf8(read_file_bounded(path)?).map_err(|_| RecoveryError::Corrupt)
}

pub fn default_directory() -> PathBuf {
    crate::qa_env::anchor_dir()
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root().join("tools/.cache/runc"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryReference {
    pub schema_version: u32,
    pub handle: String,
    pub source_digest: String,
    pub size_bytes: usize,
    pub store_id: String,
    pub expires_at: u64,
    pub observed_at: u64,
    pub lease_state: String,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Selector {
    #[default]
    Whole,
    /// Zero-based, half-open original byte range.
    Bytes { start: usize, end: usize },
    /// One-based inclusive lines; original terminators remain in the result.
    Lines { start: usize, end: usize },
    /// Exact source JSON value bytes, including quotes for string values.
    Json { path: Vec<JsonStep> },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JsonStep {
    Field {
        name: String,
    },
    Index {
        index: usize,
    },
    /// String-key equality only; duplicate matches are an error, never first-win.
    Key {
        field: String,
        value: String,
    },
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedArtifact {
    pub reference: RecoveryReference,
    pub start_byte: usize,
    pub end_byte: usize,
    pub selected_digest: String,
    pub content_encoding: &'static str,
    pub content: String,
    pub disposition: &'static str,
    pub fidelity: &'static str,
}
impl ResolvedArtifact {
    pub fn bytes(&self) -> Result<Vec<u8>, RecoveryError> {
        if self.content_encoding == "utf-8" {
            Ok(self.content.as_bytes().to_vec())
        } else {
            rightkit_fs::digest::unhex(&self.content).ok_or(RecoveryError::Corrupt)
        }
    }
}

pub struct RecoveryStore {
    directory: PathBuf,
}
impl RecoveryStore {
    pub fn at(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }
    pub fn configured() -> Self {
        Self::at(default_directory())
    }
    fn connection(&self) -> Result<Connection, RecoveryError> {
        std::fs::create_dir_all(&self.directory).map_err(|_| RecoveryError::Unavailable)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.directory, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| RecoveryError::Unavailable)?;
        }
        let path = self.directory.join("push-artifacts.sqlite");
        // Admission before SQLite touches the name: the directory is pinned without following a link
        // (`StateDir::open`) and the entry is looked at without following it (`stat`), so a symlink
        // (a dangling one included, which would make SQLite create the file through it) or anything
        // that is not a regular file is refused. This is an admission check, not an atomic open: the
        // name can still change between this look and the open below.
        let pinned =
            rightkit_fs::StateDir::open(&self.directory).map_err(|_| RecoveryError::Unavailable)?;
        match pinned.stat("push-artifacts.sqlite") {
            Ok(None) => {}
            Ok(Some(entry)) if entry.kind == rightkit_fs::EntryKind::File => {}
            Ok(Some(_)) => return Err(RecoveryError::Denied),
            Err(_) => return Err(RecoveryError::Unavailable),
        }
        // Durable Push originals: rightkit-db writer with Full durability, 250ms busy wait
        // installed before any bootstrap statement, owner-only file (default), schema through
        // `PUSH_STORE_MIGRATIONS`.
        let mut options = rightkit_db::DbOptions::new(&path);
        options.busy_timeout = Duration::from_millis(250);
        options.durability = rightkit_db::DurabilityProfile::Full;
        let (connection, _) = options
            .open_writer_migrated(
                PUSH_STORE_MIGRATIONS,
                &rightkit_db::MigrateOptions::default(),
            )
            .map_err(open_error)?;
        let page_size: i64 = connection
            .query_row("PRAGMA page_size", [], |r| r.get(0))
            .map_err(db_error)?;
        if page_size <= 0 {
            return Err(RecoveryError::Corrupt);
        }
        connection
            .pragma_update(None, "max_page_count", 512_i64 * 1024 * 1024 / page_size)
            .map_err(db_error)?;
        Ok(connection)
    }
    pub fn identity(&self) -> Result<String, RecoveryError> {
        self.connection()?
            .query_row("SELECT identity FROM push_store WHERE id=1", [], |r| {
                r.get(0)
            })
            .map_err(db_error)
    }
    fn reference(
        connection: &Connection,
        handle_hash: &str,
        source_hash: &str,
        size: usize,
        expires: u64,
        now: u64,
        created: u64,
    ) -> Result<RecoveryReference, RecoveryError> {
        let store_id = connection
            .query_row("SELECT identity FROM push_store WHERE id=1", [], |r| {
                r.get(0)
            })
            .map_err(db_error)?;
        let remaining = expires.saturating_sub(now);
        // "Near expiry" means inside the final tenth of the lease's own
        // lifetime, capped at 60s absolute so long-lived leases still warn
        // with a sane lead time. A freshly published/renewed handle (remaining
        // == full lease length) must never be reported near_expiry merely
        // because its total TTL happens to be short.
        let lease_length = expires.saturating_sub(created);
        let near_expiry_window = (lease_length / 10).min(60_000);
        Ok(RecoveryReference {
            schema_version: 1,
            handle: format!("mr://anchor/{handle_hash}"),
            source_digest: format!("sha256:{source_hash}"),
            size_bytes: size,
            store_id,
            expires_at: expires,
            observed_at: now,
            lease_state: if remaining <= near_expiry_window && remaining < lease_length {
                "near_expiry"
            } else {
                "active"
            }
            .into(),
        })
    }
    fn prune_tombstones(connection: &Connection, now: u64) -> Result<(), RecoveryError> {
        let now_i64 = i64::try_from(now).map_err(|_| RecoveryError::Limit)?;
        connection
            .execute("DELETE FROM push_tombstones WHERE until<=?1", [now_i64])
            .map_err(db_error)?;
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM push_tombstones", [], |r| r.get(0))
            .map_err(db_error)?;
        let excess = count.saturating_sub(MAX_TOMBSTONES as i64);
        if excess > 0 {
            connection.execute(
                "DELETE FROM push_tombstones WHERE rowid IN (SELECT rowid FROM push_tombstones ORDER BY until ASC, rowid ASC LIMIT ?1)",
                [excess],
            ).map_err(db_error)?;
        }
        Ok(())
    }
    fn compact(connection: &Connection, now: u64) -> Result<(), RecoveryError> {
        let now_i64 = i64::try_from(now).map_err(|_| RecoveryError::Limit)?;
        let tombstone_until = now.saturating_add(MAX_TTL_MS).min(i64::MAX as u64) as i64;
        connection
            .execute(
                "INSERT OR REPLACE INTO push_tombstones(scope,handle_digest,state,until)
             SELECT scope,handle_digest,'invalidated',?1 FROM push_originals
             WHERE invalidated<>0 AND handle_digest IS NOT NULL",
                [tombstone_until],
            )
            .map_err(db_error)?;
        connection
            .execute(
                "INSERT OR IGNORE INTO push_tombstones(scope,handle_digest,state,until)
             SELECT scope,handle_digest,'expired',?2 FROM push_originals
             WHERE expires<=?1 AND handle_digest IS NOT NULL",
                params![now_i64, tombstone_until],
            )
            .map_err(db_error)?;
        connection
            .execute(
                "DELETE FROM push_originals WHERE invalidated<>0 OR expires<=?1",
                [now_i64],
            )
            .map_err(db_error)?;
        Self::prune_tombstones(connection, now)
    }
    fn terminal_error(
        connection: &Connection,
        scope: &str,
        handle_hash: &str,
    ) -> Result<Option<RecoveryError>, RecoveryError> {
        let state: Option<String> = connection
            .query_row(
                "SELECT state FROM push_tombstones WHERE scope=?1 AND handle_digest=?2",
                params![scope, handle_hash],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        Ok(match state.as_deref() {
            Some("expired") => Some(RecoveryError::Expired),
            Some("invalidated") => Some(RecoveryError::Invalidated),
            Some(_) => Some(RecoveryError::Corrupt),
            None => None,
        })
    }
    fn mint_handle(connection: &Connection, scope: &str) -> Result<String, RecoveryError> {
        for _ in 0..8 {
            let mut nonce = [0u8; 32];
            getrandom::fill(&mut nonce).map_err(|_| RecoveryError::Unavailable)?;
            let candidate = rightkit_fs::digest::hex(&nonce);
            let active: i64 = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM push_originals WHERE scope=?1 AND handle_digest=?2)",
                params![scope, candidate], |r| r.get(0),
            ).map_err(db_error)?;
            let terminal: i64 = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM push_tombstones WHERE scope=?1 AND handle_digest=?2)",
                params![scope, candidate], |r| r.get(0),
            ).map_err(db_error)?;
            if active == 0 && terminal == 0 {
                return Ok(candidate);
            }
        }
        Err(RecoveryError::Unavailable)
    }
    pub fn publish(
        &self,
        scope: &RecoveryScope,
        bytes: &[u8],
        ttl_ms: u64,
        now: u64,
    ) -> Result<RecoveryReference, RecoveryError> {
        self.publish_with_control(
            scope,
            bytes,
            ttl_ms,
            now,
            Deadline::after(&SystemClock, Duration::from_secs(120)),
            &CancellationToken::new(),
        )
    }
    pub fn publish_with_control(
        &self,
        scope: &RecoveryScope,
        bytes: &[u8],
        ttl_ms: u64,
        now: u64,
        deadline: Deadline,
        cancellation: &CancellationToken,
    ) -> Result<RecoveryReference, RecoveryError> {
        check_control(deadline, cancellation)?;
        if bytes.len() > MAX_ARTIFACT_BYTES
            || ttl_ms == 0
            || ttl_ms > MAX_TTL_MS
            || now > i64::MAX as u64 - ttl_ms
        {
            return Err(RecoveryError::Limit);
        }
        let source_hash = digest(bytes);
        let mut connection = self.connection()?;
        check_control(deadline, cancellation)?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        Self::compact(&tx, now)?;
        check_control(deadline, cancellation)?;
        let existing: Option<(String, Vec<u8>, usize, u64, u64)> = tx.query_row(
            "SELECT handle_digest,content,size,expires,created FROM push_originals WHERE scope=?1 AND digest=?2 AND invalidated=0",
            params![scope.id, source_hash], |r| Ok((r.get(0)?, r.get(1)?, sql_size(r,2)?, sql_u64(r,3)?, sql_u64(r,4)?)),
        ).optional().map_err(db_error)?;
        let (handle_hash, expires, created) = if let Some((
            handle_hash,
            retained,
            size,
            expires,
            created,
        )) = existing
        {
            if retained.len() != size || retained != bytes || digest(&retained) != source_hash {
                return Err(RecoveryError::Corrupt);
            }
            (handle_hash, expires, created)
        } else {
            let (total, scoped, count): (u64, u64, u64) = tx.query_row(
                "SELECT COALESCE(SUM(size),0), COALESCE(SUM(CASE WHEN scope=?1 THEN size ELSE 0 END),0), COUNT(*) FROM push_originals",
                [&scope.id], |r| Ok((sql_u64(r,0)?, sql_u64(r,1)?, sql_u64(r,2)?)),
            ).map_err(db_error)?;
            if total + bytes.len() as u64 > MAX_STORE_BYTES
                || scoped + bytes.len() as u64 > MAX_SCOPE_BYTES
                || count >= MAX_ARTIFACTS
            {
                return Err(RecoveryError::Limit);
            }
            let expires = now + ttl_ms;
            let handle_hash = Self::mint_handle(&tx, &scope.id)?;
            tx.execute(
                "INSERT INTO push_originals(scope,digest,handle_digest,content,size,created,expires) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![scope.id, source_hash, handle_hash, bytes, bytes.len() as i64, now as i64, expires as i64],
            ).map_err(db_error)?;
            (handle_hash, expires, now)
        };
        let retained: Vec<u8> = tx
            .query_row(
                "SELECT content FROM push_originals WHERE scope=?1 AND handle_digest=?2",
                params![scope.id, handle_hash],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        if retained != bytes || digest(&retained) != source_hash {
            return Err(RecoveryError::Corrupt);
        }
        let reference = Self::reference(
            &tx,
            &handle_hash,
            &source_hash,
            bytes.len(),
            expires,
            now,
            created,
        )?;
        check_control(deadline, cancellation)?;
        tx.commit().map_err(db_error)?;
        Ok(reference)
    }
    pub fn resolve(
        &self,
        scope: &RecoveryScope,
        handle: &str,
        selector: &Selector,
        max_bytes: usize,
        now: u64,
    ) -> Result<ResolvedArtifact, RecoveryError> {
        self.resolve_with_control(
            scope,
            handle,
            selector,
            max_bytes,
            now,
            Deadline::after(&SystemClock, Duration::from_secs(120)),
            &CancellationToken::new(),
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub fn resolve_with_control(
        &self,
        scope: &RecoveryScope,
        handle: &str,
        selector: &Selector,
        max_bytes: usize,
        now: u64,
        deadline: Deadline,
        cancellation: &CancellationToken,
    ) -> Result<ResolvedArtifact, RecoveryError> {
        check_control(deadline, cancellation)?;
        let handle_hash = crate::ledger::identifier::AnchorRef::parse(handle)
            .map_err(|_| RecoveryError::InvalidAnchor)?
            .digest();
        if max_bytes == 0 || max_bytes > MAX_RESTORE_BYTES {
            return Err(RecoveryError::Limit);
        }
        let connection = self.connection()?;
        let metadata: Option<(String, usize, usize, u64, bool, u64)> = connection.query_row(
            "SELECT digest,size,length(content),expires,invalidated,created FROM push_originals WHERE scope=?1 AND handle_digest=?2",
            params![scope.id, handle_hash], |r| Ok((r.get(0)?, sql_size(r,1)?, sql_size(r,2)?, sql_u64(r,3)?, r.get(4)?, sql_u64(r,5)?)),
        ).optional().map_err(db_error)?;
        let Some((source_hash, size, stored_size, expires, invalidated, created)) = metadata else {
            return Err(Self::terminal_error(&connection, &scope.id, handle_hash)?
                .unwrap_or(RecoveryError::NotFound));
        };
        if invalidated {
            return Err(RecoveryError::Invalidated);
        }
        if now >= expires {
            return Err(RecoveryError::Expired);
        }
        if size != stored_size || size > MAX_ARTIFACT_BYTES {
            return Err(RecoveryError::Corrupt);
        }
        let bytes: Vec<u8> = connection
            .query_row(
                "SELECT content FROM push_originals WHERE scope=?1 AND handle_digest=?2",
                params![scope.id, handle_hash],
                |r| r.get(0),
            )
            .map_err(db_error)?;
        check_control(deadline, cancellation)?;
        if digest(&bytes) != source_hash {
            return Err(RecoveryError::Corrupt);
        }
        let (start, end) = select_bytes(&bytes, selector)?;
        check_control(deadline, cancellation)?;
        if end - start > max_bytes {
            return Err(RecoveryError::Limit);
        }
        let selected = &bytes[start..end];
        let (content_encoding, content) = match std::str::from_utf8(selected) {
            Ok(text) => ("utf-8", text.to_owned()),
            Err(_) => ("hex", rightkit_fs::digest::hex(selected)),
        };
        super::telemetry::record(
            "restore",
            size,
            selected.len(),
            Some("status=verified"),
            Some(&source_hash),
        );
        Ok(ResolvedArtifact {
            reference: Self::reference(
                &connection,
                handle_hash,
                &source_hash,
                size,
                expires,
                now,
                created,
            )?,
            start_byte: start,
            end_byte: end,
            selected_digest: format!("sha256:{}", digest(selected)),
            content_encoding,
            content,
            disposition: "exact",
            fidelity: "exact_bytes",
        })
    }
    /// Explicit compare-and-swap renewal. Reads and duplicate publication never
    /// call this operation; expired/invalidated handles cannot be resurrected.
    pub fn renew(
        &self,
        scope: &RecoveryScope,
        handle: &str,
        expected_expiry: u64,
        ttl_ms: u64,
        now: u64,
    ) -> Result<RecoveryReference, RecoveryError> {
        if ttl_ms == 0 || ttl_ms > MAX_TTL_MS || now > i64::MAX as u64 - ttl_ms {
            return Err(RecoveryError::Limit);
        }
        let resolved =
            self.resolve(scope, handle, &Selector::Bytes { start: 0, end: 0 }, 1, now)?;
        if resolved.reference.expires_at != expected_expiry {
            return Err(RecoveryError::Denied);
        }
        let handle_hash = crate::ledger::identifier::AnchorRef::parse(handle)
            .map_err(|_| RecoveryError::InvalidAnchor)?
            .digest();
        let source_hash = resolved
            .reference
            .source_digest
            .strip_prefix("sha256:")
            .ok_or(RecoveryError::Corrupt)?;
        let expires = (now + ttl_ms).max(expected_expiry);
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE push_originals SET expires=?1 WHERE scope=?2 AND handle_digest=?3 AND expires=?4 AND invalidated=0",
            params![expires as i64, scope.id, handle_hash, expected_expiry as i64],
        ).map_err(db_error)?;
        if changed != 1 {
            return Err(RecoveryError::Denied);
        }
        // A successful renewal restarts the lease clock: `now` is the new
        // creation point for near-expiry purposes, matching a fresh publish.
        Self::reference(
            &connection,
            handle_hash,
            source_hash,
            resolved.reference.size_bytes,
            expires,
            now,
            now,
        )
    }
    pub fn invalidate(&self, scope: &RecoveryScope, handle: &str) -> Result<(), RecoveryError> {
        let handle_hash = crate::ledger::identifier::AnchorRef::parse(handle)
            .map_err(|_| RecoveryError::InvalidAnchor)?
            .digest();
        let mut connection = self.connection()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_error)?;
        let exists: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM push_originals WHERE scope=?1 AND handle_digest=?2",
                params![scope.id, handle_hash],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        if exists.is_none() {
            return Err(Self::terminal_error(&tx, &scope.id, handle_hash)?
                .unwrap_or(RecoveryError::NotFound));
        }
        let now = now_ms();
        let until = now.saturating_add(MAX_TTL_MS).min(i64::MAX as u64) as i64;
        tx.execute(
            "INSERT OR REPLACE INTO push_tombstones(scope,handle_digest,state,until) VALUES(?1,?2,'invalidated',?3)",
            params![scope.id, handle_hash, until],
        ).map_err(db_error)?;
        tx.execute(
            "DELETE FROM push_originals WHERE scope=?1 AND handle_digest=?2",
            params![scope.id, handle_hash],
        )
        .map_err(db_error)?;
        Self::prune_tombstones(&tx, now)?;
        tx.commit().map_err(db_error)?;
        Ok(())
    }
}

fn check_control(
    deadline: Deadline,
    cancellation: &CancellationToken,
) -> Result<(), RecoveryError> {
    if cancellation.is_cancelled() || deadline.is_exhausted(&SystemClock) {
        Err(RecoveryError::Cancelled)
    } else {
        Ok(())
    }
}

pub fn select_bytes(bytes: &[u8], selector: &Selector) -> Result<(usize, usize), RecoveryError> {
    match selector {
        Selector::Whole => Ok((0, bytes.len())),
        Selector::Bytes { start, end } if start <= end && *end <= bytes.len() => Ok((*start, *end)),
        Selector::Bytes { .. } => Err(RecoveryError::SelectorMiss),
        Selector::Lines { start, end } => {
            if *start == 0 || end < start {
                return Err(RecoveryError::InvalidSelector);
            }
            let mut begin = None;
            for (line, (offset, chunk)) in (1..).zip(byte_lines(bytes)) {
                if line == *start {
                    begin = Some(offset);
                }
                if line == *end {
                    return Ok((
                        begin.ok_or(RecoveryError::SelectorMiss)?,
                        offset + chunk.len(),
                    ));
                }
            }
            Err(RecoveryError::SelectorMiss)
        }
        Selector::Json { path } => {
            if path.len() > 64 {
                return Err(RecoveryError::Limit);
            }
            let tree = JsonNode::parse(bytes)?;
            let mut node = &tree;
            for step in path {
                node = match (step, &node.children) {
                    (JsonStep::Field { name }, JsonChildren::Object(fields)) => {
                        fields.iter().find(|(key, _)| key == name).map(|(_, v)| v)
                    }
                    (JsonStep::Index { index }, JsonChildren::Array(values)) => values.get(*index),
                    (JsonStep::Key { field, value }, JsonChildren::Array(values)) => {
                        let matches = values
                            .iter()
                            .filter(|n| match &n.children {
                                JsonChildren::Object(fields) => fields.iter().any(|(key, n)| {
                                    key == field
                                        && serde_json::from_slice::<String>(&bytes[n.start..n.end])
                                            .is_ok_and(|s| &s == value)
                                }),
                                _ => false,
                            })
                            .collect::<Vec<_>>();
                        if matches.len() > 1 {
                            return Err(RecoveryError::InvalidSelector);
                        }
                        matches.first().copied()
                    }
                    _ => return Err(RecoveryError::InvalidSelector),
                }
                .ok_or(RecoveryError::SelectorMiss)?;
            }
            Ok((node.start, node.end))
        }
    }
}
pub fn byte_lines(bytes: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    let mut offset = 0;
    bytes.split_inclusive(|b| *b == b'\n').map(move |line| {
        let at = offset;
        offset += line.len();
        (at, line)
    })
}

// A bounded lossless JSON span index. serde validates scalar syntax; this parser
// additionally rejects duplicate keys instead of laundering them through Value.
struct JsonNode {
    start: usize,
    end: usize,
    children: JsonChildren,
}
enum JsonChildren {
    Scalar,
    Object(Vec<(String, JsonNode)>),
    Array(Vec<JsonNode>),
}
impl JsonNode {
    fn parse(bytes: &[u8]) -> Result<Self, RecoveryError> {
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(RecoveryError::Limit);
        }
        let mut cursor = 0;
        let mut nodes = 0;
        let node = Self::value(bytes, &mut cursor, 0, &mut nodes)?;
        Self::ws(bytes, &mut cursor);
        if cursor != bytes.len() {
            return Err(RecoveryError::InvalidSelector);
        }
        Ok(node)
    }
    fn ws(bytes: &[u8], at: &mut usize) {
        while bytes
            .get(*at)
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
        {
            *at += 1;
        }
    }
    fn string_end(bytes: &[u8], at: &mut usize) -> Result<(), RecoveryError> {
        if bytes.get(*at) != Some(&b'"') {
            return Err(RecoveryError::InvalidSelector);
        }
        *at += 1;
        loop {
            match bytes.get(*at) {
                Some(b'"') => {
                    *at += 1;
                    return Ok(());
                }
                Some(b'\\') => {
                    *at += 2;
                }
                Some(_) => *at += 1,
                None => return Err(RecoveryError::InvalidSelector),
            }
        }
    }
    fn value(
        bytes: &[u8],
        at: &mut usize,
        depth: usize,
        nodes: &mut usize,
    ) -> Result<Self, RecoveryError> {
        *nodes += 1;
        if depth > 64 || *nodes > 100_000 {
            return Err(RecoveryError::Limit);
        }
        Self::ws(bytes, at);
        let start = *at;
        let children = match bytes.get(*at) {
            Some(b'{') => {
                *at += 1;
                Self::ws(bytes, at);
                let mut fields = Vec::new();
                let mut keys = std::collections::HashSet::new();
                if bytes.get(*at) != Some(&b'}') {
                    loop {
                        Self::ws(bytes, at);
                        let key_start = *at;
                        Self::string_end(bytes, at)?;
                        let key: String = serde_json::from_slice(&bytes[key_start..*at])
                            .map_err(|_| RecoveryError::InvalidSelector)?;
                        if !keys.insert(key.clone()) {
                            return Err(RecoveryError::InvalidSelector);
                        }
                        Self::ws(bytes, at);
                        if bytes.get(*at) != Some(&b':') {
                            return Err(RecoveryError::InvalidSelector);
                        }
                        *at += 1;
                        fields.push((key, Self::value(bytes, at, depth + 1, nodes)?));
                        Self::ws(bytes, at);
                        match bytes.get(*at) {
                            Some(b',') => *at += 1,
                            Some(b'}') => break,
                            _ => return Err(RecoveryError::InvalidSelector),
                        }
                    }
                }
                *at += 1;
                JsonChildren::Object(fields)
            }
            Some(b'[') => {
                *at += 1;
                Self::ws(bytes, at);
                let mut values = Vec::new();
                if bytes.get(*at) != Some(&b']') {
                    loop {
                        values.push(Self::value(bytes, at, depth + 1, nodes)?);
                        Self::ws(bytes, at);
                        match bytes.get(*at) {
                            Some(b',') => *at += 1,
                            Some(b']') => break,
                            _ => return Err(RecoveryError::InvalidSelector),
                        }
                    }
                }
                *at += 1;
                JsonChildren::Array(values)
            }
            Some(b'"') => {
                Self::string_end(bytes, at)?;
                JsonChildren::Scalar
            }
            Some(_) => {
                while bytes.get(*at).is_some_and(|b| {
                    !matches!(b, b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t')
                }) {
                    *at += 1;
                }
                JsonChildren::Scalar
            }
            None => return Err(RecoveryError::InvalidSelector),
        };
        // Validate leaf syntax without converting/reformatting original numbers.
        if matches!(children, JsonChildren::Scalar) {
            serde_json::from_slice::<serde_json::Value>(&bytes[start..*at])
                .map_err(|_| RecoveryError::InvalidSelector)?;
        }
        Ok(Self {
            start,
            end: *at,
            children,
        })
    }
}

/// Formatting-only JSON codec. Strings and number lexemes are unchanged.
pub fn minify_json(text: &str) -> Option<String> {
    JsonNode::parse(text.as_bytes()).ok()?;
    let mut quoted = false;
    let mut escaped = false;
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if quoted {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
        } else if ch == '"' {
            quoted = true;
            out.push(ch);
        } else if !matches!(ch, ' ' | '\t' | '\r' | '\n') {
            out.push(ch);
        }
    }
    Some(out)
}
