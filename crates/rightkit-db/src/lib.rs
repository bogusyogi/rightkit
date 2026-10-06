#![forbid(unsafe_code)]

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::ffi::OsString;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use zeroize::Zeroizing;

mod backup;
mod migrate;
mod recover;

pub use backup::{online_backup_to, BackupProgress};
pub use migrate::{
    add_column_if_missing, column_exists, migrate, pending_marker, rollback_to, user_version, Migration,
    MigrationReport, MigrateOptions, PendingMarker, Step,
};
pub use recover::{quarantine_database, OpenOutcome, RecoveryPolicy};
#[cfg(feature = "sqlcipher")]
pub use recover::rekey;

const BUSY_TIMEOUT_MS: u64 = 5_000;
const JOURNAL_SIZE_LIMIT: i64 = 67_108_864;

pub type Result<T> = std::result::Result<T, DbError>;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("a SQLCipher key requires the rightkit-db `sqlcipher` feature")]
    SqlcipherFeatureRequired,
    #[error("SQLCipher key contains characters that are unsafe for PRAGMA interpolation")]
    InvalidSqlcipherKey,
    #[error("database integrity check failed: {0}")]
    Integrity(String),
    #[error("read-pool capacity must be greater than zero")]
    InvalidPoolCapacity,
    #[error("database path has no file name")]
    InvalidDatabasePath,
    #[error("database is at schema v{current}, newer than this build understands (v{max}) and not forward compatible")]
    NewerDatabase { current: i64, max: i64 },
    #[error("invalid migration list: {0}")]
    InvalidMigrations(String),
    #[error("migration v{version} ({name}) failed and was rolled back: {source}")]
    MigrationFailed {
        version: i64,
        name: &'static str,
        #[source]
        source: rusqlite::Error,
    },
    #[error("cannot roll back: {0}")]
    Rollback(String),
    #[error("database backup failed: {0}")]
    Backup(String),
    #[error("no candidate key opened the database")]
    NoKeyOpened,
    #[error(transparent)]
    Fs(#[from] rightkit_fs::FsError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheProfile {
    Default,
    LargeWorkingSet,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DurabilityProfile {
    Full,
    Normal,
}

/// How the journal mode is chosen. WAL needs working shared-memory locking, which network
/// filesystems often lack and which SQLite will silently downgrade; the default therefore
/// *verifies* the mode that took effect and falls back to a rollback journal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JournalSelect {
    /// WAL when the filesystem honors it and is not a detected network mount; else DELETE.
    WalWithFallback,
    /// WAL or fail (no fallback).
    WalStrict,
    /// Rollback journal.
    Delete,
}

/// Environment kill-switch (`wal` | `delete`) for a misclassified mount in the field.
pub const JOURNAL_ENV: &str = "RIGHTKIT_SQLITE_JOURNAL_MODE";

/// Post-key, pre-pragma per-connection initializer (register functions, collations, ...).
pub type ConnInit = Arc<dyn Fn(&Connection) -> rusqlite::Result<()> + Send + Sync>;

#[derive(Clone)]
pub struct DbOptions {
    pub path: PathBuf,
    /// SQLCipher passphrase-style key (base64-ish charset). See also [`DbOptions::raw_key`].
    pub key: Option<Zeroizing<String>>,
    /// SQLCipher raw 256-bit key, applied as `x'<hex>'` (no KDF). Takes precedence over `key`.
    pub raw_key: Option<Zeroizing<[u8; 32]>>,
    pub profile: CacheProfile,
    pub durability: DurabilityProfile,
    pub foreign_keys: bool,
    pub integrity_check_on_open: bool,
    pub journal: JournalSelect,
    /// Narrow the database and its sidecars to owner-only (0600) on unix. Default on.
    pub owner_only: bool,
    pub init: Option<ConnInit>,
}

