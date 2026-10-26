//! `EncryptedStore`: open with key selection, the owned writer and managed read pools, backup,
//! restore, and the drain and resume that every transition uses.

use super::error::LifecycleError;
use super::keystore::{
    Key, KeySource, KeyStoreError, PendingKey, ProtectedKeySession, ProtectedKeyStore,
};
use super::rotate::{recover_with_guard, Faults, NoFaults, RecoveryOutcome};
use super::transition::{
    acquire_shared, journal_exists, next_id, JournalState, RootIdentity, StoreTransition,
    TransitionKind, TransitionOptions,
};
use crate::{
    lock_recover, restore_database_locked, DbError, DbOptions, IntegrityCheck,
    PooledReadConnection, ReadPool, ReadPoolOptions, RestoreOptions, RestoreReceipt, StoreLock,
};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

/// Checks the caller's own schema or domain rules on a store opened with the key under test.
/// `Err(reason)` fails the rotation (rolled back), restore (rolled back) or relocation (not
/// activated). Drop every connection the check opens before it returns.
pub type StoreValidate = Arc<dyn Fn(&DbOptions) -> Result<(), String> + Send + Sync>;

/// The host's domain workers (the app's own background threads and services that use the store).
/// The store stops and resumes them around a transition instead of the host keeping a local mutex.
pub trait HostQuiesce: Send + Sync {
    /// Stop everything that uses the store. `Err(reason)` is `Busy`: nothing is changed.
    fn stop(&self, kind: TransitionKind) -> Result<(), String>;

    /// The transition finished (successfully or not) and the store is usable again. Not called
    /// when the store stays blocked for recovery.
    fn resume(&self) {}
}

/// How to open and manage one encrypted database.
#[derive(Clone)]
pub struct EncryptedStoreConfig {
    /// Path and open options. Any `key` or `raw_key` set here is ignored: the key always comes from
    /// `key_store`.
    pub db: DbOptions,
    pub key_store: Arc<dyn ProtectedKeyStore>,
    /// Explicit legacy staging account aliases (for example `SQLCipher/DBKey.staging`), read as
    /// candidate keys and migrated to the canonical slot when one is the sole viable key.
    pub legacy_staging_accounts: Vec<String>,
    /// Create the database when the file is missing and a primary key is stored. Default `false`:
    /// a missing file is `StoreMissing`, never an empty database. A file that exists but that no
    /// key opens is never replaced, whatever this is set to.
    pub create_if_missing: bool,
    /// Delete a legacy staging item after canonical storage and a reopen readback prove recovery
    /// stays possible without it. Default `true`.
    pub delete_legacy_after_migration: bool,
    pub transition: TransitionOptions,
    pub host: Option<Arc<dyn HostQuiesce>>,
    pub validate: Option<StoreValidate>,
    /// How long a drain waits for an in-flight writer and for checked-out pool readers.
    pub drain_timeout: Duration,
}

impl EncryptedStoreConfig {
    pub fn new(db: DbOptions, key_store: Arc<dyn ProtectedKeyStore>) -> Self {
        Self {
            db,
            key_store,
            legacy_staging_accounts: Vec::new(),
            create_if_missing: false,
            delete_legacy_after_migration: true,
            transition: TransitionOptions::default(),
            host: None,
            validate: None,
            drain_timeout: Duration::from_secs(5),
        }
    }

    pub fn with_legacy_staging_account(mut self, alias: impl Into<String>) -> Self {
        self.legacy_staging_accounts.push(alias.into());
        self
    }

    pub fn with_create_if_missing(mut self, on: bool) -> Self {
        self.create_if_missing = on;
        self
    }

    pub fn with_delete_legacy_after_migration(mut self, on: bool) -> Self {
        self.delete_legacy_after_migration = on;
        self
    }

    pub fn with_transition(mut self, options: TransitionOptions) -> Self {
        self.transition = options;
        self
    }

    pub fn with_host(mut self, host: Arc<dyn HostQuiesce>) -> Self {
        self.host = Some(host);
        self
    }

    pub fn with_validate(
        mut self,
        validate: impl Fn(&DbOptions) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.validate = Some(Arc::new(validate));
        self
    }

    pub fn with_drain_timeout(mut self, timeout: Duration) -> Self {
        self.drain_timeout = timeout;
        self
    }

    /// Validate the configuration and strip any key from the database options.
    pub(super) fn checked(mut self) -> Result<Self, LifecycleError> {
        if self.db.path.file_name().is_none() {
            return Err(LifecycleError::InvalidConfig(
                "the database path has no file name".into(),
            ));
        }
        if self.db.path.to_str().is_none() {
            return Err(LifecycleError::InvalidConfig(
                "the database path is not valid UTF-8, so it cannot be journaled".into(),
            ));
        }
        self.db.key = None;
        self.db.raw_key = None;
        Ok(self)
    }

    /// The database options carrying `key`.
    pub(super) fn keyed(&self, key: &Key) -> DbOptions {
        let mut options = self.db.clone();
        options.key = Some(key.clone());
        options.raw_key = None;
        options
    }
}

