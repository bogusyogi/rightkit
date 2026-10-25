//! The encrypted-store lifecycle (PK03-3..6): key selection, rotation and recovery, interruption
//! after every phase, the guard, backup/restore quiescence and relocation.
//!
//! Needs `--features sqlcipher,encrypted-lifecycle` (or `sqlcipher-system,encrypted-lifecycle`).
//! Key storage is an in-memory `ProtectedKeyStore` with injectable failures.
#![cfg(all(feature = "encrypted-lifecycle", feature = "cipher"))]

use rightkit_db::encrypted_lifecycle::{
    EncryptedStore, EncryptedStoreConfig, Faults, Key, KeySource, KeyStoreError, KeyStoreErrorKind,
    Killed, LifecycleError, PendingKey, PendingRevision, Phase, ProtectedKeySession,
    ProtectedKeyStore, RecoveryOutcome, RecoveryStep, RekeyFault, RekeyStep, RelocationPlan,
    RelocationReport, RotationReport, RotationRequest, StoreTransition, TransitionKind,
    TransitionOptions,
};
use rightkit_db::{DbOptions, ReadPoolOptions, RestoreOptions, StoreLock, Zeroizing};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const OLD: &str = "b2xkLWtleQ==";
const NEW: &str = "bmV3LWtleQ==";
const OTHER: &str = "b3RoZXIta2V5";
const LEGACY: &str = "bGVnYWN5LWtleQ==";
const LEGACY_ACCOUNT: &str = "SQLCipher/DBKey.staging";

// ----- an in-memory protected key store with injectable failures ------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Promote {
    #[default]
    Normal,
    /// The call fails and the primary key is not written.
    FailNoWrite,
    /// The primary key is written, the pending marker is left, and the call returns an error (the
    /// credentials adapter's primary-written, cleanup-failed shape).
    WriteThenFail,
    Unsettled,
}

#[derive(Default)]
struct Inner {
    primary: Option<String>,
    pending: Option<(String, String)>,
    recovery: Option<String>,
    legacy: HashMap<String, String>,
    counter: u64,
    fail_read_primary: Option<KeyStoreErrorKind>,
    fail_read_legacy: bool,
    promote: Promote,
    fail_discard: bool,
    fail_delete_legacy: bool,
    late_promotion: Option<(
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::SyncSender<()>,
    )>,
}

#[derive(Clone, Default)]
struct FakeKeys(
    Arc<Mutex<Inner>>,
    Arc<std::sync::atomic::AtomicBool>,
    Arc<std::sync::atomic::AtomicBool>,
);

impl FakeKeys {
    fn with_primary(key: &str) -> Self {
        let keys = Self::default();
        keys.0.lock().unwrap().primary = Some(key.to_string());
        keys
    }

    fn primary(&self) -> Option<String> {
        self.0.lock().unwrap().primary.clone()
    }

    fn pending(&self) -> Option<String> {
        self.0.lock().unwrap().pending.clone().map(|(key, _)| key)
    }

    fn recovery(&self) -> Option<String> {
        self.0.lock().unwrap().recovery.clone()
    }

    fn legacy(&self, alias: &str) -> Option<String> {
        self.0.lock().unwrap().legacy.get(alias).cloned()
    }

    fn set_pending(&self, key: &str) {
        self.0.lock().unwrap().pending = Some((key.to_string(), "rev-test".to_string()));
    }

    fn set_recovery(&self, key: &str) {
        self.0.lock().unwrap().recovery = Some(key.to_string());
    }

    fn set_legacy(&self, alias: &str, key: &str) {
        self.0
            .lock()
            .unwrap()
            .legacy
            .insert(alias.to_string(), key.to_string());
    }

    fn fail_read_primary(&self, kind: Option<KeyStoreErrorKind>) {
        self.0.lock().unwrap().fail_read_primary = kind;
    }

    fn fail_read_legacy(&self, on: bool) {
        self.0.lock().unwrap().fail_read_legacy = on;
    }

    fn promote(&self, mode: Promote) {
        self.0.lock().unwrap().promote = mode;
    }

    fn fail_discard(&self, on: bool) {
        self.0.lock().unwrap().fail_discard = on;
    }

    fn fail_delete_legacy(&self, on: bool) {
        self.0.lock().unwrap().fail_delete_legacy = on;
    }

    /// Every stored key, for "at least one viable key is retained" checks.
    fn stored_keys(&self) -> Vec<String> {
        let inner = self.0.lock().unwrap();
        let mut keys = Vec::new();
        keys.extend(inner.primary.clone());
        keys.extend(inner.pending.clone().map(|(key, _)| key));
        keys.extend(inner.recovery.clone());
        keys.extend(inner.legacy.values().cloned());
        keys
    }
}

