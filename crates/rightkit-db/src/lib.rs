// Unsafe is confined to the host-name FFI in `host`.
#![deny(unsafe_code)]

#[cfg(all(feature = "sqlcipher", feature = "sqlcipher-system"))]
compile_error!("rightkit-db features `sqlcipher` and `sqlcipher-system` are mutually exclusive; enable exactly one");

use rusqlite::{Connection, OpenFlags, Transaction, TransactionBehavior};
use std::ffi::OsString;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

mod backup;
#[allow(unsafe_code)]
mod host;
pub use host::per_host_path;
/// The exact `rusqlite` this crate links (`links = "sqlite3"` allows one per build). Apps name
/// `Connection`, `params!`, `Row`, `OptionalExtension`, `TransactionBehavior` through this path
/// instead of declaring their own `rusqlite` dependency.
pub use rusqlite;
mod migrate;
mod recover;
mod unreadable;

pub use backup::{online_backup_to, restore_from, BackupProgress};
pub use migrate::{
    add_column_if_missing, column_exists, migrate, migration_version, pending_marker, rollback_to,
    rollback_to_with_storage, user_version, LegacyVersionTable, MigrateOptions, Migration,
    MigrationReport, PendingMarker, Step, VersionStorage,
};
#[cfg(feature = "cipher")]
pub use recover::rekey;
pub use recover::{
    quarantine_database, quarantine_database_opts, quarantine_database_with, MoveMode, OpenOutcome,
    QuarantineOptions, QuarantineStamp, RecoveryPolicy, SidecarPolicy,
};

pub use unreadable::{
    classify_open_failure, unreadable_counter_path, RecoveryAction, StorageProblem,
    UnreadableCause, UnreadablePolicy, UnreadableRecovery,
};

const BUSY_TIMEOUT_MS: u64 = 5_000;
const JOURNAL_SIZE_LIMIT: i64 = 67_108_864;

pub type Result<T> = std::result::Result<T, DbError>;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("a SQLCipher key requires the rightkit-db `sqlcipher` or `sqlcipher-system` feature")]
    SqlcipherFeatureRequired,
    #[error("SQLCipher key contains characters that are unsafe for PRAGMA interpolation")]
    InvalidSqlcipherKey,
    /// An open-time `quick_check` or `integrity_check` failed. Open-time recovery treats it as an
    /// unreadable database (see `DbOptions::open_writer_recovering`).
    #[error("database integrity check failed: {0}")]
    Integrity(String),
    #[error("quarantine suffix {0:?} must be non-empty and contain no path separator or NUL")]
    InvalidQuarantineSuffix(String),
    #[error("read-pool capacity must be greater than zero")]
    InvalidPoolCapacity,
    #[error("reader cache size must be between 1 and 2147483647 KiB, got {0}")]
    InvalidReaderCacheSize(u32),
    /// Checkout after [`ReadPool::close`] or [`ReadPool::close_and_wait`].
    #[error("read pool is closed")]
    ReadPoolClosed,
    /// [`ReadPool::close_and_wait`] timed out. `outstanding` counts readers still checked out or
    /// opening.
    #[error("read pool close timed out after {timeout:?} with {outstanding} reader(s) still out")]
    ReadPoolCloseTimeout {
        outstanding: usize,
        timeout: Duration,
    },
    /// A reader did not come up read-only: `PRAGMA query_only` did not stay on, or a read-only
    /// handle reported a writable main database.
    #[error("reader connection is not read-only: {0}")]
    ReadOnlyNotEffective(String),
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
    /// The database file could not be read (permission or I/O failure) or its storage is
    /// unavailable. Not corruption: the file was not touched unless `quarantine_after` was
    /// reached, in which case recovery succeeds instead of returning this. See
    /// [`UnreadablePolicy`].
    #[error(
        "database {} cannot be read: {cause} ({consecutive_failures} consecutive failed open(s)): {source}",
        .path.display()
    )]
    Unreadable {
        path: PathBuf,
        cause: UnreadableCause,
        /// Consecutive failed opens recorded so far (0 under [`UnreadablePolicy::Fail`] and for
        /// storage problems, which are never counted).
        consecutive_failures: u32,
        /// The count at which [`UnreadablePolicy::QuarantineAfter`] would set the file aside.
        quarantine_after: Option<u32>,
        #[source]
        source: Box<DbError>,
    },
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
#[non_exhaustive]
pub enum JournalSelect {
    /// WAL when the filesystem honors it and is not a detected network mount; else a
    /// rollback journal (TRUNCATE with [`DbOptions::per_host_fallback`], else DELETE).
    WalWithFallback,
    /// WAL or fail (no fallback).
    WalStrict,
    /// Rollback journal, deleted after each transaction.
    Delete,
    /// Rollback journal, truncated after each transaction.
    Truncate,
}