pub(super) fn key_store_error(slot: &str, error: KeyStoreError) -> LifecycleError {
    LifecycleError::key_store(slot, error)
}

// ----- probing -----------------------------------------------------------------------------

/// What opening the file with one key showed.
pub(super) enum Probe {
    Opens,
    /// SQLite reports not-a-database or corrupt: a wrong key, or a damaged file.
    KeyRejected,
    /// A failure that says nothing about the key (locked, I/O, permission, device not ready).
    Other(DbError),
}

pub(super) fn probe(
    db: &DbOptions,
    path: &Path,
    key: &Key,
    check: IntegrityCheck,
    flags: OpenFlags,
) -> Probe {
    let mut options = db.clone();
    options.key = Some(key.clone());
    options.raw_key = None;
    options.integrity = check;
    options.integrity_check_on_open = false;
    match options.open_existing_at(path, flags) {
        Ok(conn) => {
            drop(conn);
            Probe::Opens
        }
        Err(error) => classify_probe(error),
    }
}

fn classify_probe(error: DbError) -> Probe {
    if matches!(error, DbError::Integrity(_)) {
        return Probe::Other(error);
    }
    if crate::recover::is_unreadable(&error) {
        return Probe::KeyRejected;
    }
    Probe::Other(error)
}

/// A usable key that is not a plausible passphrase for this crate cannot open anything.
pub(super) fn key_is_usable(key: &Key) -> bool {
    crate::validate_key(Some(key.as_str())).is_ok()
}

pub(super) fn is_plaintext_sqlite(path: &Path) -> bool {
    use std::io::Read;
    let mut header = [0u8; 16];
    match std::fs::File::open(path).and_then(|mut file| file.read_exact(&mut header)) {
        Ok(()) => &header == b"SQLite format 3\0",
        Err(_) => false,
    }
}

pub(super) fn db_unavailable(path: &Path, error: DbError) -> LifecycleError {
    let cause = crate::classify_open_failure(&error, path);
    LifecycleError::DatabaseUnavailable {
        path: path.to_path_buf(),
        cause,
        source: Box::new(error),
    }
}

/// `Ok(true)` when the file exists, `Ok(false)` when it is not there.
pub(super) fn file_present(path: &Path) -> Result<bool, LifecycleError> {
    match std::fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(db_unavailable(
            path,
            DbError::Fs(rightkit_fs::FsError::io(path, error)),
        )),
    }
}

// ----- reading every key slot --------------------------------------------------------------

pub(super) enum SlotRead {
    Absent,
    Key(Key),
    Failed(KeyStoreError),
}

impl SlotRead {
    fn from_result(result: Result<Option<Key>, KeyStoreError>) -> Self {
        match result {
            Ok(Some(key)) => Self::Key(key),
            Ok(None) => Self::Absent,
            Err(error) => Self::Failed(error),
        }
    }
}

pub(super) enum PendingRead {
    Absent,
    Key(PendingKey),
    Failed(KeyStoreError),
}

pub(super) struct Slots {
    pub primary: SlotRead,
    pub pending: PendingRead,
    pub recovery: SlotRead,
    pub legacy: Vec<(String, SlotRead)>,
}

impl Slots {
    pub(super) fn read(cfg: &EncryptedStoreConfig, ks: &mut dyn ProtectedKeySession) -> Self {
        let primary = SlotRead::from_result(ks.read_primary());
        let pending = match ks.read_pending() {
            Ok(Some(pending)) => PendingRead::Key(pending),
            Ok(None) => PendingRead::Absent,
            Err(error) => PendingRead::Failed(error),
        };
        let recovery = SlotRead::from_result(ks.read_recovery());
        let legacy = cfg
            .legacy_staging_accounts
            .iter()
            .map(|alias| {
                (
                    alias.clone(),
                    SlotRead::from_result(ks.read_legacy_staging(alias)),
                )
            })
            .collect();
        Self {
            primary,
            pending,
            recovery,
            legacy,
        }
    }

    /// Candidate keys in preference order (primary, pending, mapped legacy staging, recovery),
    /// without duplicates (the first source of an equal key wins).
    fn candidates(&self) -> Vec<(KeySource, Key)> {
        let mut out: Vec<(KeySource, Key)> = Vec::new();
        let mut add = |source: KeySource, key: &Key| {
            if !out.iter().any(|(_, known)| known.as_str() == key.as_str()) {
                out.push((source, key.clone()));
            }
        };
        if let SlotRead::Key(key) = &self.primary {
            add(KeySource::Primary, key);
        }
        if let PendingRead::Key(pending) = &self.pending {
            add(KeySource::Pending, &pending.key);
        }
        for (alias, slot) in &self.legacy {
            if let SlotRead::Key(key) = slot {
                add(KeySource::LegacyStaging(alias.clone()), key);
            }
        }
        if let SlotRead::Key(key) = &self.recovery {
            add(KeySource::Recovery, key);
        }
        out
    }