impl ProtectedKeyStore for FakeKeys {
    fn lock_rotation(
        &self,
        timeout: Duration,
    ) -> Result<Box<dyn ProtectedKeySession + Send + 'static>, KeyStoreError> {
        use std::sync::atomic::Ordering;
        let started = std::time::Instant::now();
        loop {
            if !self.2.load(Ordering::Acquire)
                && self
                    .1
                    .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            {
                return Ok(Box::new(FakeSession { keys: self.clone() }));
            }
            if started.elapsed() >= timeout {
                return Err(KeyStoreError::unavailable("rotation session busy"));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

struct FakeSession {
    keys: FakeKeys,
}

impl Drop for FakeSession {
    fn drop(&mut self) {
        self.keys
            .1
            .store(false, std::sync::atomic::Ordering::Release);
    }
}

impl ProtectedKeySession for FakeSession {
    fn read_primary(&mut self) -> Result<Option<Key>, KeyStoreError> {
        let inner = self.keys.0.lock().unwrap();
        if let Some(kind) = inner.fail_read_primary {
            return Err(KeyStoreError::new(kind, "primary slot unavailable"));
        }
        Ok(inner.primary.clone().map(Zeroizing::new))
    }

    fn read_pending(&mut self) -> Result<Option<PendingKey>, KeyStoreError> {
        let inner = self.keys.0.lock().unwrap();
        Ok(inner.pending.clone().map(|(key, revision)| PendingKey {
            key: Zeroizing::new(key),
            revision: PendingRevision(revision),
        }))
    }

    fn write_pending(&mut self, key: &Key) -> Result<PendingRevision, KeyStoreError> {
        let mut inner = self.keys.0.lock().unwrap();
        inner.counter += 1;
        let revision = format!("rev-{}", inner.counter);
        inner.pending = Some((key.as_str().to_string(), revision.clone()));
        Ok(PendingRevision(revision))
    }

    fn promote_pending(&mut self, revision: &PendingRevision) -> Result<(), KeyStoreError> {
        let mut inner = self.keys.0.lock().unwrap();
        let mode = inner.promote;
        if mode == Promote::FailNoWrite {
            return Err(KeyStoreError::other("promotion refused"));
        }
        let Some((key, staged)) = inner.pending.clone() else {
            return Err(KeyStoreError::other("no pending key"));
        };
        if staged != revision.0 {
            return Err(KeyStoreError::other("stale pending revision"));
        }
        if mode == Promote::Unsettled {
            let (release, done) = inner.late_promotion.take().unwrap();
            self.keys
                .2
                .store(true, std::sync::atomic::Ordering::Release);
            let keys = self.keys.clone();
            std::thread::spawn(move || {
                release.recv_timeout(Duration::from_secs(5)).unwrap();
                let mut inner = keys.0.lock().unwrap();
                inner.primary = Some(key);
                inner.pending = None;
                keys.2.store(false, std::sync::atomic::Ordering::Release);
                drop(inner);
                done.send(()).unwrap();
            });
            return Err(KeyStoreError::new(
                KeyStoreErrorKind::Unsettled,
                "promotion worker still active",
            ));
        }
        inner.primary = Some(key);
        if mode == Promote::WriteThenFail {
            return Err(KeyStoreError::other(
                "cleanup failed after the primary key was written",
            ));
        }
        inner.pending = None;
        Ok(())
    }

    fn discard_pending(&mut self, revision: &PendingRevision) -> Result<(), KeyStoreError> {
        let mut inner = self.keys.0.lock().unwrap();
        if inner.fail_discard {
            return Err(KeyStoreError::other("discard refused"));
        }
        if inner
            .pending
            .as_ref()
            .is_some_and(|(_, staged)| *staged == revision.0)
        {
            inner.pending = None;
        }
        Ok(())
    }

    fn read_legacy_staging(&mut self, alias: &str) -> Result<Option<Key>, KeyStoreError> {
        let inner = self.keys.0.lock().unwrap();
        if inner.fail_read_legacy {
            return Err(KeyStoreError::denied("legacy staging access denied"));
        }
        Ok(inner.legacy.get(alias).cloned().map(Zeroizing::new))
    }

    fn delete_legacy_staging(&mut self, alias: &str) -> Result<(), KeyStoreError> {
        let mut inner = self.keys.0.lock().unwrap();
        if inner.fail_delete_legacy {
            return Err(KeyStoreError::other("delete refused"));
        }
        inner.legacy.remove(alias);
        Ok(())
    }

    fn read_recovery(&mut self) -> Result<Option<Key>, KeyStoreError> {
        Ok(self
            .keys
            .0
            .lock()
            .unwrap()
            .recovery
            .clone()
            .map(Zeroizing::new))
    }

    fn write_recovery(&mut self, key: &Key) -> Result<(), KeyStoreError> {
        self.keys.0.lock().unwrap().recovery = Some(key.as_str().to_string());
        Ok(())
    }

    fn clear_recovery(&mut self) -> Result<(), KeyStoreError> {
        self.keys.0.lock().unwrap().recovery = None;
        Ok(())
    }
}

// ----- the fault seam -------------------------------------------------------------------------

#[derive(Default)]
struct TestFaults {
    die: Option<Phase>,
    forward: Option<RekeyFault>,
    rollback: Option<RekeyFault>,
    hide_database_to: Option<PathBuf>,
}

impl Faults for TestFaults {
    fn die_after(&self, phase: Phase) -> bool {
        self.die == Some(phase)
    }

    fn rekey(&self, step: RekeyStep) -> RekeyFault {
        match step {
            RekeyStep::Forward => self.forward.unwrap_or(RekeyFault::None),
            RekeyStep::Rollback => self.rollback.unwrap_or(RekeyFault::None),
        }
    }

    fn before_readback(&self, db: &Path) {
        if let Some(to) = &self.hide_database_to {
            std::fs::rename(db, to).unwrap();
        }
    }
}

// ----- helpers --------------------------------------------------------------------------------

fn make_db(dir: &Path, name: &str, key: &str) -> PathBuf {
    let path = dir.join(name);
    let conn = DbOptions::new(&path)
        .with_passphrase(key)
        .open_writer()
        .unwrap();
    conn.execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1);")
        .unwrap();
    drop(conn);
    path
}

fn opens_with(path: &Path, key: &str) -> bool {
    match DbOptions::new(path)
        .with_passphrase(key)
        .open_existing_writer()
    {
        Ok(conn) => conn
            .query_row("SELECT count(*) FROM t", [], |row| row.get::<_, i64>(0))
            .is_ok(),
        Err(_) => false,
    }
}

fn any_stored_key_opens(path: &Path, keys: &FakeKeys) -> bool {
    keys.stored_keys().iter().any(|key| opens_with(path, key))
}

fn transition_options() -> TransitionOptions {
    TransitionOptions {
        acquire_timeout: Duration::from_millis(300),
        poll: Duration::from_millis(10),
        label: "test".to_string(),
    }
}

fn config(path: &Path, keys: &FakeKeys) -> EncryptedStoreConfig {
    EncryptedStoreConfig::new(DbOptions::new(path), Arc::new(keys.clone()))
        .with_transition(transition_options())
        .with_drain_timeout(Duration::from_millis(300))
}

fn open_err(config: EncryptedStoreConfig) -> LifecycleError {
    match EncryptedStore::open(config) {
        Ok(_) => panic!("the open was expected to fail"),
        Err(error) => error,
    }
}

fn count(store: &EncryptedStore) -> i64 {
    store
        .with_writer(|conn| {
            conn.query_row("SELECT count(*) FROM t", [], |row| row.get::<_, i64>(0))
                .unwrap()
        })
        .unwrap()
}

fn journal_file(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push(".transition-journal");
    PathBuf::from(name)
}

fn journal_exists(db: &Path) -> bool {
    std::fs::symlink_metadata(journal_file(db)).is_ok()
}

fn rotate(store: &EncryptedStore, faults: &TestFaults) -> RotationReport {
    match store.rotate_key_with_faults(RotationRequest::new(NEW), faults) {
        Ok(report) => report,
        Err(killed) => panic!("unexpected simulated death {killed:?}"),
    }
}

fn dir_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// ----- open and key selection (PK03-3) --------------------------------------------------------

#[test]
fn open_uses_the_primary_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    assert_eq!(store.key_source(), KeySource::Primary);
    assert!(store.degraded().is_none());
    assert_eq!(count(&store), 1);
    assert!(!journal_exists(&path));
}

#[test]
fn a_missing_store_is_never_created_unless_asked_and_keyed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.db");
    let keys = FakeKeys::with_primary(OLD);
    assert!(matches!(
        open_err(config(&path, &keys)),
        LifecycleError::StoreMissing { .. }
    ));
    assert!(!path.exists(), "no empty database was created");