impl DbOptions {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            key: None,
            raw_key: None,
            profile: CacheProfile::Default,
            durability: DurabilityProfile::Normal,
            foreign_keys: false,
            integrity_check_on_open: false,
            journal: JournalSelect::WalWithFallback,
            owner_only: true,
            init: None,
        }
    }

    pub fn with_passphrase(mut self, key: impl Into<String>) -> Self {
        self.key = Some(Zeroizing::new(key.into()));
        self.raw_key = None;
        self
    }

    pub fn with_raw_key(mut self, key: [u8; 32]) -> Self {
        self.raw_key = Some(Zeroizing::new(key));
        self.key = None;
        self
    }

    pub fn is_keyed(&self) -> bool {
        self.key.is_some() || self.raw_key.is_some()
    }

    pub fn open_writer(&self) -> Result<Connection> {
        self.open(ConnectionKind::Writer)
    }

    pub fn open_reader(&self) -> Result<Connection> {
        self.open(ConnectionKind::Reader)
    }

    pub fn open_read_pool(&self, cap: usize) -> Result<ReadPool> {
        ReadPool::open(self.clone(), cap)
    }

    fn open(&self, kind: ConnectionKind) -> Result<Connection> {
        let key = self.key_pragma_value()?;
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(&self.path, flags)?;
        // Narrow the main file before any write so WAL/SHM sidecars inherit owner-only.
        if self.owner_only {
            rightkit_fs::restrict_owner_only(&self.path)?;
        }
        apply_key_first(&conn, key.as_ref().map(|k| k.as_str()))?;
        verify_key(&conn)?;
        if let Some(init) = &self.init {
            init(&conn)?;
        }
        configure_connection(&conn, self, kind)?;
        if self.integrity_check_on_open {
            integrity_check(&conn, self.is_keyed())?;
        }
        if self.owner_only {
            harden_sidecars(&self.path)?;
        }
        Ok(conn)
    }

    /// The `PRAGMA key` right-hand side (already quoted), validated against injection.
    fn key_pragma_value(&self) -> Result<Option<Zeroizing<String>>> {
        if let Some(raw) = &self.raw_key {
            #[cfg(not(feature = "sqlcipher"))]
            {
                let _ = raw;
                return Err(DbError::SqlcipherFeatureRequired);
            }
            #[cfg(feature = "sqlcipher")]
            {
                let hex: String = raw.iter().map(|b| format!("{b:02x}")).collect();
                return Ok(Some(Zeroizing::new(format!("\"x'{hex}'\""))));
            }
        }
        let key = self.key.as_ref().map(|key| key.as_str());
        validate_key(key)?;
        Ok(key.map(|k| Zeroizing::new(format!("'{k}'"))))
    }
}

fn harden_sidecars(db: &Path) -> Result<()> {
    for suffix in ["-wal", "-shm"] {
        let mut name = OsString::from(db.file_name().ok_or(DbError::InvalidDatabasePath)?);
        name.push(suffix);
        rightkit_fs::restrict_owner_only(&db.with_file_name(name))?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ConnectionKind {
    Writer,
    Reader,
}

fn configure_connection(
    conn: &Connection,
    options: &DbOptions,
    kind: ConnectionKind,
) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))?;
    configure_journal(conn, options, kind)?;
    let synchronous = match options.durability {
        DurabilityProfile::Full => "FULL",
        DurabilityProfile::Normal => "NORMAL",
    };
    conn.pragma_update(None, "synchronous", synchronous)?;
    conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    if options.foreign_keys {
        conn.pragma_update(None, "foreign_keys", true)?;
    }
    if options.profile == CacheProfile::LargeWorkingSet {
        conn.pragma_update(None, "cache_size", -65_536_i64)?;
        conn.pragma_update(None, "mmap_size", 268_435_456_i64)?;
    }
    optimize_on_open(conn)?;
    if matches!(kind, ConnectionKind::Reader) {
        conn.pragma_update(None, "query_only", true)?;
    }
    Ok(())
}