    /// The first slot that could not be read, as `(slot name, error)`.
    fn first_failure(&self) -> Option<(String, KeyStoreError)> {
        if let SlotRead::Failed(error) = &self.primary {
            return Some(("primary".to_string(), error.clone()));
        }
        if let PendingRead::Failed(error) = &self.pending {
            return Some(("pending".to_string(), error.clone()));
        }
        for (alias, slot) in &self.legacy {
            if let SlotRead::Failed(error) = slot {
                return Some((format!("legacy staging {alias}"), error.clone()));
            }
        }
        if let SlotRead::Failed(error) = &self.recovery {
            return Some(("recovery".to_string(), error.clone()));
        }
        None
    }
}

/// The result of trying every candidate key against the file.
pub(super) struct Scan {
    pub opened: Option<(KeySource, Key)>,
    pub tried: Vec<KeySource>,
    pub db_error: Option<DbError>,
    pub failure: Option<(String, KeyStoreError)>,
    pub candidates: usize,
    pub slots: Slots,
}

pub(super) fn scan_candidates(
    cfg: &EncryptedStoreConfig,
    path: &Path,
    ks: &mut dyn ProtectedKeySession,
) -> Scan {
    let slots = Slots::read(cfg, ks);
    let candidates = slots.candidates();
    let count = candidates.len();
    let mut opened = None;
    let mut tried = Vec::new();
    let mut db_error = None;
    for (source, key) in candidates {
        if !key_is_usable(&key) {
            tried.push(source);
            continue;
        }
        match probe(
            &cfg.db,
            path,
            &key,
            IntegrityCheck::Off,
            OpenFlags::SQLITE_OPEN_READ_WRITE,
        ) {
            Probe::Opens => {
                opened = Some((source, key));
                break;
            }
            Probe::KeyRejected => tried.push(source),
            Probe::Other(error) => {
                db_error.get_or_insert(error);
            }
        }
    }
    let failure = slots.first_failure();
    Scan {
        opened,
        tried,
        db_error,
        failure,
        candidates: count,
        slots,
    }
}

impl Scan {
    /// Why nothing opened, as the error to return (the file is never touched).
    pub(super) fn into_error(self, path: &Path) -> LifecycleError {
        if let Some(error) = self.db_error {
            return db_unavailable(path, error);
        }
        if let Some((slot, error)) = self.failure {
            return LifecycleError::key_store(slot, error);
        }
        if self.candidates == 0 {
            return LifecycleError::KeyMissing;
        }
        LifecycleError::NoKeyOpens {
            path: path.to_path_buf(),
            tried: self.tried,
        }
    }
}

// ----- selection and establishing -----------------------------------------------------------

enum Selection {
    Ready {
        key: Key,
        source: KeySource,
        degraded: Option<String>,
    },
    /// Journal, leftovers or a primary that does not open: reconcile under the exclusive guard.
    Recover,
}

fn select(
    cfg: &EncryptedStoreConfig,
    accept_unclean: bool,
    ks: &mut dyn ProtectedKeySession,
) -> Result<Selection, LifecycleError> {
    let path = cfg.db.effective_path();
    let journal_present = journal_exists(&cfg.db.path);
    if journal_present && !accept_unclean {
        return Ok(Selection::Recover);
    }
    if !file_present(&path)? {
        if journal_present || !cfg.create_if_missing {
            return Err(LifecycleError::StoreMissing { path });
        }
        return match ks.read_primary() {
            Ok(Some(key)) => Ok(Selection::Ready {
                key,
                source: KeySource::Primary,
                degraded: None,
            }),
            Ok(None) => Err(LifecycleError::KeyMissing),
            Err(error) => Err(key_store_error("primary", error)),
        };
    }
    if is_plaintext_sqlite(&path) {
        return Err(LifecycleError::NotEncrypted { path });
    }
    if let Ok(Some(primary)) = ks.read_primary() {
        if key_is_usable(&primary) {
            match probe(
                &cfg.db,
                &path,
                &primary,
                IntegrityCheck::Off,
                OpenFlags::SQLITE_OPEN_READ_WRITE,
            ) {
                Probe::Opens => {
                    let pending = ks.read_pending();
                    if matches!(pending, Ok(None)) && !journal_present {
                        return Ok(Selection::Ready {
                            key: primary,
                            source: KeySource::Primary,
                            degraded: None,
                        });
                    }
                    if !accept_unclean {
                        return Ok(Selection::Recover);
                    }
                    let note = match pending {
                        Ok(None) => "a transition journal remains",
                        Ok(Some(_)) => "a pending key remains",
                        Err(_) => "the pending key could not be read",
                    };
                    return Ok(Selection::Ready {
                        key: primary,
                        source: KeySource::Primary,
                        degraded: Some(note.to_string()),
                    });
                }
                Probe::KeyRejected => {}
                Probe::Other(error) => return Err(db_unavailable(&path, error)),
            }
        }
    }
    if !accept_unclean {
        return Ok(Selection::Recover);
    }
    // Recovery already ran and could not clean everything up: use the first key that opens.
    let scan = scan_candidates(cfg, &path, ks);
    match scan.opened {
        Some((source, key)) => Ok(Selection::Ready {
            key,
            source,
            degraded: Some("key storage reconciliation is incomplete".to_string()),
        }),
        None => Err(scan.into_error(&path)),
    }
}