    let no_key = FakeKeys::default();
    assert!(matches!(
        open_err(config(&path, &no_key).with_create_if_missing(true)),
        LifecycleError::KeyMissing
    ));
    assert!(!path.exists(), "no key, no database");

    let store = EncryptedStore::open(config(&path, &keys).with_create_if_missing(true)).unwrap();
    store
        .with_writer(|conn| {
            conn.execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1);")
        })
        .unwrap()
        .unwrap();
    drop(store);
    assert!(opens_with(&path, OLD));
}

#[test]
fn keys_that_do_not_open_the_file_never_replace_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let before = std::fs::read(&path).unwrap();
    let keys = FakeKeys::with_primary(OTHER);
    keys.set_pending(NEW);
    let error = open_err(config(&path, &keys));
    assert!(
        matches!(error, LifecycleError::NoKeyOpens { .. }),
        "got {error:?}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        before,
        "the file is untouched"
    );
    assert!(
        dir_names(dir.path())
            .iter()
            .all(|name| !name.contains("corrupt")),
        "nothing was quarantined"
    );
    assert!(opens_with(&path, OLD));
    assert_eq!(
        keys.primary().as_deref(),
        Some(OTHER),
        "no key slot was changed"
    );
    assert_eq!(keys.pending().as_deref(), Some(NEW));
}

#[test]
fn a_plaintext_file_is_classified_and_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plain.db");
    let conn = rightkit_db::rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE t(x INTEGER);").unwrap();
    drop(conn);
    let before = std::fs::read(&path).unwrap();
    let keys = FakeKeys::with_primary(OLD);
    assert!(matches!(
        open_err(config(&path, &keys)),
        LifecycleError::NotEncrypted { .. }
    ));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn primary_unavailable_is_classified_and_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let before = std::fs::read(&path).unwrap();
    let keys = FakeKeys::with_primary(OLD);
    keys.fail_read_primary(Some(KeyStoreErrorKind::Unavailable));
    match open_err(config(&path, &keys)) {
        LifecycleError::KeyStore { slot, source } => {
            assert_eq!(slot, "primary");
            assert_eq!(source.kind, KeyStoreErrorKind::Unavailable);
        }
        other => panic!("expected a classified key store failure, got {other:?}"),
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    keys.fail_read_primary(None);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    assert_eq!(count(&store), 1);
}

#[test]
fn primary_unavailable_but_the_pending_key_opens_the_file() {
    let dir = tempfile::tempdir().unwrap();
    // The database was rekeyed to NEW but the process stopped before promotion.
    let path = make_db(dir.path(), "app.db", NEW);
    let keys = FakeKeys::with_primary(OLD);
    keys.set_pending(NEW);
    keys.fail_read_primary(Some(KeyStoreErrorKind::Unavailable));
    // Promotion is refused too, so the pending slot is the only readable key that opens the file.
    keys.promote(Promote::FailNoWrite);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    assert_eq!(store.key_source(), KeySource::Pending);
    assert!(
        store.degraded().is_some(),
        "the primary slot could not be read back, so key storage is not canonical yet"
    );
    assert_eq!(count(&store), 1);
    assert!(
        keys.pending().is_some(),
        "the pending key is retained while it is the only readable key that opens the file"
    );

    keys.fail_read_primary(None);
    keys.promote(Promote::Normal);
    let report = store.recover_rotation();
    assert!(!report.is_blocked(), "{report:?}");
    assert_eq!(keys.primary().as_deref(), Some(NEW));
    assert!(keys.pending().is_none());
    assert!(store.degraded().is_none());
    assert_eq!(store.key_source(), KeySource::Primary);
    assert_eq!(count(&store), 1);
}

#[test]
fn an_unreadable_legacy_staging_account_is_a_key_store_failure_not_a_missing_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", LEGACY);
    let keys = FakeKeys::default();
    keys.set_legacy(LEGACY_ACCOUNT, LEGACY);
    keys.fail_read_legacy(true);
    let cfg = config(&path, &keys).with_legacy_staging_account(LEGACY_ACCOUNT);
    match open_err(cfg) {
        LifecycleError::KeyStore { slot, source } => {
            assert!(slot.contains("legacy"), "slot was {slot}");
            assert_eq!(source.kind, KeyStoreErrorKind::Denied);
        }
        other => panic!("expected a classified key store failure, got {other:?}"),
    }
    assert!(opens_with(&path, LEGACY));
    assert_eq!(
        keys.legacy(LEGACY_ACCOUNT).as_deref(),
        Some(LEGACY),
        "the item is kept"
    );

    // With a working primary key the legacy account is never read.
    let path2 = make_db(dir.path(), "second.db", OLD);
    let keys2 = FakeKeys::with_primary(OLD);
    keys2.set_legacy(LEGACY_ACCOUNT, LEGACY);
    keys2.fail_read_legacy(true);
    let store =
        EncryptedStore::open(config(&path2, &keys2).with_legacy_staging_account(LEGACY_ACCOUNT))
            .unwrap();
    assert_eq!(count(&store), 1);
}

#[test]
fn legacy_staging_as_the_sole_viable_key_is_migrated_then_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", LEGACY);
    let keys = FakeKeys::default();
    keys.set_legacy(LEGACY_ACCOUNT, LEGACY);
    let store =
        EncryptedStore::open(config(&path, &keys).with_legacy_staging_account(LEGACY_ACCOUNT))
            .unwrap();
    assert_eq!(
        keys.primary().as_deref(),
        Some(LEGACY),
        "canonical storage holds the key"
    );
    assert!(
        keys.legacy(LEGACY_ACCOUNT).is_none(),
        "the legacy item is deleted after readback"
    );
    assert_eq!(store.key_source(), KeySource::Primary);
    assert_eq!(count(&store), 1);
    drop(store);
    assert!(opens_with(&path, LEGACY));
}