impl JournalSelect {
    fn is_rollback(self) -> bool {
        matches!(self, JournalSelect::Delete | JournalSelect::Truncate)
    }
}

/// Environment kill-switch (`wal` | `delete` | `truncate`) for a misclassified mount in the
/// field. An app's own variable ([`DbOptions::journal_env`]) is checked first.
pub const JOURNAL_ENV: &str = "RIGHTKIT_SQLITE_JOURNAL_MODE";

/// Post-key, pre-pragma per-connection initializer (register functions, collations, ...).
/// Open-time integrity check, selected by [`DbOptions::integrity`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum IntegrityCheck {
    /// No check at open (default).
    #[default]
    Off,
    /// `PRAGMA quick_check`: structural page checks, without the index-to-table cross-check. It
    /// costs far less than `Full` on large databases.
    Quick,
    /// `PRAGMA integrity_check`, or `PRAGMA cipher_integrity_check` for a keyed database.
    Full,
}

pub type ConnInit = Arc<dyn Fn(&Connection) -> rusqlite::Result<()> + Send + Sync>;

#[derive(Clone)]
pub struct DbOptions {
    pub path: PathBuf,
    /// SQLite busy wait, installed before key verification or any bootstrap statement.
    /// Default: five seconds.
    pub busy_timeout: Duration,
    /// SQLCipher passphrase-style key (base64-ish charset). See also [`DbOptions::raw_key`].
    pub key: Option<Zeroizing<String>>,
    /// SQLCipher raw 256-bit key, applied as `x'<hex>'` (no KDF). Takes precedence over `key`.
    pub raw_key: Option<Zeroizing<[u8; 32]>>,
    pub profile: CacheProfile,
    pub durability: DurabilityProfile,
    pub foreign_keys: bool,
    /// Legacy switch: `true` runs a full check on every open. Ignored when `integrity` is not `Off`.
    pub integrity_check_on_open: bool,
    /// Open-time integrity check (default `Off`). A failed check is [`DbError::Integrity`], which
    /// `open_writer_recovering` treats as unreadable.
    pub integrity: IntegrityCheck,
    pub journal: JournalSelect,
    /// Narrow the database and its sidecars to owner-only (0600) on unix. Default on. Set it to
    /// `false` when sibling processes must read the file (for example a helper running as another
    /// user); that is the intended switch, not a bug.
    pub owner_only: bool,
    pub init: Option<ConnInit>,
    /// The app's own kill-switch variable (e.g. `CODERIGHT_SQLITE_JOURNAL_MODE`), checked
    /// before [`JOURNAL_ENV`].
    pub journal_env: Option<String>,
    /// When a rollback journal is in force (network mount, kill switch, or WAL refused),
    /// open the per-host sibling [`per_host_path`] instead of the requested file, so hosts
    /// sharing a network directory never share one rollback-journaled database. The
    /// requested file is not created. Default off.
    pub per_host_fallback: bool,
    /// Sets `PRAGMA secure_delete` on every connection at open: `Some(true)` overwrites freed
    /// pages, `Some(false)` does not, `None` (default) leaves SQLite's compile-time default.
    pub secure_delete: Option<bool>,
    /// Run `PRAGMA optimize` on each writer connection at open (default `true`). Set it to `false`
    /// to leave planner statistics alone at open, for example when the file is opened only to
    /// inspect it. Readers never run it, whatever this is set to.
    pub optimize_on_open: bool,
}