/// An open store: the key that opened it, its writer and its shared guard.
pub(super) struct Established {
    pub key: Key,
    pub source: KeySource,
    pub writer: Connection,
    pub guard: StoreLock,
    pub degraded: Option<String>,
}

/// Select the key, reconcile any unfinished transition, take the shared guard and open the writer.
///
/// A clean store (journal absent, primary key opens, no pending key) takes the fast path under the
/// shared guard only. Anything else drops the shared guard, reconciles under the exclusive guard
/// and tries again, at most three times.
pub(super) fn establish(
    cfg: &EncryptedStoreConfig,
    faults: &dyn Faults,
) -> Result<Established, LifecycleError> {
    let mut accept_unclean = false;
    for _ in 0..3 {
        let guard = acquire_shared(&cfg.db.path, &cfg.transition)?;
        let mut session = cfg
            .key_store
            .lock_rotation(cfg.transition.acquire_timeout)
            .map_err(|error| key_store_error("rotation lock", error))?;
        match select(cfg, accept_unclean, &mut *session)? {
            Selection::Ready {
                key,
                source,
                degraded,
            } => {
                let writer = cfg.keyed(&key).open_writer()?;
                drop(session);
                return Ok(Established {
                    key,
                    source,
                    writer,
                    guard,
                    degraded,
                });
            }
            Selection::Recover => {
                drop(session);
                drop(guard);
                let report = recover_with_guard(cfg, faults)?;
                if let RecoveryOutcome::Blocked(reason) = report.outcome {
                    return Err(reason.into_error(&cfg.db.effective_path()));
                }
                accept_unclean = true;
            }
        }
    }
    Err(LifecycleError::busy(
        "the store kept changing while it was being opened",
    ))
}

/// Explicit relocation settlement has already selected this root. Ordinary open must not
/// bypass its journal; this path verifies identity & canonical storage under fresh authority.
pub(super) fn establish_relocation(
    cfg: &EncryptedStoreConfig,
    expected: &RootIdentity,
    key: &Key,
) -> Result<Established, LifecycleError> {
    let guard = acquire_shared(&cfg.db.path, &cfg.transition)?;
    let mut session = cfg
        .key_store
        .lock_rotation(cfg.transition.acquire_timeout)
        .map_err(|error| key_store_error("rotation lock", error))?;
    let primary = session
        .read_primary()
        .map_err(|error| key_store_error("primary", error))?
        .ok_or(LifecycleError::KeyMissing)?;
    // An identity that cannot be read (after the bounded retry of a transient Windows refusal) is
    // the error, not "the root changed".
    if expected.file.is_none()
        || primary.as_str() != key.as_str()
        || RootIdentity::try_capture(&cfg.db.path)? != *expected
    {
        return Err(LifecycleError::RecoveryRequired {
            detail: "selected relocation root or canonical key changed".into(),
        });
    }
    let writer = cfg.keyed(&primary).open_existing_writer()?;
    if RootIdentity::try_capture(&cfg.db.path)? != *expected {
        return Err(LifecycleError::RecoveryRequired {
            detail: "selected relocation root changed while reopening".into(),
        });
    }
    drop(session);
    Ok(Established {
        key: primary,
        source: KeySource::Primary,
        writer,
        guard,
        degraded: None,
    })
}

// ----- the store ---------------------------------------------------------------------------

pub(super) enum Mode {
    Ready,
    Quiesced(&'static str),
    Blocked(String),
}

pub(super) struct Live {
    pub key: Key,
    pub source: KeySource,
    // Declared before `guard`: the writer closes before the shared guard is released.
    pub writer: Option<Connection>,
    pub guard: Option<StoreLock>,
    pub mode: Mode,
    pub degraded: Option<String>,
}

impl Live {
    pub(super) fn ready(&self) -> Result<(), LifecycleError> {
        match &self.mode {
            Mode::Ready => Ok(()),
            Mode::Quiesced(why) => Err(LifecycleError::busy(format!(
                "a {why} transition is in progress"
            ))),
            Mode::Blocked(detail) => Err(LifecycleError::RecoveryRequired {
                detail: detail.clone(),
            }),
        }
    }
}

pub(super) struct PoolSlot {
    capacity: usize,
    options: ReadPoolOptions,
    pool: Mutex<PoolState>,
}

struct PoolGeneration {
    pool: ReadPool,
    _guard: StoreLock,
}
struct PoolState {
    pool: Option<Arc<PoolGeneration>>,
    retired: Vec<Arc<PoolGeneration>>,
    revoked: bool,
}

struct PoolLease {
    slot: Arc<PoolSlot>,
    _generation: Arc<PoolGeneration>,
}

impl Drop for PoolLease {
    fn drop(&mut self) {
        // PooledReadConnection returns/closes its backend before releasing this lease.
        lock_recover(&self.slot.pool)
            .retired
            .retain(|old| old.pool.total_len() != 0);
    }
}

impl PoolSlot {
    fn close(&self, timeout: Duration) -> Result<(), LifecycleError> {
        let started = Instant::now();
        let generations = {
            let mut state = lock_recover(&self.pool);
            if let Some(pool) = state.pool.take() {
                state.retired.push(pool);
            }
            state.retired.clone()
        };
        for generation in generations {
            generation
                .pool
                .close_and_wait(timeout.saturating_sub(started.elapsed()))
                .map_err(|error| {
                    LifecycleError::busy(format!(
                        "a read pool still has readers checked out: {error}"
                    ))
                })?;
            lock_recover(&self.pool)
                .retired
                .retain(|old| !Arc::ptr_eq(old, &generation));
        }
        Ok(())
    }

