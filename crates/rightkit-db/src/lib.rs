#![forbid(unsafe_code)]

use rusqlite::{Connection, OpenFlags};
use std::ffi::OsString;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use zeroize::Zeroizing;

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

#[derive(Clone)]
pub struct DbOptions {
    pub path: PathBuf,
    pub key: Option<Zeroizing<String>>,
    pub profile: CacheProfile,
    pub durability: DurabilityProfile,
    pub foreign_keys: bool,
    pub integrity_check_on_open: bool,
}

impl DbOptions {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            key: None,
            profile: CacheProfile::Default,
            durability: DurabilityProfile::Normal,
            foreign_keys: false,
            integrity_check_on_open: false,
        }
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
        let key = self.key.as_ref().map(|key| key.as_str());
        validate_key(key)?;
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let conn = Connection::open_with_flags(&self.path, flags)?;
        apply_key_first(&conn, key)?;
        verify_key(&conn)?;
        configure_connection(&conn, self, kind)?;
        if self.integrity_check_on_open {
            integrity_check(&conn, self.key.is_some())?;
        }
        Ok(conn)
    }
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
    conn.pragma_update(None, "journal_mode", "WAL")?;
    let synchronous = match options.durability {
        DurabilityProfile::Full => "FULL",
        DurabilityProfile::Normal => "NORMAL",
    };
    conn.pragma_update(None, "synchronous", synchronous)?;
    conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT)?;
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

#[cfg(feature = "sqlcipher")]
fn apply_key_first(conn: &Connection, key: Option<&str>) -> Result<()> {
    if let Some(key) = key {
        let statement = Zeroizing::new(format!("PRAGMA key = '{key}';"));
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