#[test]
fn the_legacy_item_is_kept_when_deletion_is_off_or_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "keep.db", LEGACY);
    let keys = FakeKeys::default();
    keys.set_legacy(LEGACY_ACCOUNT, LEGACY);
    let cfg = config(&path, &keys)
        .with_legacy_staging_account(LEGACY_ACCOUNT)
        .with_delete_legacy_after_migration(false);
    drop(EncryptedStore::open(cfg).unwrap());
    assert_eq!(keys.primary().as_deref(), Some(LEGACY));
    assert!(
        keys.legacy(LEGACY_ACCOUNT).is_some(),
        "deletion was switched off"
    );

    let path2 = make_db(dir.path(), "fail.db", LEGACY);
    let keys2 = FakeKeys::default();
    keys2.set_legacy(LEGACY_ACCOUNT, LEGACY);
    keys2.fail_delete_legacy(true);
    let cfg2 = config(&path2, &keys2).with_legacy_staging_account(LEGACY_ACCOUNT);
    let store = EncryptedStore::open(cfg2).unwrap();
    assert_eq!(count(&store), 1, "a failed deletion never blocks the store");
    assert!(keys2.legacy(LEGACY_ACCOUNT).is_some());
    assert_eq!(keys2.primary().as_deref(), Some(LEGACY));
    // The retry deletes it once the failure clears.
    keys2.fail_delete_legacy(false);
    let report = store.recover_rotation();
    assert!(
        report.steps.contains(&RecoveryStep::LegacyStagingDeleted {
            alias: LEGACY_ACCOUNT.to_string()
        }),
        "{report:?}"
    );
    assert!(keys2.legacy(LEGACY_ACCOUNT).is_none());
}

#[test]
fn the_recovery_slot_is_the_sole_viable_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::default();
    keys.set_recovery(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    assert_eq!(keys.primary().as_deref(), Some(OLD));
    assert!(
        keys.recovery().is_none(),
        "redundant once it is the primary key"
    );
    assert_eq!(count(&store), 1);
}

// ----- rotation (PK03-4) ----------------------------------------------------------------------

#[test]
fn rotation_commits_and_both_keys_are_proven_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    let report = store.rotate_key(RotationRequest::new(NEW));
    assert!(matches!(report, RotationReport::Committed), "{report:?}");
    assert_eq!(keys.primary().as_deref(), Some(NEW));
    assert!(keys.pending().is_none());
    assert!(keys.recovery().is_none());
    assert!(!journal_exists(&path));
    assert_eq!(count(&store), 1, "the store reopened on the new key");
    assert_eq!(store.key_source(), KeySource::Primary);
    drop(store);
    assert!(opens_with(&path, NEW));
    assert!(!opens_with(&path, OLD));
}

#[test]
fn retain_previous_keeps_the_old_key_in_the_recovery_slot() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    let report = store.rotate_key(RotationRequest::new(NEW).retaining_previous());
    assert!(matches!(report, RotationReport::Committed), "{report:?}");
    assert_eq!(keys.recovery().as_deref(), Some(OLD));
    assert_eq!(keys.primary().as_deref(), Some(NEW));
}

#[test]
fn a_rekey_failure_is_not_committed_and_leaves_the_old_key_everywhere() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    let faults = TestFaults {
        forward: Some(RekeyFault::FailBefore),
        ..TestFaults::default()
    };
    match rotate(&store, &faults) {
        RotationReport::NotCommitted {
            error,
            cleanup_pending,
        } => {
            assert!(matches!(error, LifecycleError::Db(_)), "{error:?}");
            assert!(cleanup_pending.is_empty(), "{cleanup_pending:?}");
        }
        other => panic!("expected NotCommitted, got {other:?}"),
    }
    assert_eq!(keys.primary().as_deref(), Some(OLD));
    assert!(keys.pending().is_none(), "the staged key was discarded");
    assert!(keys.recovery().is_none(), "the recovery slot was put back");
    assert!(!journal_exists(&path));
    assert_eq!(count(&store), 1, "the store is usable on the old key");
    drop(store);
    assert!(opens_with(&path, OLD));
}

#[test]
fn a_rekey_that_applied_but_returned_an_error_is_committed_from_readback() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    let faults = TestFaults {
        forward: Some(RekeyFault::FailAfter),
        ..TestFaults::default()
    };
    let report = rotate(&store, &faults);
    assert!(matches!(report, RotationReport::Committed), "{report:?}");
    assert_eq!(keys.primary().as_deref(), Some(NEW));
    drop(store);
    assert!(opens_with(&path, NEW));
}

#[test]
fn a_promotion_that_wrote_the_primary_but_failed_cleanup_never_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    keys.promote(Promote::WriteThenFail);
    keys.fail_discard(true);
    match store.rotate_key(RotationRequest::new(NEW)) {
        RotationReport::CommittedCleanupPending { pending } => {
            assert!(!pending.is_empty());
        }
        other => panic!("expected CommittedCleanupPending, got {other:?}"),
    }
    assert_eq!(keys.primary().as_deref(), Some(NEW), "the commit stands");
    assert!(
        keys.pending().is_some(),
        "the pending marker is the leftover"
    );
    assert!(
        journal_exists(&path),
        "the journal stays so recovery finishes the cleanup"
    );
    assert!(
        store.degraded().is_some(),
        "the store is usable but reports the unfinished cleanup"
    );
    assert_eq!(count(&store), 1);

    keys.promote(Promote::Normal);
    keys.fail_discard(false);
    let report = store.recover_rotation();
    assert!(!report.is_blocked(), "{report:?}");
    assert!(keys.pending().is_none());
    assert!(!journal_exists(&path));
    assert!(store.degraded().is_none());
    assert_eq!(keys.primary().as_deref(), Some(NEW));
    drop(store);
    assert!(opens_with(&path, NEW));
}

#[test]
fn a_promotion_that_did_not_write_is_a_verified_noncommit_and_rolls_the_database_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    keys.promote(Promote::FailNoWrite);
    match store.rotate_key(RotationRequest::new(NEW)) {
        RotationReport::NotCommitted { error, .. } => {
            assert!(
                matches!(error, LifecycleError::KeyStore { .. }),
                "{error:?}"
            );
        }
        other => panic!("expected NotCommitted, got {other:?}"),
    }
    assert_eq!(keys.primary().as_deref(), Some(OLD));
    assert!(keys.pending().is_none());
    assert!(keys.recovery().is_none());
    assert_eq!(count(&store), 1);
    drop(store);
    assert!(
        opens_with(&path, OLD),
        "the database was rolled back to the old key"
    );
    assert!(!opens_with(&path, NEW));
}