    fn reopen(&self, cfg: &EncryptedStoreConfig, db: &DbOptions) -> Result<(), LifecycleError> {
        let mut state = lock_recover(&self.pool);
        if state.revoked {
            return Ok(());
        }
        let guard = acquire_shared(&db.path, &cfg.transition)?;
        let pool = db.open_read_pool_with(self.capacity, self.options)?;
        if let Some(previous) = state.pool.take() {
            previous.pool.close();
            if previous.pool.total_len() != 0 {
                state.retired.push(previous);
            }
        }
        state.pool = Some(Arc::new(PoolGeneration {
            pool,
            _guard: guard,
        }));
        Ok(())
    }
}

/// A read pool the store can drain and reopen with the current key around a transition. While a
/// transition is running `get` returns `Busy` instead of waiting.
#[derive(Clone)]
pub struct ManagedPool {
    slot: Arc<PoolSlot>,
}

impl Drop for EncryptedStore {
    fn drop(&mut self) {
        for slot in self.live_pools() {
            ManagedPool { slot }.close();
        }
    }
}

impl ManagedPool {
    fn current(&self) -> Result<Arc<PoolGeneration>, LifecycleError> {
        let state = lock_recover(&self.slot.pool);
        if state.revoked {
            return Err(LifecycleError::busy(
                "the managed pool was permanently closed",
            ));
        }
        state.pool.clone().ok_or_else(|| {
            LifecycleError::busy("the store is in a transition; no reader is available")
        })
    }

    pub fn get(&self) -> Result<PooledReadConnection, LifecycleError> {
        let generation = self.current()?;
        let reader = generation.pool.get().map_err(closed_to_busy)?;
        Ok(reader.with_lifecycle_lease(Arc::new(PoolLease {
            slot: self.slot.clone(),
            _generation: generation,
        })))
    }

    pub fn get_timeout(
        &self,
        timeout: Duration,
    ) -> Result<Option<PooledReadConnection>, LifecycleError> {
        let generation = self.current()?;
        let reader = generation
            .pool
            .get_timeout(timeout)
            .map_err(closed_to_busy)?;
        Ok(reader.map(|reader| {
            reader.with_lifecycle_lease(Arc::new(PoolLease {
                slot: self.slot.clone(),
                _generation: generation,
            }))
        }))
    }

    pub fn capacity(&self) -> usize {
        self.slot.capacity
    }

    /// Permanently revoke this pool & every clone. Idle readers close now; checked-out
    /// readers close on return. Lifecycle resume never reopens a revoked pool.
    pub fn close(&self) {
        let mut state = lock_recover(&self.slot.pool);
        state.revoked = true;
        if let Some(pool) = state.pool.take() {
            state.retired.push(pool);
        }
        for generation in &state.retired {
            generation.pool.close();
        }
        state.retired.retain(|old| old.pool.total_len() != 0);
    }

    /// Permanently revoke every clone & wait up to `timeout` for checked-out readers.
    /// Timeout retains outstanding generations; retry after reader return to finish draining.
    pub fn close_and_wait(&self, timeout: Duration) -> Result<(), LifecycleError> {
        self.close();
        self.slot.close(timeout)
    }
}

fn closed_to_busy(error: DbError) -> LifecycleError {
    match error {
        DbError::ReadPoolClosed => {
            LifecycleError::busy("the store is in a transition; the read pool was closed")
        }
        other => LifecycleError::Db(other),
    }
}

/// An encrypted database with its key selection, writer and read pools under one lifecycle.
///
/// Every connection a transition has to drain must come from this store (`with_writer`,
/// `read_pool`): the store closes them, runs the transition, then reopens them with the key that
/// protected storage reads back. A connection the app opens on its own is not drained; the guard
/// still excludes other cooperating processes, and Windows will refuse a rename under an open one.
pub struct EncryptedStore {
    pub(super) cfg: Mutex<EncryptedStoreConfig>,
    pub(super) live: Mutex<Live>,
    pub(super) pools: Mutex<Vec<Arc<PoolSlot>>>,
    /// Why the paths the most recent relocation or relocation recovery left behind were kept.
    pub(super) relocation_cleanup_errors: Mutex<Vec<(PathBuf, String)>>,
}

impl EncryptedStore {
    pub(super) fn from_relocation(cfg: EncryptedStoreConfig, established: Established) -> Self {
        let Established {
            key,
            source,
            writer,
            guard,
            degraded,
        } = established;
        Self {
            cfg: Mutex::new(cfg),
            live: Mutex::new(Live {
                key,
                source,
                writer: Some(writer),
                guard: Some(guard),
                mode: Mode::Ready,
                degraded,
            }),
            pools: Mutex::new(Vec::new()),
            relocation_cleanup_errors: Mutex::new(Vec::new()),
        }
    }
    /// Open the existing encrypted database.
    ///
    /// Under the shared guard: if a transition journal remains, a pending key is staged, or the
    /// primary key does not open the file, the store is first reconciled under the exclusive guard
    /// (see `recover_rotation`): a key that opens the file is made canonical, leftovers are cleaned
    /// up, nothing is deleted while it is the only key that opens the file.
    ///
    /// Failures are classified ([`LifecycleError`]): a missing file, a plaintext file, a missing
    /// key, denied or unavailable key storage, a key that does not open the file, an unreadable
    /// file. None of them creates, quarantines or replaces a database.
    pub fn open(config: EncryptedStoreConfig) -> Result<Self, LifecycleError> {
        let config = config.checked()?;
        let established = establish(&config, &NoFaults)?;
        let Established {
            key,
            source,
            writer,
            guard,
            degraded,
        } = established;
        Ok(Self {
            cfg: Mutex::new(config),
            live: Mutex::new(Live {
                key,
                source,
                writer: Some(writer),
                guard: Some(guard),
                mode: Mode::Ready,
                degraded,
            }),
            pools: Mutex::new(Vec::new()),
            relocation_cleanup_errors: Mutex::new(Vec::new()),
        })
    }