impl DbOptions {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            busy_timeout: Duration::from_millis(BUSY_TIMEOUT_MS),
            key: None,
            raw_key: None,
            profile: CacheProfile::Default,
            durability: DurabilityProfile::Normal,
            foreign_keys: false,
            integrity_check_on_open: false,
            integrity: IntegrityCheck::Off,
            journal: JournalSelect::WalWithFallback,
            owner_only: true,
            init: None,
            journal_env: None,
            per_host_fallback: false,
            secure_delete: None,
            optimize_on_open: true,
        }
    }

    pub fn with_journal_env(mut self, name: impl Into<String>) -> Self {
        self.journal_env = Some(name.into());
        self
    }

    pub fn with_per_host_fallback(mut self, on: bool) -> Self {
        self.per_host_fallback = on;
        self
    }

    /// Turn `PRAGMA optimize` off at writer open (see [`DbOptions::optimize_on_open`]).
    pub fn without_optimize_on_open(mut self) -> Self {
        self.optimize_on_open = false;
        self
    }

    /// The file connections actually open: the requested path, or its per-host sibling when
    /// [`Self::per_host_fallback`] applies (a rollback journal is in force, or a writer
    /// already fell back there).
    pub fn effective_path(&self) -> PathBuf {
        if !self.per_host_fallback {
            return self.path.clone();
        }
        let sibling = per_host_path(&self.path);
        if effective_journal(self).is_rollback() || (sibling.exists() && !self.path.exists()) {
            sibling
        } else {
            self.path.clone()
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
        self.open(ConnectionKind::Reader { cache_kib: None })
    }

    /// Open exactly `path` read-only, without creating it or running mutating bootstrap.
    /// URI parameters are passed to SQLite unchanged. Journal fallback, permissions,
    /// `init`, optimization, and migrations are skipped; key and timeout still apply.
    pub fn open_read_only(&self) -> Result<Connection> {
        self.open_existing_at(&self.path, OpenFlags::SQLITE_OPEN_READ_ONLY)
    }

    /// Open exactly `path` read-write without CREATE or mutating bootstrap.
    /// Uses the same minimal setup as [`Self::open_read_only`].
    pub fn open_existing_writer(&self) -> Result<Connection> {
        self.open_existing_at(&self.path, OpenFlags::SQLITE_OPEN_READ_WRITE)
    }

    /// Open a private, ephemeral database. `path`, filesystem permissions, journal
    /// selection/fallback, and mmap are ignored; other bootstrap options still apply.
    pub fn open_in_memory(&self) -> Result<Connection> {
        let key = self.key_pragma_value()?;
        let conn = Connection::open_in_memory()?;
        self.initialize(&conn, key.as_ref().map(|k| k.as_str()))?;
        configure_connection(&conn, self, ConnectionKind::Memory, None)?;
        self.check_on_open(&conn)?;
        Ok(conn)
    }

    fn open_existing_at(&self, path: &Path, access: OpenFlags) -> Result<Connection> {
        let key = self.key_pragma_value()?;
        let conn = Connection::open_with_flags(
            path,
            access | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(self.busy_timeout)?;
        apply_key_first(&conn, key.as_ref().map(|k| k.as_str()))?;
        verify_key(&conn)?;
        self.check_on_open(&conn)?;
        Ok(conn)
    }

    fn initialize(&self, conn: &Connection, key: Option<&str>) -> Result<()> {
        conn.busy_timeout(self.busy_timeout)?;
        apply_key_first(conn, key)?;
        verify_key(conn)?;
        if let Some(init) = &self.init {
            init(conn)?;
        }
        Ok(())
    }

    /// A read pool with [`ReadPoolOptions::new`]: read-write file handles, with `query_only` set
    /// and verified on every reader. Same as `open_read_pool_with(cap, ReadPoolOptions::new())`.
    pub fn open_read_pool(&self, cap: usize) -> Result<ReadPool> {
        ReadPool::open(self.clone(), cap, ReadPoolOptions::new())
    }

    /// A read pool configured by [`ReadPoolOptions`]. A read-only pool needs the database to exist.
    pub fn open_read_pool_with(&self, cap: usize, pool: ReadPoolOptions) -> Result<ReadPool> {
        ReadPool::open(self.clone(), cap, pool)
    }

    fn open_pool_reader(&self, pool: ReadPoolOptions) -> Result<Connection> {
        if pool.read_only {
            self.open_pool_read_only(pool.cache_kib)
        } else {
            self.open(ConnectionKind::Reader {
                cache_kib: pool.cache_kib,
            })
        }
    }

    /// A pool reader opened with `SQLITE_OPEN_READ_ONLY`, so the file refuses writes even after
    /// `query_only` is turned off. It never creates the file, changes the journal mode, narrows
    /// permissions, or runs `optimize` or `init`; the writer owns those.
    fn open_pool_read_only(&self, cache_kib: Option<u32>) -> Result<Connection> {
        let path = self.effective_path();
        let key = self.key_pragma_value()?;
        let conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(self.busy_timeout)?;
        apply_key_first(&conn, key.as_ref().map(|k| k.as_str()))?;
        verify_key(&conn)?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        if self.foreign_keys {
            conn.pragma_update(None, "foreign_keys", true)?;
        }
        apply_cache_settings(&conn, self, ConnectionKind::Reader { cache_kib })?;
        enforce_query_only(&conn)?;
        if !conn.is_readonly("main")? {
            return Err(DbError::ReadOnlyNotEffective(
                "SQLITE_OPEN_READ_ONLY handle reports a writable main database".into(),
            ));
        }
        self.check_on_open(&conn)?;
        Ok(conn)
    }

    fn open(&self, kind: ConnectionKind) -> Result<Connection> {
        let path = self.effective_path();
        let existed = path.exists();
        // The per-host sibling is always rollback-journaled, whoever opens it.
        let forced = (path != self.path).then(|| match effective_journal(self) {
            mode if mode.is_rollback() => mode,
            _ => fallback_mode(self),
        });
        let (conn, journal) = self.open_at(&path, kind, forced)?;
        // WAL refused at runtime: with per-host fallback, the rollback journal belongs on the
        // per-host sibling, and a file this open created at the requested path goes away.
        if journal == Opened::FellBack && self.per_host_fallback && path == self.path {
            drop(conn);
            if !existed {
                for suffix in ["", "-journal", "-wal", "-shm"] {
                    let mut name = OsString::from(path.as_os_str());
                    name.push(suffix);
                    let _ = std::fs::remove_file(PathBuf::from(name));
                }
            }
            let sibling = per_host_path(&self.path);
            return Ok(self
                .open_at(&sibling, kind, Some(JournalSelect::Truncate))?
                .0);
        }
        Ok(conn)
    }

    fn open_at(
        &self,
        path: &Path,
        kind: ConnectionKind,
        forced: Option<JournalSelect>,
    ) -> Result<(Connection, Opened)> {
        let key = self.key_pragma_value()?;
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(path, flags)?;
        // Narrow the main file before any write so WAL/SHM sidecars inherit owner-only.
        if self.owner_only {
            rightkit_fs::restrict_owner_only(path)?;
        }
        self.initialize(&conn, key.as_ref().map(|k| k.as_str()))?;
        if let Some(on) = self.secure_delete {
            conn.pragma_update(None, "secure_delete", if on { "ON" } else { "OFF" })?;
        }
        let journal = configure_connection(&conn, self, kind, forced)?;
        self.check_on_open(&conn)?;
        if self.owner_only {
            harden_sidecars(path)?;
        }
        Ok((conn, journal))
    }

    /// Run the open-time check selected by `integrity`; `integrity_check_on_open` alone means `Full`.
    fn check_on_open(&self, conn: &Connection) -> Result<()> {
        let mode = match self.integrity {
            IntegrityCheck::Off if self.integrity_check_on_open => IntegrityCheck::Full,
            mode => mode,
        };
        match (mode, self.is_keyed()) {
            (IntegrityCheck::Off, _) => Ok(()),
            (IntegrityCheck::Quick, _) => run_integrity_pragma(conn, "PRAGMA quick_check"),
            (IntegrityCheck::Full, true) => {
                run_integrity_pragma(conn, "PRAGMA cipher_integrity_check")
            }
            (IntegrityCheck::Full, false) => run_integrity_pragma(conn, "PRAGMA integrity_check"),
        }
    }

    /// The `PRAGMA key` right-hand side (already quoted), validated against injection.
    fn key_pragma_value(&self) -> Result<Option<Zeroizing<String>>> {
        if let Some(raw) = &self.raw_key {
            #[cfg(not(feature = "cipher"))]
            {
                let _ = raw;
                return Err(DbError::SqlcipherFeatureRequired);
            }
            #[cfg(feature = "cipher")]
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

/// Default-options read-only open; never creates a missing database.
pub fn open_read_only(path: impl AsRef<Path>) -> Result<Connection> {
    DbOptions::new(path.as_ref()).open_read_only()
}

/// Default-options read-write open; never creates a missing database.
pub fn open_existing_writer(path: impl AsRef<Path>) -> Result<Connection> {
    DbOptions::new(path.as_ref()).open_existing_writer()
}

/// Default-options private in-memory database.
pub fn open_in_memory() -> Result<Connection> {
    DbOptions::new(":memory:").open_in_memory()
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
    /// `cache_kib` overrides the reader cache size; see [`ReadPoolOptions::with_reader_cache_kib`].
    Reader {
        cache_kib: Option<u32>,
    },
    Memory,
}

impl ConnectionKind {
    fn reader_cache_kib(self) -> Option<u32> {
        match self {
            ConnectionKind::Reader { cache_kib } => cache_kib,
            ConnectionKind::Writer | ConnectionKind::Memory => None,
        }
    }
}

/// What journal setup did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Opened {
    AsRequested,
    /// WAL was wanted but not honored; a rollback journal is in force.
    FellBack,
}

fn configure_connection(
    conn: &Connection,
    options: &DbOptions,
    kind: ConnectionKind,
    forced: Option<JournalSelect>,
) -> Result<Opened> {
    let opened = if matches!(kind, ConnectionKind::Memory) {
        Opened::AsRequested
    } else {
        configure_journal(conn, options, kind, forced)?
    };
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
    apply_cache_settings(conn, options, kind)?;
    if matches!(kind, ConnectionKind::Reader { .. }) {
        // Readers never run `PRAGMA optimize`: it can write statistics, and the writer runs it
        // when it opens. `query_only` is set after every other reader setup statement.
        enforce_query_only(conn)?;
    } else if options.optimize_on_open {
        optimize_on_open(conn)?;
    }
    Ok(opened)
}

/// Profile cache and mmap sizes, then the reader override, which wins over the profile on readers
/// only. Writers and in-memory connections keep the profile size.
fn apply_cache_settings(
    conn: &Connection,
    options: &DbOptions,
    kind: ConnectionKind,
) -> Result<()> {
    if options.profile == CacheProfile::LargeWorkingSet {
        conn.pragma_update(None, "cache_size", -65_536_i64)?;
        if !matches!(kind, ConnectionKind::Memory) {
            conn.pragma_update(None, "mmap_size", 268_435_456_i64)?;
        }
    }
    if let Some(kib) = kind.reader_cache_kib() {
        conn.pragma_update(None, "cache_size", -i64::from(kib))?;
    }
    Ok(())
}

/// Sets `PRAGMA query_only = ON` on a reader and reads it back, so a reader that did not come up
/// query-only is never handed out.
fn enforce_query_only(conn: &Connection) -> Result<()> {
    conn.pragma_update(None, "query_only", true)?;
    let on: i64 = conn.query_row("PRAGMA query_only", [], |row| row.get(0))?;
    if on != 1 {
        return Err(DbError::ReadOnlyNotEffective(
            "PRAGMA query_only did not stay ON".into(),
        ));
    }
    Ok(())
}

fn set_rollback(conn: &Connection, mode: JournalSelect) -> Result<()> {
    let pragma = if mode == JournalSelect::Truncate {
        "PRAGMA journal_mode=TRUNCATE"
    } else {
        "PRAGMA journal_mode=DELETE"
    };
    conn.query_row(pragma, [], |row| row.get::<_, String>(0))?;
    Ok(())
}

fn configure_journal(
    conn: &Connection,
    options: &DbOptions,
    kind: ConnectionKind,
    forced: Option<JournalSelect>,
) -> Result<Opened> {
    let wanted = forced.unwrap_or_else(|| effective_journal(options));
    if wanted.is_rollback() {
        // Rollback modes are per connection: a reader left in the default DELETE mode would
        // try to delete a TRUNCATE writer's open journal (IOERR_DELETE on Windows).
        set_rollback(conn, wanted)?;
        return Ok(Opened::AsRequested);
    }
    let honored = conn
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))
        .map(|mode| mode.eq_ignore_ascii_case("wal"))
        .unwrap_or(false)
        // `journal_mode=WAL` can report success before the -shm/-wal sidecars are usable;
        // one empty write transaction proves the sidecars work before anything depends on them.
        && (matches!(kind, ConnectionKind::Reader { .. })
            || conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").is_ok());
    if honored {
        return Ok(Opened::AsRequested);
    }
    if wanted == JournalSelect::WalStrict {
        return Err(DbError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR),
            Some("WAL journal mode was not honored by this filesystem".into()),
        )));
    }
    let _ = conn.execute_batch("ROLLBACK;");
    set_rollback(conn, fallback_mode(options))?;
    Ok(Opened::FellBack)
}