#[test]
fn a_failed_rollback_is_indeterminate_retains_both_keys_and_blocks_writes_until_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    keys.promote(Promote::FailNoWrite);
    let faults = TestFaults {
        rollback: Some(RekeyFault::FailBefore),
        ..TestFaults::default()
    };
    match rotate(&store, &faults) {
        RotationReport::Indeterminate { detail } => {
            assert!(detail.contains("rolled back"), "{detail}");
        }
        other => panic!("expected Indeterminate, got {other:?}"),
    }
    assert!(store.is_blocked());
    assert!(matches!(
        store.with_writer(|_| ()),
        Err(LifecycleError::RecoveryRequired { .. })
    ));
    assert_eq!(
        keys.primary().as_deref(),
        Some(OLD),
        "both viable keys are retained"
    );
    assert_eq!(keys.pending().as_deref(), Some(NEW));
    assert_eq!(keys.recovery().as_deref(), Some(OLD));
    assert!(journal_exists(&path));
    assert!(opens_with(&path, NEW), "the database holds the new key");

    // Another opener meets the journal, reconciles from the real state and succeeds.
    keys.promote(Promote::Normal);
    let report = store.recover_rotation();
    assert!(!report.is_blocked(), "{report:?}");
    assert_eq!(keys.primary().as_deref(), Some(NEW));
    assert!(keys.pending().is_none());
    assert!(!store.is_blocked());
    assert_eq!(count(&store), 1);
}

#[test]
fn an_indeterminate_readback_retains_everything_and_recovery_resolves_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let hidden = dir.path().join("hidden.bin");
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    let faults = TestFaults {
        hide_database_to: Some(hidden.clone()),
        ..TestFaults::default()
    };
    match rotate(&store, &faults) {
        RotationReport::Indeterminate { detail } => {
            assert!(detail.contains("could not be checked"), "{detail}");
        }
        other => panic!("expected Indeterminate, got {other:?}"),
    }
    assert!(store.is_blocked());
    assert!(journal_exists(&path));
    assert_eq!(keys.primary().as_deref(), Some(OLD));
    assert_eq!(keys.pending().as_deref(), Some(NEW));
    assert_eq!(keys.recovery().as_deref(), Some(OLD));

    // While the file is unreachable, recovery changes nothing.
    let report = store.recover_rotation();
    assert!(report.is_blocked(), "{report:?}");
    assert_eq!(keys.primary().as_deref(), Some(OLD));
    assert_eq!(keys.pending().as_deref(), Some(NEW));

    std::fs::rename(&hidden, &path).unwrap();
    let report = store.recover_rotation();
    assert!(!report.is_blocked(), "{report:?}");
    assert!(report.steps.contains(&RecoveryStep::PendingPromoted));
    assert_eq!(keys.primary().as_deref(), Some(NEW));
    assert!(keys.pending().is_none());
    assert!(keys.recovery().is_none());
    assert!(!journal_exists(&path));
    assert_eq!(count(&store), 1);
    drop(store);
    assert!(opens_with(&path, NEW));
}

#[test]
fn a_failed_validation_rolls_the_rotation_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(
        config(&path, &keys).with_validate(|_| Err("the schema check refused".to_string())),
    )
    .unwrap();
    match store.rotate_key(RotationRequest::new(NEW)) {
        RotationReport::NotCommitted { error, .. } => {
            assert!(
                matches!(error, LifecycleError::ValidationFailed(_)),
                "{error:?}"
            );
        }
        other => panic!("expected NotCommitted, got {other:?}"),
    }
    assert_eq!(keys.primary().as_deref(), Some(OLD));
    assert!(keys.pending().is_none());
    assert_eq!(count(&store), 1);
    drop(store);
    assert!(opens_with(&path, OLD));
}

#[test]
fn rotation_to_the_current_key_or_an_invalid_key_is_refused_before_any_change() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    for key in [OLD, "not a valid key; DROP TABLE t"] {
        match store.rotate_key(RotationRequest::new(key)) {
            RotationReport::NotCommitted {
                cleanup_pending, ..
            } => {
                assert!(cleanup_pending.is_empty())
            }
            other => panic!("expected NotCommitted, got {other:?}"),
        }
    }
    assert_eq!(keys.primary().as_deref(), Some(OLD));
    assert!(keys.pending().is_none());
    assert!(keys.recovery().is_none());
    assert_eq!(count(&store), 1);
}

// ----- interruption after every phase (PK03-6) ------------------------------------------------

#[test]
fn a_process_death_after_every_phase_leaves_a_viable_key_and_the_next_open_reconciles() {
    for phase in Phase::ALL {
        let dir = tempfile::tempdir().unwrap();
        let path = make_db(dir.path(), "app.db", OLD);
        let keys = FakeKeys::with_primary(OLD);
        let store = EncryptedStore::open(config(&path, &keys)).unwrap();
        let faults = TestFaults {
            die: Some(phase),
            ..TestFaults::default()
        };
        match store.rotate_key_with_faults(RotationRequest::new(NEW), &faults) {
            Err(killed) => assert_eq!(killed, Killed(phase)),
            Ok(report) => panic!("{phase:?}: the rotation was expected to stop, got {report:?}"),
        }
        drop(store);

        assert!(
            any_stored_key_opens(&path, &keys),
            "{phase:?}: at least one durably stored key must still open the database"
        );

        // The next process.
        let reopened = EncryptedStore::open(config(&path, &keys))
            .unwrap_or_else(|error| panic!("{phase:?}: the reopen failed: {error:?}"));
        assert_eq!(count(&reopened), 1, "{phase:?}");
        let primary = keys.primary().expect("a primary key is stored");
        let expected = if phase >= Phase::Rekeyed { NEW } else { OLD };
        assert_eq!(
            primary, expected,
            "{phase:?}: the database and the primary key agree"
        );
        assert!(
            keys.pending().is_none(),
            "{phase:?}: pending was reconciled"
        );
        assert!(
            keys.recovery().is_none(),
            "{phase:?}: recovery was cleared once consistent"
        );
        assert!(!journal_exists(&path), "{phase:?}: the journal was cleared");
        drop(reopened);
        assert!(opens_with(&path, expected), "{phase:?}");
    }
}

// ----- the guard, drain and Busy (PK03-3, PK03-6) ---------------------------------------------

#[test]
fn a_second_transition_on_the_same_store_is_busy_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let options = transition_options();
    let first =
        StoreTransition::begin(TransitionKind::Rotate, &[path.as_path()], &options).unwrap();
    assert_eq!(first.kind(), TransitionKind::Rotate);
    match StoreTransition::begin(TransitionKind::Restore, &[path.as_path()], &options) {
        Err(error) => assert!(error.is_busy(), "{error:?}"),
        Ok(_) => panic!("the guard must be exclusive"),
    }
    assert!(!journal_exists(&path));
    first.settle();
    StoreTransition::begin(TransitionKind::Rotate, &[path.as_path()], &options)
        .unwrap()
        .settle();
}