    pub(super) fn config(&self) -> EncryptedStoreConfig {
        lock_recover(&self.cfg).clone()
    }

    /// The database path the store currently serves (it changes after a successful relocation).
    pub fn db_path(&self) -> PathBuf {
        lock_recover(&self.cfg).db.path.clone()
    }

    /// Why each path the store's most recent relocation (`relocate`, a session's `settle`) or
    /// relocation recovery left behind was kept, in the same order as the report's
    /// `cleanup_retained`: the last error of the removal that was given up (it carries the OS
    /// error text, for example `Access is denied. (os error 5)`), or the identity check that
    /// stopped it. Empty when nothing was retained, and at the start of every `relocate`.
    pub fn relocation_cleanup_errors(&self) -> Vec<(PathBuf, String)> {
        lock_recover(&self.relocation_cleanup_errors).clone()
    }

    pub(super) fn note_relocation_cleanup_errors(&self, errors: Vec<(PathBuf, String)>) {
        *lock_recover(&self.relocation_cleanup_errors) = errors;
    }

    /// Where the key that opened the store came from.
    pub fn key_source(&self) -> KeySource {
        lock_recover(&self.live).source.clone()
    }

    /// `Some(reason)` while key storage is open and usable but not fully reconciled (for example a
    /// pending key that could not be discarded). Call `recover_rotation` to finish.
    pub fn degraded(&self) -> Option<String> {
        lock_recover(&self.live).degraded.clone()
    }

    /// True while ordinary use is blocked until `recover_rotation` resolves an unknown state.
    pub fn is_blocked(&self) -> bool {
        matches!(lock_recover(&self.live).mode, Mode::Blocked(_))
    }

    /// Run `f` on the store's writer connection. Returns `Busy` during a transition and
    /// `RecoveryRequired` while an unknown state blocks writes.
    pub fn with_writer<R>(
        &self,
        f: impl FnOnce(&mut Connection) -> R,
    ) -> Result<R, LifecycleError> {
        let mut live = lock_recover(&self.live);
        live.ready()?;
        match live.writer.as_mut() {
            Some(conn) => Ok(f(conn)),
            None => Err(LifecycleError::busy("the writer is not open")),
        }
    }

    /// A read pool the store drains and reopens around transitions.
    pub fn read_pool(
        &self,
        capacity: usize,
        options: ReadPoolOptions,
    ) -> Result<ManagedPool, LifecycleError> {
        self.read_pool_with_faults(capacity, options, &NoFaults)
    }

    /// Read-pool admission with the deterministic transition-interleaving test seam.
    #[doc(hidden)]
    pub fn read_pool_with_faults(
        &self,
        capacity: usize,
        options: ReadPoolOptions,
        faults: &dyn Faults,
    ) -> Result<ManagedPool, LifecycleError> {
        let cfg = self.config();
        // Admission and registry insertion are one critical section with drain's mode flip.
        // Once drain can quiesce, every admitted connection is already in its pool registry.
        let live = lock_recover(&self.live);
        live.ready()?;
        let db = cfg.keyed(&live.key);
        let guard = acquire_shared(&db.path, &cfg.transition)?;
        let pool = db.open_read_pool_with(capacity, options)?;
        faults.before_pool_registration();
        let slot = Arc::new(PoolSlot {
            capacity,
            options,
            pool: Mutex::new(PoolState {
                pool: Some(Arc::new(PoolGeneration {
                    pool,
                    _guard: guard,
                })),
                retired: Vec::new(),
                revoked: false,
            }),
        });
        lock_recover(&self.pools).push(slot.clone());
        drop(live);
        Ok(ManagedPool { slot })
    }