fn configure_journal(conn: &Connection, options: &DbOptions, kind: ConnectionKind) -> Result<()> {
    let wanted = effective_journal(options);
    if wanted == JournalSelect::Delete {
        if matches!(kind, ConnectionKind::Writer) {
            conn.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get::<_, String>(0))?;
        }
        return Ok(());
    }
    let honored = conn
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))
        .map(|mode| mode.eq_ignore_ascii_case("wal"))
        .unwrap_or(false)
        // `journal_mode=WAL` can report success before the -shm/-wal sidecars are usable;
        // one empty write transaction proves the sidecars work before anything depends on them.
        && (matches!(kind, ConnectionKind::Reader)
            || conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").is_ok());
    if honored {
        return Ok(());
    }
    if wanted == JournalSelect::WalStrict {
        return Err(DbError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
            Some("WAL journal mode was not honored by this filesystem".into()),
        )));
    }
    let _ = conn.execute_batch("ROLLBACK;");
    conn.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get::<_, String>(0))?;
    Ok(())
}

fn effective_journal(options: &DbOptions) -> JournalSelect {
    match std::env::var(JOURNAL_ENV).ok().as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("wal") => return JournalSelect::WalStrict,
        Some("delete") | Some("truncate") => return JournalSelect::Delete,
        _ => {}
    }
    if options.journal == JournalSelect::WalWithFallback && rightkit_fs::is_network_filesystem(&options.path) {
        return JournalSelect::Delete;
    }
    options.journal
}

/// The journal mode SQLite is actually running (`wal`, `delete`, ...).
pub fn journal_mode(conn: &Connection) -> Result<String> {
    Ok(conn.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?)
}

/// Start a transaction that reserves SQLite's single writer slot up front.
/// This keeps read-then-write work out of the unretryable BUSY_SNAPSHOT path.
pub fn write_txn(conn: &mut Connection) -> Result<Transaction<'_>> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(Into::into)
}

#[cfg(feature = "sqlcipher")]
fn apply_key_first(conn: &Connection, key: Option<&str>) -> Result<()> {
    if let Some(key) = key {
        let statement = Zeroizing::new(format!("PRAGMA key = {key};"));
        conn.execute_batch(statement.as_str())?;
    }
    Ok(())
}

#[cfg(not(feature = "sqlcipher"))]
fn apply_key_first(_conn: &Connection, key: Option<&str>) -> Result<()> {
    if key.is_some() {
        return Err(DbError::SqlcipherFeatureRequired);
    }
    Ok(())
}

fn validate_key(key: Option<&str>) -> Result<()> {
    #[cfg(not(feature = "sqlcipher"))]
    if key.is_some() {
        return Err(DbError::SqlcipherFeatureRequired);
    }
    if let Some(key) = key {
        let safe = !key.is_empty()
            && key.len() <= 4_096
            && key.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'-' | b'_')
            });
        if !safe {
            return Err(DbError::InvalidSqlcipherKey);
        }
    }
    Ok(())
}

fn verify_key(conn: &Connection) -> Result<()> {
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    })?;
    Ok(())
}

fn integrity_check(conn: &Connection, keyed: bool) -> Result<()> {
    let pragma = if keyed {
        "PRAGMA cipher_integrity_check"
    } else {
        "PRAGMA integrity_check"
    };
    let mut statement = conn.prepare(pragma)?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let messages = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    if messages.is_empty()
        || messages
            .iter()
            .all(|message| message.eq_ignore_ascii_case("ok"))
    {
        return Ok(());
    }
    Err(DbError::Integrity(messages.join("; ")))
}

#[derive(Clone)]
pub struct ReadPool {
    inner: Arc<ReadPoolInner>,
}

struct ReadPoolInner {
    options: DbOptions,
    cap: usize,
    state: Mutex<ReadPoolState>,
    available: Condvar,
}

#[derive(Default)]
struct ReadPoolState {
    idle: Vec<Connection>,
    total: usize,
}