#[test]
fn another_opener_holding_the_store_makes_rotation_busy_and_nothing_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    let other_opener = StoreLock::shared(&path, &rightkit_fs::LockOptions::default()).unwrap();
    match store.rotate_key(RotationRequest::new(NEW)) {
        RotationReport::NotCommitted {
            error,
            cleanup_pending,
        } => {
            assert!(error.is_busy(), "{error:?}");
            assert!(cleanup_pending.is_empty());
        }
        other => panic!("expected NotCommitted, got {other:?}"),
    }
    assert_eq!(keys.primary().as_deref(), Some(OLD));
    assert!(keys.pending().is_none());
    assert!(keys.recovery().is_none());
    assert!(!journal_exists(&path));
    assert_eq!(count(&store), 1, "the store resumed with nothing changed");
    drop(other_opener);
    assert!(matches!(
        store.rotate_key(RotationRequest::new(NEW)),
        RotationReport::Committed
    ));
}

#[test]
fn a_pool_with_active_readers_is_busy_before_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    let pool = store.read_pool(2, ReadPoolOptions::new()).unwrap();
    let reader = pool.get().unwrap();
    match store.rotate_key(RotationRequest::new(NEW)) {
        RotationReport::NotCommitted {
            error,
            cleanup_pending,
        } => {
            assert!(error.is_busy(), "{error:?}");
            assert!(cleanup_pending.is_empty());
        }
        other => panic!("expected NotCommitted, got {other:?}"),
    }
    assert_eq!(keys.primary().as_deref(), Some(OLD));
    assert!(keys.pending().is_none());
    assert!(keys.recovery().is_none());
    assert!(!journal_exists(&path));
    assert_eq!(count(&store), 1, "the writer was reopened");
    let rows: i64 = pool
        .get()
        .unwrap()
        .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1, "the pool was reopened");

    drop(reader);
    assert!(matches!(
        store.rotate_key(RotationRequest::new(NEW)),
        RotationReport::Committed
    ));
    let rows: i64 = pool
        .get()
        .unwrap()
        .query_row("SELECT count(*) FROM t", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1, "the pool reopened on the new key");
}

struct StoppingHost {
    refuse: bool,
    stops: Mutex<u32>,
    resumes: Mutex<u32>,
}

impl rightkit_db::encrypted_lifecycle::HostQuiesce for StoppingHost {
    fn stop(&self, _kind: TransitionKind) -> Result<(), String> {
        *self.stops.lock().unwrap() += 1;
        if self.refuse {
            Err("a worker is mid-job".to_string())
        } else {
            Ok(())
        }
    }

    fn resume(&self) {
        *self.resumes.lock().unwrap() += 1;
    }
}

#[test]
fn the_host_is_stopped_before_a_transition_and_a_refusal_is_busy() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let host = Arc::new(StoppingHost {
        refuse: true,
        stops: Mutex::new(0),
        resumes: Mutex::new(0),
    });
    let store = EncryptedStore::open(config(&path, &keys).with_host(host.clone())).unwrap();
    match store.rotate_key(RotationRequest::new(NEW)) {
        RotationReport::NotCommitted { error, .. } => assert!(error.is_busy(), "{error:?}"),
        other => panic!("expected NotCommitted, got {other:?}"),
    }
    assert_eq!(*host.stops.lock().unwrap(), 1);
    assert_eq!(keys.primary().as_deref(), Some(OLD));
    assert_eq!(count(&store), 1);
}

// ----- backup, restore and relocation (PK03-5) ------------------------------------------------

#[test]
fn backup_and_restore_round_trip_through_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    let backup = dir.path().join("app.bak");
    store.backup_to(&backup).unwrap();
    store
        .with_writer(|conn| conn.execute("INSERT INTO t VALUES (2)", []))
        .unwrap()
        .unwrap();
    assert_eq!(count(&store), 2);

    let receipt = store.restore(&backup, &RestoreOptions::default()).unwrap();
    let previous = receipt.previous.expect("the previous store is kept");
    assert!(previous.exists());
    assert_eq!(count(&store), 1, "the restored store is open again");
    assert!(!journal_exists(&path));
    assert!(opens_with(&previous, OLD), "the previous store is intact");
}

#[test]
fn a_replacement_under_another_key_is_refused_and_the_live_store_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let foreign = make_db(dir.path(), "foreign.db", OTHER);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    match store.restore(&foreign, &RestoreOptions::default()) {
        Err(LifecycleError::WrongDestinationKey { .. }) => {}
        other => panic!("expected WrongDestinationKey, got {:?}", other.err()),
    }
    assert_eq!(count(&store), 1);
    assert!(!journal_exists(&path));
    assert!(dir_names(dir.path())
        .iter()
        .all(|name| !name.contains("pre-restore")));
}

#[test]
fn relocation_copies_verifies_switches_the_key_store_and_keeps_the_old_root() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    let new_root = dir.path().join("new-root");
    std::fs::create_dir(&new_root).unwrap();
    let new_path = new_root.join("app.db");
    let new_keys = FakeKeys::default();
    let plan = RelocationPlan::new(&new_path).with_new_key_store(Arc::new(new_keys.clone()));
    match store.relocate(plan) {
        RelocationReport::Relocated {
            old_root,
            new_root: reported,
            key_store_switched,
        } => {
            assert_eq!(old_root, path);
            assert_eq!(reported, new_path);
            assert!(key_store_switched);
        }
        other => panic!("expected Relocated, got {other:?}"),
    }
    assert_eq!(store.db_path(), new_path);
    assert_eq!(
        new_keys.primary().as_deref(),
        Some(OLD),
        "the key was read back at the new root"
    );
    assert_eq!(count(&store), 1);
    assert!(
        opens_with(&path, OLD),
        "the old root is intact and openable"
    );
    assert!(!journal_exists(&path));
    assert!(!journal_exists(&new_path));
    drop(store);
    assert!(opens_with(&new_path, OLD));
}

#[test]
fn relocation_refuses_a_collision_and_a_wrong_destination_key_before_touching_anything() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();

    let taken = dir.path().join("taken.db");
    std::fs::write(&taken, b"someone else's file").unwrap();
    match store.relocate(RelocationPlan::new(&taken)) {
        RelocationReport::NotRelocated { error, .. } => {
            assert!(
                matches!(error, LifecycleError::DestinationExists { .. }),
                "{error:?}"
            );
        }
        other => panic!("expected NotRelocated, got {other:?}"),
    }
    assert_eq!(std::fs::read(&taken).unwrap(), b"someone else's file");

    let elsewhere = dir.path().join("elsewhere.db");
    let wrong = FakeKeys::with_primary(OTHER);
    let plan = RelocationPlan::new(&elsewhere).with_new_key_store(Arc::new(wrong.clone()));
    match store.relocate(plan) {
        RelocationReport::NotRelocated { error, .. } => {
            assert!(
                matches!(error, LifecycleError::WrongDestinationKey { .. }),
                "{error:?}"
            );
        }
        other => panic!("expected NotRelocated, got {other:?}"),
    }
    assert!(!elsewhere.exists(), "nothing was copied");
    assert_eq!(
        wrong.primary().as_deref(),
        Some(OTHER),
        "the destination key store is unchanged"
    );
    assert_eq!(store.db_path(), path);
    assert_eq!(count(&store), 1);
    assert!(!journal_exists(&path));
}