    fn live_pools(&self) -> Vec<Arc<PoolSlot>> {
        let mut pools = lock_recover(&self.pools);
        // Checked-out readers lease the slot, so dropping public handles cannot hide them.
        pools.retain(|slot| Arc::strong_count(slot) > 1);
        pools.clone()
    }

    /// `(key, source, degraded)` of a ready store.
    pub(super) fn snapshot(&self) -> Result<(Key, KeySource, Option<String>), LifecycleError> {
        let live = lock_recover(&self.live);
        live.ready()?;
        Ok((live.key.clone(), live.source.clone(), live.degraded.clone()))
    }

    pub(super) fn block(&self, detail: String) {
        let mut live = lock_recover(&self.live);
        live.mode = Mode::Blocked(detail);
        live.writer = None;
        live.guard = None;
    }

    /// Stop the host, wait (bounded) for an in-flight writer, close the writer and every managed
    /// pool, and release this store's own shared guard so the exclusive guard can be taken.
    ///
    /// On any failure the store is resumed before the error is returned, so a `Busy` from here
    /// leaves the writer and pools open again with nothing changed. `allow_blocked` lets recovery
    /// start from a blocked store.
    pub(super) fn drain(
        &self,
        kind: TransitionKind,
        allow_blocked: bool,
    ) -> Result<(), LifecycleError> {
        let cfg = self.config();
        let deadline = Instant::now() + cfg.drain_timeout;
        let mut live = loop {
            match self.live.try_lock() {
                Ok(guard) => break guard,
                Err(TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return Err(LifecycleError::busy("a writer is still in use"));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        };
        match &live.mode {
            Mode::Ready => {}
            Mode::Quiesced(why) => {
                return Err(LifecycleError::busy(format!(
                    "a {why} transition is already in progress"
                )))
            }
            Mode::Blocked(detail) => {
                if !allow_blocked {
                    return Err(LifecycleError::RecoveryRequired {
                        detail: detail.clone(),
                    });
                }
            }
        }
        live.mode = Mode::Quiesced(kind.as_str());
        let writer = live.writer.take();
        let guard = live.guard.take();
        drop(live);
        // The file closes before anything else is released.
        drop(writer);
        // The host stops after the mode flips, so a worker that calls `with_writer` meanwhile gets
        // `Busy` at once instead of blocking on the lock the host's stop is waiting for.
        if let Some(host) = &cfg.host {
            if let Err(reason) = host.stop(kind) {
                drop(guard);
                let _ = self.resume();
                return Err(LifecycleError::busy(format!(
                    "the host did not stop its workers: {reason}"
                )));
            }
        }
        let mut result = Ok(());
        for slot in self.live_pools() {
            if let Err(error) = slot.close(cfg.drain_timeout) {
                result = Err(error);
                break;
            }
        }
        drop(guard);
        if let Err(error) = result {
            let _ = self.resume();
            return Err(error);
        }
        Ok(())
    }

    /// Reopen the writer and pools after a transition, from what key storage reads back. A failure
    /// leaves the store blocked with the reason.
    pub(super) fn resume(&self) -> Result<(), LifecycleError> {
        let cfg = self.config();
        match establish(&cfg, &NoFaults) {
            Ok(established) => {
                self.install(established, &cfg);
                Ok(())
            }
            Err(error) => {
                self.block(format!(
                    "the store could not be reopened after a transition: {error}"
                ));
                Err(error)
            }
        }
    }

    pub(super) fn install(&self, established: Established, cfg: &EncryptedStoreConfig) {
        let Established {
            key,
            source,
            writer,
            guard,
            degraded,
        } = established;
        let db = cfg.keyed(&key);
        {
            let mut live = lock_recover(&self.live);
            live.key = key;
            live.source = source;
            live.writer = Some(writer);
            live.guard = Some(guard);
            live.degraded = degraded;
            live.mode = Mode::Ready;
        }
        let mut pool_error = None;
        for slot in self.live_pools() {
            if let Err(error) = slot.reopen(cfg, &db) {
                pool_error.get_or_insert(error);
            }
        }
        if let Some(error) = pool_error {
            lock_recover(&self.live).degraded =
                Some(format!("a read pool could not be reopened: {error}"));
        }
        if let Some(host) = &cfg.host {
            host.resume();
        }
    }

    /// The current key of a ready store.
    pub(super) fn current_key(&self) -> Result<Key, LifecycleError> {
        let live = lock_recover(&self.live);
        live.ready()?;
        Ok(live.key.clone())
    }

    /// Online backup through the store's writer, encrypted with the store key.
    ///
    /// SQLite's backup API reads a consistent snapshot while the writer stays open, so it needs no
    /// drain; it does hold the writer slot for the call. Before copying, the primary key read back
    /// from protected storage must equal the key that opened the store: a backup made under a key
    /// that storage does not hold could not be restored.
    pub fn backup_to(&self, target: &Path) -> Result<(), LifecycleError> {
        let cfg = self.config();
        let live = lock_recover(&self.live);
        live.ready()?;
        // `live.guard` already holds the transition guard; acquire slots before copying.
        let mut session = cfg
            .key_store
            .lock_rotation(cfg.transition.acquire_timeout)
            .map_err(|error| key_store_error("rotation lock", error))?;
        match session.read_primary() {
            Ok(Some(primary)) if primary.as_str() == live.key.as_str() => {}
            Ok(_) => {
                return Err(LifecycleError::KeyStoreMismatch {
                    detail: "the primary key in protected storage differs from the open key".into(),
                })
            }
            Err(error) => return Err(key_store_error("primary", error)),
        }
        let db = cfg.keyed(&live.key);
        let Some(conn) = live.writer.as_ref() else {
            return Err(LifecycleError::busy("the writer is not open"));
        };
        crate::backup::online_backup_sized(&db, conn, target, None)?;
        Ok(())
    }

    /// Restore `replacement` over the live database through the existing journaled
    /// [`restore_database_locked`], under the transition guard: the host stops, the writer and
    /// pools close, the guard and the key store's rotation lock are taken, the replacement is
    /// checked against the store key (a replacement under another key is `WrongDestinationKey` and
    /// the live store is untouched), restored, verified with the store key and the config's
    /// validation callback, and the store is reopened. A failed verification rolls the restore back.
    /// The previous store is kept at the receipt's path; nothing is deleted.
    pub fn restore(
        &self,
        replacement: &Path,
        restore: &RestoreOptions<'_>,
    ) -> Result<RestoreReceipt, LifecycleError> {
        let cfg = self.config();
        let key = self.current_key()?;
        self.drain(TransitionKind::Restore, false)?;
        let outcome = self.restore_guarded(&cfg, &key, replacement, restore);
        // A failed resume leaves the store blocked with the reason; the restore's own result stands.
        let _ = self.resume();
        outcome
    }

    fn restore_guarded(
        &self,
        cfg: &EncryptedStoreConfig,
        key: &Key,
        replacement: &Path,
        restore: &RestoreOptions<'_>,
    ) -> Result<RestoreReceipt, LifecycleError> {
        let transition = StoreTransition::begin(
            TransitionKind::Restore,
            &[cfg.db.path.as_path()],
            &cfg.transition,
        )?;
        // A selected relocation remains replay authority until its coordinator settles.
        // Check under exclusive ownership; restore must never replace another transition's
        // journal, even after relocation released its guard to reopen ordinary writers.
        if super::transition::read_journal(&cfg.db.path)?.is_some() {
            return Err(LifecycleError::RecoveryRequired {
                detail: "an unfinished transition must settle before restore".into(),
            });
        }
        let mut session = cfg
            .key_store
            .lock_rotation(cfg.transition.acquire_timeout)
            .map_err(|error| key_store_error("rotation lock", error))?;
        match session.read_primary() {
            Ok(Some(primary)) if primary.as_str() == key.as_str() => {}
            Ok(_) => {
                return Err(LifecycleError::KeyStoreMismatch {
                    detail: "the primary key in protected storage differs from the open key".into(),
                })
            }
            Err(error) => return Err(key_store_error("primary", error)),
        }
        match probe(
            &cfg.db,
            replacement,
            key,
            IntegrityCheck::Off,
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            Probe::Opens => {}
            Probe::KeyRejected => {
                return Err(LifecycleError::WrongDestinationKey {
                    path: replacement.to_path_buf(),
                    detail: "the replacement does not open with the store's key".into(),
                })
            }
            Probe::Other(error) => return Err(db_unavailable(replacement, error)),
        }
        let journal = super::transition::Journal {
            kind: TransitionKind::Restore,
            state: JournalState::Active,
            phase: "restoring".to_string(),
            retain_previous: false,
            pending: None,
            id: next_id(),
            // Recorded for diagnosis only: no check reads a restore's root identities back (only
            // relocation compares them), so the infallible snapshot is enough here.
            old_root: RootIdentity::capture(&cfg.db.path),
            new_root: Some(RootIdentity::capture(replacement)),
            relocation: None,
        };
        transition.write_journal(&journal)?;
        let db = cfg.keyed(key);
        let verify = |placed: &DbOptions| -> Result<(), String> {
            match placed.open_existing_writer() {
                Ok(conn) => drop(conn),
                Err(error) => {
                    return Err(format!(
                        "the restored store does not open with the store key: {error}"
                    ))
                }
            }
            if let Some(user) = restore.verify {
                user(placed)?;
            }
            if let Some(validate) = &cfg.validate {
                validate(placed)?;
            }
            Ok(())
        };
        let verify_ref: &dyn Fn(&DbOptions) -> Result<(), String> = &verify;
        let wrapped = RestoreOptions {
            previous_infix: restore.previous_infix,
            failed_infix: restore.failed_infix,
            verify: Some(verify_ref),
        };
        let result = restore_database_locked(&db, replacement, &wrapped);
        // The restore's own journal decides what recovery does if it did not finish; this one is
        // removed once the restore either completed or rolled back completely.
        let finished = match &result {
            Ok(_) => true,
            Err(DbError::RestoreFailed { rolled_back, .. }) => *rolled_back,
            Err(_) => true,
        };
        if finished {
            let _ = transition.clear_journal();
        }
        drop(session);
        transition.settle();
        result.map_err(LifecycleError::from)
    }
}