pub struct PooledReadConnection {
    conn: Option<Connection>,
    pool: Weak<ReadPoolInner>,
}

impl Deref for PooledReadConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        self.conn.as_ref().expect("pooled connection is present")
    }
}

impl Drop for PooledReadConnection {
    fn drop(&mut self) {
        let Some(conn) = self.conn.take() else {
            return;
        };
        let Some(pool) = self.pool.upgrade() else {
            return;
        };
        let mut state = lock_recover(&pool.state);
        state.idle.push(conn);
        pool.available.notify_one();
    }
}

impl ReadPool {
    fn open(options: DbOptions, cap: usize) -> Result<Self> {
        if cap == 0 {
            return Err(DbError::InvalidPoolCapacity);
        }
        let pool = Self {
            inner: Arc::new(ReadPoolInner {
                options,
                cap,
                state: Mutex::new(ReadPoolState::default()),
                available: Condvar::new(),
            }),
        };
        let conn = pool.inner.options.open_reader()?;
        let mut state = lock_recover(&pool.inner.state);
        state.total = 1;
        state.idle.push(conn);
        drop(state);
        Ok(pool)
    }

    pub fn get(&self) -> Result<PooledReadConnection> {
        let mut state = lock_recover(&self.inner.state);
        loop {
            if let Some(conn) = state.idle.pop() {
                return Ok(PooledReadConnection {
                    conn: Some(conn),
                    pool: Arc::downgrade(&self.inner),
                });
            }
            if state.total < self.inner.cap {
                state.total += 1;
                drop(state);
                return match self.inner.options.open_reader() {
                    Ok(conn) => Ok(PooledReadConnection {
                        conn: Some(conn),
                        pool: Arc::downgrade(&self.inner),
                    }),
                    Err(error) => {
                        let mut state = lock_recover(&self.inner.state);
                        state.total -= 1;
                        self.inner.available.notify_one();
                        Err(error)
                    }
                };
            }
            state = match self.inner.available.wait(state) {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }

    pub fn capacity(&self) -> usize {
        self.inner.cap
    }

    pub fn idle_len(&self) -> usize {
        lock_recover(&self.inner.state).idle.len()
    }

    pub fn total_len(&self) -> usize {
        lock_recover(&self.inner.state).total
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckpointStatus {
    pub busy: i64,
    pub log_frames: i64,
    pub checkpointed_frames: i64,
}

pub fn checkpoint_truncate(conn: &Connection) -> Result<CheckpointStatus> {
    checkpoint(conn, "PRAGMA wal_checkpoint(TRUNCATE)")
}

pub fn checkpoint_if_wal_larger_than(
    conn: &Connection,
    db: &Path,
    bytes: u64,
) -> Result<Option<CheckpointStatus>> {
    let wal = wal_path(db)?;
    let size = match std::fs::metadata(wal) {
        Ok(metadata) => metadata.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Ok(None),
    };
    if size <= bytes {
        return Ok(None);
    }
    checkpoint(conn, "PRAGMA wal_checkpoint(PASSIVE)").map(Some)
}

fn checkpoint(conn: &Connection, pragma: &str) -> Result<CheckpointStatus> {
    conn.query_row(pragma, [], |row| {
        Ok(CheckpointStatus {
            busy: row.get(0)?,
            log_frames: row.get(1)?,
            checkpointed_frames: row.get(2)?,
        })
    })
    .map_err(Into::into)
}

fn wal_path(db: &Path) -> Result<PathBuf> {
    let file_name = db.file_name().ok_or(DbError::InvalidDatabasePath)?;
    let mut wal_name = OsString::from(file_name);
    wal_name.push("-wal");
    Ok(db.with_file_name(wal_name))
}

pub fn optimize_on_open(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA optimize=0x10002;")?;
    Ok(())
}

pub fn optimize_boundary(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA optimize;")?;
    Ok(())
}