#[test]
fn a_failed_validation_leaves_the_old_root_serving_and_removes_only_its_own_copy() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(
        config(&path, &keys).with_validate(|_| Err("the schema check refused".to_string())),
    )
    .unwrap();
    let new_path = dir.path().join("moved.db");
    match store.relocate(RelocationPlan::new(&new_path)) {
        RelocationReport::NotRelocated {
            error,
            copy_retained,
            ..
        } => {
            assert!(
                matches!(error, LifecycleError::ValidationFailed(_)),
                "{error:?}"
            );
            assert!(copy_retained.is_none());
        }
        other => panic!("expected NotRelocated, got {other:?}"),
    }
    assert!(
        !new_path.exists(),
        "the copy this call published was removed"
    );
    assert_eq!(store.db_path(), path);
    assert_eq!(count(&store), 1);
    assert!(!journal_exists(&path));
    assert!(!journal_exists(&new_path));
}

// ----- recovery without an open store ---------------------------------------------------------

#[test]
fn recover_rotation_without_an_open_store_reconciles_a_dead_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    let faults = TestFaults {
        die: Some(Phase::Rekeyed),
        ..TestFaults::default()
    };
    assert!(store
        .rotate_key_with_faults(RotationRequest::new(NEW), &faults)
        .is_err());
    drop(store);

    let report = rightkit_db::encrypted_lifecycle::recover_rotation(&config(&path, &keys));
    assert!(
        matches!(report.outcome, RecoveryOutcome::Reconciled),
        "{report:?}"
    );
    assert_eq!(report.opened_with, Some(KeySource::Pending));
    assert_eq!(keys.primary().as_deref(), Some(NEW));
    assert!(keys.pending().is_none());
    assert!(!journal_exists(&path));

    let again = rightkit_db::encrypted_lifecycle::recover_rotation(&config(&path, &keys));
    assert!(
        matches!(again.outcome, RecoveryOutcome::Consistent),
        "{again:?}"
    );
}

#[test]
fn owned_session_moves_threads_and_excludes_competitors_until_drop() {
    let keys = FakeKeys::with_primary(OLD);
    let mut session = keys.lock_rotation(Duration::ZERO).unwrap();
    let revision = session
        .write_pending(&Zeroizing::new(NEW.to_string()))
        .unwrap();
    let competing = keys.clone();
    let session = std::thread::spawn(move || {
        assert_eq!(session.read_primary().unwrap().unwrap().as_str(), OLD);
        assert!(competing.lock_rotation(Duration::ZERO).is_err());
        assert_eq!(session.read_pending().unwrap().unwrap().revision, revision);
        session
    })
    .join()
    .unwrap();
    assert!(keys.lock_rotation(Duration::ZERO).is_err());
    drop(session);
    let mut next = keys.lock_rotation(Duration::ZERO).unwrap();
    assert_eq!(next.read_pending().unwrap().unwrap().key.as_str(), NEW);
}

#[test]
fn interrupted_rekey_releases_session_without_losing_viable_pending_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "owned-session.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    let result = store.rotate_key_with_faults(
        RotationRequest::new(NEW),
        &TestFaults {
            die: Some(Phase::Rekeyed),
            ..TestFaults::default()
        },
    );
    assert!(matches!(result, Err(Killed(Phase::Rekeyed))));
    let mut session = keys.lock_rotation(Duration::ZERO).unwrap();
    let pending = session.read_pending().unwrap().unwrap();
    assert!(opens_with(&path, pending.key.as_str()));
    assert_eq!(session.read_recovery().unwrap().unwrap().as_str(), OLD);
    drop(session);
    drop(store);
    let recovered = EncryptedStore::open(config(&path, &keys)).unwrap();
    assert_eq!(count(&recovered), 1);
    assert_eq!(keys.primary().as_deref(), Some(NEW));
}

#[test]
fn relocation_acquires_destination_session_before_publishing_copy() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "source-session.db", OLD);
    let new_path = dir.path().join("destination-session.db");
    let source = FakeKeys::with_primary(OLD);
    let destination = FakeKeys::default();
    let store = EncryptedStore::open(config(&path, &source)).unwrap();
    let held = destination.lock_rotation(Duration::ZERO).unwrap();
    let report = store
        .relocate(RelocationPlan::new(&new_path).with_new_key_store(Arc::new(destination.clone())));
    match report {
        RelocationReport::NotRelocated { error, .. } => assert!(
            matches!(error, LifecycleError::KeyStore { .. }),
            "the busy destination session is a key-store refusal: {error:?}"
        ),
        other => panic!("expected NotRelocated, got {other:?}"),
    }
    assert!(!new_path.exists());
    assert_eq!(count(&store), 1);
    drop(held);
    assert!(destination.primary().is_none());
}

#[test]
fn relocation_reuses_only_same_configured_key_store_arc() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "source-arc.db", OLD);
    let new_path = dir.path().join("destination-arc.db");
    let keys = FakeKeys::with_primary(OLD);
    let cfg = config(&path, &keys);
    let same = cfg.key_store.clone();
    let store = EncryptedStore::open(cfg).unwrap();
    let started = std::time::Instant::now();
    let report = store.relocate(RelocationPlan::new(&new_path).with_new_key_store(same));
    // A `RelocatedCleanupPending` report lists every retained path with the error that kept it,
    // so the `{report:?}` below names the cause when this recurs.
    assert!(
        matches!(report, RelocationReport::Relocated { .. }),
        "expected Relocated, got {report:?} after {:?}",
        started.elapsed()
    );
    assert_eq!(count(&store), 1);
}