/// The rollback journal used when WAL is unavailable: TRUNCATE with per-host fallback
/// (CodeRight's network mode), DELETE otherwise.
fn fallback_mode(options: &DbOptions) -> JournalSelect {
    if options.per_host_fallback {
        JournalSelect::Truncate
    } else {
        JournalSelect::Delete
    }
}

/// Kill switches (the app's own variable first, then [`JOURNAL_ENV`]); set-but-empty or
/// unrecognized values are ignored.
fn env_journal(options: &DbOptions) -> Option<JournalSelect> {
    options
        .journal_env
        .iter()
        .map(String::as_str)
        .chain([JOURNAL_ENV])
        .find_map(|name| {
            match std::env::var(name)
                .ok()?
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "wal" => Some(JournalSelect::WalStrict),
                "delete" => Some(JournalSelect::Delete),
                "truncate" => Some(JournalSelect::Truncate),
                _ => None,
            }
        })
}

fn effective_journal(options: &DbOptions) -> JournalSelect {
    if let Some(mode) = env_journal(options) {
        return mode;
    }
    if options.journal == JournalSelect::WalWithFallback
        && rightkit_fs::is_network_filesystem(&options.path)
    {
        return fallback_mode(options);
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

#[cfg(feature = "cipher")]
fn apply_key_first(conn: &Connection, key: Option<&str>) -> Result<()> {
    if let Some(key) = key {
        let statement = Zeroizing::new(format!("PRAGMA key = {key};"));
        conn.execute_batch(statement.as_str())?;
    }
    Ok(())
}

#[cfg(not(feature = "cipher"))]
fn apply_key_first(_conn: &Connection, key: Option<&str>) -> Result<()> {
    if key.is_some() {
        return Err(DbError::SqlcipherFeatureRequired);
    }
    Ok(())
}

fn validate_key(key: Option<&str>) -> Result<()> {
    #[cfg(not(feature = "cipher"))]
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

fn run_integrity_pragma(conn: &Connection, pragma: &str) -> Result<()> {
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

/// Options for a [`ReadPool`]. The default ([`ReadPoolOptions::new`]) matches
/// [`DbOptions::open_read_pool`]: read-write file handles, with `query_only` set and verified on
/// every reader. Build with the `with_*` methods; the fields are private.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReadPoolOptions {
    read_only: bool,
    cache_kib: Option<u32>,
}

impl ReadPoolOptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens each reader with `SQLITE_OPEN_READ_ONLY`, so writes fail even if `query_only` is later
    /// turned off. The database must already exist: the pool never creates it, changes its journal
    /// mode, narrows its permissions, or runs `optimize` or `init`. A rollback journal left by a
    /// crash is recovered by a writer, not by a read-only reader. Default off.
    pub fn with_read_only(mut self, on: bool) -> Self {
        self.read_only = on;
        self
    }

    /// Page cache size for each reader connection, in KiB. It is applied as `PRAGMA cache_size`
    /// set to `-kib`, so the size does not depend on the page size. Valid values are 1 to
    /// 2147483647; the pool checks this when it opens. It overrides
    /// [`CacheProfile::LargeWorkingSet`] on readers only. Writers and [`DbOptions::open_reader`]
    /// keep the profile size. Default: the profile's size.
    pub fn with_reader_cache_kib(mut self, kib: u32) -> Self {
        self.cache_kib = Some(kib);
        self
    }
}

/// A bounded pool of reader connections. Clones share one pool.
#[derive(Clone)]
pub struct ReadPool {
    inner: Arc<ReadPoolInner>,
}

struct ReadPoolInner {
    options: DbOptions,
    pool_options: ReadPoolOptions,
    cap: usize,
    state: Mutex<ReadPoolState>,
    /// Wakes a `get` waiting for capacity, and every waiter when the pool closes.
    available: Condvar,
    /// Wakes `close_and_wait` when a connection is released after the pool closed.
    drained: Condvar,
}

#[derive(Default)]
struct ReadPoolState {
    idle: Vec<Connection>,
    /// Idle, checked-out, and still-opening connections.
    total: usize,
    closed: bool,
}

impl ReadPoolInner {
    fn open_reader(&self) -> Result<Connection> {
        self.options.open_pool_reader(self.pool_options)
    }
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
        if state.closed {
            // Close the file before the count drops, so `close_and_wait` returns only once it is
            // released. Checked under the same lock as `close`, so no connection is parked idle
            // after the pool has closed.
            drop(conn);
            state.total -= 1;
            pool.drained.notify_all();
        } else {
            state.idle.push(conn);
            pool.available.notify_one();
        }
    }
}

impl ReadPool {
    fn open(options: DbOptions, cap: usize, pool_options: ReadPoolOptions) -> Result<Self> {
        if cap == 0 {
            return Err(DbError::InvalidPoolCapacity);
        }
        if let Some(kib) = pool_options.cache_kib {
            if kib == 0 || kib > i32::MAX.unsigned_abs() {
                return Err(DbError::InvalidReaderCacheSize(kib));
            }
        }
        let pool = Self {
            inner: Arc::new(ReadPoolInner {
                options,
                pool_options,
                cap,
                state: Mutex::new(ReadPoolState::default()),
                available: Condvar::new(),
                drained: Condvar::new(),
            }),
        };
        let conn = pool.inner.open_reader()?;
        let mut state = lock_recover(&pool.inner.state);
        state.total = 1;
        state.idle.push(conn);
        drop(state);
        Ok(pool)
    }

    /// Checks out an idle reader, opens a new one while under the cap, or waits for a return.
    /// Fails with [`DbError::ReadPoolClosed`] once the pool is closed, including while waiting.
    pub fn get(&self) -> Result<PooledReadConnection> {
        let mut state = lock_recover(&self.inner.state);
        loop {
            if state.closed {
                return Err(DbError::ReadPoolClosed);
            }
            if let Some(conn) = state.idle.pop() {
                return Ok(PooledReadConnection {
                    conn: Some(conn),
                    pool: Arc::downgrade(&self.inner),
                });
            }
            if state.total < self.inner.cap {
                state.total += 1;
                drop(state);
                return self.open_checked_out();
            }
            state = match self.inner.available.wait(state) {
                Ok(state) => state,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }

    /// Opens a reader for a `get` that reserved a slot. If the pool closed meanwhile, the new
    /// reader is dropped and the caller gets [`DbError::ReadPoolClosed`].
    fn open_checked_out(&self) -> Result<PooledReadConnection> {
        match self.inner.open_reader() {
            Ok(conn) => {
                let mut state = lock_recover(&self.inner.state);
                if state.closed {
                    drop(conn);
                    state.total -= 1;
                    self.inner.drained.notify_all();
                    return Err(DbError::ReadPoolClosed);
                }
                Ok(PooledReadConnection {
                    conn: Some(conn),
                    pool: Arc::downgrade(&self.inner),
                })
            }
            Err(error) => {
                let mut state = lock_recover(&self.inner.state);
                state.total -= 1;
                self.inner.available.notify_one();
                self.inner.drained.notify_all();
                Err(error)
            }
        }
    }

    /// Stops handing out readers and drops the idle ones now. A reader still checked out closes
    /// when it is returned. Calling it again does nothing.
    pub fn close(&self) {
        let idle = {
            let mut state = lock_recover(&self.inner.state);
            if state.closed {
                return;
            }
            state.closed = true;
            self.inner.available.notify_all();
            std::mem::take(&mut state.idle)
        };
        let closed = idle.len();
        // Files close before the count drops, so `close_and_wait` sees them released.
        drop(idle);
        let mut state = lock_recover(&self.inner.state);
        state.total -= closed;
        self.inner.drained.notify_all();
    }

    /// [`Self::close`], then waits up to `timeout` for every checked-out reader to be returned.
    /// `Ok(())` means every connection is closed and its database file released. Otherwise it
    /// returns [`DbError::ReadPoolCloseTimeout`] with the number still out. Do not call it while
    /// holding a reader from this pool: that reader cannot come back, so the call times out. A
    /// timeout too large for the clock waits in bounded slices until the readers return.
    pub fn close_and_wait(&self, timeout: Duration) -> Result<()> {
        self.close();
        let deadline = Instant::now().checked_add(timeout);
        let mut state = lock_recover(&self.inner.state);
        loop {
            if state.total == 0 {
                return Ok(());
            }
            let remaining = match deadline {
                Some(at) => at.saturating_duration_since(Instant::now()),
                None => Duration::from_secs(60),
            };
            if remaining.is_zero() {
                return Err(DbError::ReadPoolCloseTimeout {
                    outstanding: state.total,
                    timeout,
                });
            }
            let (next, _) = match self.inner.drained.wait_timeout(state, remaining) {
                Ok(pair) => pair,
                Err(poisoned) => poisoned.into_inner(),
            };
            state = next;
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