/// `busy_timeout` is SQLite's lock wait. It used to be the total deadline of every online backup
/// copy as well, so a wait shorter than the copy failed `backup_to` ("backup operation timeout
/// expired") on every OS, and on a slow hosted runner even the default five seconds did. Zero is
/// shorter than any copy. Nothing else touches these files while the test runs, so the missing
/// lock wait costs nothing here. `restore` and relocation make their copies the same way
/// (`online_backup_sized`) and are covered where the zero wait can be given to the copy alone:
/// `tests/restore.rs` hands its options to `restore_database` only, and the budget and the busy
/// wait are pinned by the unit tests in `src/backup.rs`.
#[test]
fn backup_to_is_not_bounded_by_the_sqlite_busy_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "app-budget.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let mut cfg = config(&path, &keys);
    cfg.db.busy_timeout = Duration::ZERO;
    let store = EncryptedStore::open(cfg).unwrap();
    let backup = dir.path().join("app-budget.bak");
    store.backup_to(&backup).unwrap();
    assert!(
        opens_with(&backup, OLD),
        "the backup opens with the store key"
    );
    assert_eq!(count(&store), 1);
}

#[test]
fn a_failed_publishing_record_aborts_cleanly_and_a_retry_can_relocate() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "source-record.db", OLD);
    let new_path = dir.path().join("destination-record.db");
    let keys = FakeKeys::with_primary(OLD);
    // Once the candidate has validated, the destination's journal slot becomes a non-empty
    // directory, so the next journal write (the "publishing" record) fails on every OS.
    let obstacle = journal_file(&new_path);
    let armed = Arc::new(AtomicBool::new(true));
    let cfg = config(&path, &keys).with_validate({
        let obstacle = obstacle.clone();
        let armed = armed.clone();
        move |options: &DbOptions| {
            let candidate = options.path.file_name() == Some(std::ffi::OsStr::new("candidate.db"));
            if candidate && armed.swap(false, Ordering::SeqCst) {
                std::fs::remove_file(&obstacle).map_err(|e| e.to_string())?;
                std::fs::create_dir(&obstacle).map_err(|e| e.to_string())?;
                std::fs::write(obstacle.join("held"), b"x").map_err(|e| e.to_string())?;
            }
            Ok(())
        }
    });
    let store = EncryptedStore::open(cfg).unwrap();
    let report = store.relocate(RelocationPlan::new(&new_path));
    match &report {
        RelocationReport::NotRelocated { copy_retained, .. } => {
            assert!(copy_retained.is_none(), "{report:?}")
        }
        other => panic!("expected NotRelocated, got {other:?}"),
    }
    assert!(!armed.load(Ordering::SeqCst), "the obstacle was placed");
    assert_eq!(store.db_path(), path);
    assert_eq!(count(&store), 1, "the old root still serves");
    assert!(!new_path.exists());
    let workspaces: Vec<String> = dir_names(dir.path())
        .into_iter()
        .filter(|name| name.starts_with(".rkdb-relocate-"))
        .collect();
    assert!(
        workspaces.is_empty(),
        "nothing was published, so the aborted copy is not retained: {workspaces:?}"
    );
    assert!(!journal_exists(&path), "the source journal was cleared");
    assert!(
        obstacle.is_dir(),
        "the obstacle is not the relocation's to remove"
    );
    std::fs::remove_dir_all(&obstacle).unwrap();
    let report = store.relocate(RelocationPlan::new(&new_path));
    assert!(
        matches!(report, RelocationReport::Relocated { .. }),
        "{report:?}"
    );
    assert_eq!(store.db_path(), new_path);
    assert_eq!(count(&store), 1);
}

#[test]
fn unsettled_promotion_retains_both_keys_until_late_worker_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "late-promotion.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    keys.0.lock().unwrap().late_promotion = Some((release_rx, done_tx));
    keys.promote(Promote::Unsettled);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    let report = store.rotate_key(RotationRequest::new(NEW));
    assert!(matches!(report, RotationReport::Indeterminate { .. }));
    assert!(store.is_blocked());
    assert_eq!(
        keys.primary().as_deref(),
        Some(OLD),
        "readback is still old before late mutation"
    );
    assert_eq!(keys.pending().as_deref(), Some(NEW));
    assert_eq!(keys.recovery().as_deref(), Some(OLD));
    assert!(journal_exists(&path));
    assert!(
        opens_with(&path, NEW),
        "uncertain promotion must not roll DB back"
    );
    assert!(keys.lock_rotation(Duration::ZERO).is_err());
    release_tx.send(()).unwrap();
    done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(keys.primary().as_deref(), Some(NEW));
    assert_eq!(keys.recovery().as_deref(), Some(OLD));
    assert!(any_stored_key_opens(&path, &keys));
    keys.promote(Promote::Normal);
    assert!(!store.recover_rotation().is_blocked());
    assert_eq!(count(&store), 1);
}

struct PausedPoolAdmission {
    entered: std::sync::mpsc::SyncSender<()>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl Faults for PausedPoolAdmission {
    fn before_pool_registration(&self) {
        self.entered.send(()).unwrap();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
    }
}

#[test]
fn pool_admission_cannot_escape_transition_drain_registry() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "pool-admission.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = Arc::new(
        EncryptedStore::open(config(&path, &keys).with_drain_timeout(Duration::ZERO)).unwrap(),
    );
    let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let opening = store.clone();
    let pool_thread = std::thread::spawn(move || {
        opening
            .read_pool_with_faults(
                1,
                ReadPoolOptions::new(),
                &PausedPoolAdmission {
                    entered: entered_tx,
                    release: Mutex::new(release_rx),
                },
            )
            .unwrap()
    });
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // Admission has opened SQLCipher connections but has not registered them. Rotation must
    // stop at live-state admission rather than missing this pool and reaching rekey.
    let report = store.recover_rotation();
    assert!(
        report.is_blocked(),
        "admission must block transition drain: {report:?}"
    );
    release_tx.send(()).unwrap();
    let pool = pool_thread.join().unwrap();
    assert_eq!(keys.primary().as_deref(), Some(OLD));
    let reader = pool.get().unwrap();
    assert_eq!(
        reader
            .query_row("SELECT count(*) FROM t", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(reader);
    assert!(matches!(
        store.rotate_key(RotationRequest::new(NEW)),
        RotationReport::Committed
    ));
}

#[test]
fn failed_pending_discard_preserves_recovery_during_abort() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_db(dir.path(), "abort-retention.db", OLD);
    let keys = FakeKeys::with_primary(OLD);
    let store = EncryptedStore::open(config(&path, &keys)).unwrap();
    keys.promote(Promote::FailNoWrite);
    keys.fail_discard(true);
    let report = store.rotate_key(RotationRequest::new(NEW));
    assert!(matches!(report, RotationReport::NotCommitted { .. }));
    assert_eq!(keys.recovery().as_deref(), Some(OLD));
    assert_eq!(keys.pending().as_deref(), Some(NEW));
    assert!(journal_exists(&path));
    assert!(any_stored_key_opens(&path, &keys));
}
