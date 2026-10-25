//! PK03 phased relocation: actual SQLCipher files, persistent fixture key slots & killed children.
//! Fixture slots are private test files, not a production protected-storage implementation.
#![cfg(all(feature = "encrypted-lifecycle", feature = "cipher"))]

use rightkit_db::encrypted_lifecycle::{
    recover_aborted_relocation, recover_completed_relocation, recover_relocation,
    recover_relocation_session, recover_rotation, BlockedReason, EncryptedStore,
    EncryptedStoreConfig, Key, KeyStoreError, LifecycleError, PendingKey, PendingRevision,
    ProtectedKeySession, ProtectedKeyStore, RecoveryOutcome, RelocationPlan, RelocationSettlement,
    RotationRequest,
};
use rightkit_db::{DbOptions, Zeroizing};
use rightkit_fs::{AtomicWriteOptions, AtomicWriter, FileLock, LockOptions};
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc};
use std::time::Duration;

const KEY: &str = "cmVsb2NhdGlvbi10ZXN0LWtleQ==";
const CHILD_ENV: &str = "RIGHTKIT_DB_RELOCATION_INTERRUPTION_CHILD";

fn private_write(path: &Path, bytes: &[u8]) {
    let mut writer = AtomicWriter::create_with(
        path,
        AtomicWriteOptions {
            private: true,
            ..AtomicWriteOptions::default()
        },
    )
    .unwrap();
    writer.write_all(bytes).unwrap();
    writer.commit().unwrap();
}

#[derive(Clone)]
struct DiskKeys(PathBuf);

impl DiskKeys {
    fn new(path: PathBuf) -> Self {
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn readback_failure(&self, enabled: bool) {
        let flag = self.0.join("deny-primary-read");
        if enabled {
            private_write(&flag, b"deny");
        } else if flag.exists() {
            fs::remove_file(flag).unwrap();
        }
    }
}

struct DiskSession {
    root: PathBuf,
    _lock: FileLock,
}

fn key_error(_: impl std::fmt::Display) -> KeyStoreError {
    KeyStoreError::other("persistent fixture key-slot I/O failed")
}

impl DiskSession {
    fn read(&self, name: &str) -> Result<Option<Key>, KeyStoreError> {
        match fs::read_to_string(self.root.join(name)) {
            Ok(value) => Ok(Some(Zeroizing::new(value))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(key_error(error)),
        }
    }

    fn remove(&self, name: &str) -> Result<(), KeyStoreError> {
        match fs::remove_file(self.root.join(name)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(key_error(error)),
        }
    }
}

impl ProtectedKeyStore for DiskKeys {
    fn lock_rotation(
        &self,
        timeout: Duration,
    ) -> Result<Box<dyn ProtectedKeySession + Send + 'static>, KeyStoreError> {
        let lock = FileLock::acquire(
            &self.0.join("slots.lock"),
            &LockOptions {
                timeout,
                ..LockOptions::default()
            },
        )
        .map_err(key_error)?;
        Ok(Box::new(DiskSession {
            root: self.0.clone(),
            _lock: lock,
        }))
    }
}

impl ProtectedKeySession for DiskSession {
    fn read_primary(&mut self) -> Result<Option<Key>, KeyStoreError> {
        if self.root.join("deny-primary-read").exists() {
            return Err(KeyStoreError::unavailable(
                "requested fixture readback failure",
            ));
        }
        self.read("primary")
    }

    fn read_pending(&mut self) -> Result<Option<PendingKey>, KeyStoreError> {
        let Some(value) = self.read("pending")? else {
            return Ok(None);
        };
        let (revision, key) = value
            .split_once('\n')
            .ok_or_else(|| key_error("pending format"))?;
        Ok(Some(PendingKey {
            key: Zeroizing::new(key.to_owned()),
            revision: PendingRevision(revision.to_owned()),
        }))
    }

    fn write_pending(&mut self, key: &Key) -> Result<PendingRevision, KeyStoreError> {
        let counter_path = self.root.join("counter");
        let previous = match fs::read_to_string(&counter_path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => "0".into(),
            Err(error) => return Err(key_error(error)),
        };
        let counter = previous.parse::<u64>().map_err(key_error)? + 1;
        private_write(&counter_path, counter.to_string().as_bytes());
        let revision = PendingRevision(format!("fixture-{counter}"));
        private_write(
            &self.root.join("pending"),
            format!("{}\n{}", revision.0, key.as_str()).as_bytes(),
        );
        Ok(revision)
    }

    fn promote_pending(&mut self, revision: &PendingRevision) -> Result<(), KeyStoreError> {
        let pending = self
            .read_pending()?
            .ok_or_else(|| key_error("missing pending"))?;
        if pending.revision != *revision {
            return Err(key_error("stale revision"));
        }
        private_write(&self.root.join("primary"), pending.key.as_bytes());
        self.remove("pending")?;
        if self.root.join("deny-read-after-promote").exists() {
            private_write(&self.root.join("deny-primary-read"), b"deny");
        }
        Ok(())
    }

    fn discard_pending(&mut self, revision: &PendingRevision) -> Result<(), KeyStoreError> {
        if self
            .read_pending()?
            .is_some_and(|pending| pending.revision == *revision)
        {
            self.remove("pending")?;
        }
        Ok(())
    }

    fn read_legacy_staging(&mut self, _: &str) -> Result<Option<Key>, KeyStoreError> {
        Ok(None)
    }
    fn delete_legacy_staging(&mut self, _: &str) -> Result<(), KeyStoreError> {
        Ok(())
    }
    fn read_recovery(&mut self) -> Result<Option<Key>, KeyStoreError> {
        self.read("recovery")
    }
    fn write_recovery(&mut self, key: &Key) -> Result<(), KeyStoreError> {
        private_write(&self.root.join("recovery"), key.as_bytes());
        Ok(())
    }
    fn clear_recovery(&mut self) -> Result<(), KeyStoreError> {
        self.remove("recovery")
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        for name in ["old", "new", "old-keys", "new-keys"] {
            fs::create_dir(root.join(name)).unwrap();
        }
        private_write(&root.join("old-keys/primary"), KEY.as_bytes());
        let conn = DbOptions::new(root.join("old/store.db"))
            .with_passphrase(KEY)
            .open_writer()
            .unwrap();
        conn.execute_batch(
            "CREATE TABLE relocation_value(x INTEGER); INSERT INTO relocation_value VALUES (41);",
        )
        .unwrap();
        drop(conn);
        private_write(&root.join("selected-root"), b"old");
        Self { _temp: temp, root }
    }
    fn old(&self) -> PathBuf {
        self.root.join("old/store.db")
    }
    fn new_db(&self) -> PathBuf {
        self.root.join("new/store.db")
    }
    fn config(&self) -> EncryptedStoreConfig {
        config(&self.root)
    }
    fn plan(&self) -> RelocationPlan {
        plan(&self.root)
    }
}

fn config(root: &Path) -> EncryptedStoreConfig {
    EncryptedStoreConfig::new(
        DbOptions::new(root.join("old/store.db")),
        Arc::new(DiskKeys::new(root.join("old-keys"))),
    )
}

fn plan(root: &Path) -> RelocationPlan {
    RelocationPlan::new(root.join("new/store.db"))
        .with_transaction_root(root.join("new"))
        .with_new_key_store(Arc::new(DiskKeys::new(root.join("new-keys"))))
}

fn value(path: &Path) -> i64 {
    let conn = DbOptions::new(path)
        .with_passphrase(KEY)
        .open_existing_writer()
        .unwrap();
    conn.query_row("SELECT x FROM relocation_value", [], |row| row.get(0))
        .unwrap()
}

fn journal(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push(".transition-journal");
    PathBuf::from(name)
}

struct ChildOwner(Child);
impl Drop for ChildOwner {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn kill_after(root: &Path, phase: &str) {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "relocation_interruption_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, root)
        .env("RIGHTKIT_DB_RELOCATION_CHILD_PHASE", phase)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = ChildOwner(command.spawn().unwrap());
    let stdout = child.0.stdout.take().unwrap();
    let expected = format!("RELOCATION_READY:{phase}");
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let ready = std::io::BufReader::new(stdout)
            .lines()
            .any(|line| line.is_ok_and(|line| line.ends_with(&expected)));
        let _ = tx.send(ready);
    });
    let ready = rx.recv_timeout(Duration::from_secs(30));
    child.0.kill().unwrap();
    let status = child.0.wait().unwrap();
    reader.join().unwrap();
    assert!(ready.unwrap(), "child never reached {phase}");
    assert!(!status.success(), "child must really die before recovery");
}

#[test]
fn relocation_interruption_child() {
    let Some(root) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let root = PathBuf::from(root);
    let phase = std::env::var("RIGHTKIT_DB_RELOCATION_CHILD_PHASE").unwrap();
    assert!(matches!(
        phase.as_str(),
        "quiesced" | "copied" | "selected" | "activated"
    ));
    let store = Arc::new(EncryptedStore::open(config(&root)).unwrap());
    let mut session = store.prepare_relocation(plan(&root)).unwrap();
    session.quiesce().unwrap();
    if phase != "quiesced" {
        session.copy().unwrap();
    }
    if matches!(phase.as_str(), "selected" | "activated") {
        // Actual durable, test-owned root-selector record; no DB/FS journal is fabricated.
        private_write(&root.join("selected-root"), b"new");
    }
    if phase == "activated" {
        session.reopen().unwrap();
    }
    println!("RELOCATION_READY:{phase}");
    std::io::stdout().flush().unwrap();
    let mut release = String::new();
    std::io::stdin().read_line(&mut release).unwrap();
    panic!("parent must kill child while session & guards remain live");
}

#[test]
fn killed_relocation_recovers_from_durable_root_selection_at_each_phase() {
    for phase in ["quiesced", "copied", "selected", "activated"] {
        let fixture = Fixture::new();
        kill_after(&fixture.root, phase);
        assert!(
            journal(&fixture.old()).exists(),
            "{phase}: old journal missing"
        );
        assert_eq!(
            value(&fixture.old()),
            41,
            "{phase}: source must remain openable"
        );
        let selected = fs::read(fixture.root.join("selected-root")).unwrap();
        let committed = selected == b"new";
        assert_eq!(committed, matches!(phase, "selected" | "activated"));
        if phase != "quiesced" {
            assert_eq!(value(&fixture.new_db()), 41, "{phase}: published copy lost");
        }
        let settlement = if committed {
            RelocationSettlement::Committed
        } else {
            RelocationSettlement::Aborted
        };
        let recovered = recover_relocation(fixture.config(), fixture.plan(), settlement).unwrap();
        assert!(
            recovered.report.cleanup_retained.is_empty(),
            "{phase}: {:?}",
            recovered.report
        );
        let store = recovered
            .store
            .expect("authoritative settlement must reopen selected store");
        assert_eq!(
            store.db_path(),
            if committed {
                fixture.new_db()
            } else {
                fixture.old()
            }
        );
        let served: i64 = store
            .with_writer(|conn| {
                conn.query_row("SELECT x FROM relocation_value", [], |row| row.get(0))
            })
            .unwrap()
            .unwrap();
        assert_eq!(served, 41);
        if committed {
            store
                .with_writer(|conn| conn.execute("UPDATE relocation_value SET x=42", []))
                .unwrap()
                .unwrap();
        }
        drop(store);
        assert_eq!(value(&fixture.old()), 41, "{phase}: source was modified");
        if phase != "quiesced" {
            assert_eq!(value(&fixture.new_db()), if committed { 42 } else { 41 });
        }
        assert!(
            !journal(&fixture.old()).exists(),
            "{phase}: settled old journal remains"
        );
        assert!(
            !journal(&fixture.new_db()).exists(),
            "{phase}: settled new journal remains"
        );
    }
}

#[test]
fn recovered_session_retains_replay_authority_until_explicit_committed_settlement() {
    for phase in ["selected", "activated"] {
        let fixture = Fixture::new();
        kill_after(&fixture.root, phase);
        let mut session = recover_relocation_session(fixture.config(), fixture.plan()).unwrap();
        let store = session.store();
        assert_eq!(store.db_path(), fixture.new_db());
        let served: i64 = store
            .with_writer(|conn| {
                conn.query_row("SELECT x FROM relocation_value", [], |row| row.get(0))
            })
            .unwrap()
            .unwrap();
        assert_eq!(served, 41);
        assert!(
            session.settle(RelocationSettlement::Aborted).is_err(),
            "durable destination selection cannot be aborted"
        );
        assert!(journal(&fixture.old()).exists());
        assert!(journal(&fixture.new_db()).exists());
        drop(store);
        drop(session);
        // A coordinator crash before its Completed record must remain replayable.
        assert!(journal(&fixture.old()).exists());
        assert!(journal(&fixture.new_db()).exists());
        let mut replay = recover_relocation_session(fixture.config(), fixture.plan()).unwrap();
        assert_eq!(replay.store().db_path(), fixture.new_db());
        let report = replay.settle(RelocationSettlement::Committed).unwrap();
        assert!(report.cleanup_retained.is_empty(), "{report:?}");
        assert!(!journal(&fixture.old()).exists());
        assert!(!journal(&fixture.new_db()).exists());
        assert_eq!(value(&fixture.old()), 41);
        assert_eq!(value(&fixture.new_db()), 41);
    }
}

#[test]
fn restore_after_relocation_activation_cannot_replace_replay_authority() {
    let fixture = Fixture::new();
    let store = Arc::new(EncryptedStore::open(fixture.config()).unwrap());
    let mut session = store.prepare_relocation(fixture.plan()).unwrap();
    session.quiesce().unwrap();
    session.copy().unwrap();
    session.reopen().unwrap();
    let old_journal = fs::read(journal(&fixture.old())).unwrap();
    let new_journal = fs::read(journal(&fixture.new_db())).unwrap();
    let old_bytes = fs::read(fixture.old()).unwrap();
    let new_bytes = fs::read(fixture.new_db()).unwrap();
    let old_key = fs::read(fixture.root.join("old-keys/primary")).unwrap();
    let new_key = fs::read(fixture.root.join("new-keys/primary")).unwrap();
    assert!(matches!(
        store.restore(&fixture.old(), &rightkit_db::RestoreOptions::default()),
        Err(LifecycleError::RecoveryRequired { .. })
    ));
    assert_eq!(fs::read(journal(&fixture.old())).unwrap(), old_journal);
    assert_eq!(fs::read(journal(&fixture.new_db())).unwrap(), new_journal);
    assert_eq!(fs::read(fixture.old()).unwrap(), old_bytes);
    assert_eq!(fs::read(fixture.new_db()).unwrap(), new_bytes);
    assert_eq!(
        fs::read(fixture.root.join("old-keys/primary")).unwrap(),
        old_key
    );
    assert_eq!(
        fs::read(fixture.root.join("new-keys/primary")).unwrap(),
        new_key
    );
    drop(session);
    drop(store);
    let mut replay = recover_relocation_session(fixture.config(), fixture.plan()).unwrap();
    assert_eq!(replay.store().db_path(), fixture.new_db());
    assert!(replay
        .settle(RelocationSettlement::Committed)
        .unwrap()
        .cleanup_retained
        .is_empty());
    assert!(!journal(&fixture.old()).exists());
    assert!(!journal(&fixture.new_db()).exists());
}

#[test]
fn completed_cleanup_is_idempotent_and_retains_mismatched_authority() {
    let fixture = Fixture::new();
    kill_after(&fixture.root, "activated");
    let saved = fixture.root.join("published-copy.db");
    fs::rename(fixture.new_db(), &saved).unwrap();
    private_write(&fixture.new_db(), b"foreign");
    let old_journal = fs::read(journal(&fixture.old())).unwrap();
    let new_journal = fs::read(journal(&fixture.new_db())).unwrap();
    assert!(recover_completed_relocation(fixture.config(), fixture.plan()).is_err());
    assert_eq!(fs::read(journal(&fixture.old())).unwrap(), old_journal);
    assert_eq!(fs::read(journal(&fixture.new_db())).unwrap(), new_journal);
    fs::remove_file(fixture.new_db()).unwrap();
    fs::rename(&saved, fixture.new_db()).unwrap();
    let report = recover_completed_relocation(fixture.config(), fixture.plan())
        .unwrap()
        .unwrap();
    assert!(report.cleanup_retained.is_empty(), "{report:?}");
    assert!(!journal(&fixture.old()).exists());
    assert!(!journal(&fixture.new_db()).exists());
    assert!(
        recover_completed_relocation(fixture.config(), fixture.plan())
            .unwrap()
            .is_none()
    );
    assert_eq!(value(&fixture.old()), 41);
    assert_eq!(value(&fixture.new_db()), 41);
}

#[test]
fn managed_pool_close_revokes_clones_and_lifecycle_never_resurrects_them() {
    let fixture = Fixture::new();
    let cfg = fixture
        .config()
        .with_drain_timeout(Duration::from_millis(10));
    let store = Arc::new(EncryptedStore::open(cfg).unwrap());
    let pool = store
        .read_pool(1, rightkit_db::ReadPoolOptions::new().with_read_only(true))
        .unwrap();
    let clone = pool.clone();
    let held = pool.get().unwrap();
    pool.close();
    pool.close();
    assert!(clone.get().is_err());
    let old_key = fs::read(fixture.root.join("old-keys/primary")).unwrap();
    assert!(
        !store
            .rotate_key(RotationRequest::new("cm90YXRpb24tdGVzdC1rZXk="))
            .is_committed(),
        "revoked but checked-out reader still blocks mutation"
    );
    assert_eq!(
        fs::read(fixture.root.join("old-keys/primary")).unwrap(),
        old_key
    );
    drop(held);
    assert!(store
        .rotate_key(RotationRequest::new("cm90YXRpb24tdGVzdC1rZXk="))
        .is_committed());
    assert!(pool.get().is_err());
    assert!(clone.get().is_err());
    let fresh = store
        .read_pool(1, rightkit_db::ReadPoolOptions::new().with_read_only(true))
        .unwrap();
    assert!(fresh.get().is_ok());
    drop(store);
    assert!(
        fresh.get().is_err(),
        "owner drop revokes every surviving pool handle"
    );
}

#[test]
fn managed_pool_close_and_wait_retains_timeout_then_drains_after_return() {
    let fixture = Fixture::new();
    let store = EncryptedStore::open(fixture.config()).unwrap();
    let pool = store
        .read_pool(1, rightkit_db::ReadPoolOptions::new().with_read_only(true))
        .unwrap();
    let clone = pool.clone();
    let held = pool.get().unwrap();
    let error = pool.close_and_wait(Duration::from_millis(10)).unwrap_err();
    assert!(error.is_busy());
    assert!(error
        .to_string()
        .contains("read pool still has readers checked out"));
    assert!(clone.get().is_err());
    assert_eq!(
        held.query_row("SELECT x FROM relocation_value", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        41
    );
    drop(held);
    clone.close_and_wait(Duration::from_millis(10)).unwrap();
    assert!(pool.get().is_err());
    store
        .with_writer(|conn| conn.execute_batch("VACUUM"))
        .unwrap()
        .unwrap();
    assert!(store
        .read_pool(1, rightkit_db::ReadPoolOptions::new().with_read_only(true))
        .unwrap()
        .get()
        .is_ok());
}

#[test]
fn reader_generation_remains_tracked_after_timeout_and_public_handle_drop() {
    for drop_handle in [false, true] {
        let fixture = Fixture::new();
        let cfg = fixture
            .config()
            .with_drain_timeout(Duration::from_millis(10));
        let store = EncryptedStore::open(cfg).unwrap();
        let pool = store
            .read_pool(1, rightkit_db::ReadPoolOptions::new().with_read_only(true))
            .unwrap();
        let held = pool.get().unwrap();
        let pool = if drop_handle {
            drop(pool);
            None
        } else {
            Some(pool)
        };
        let primary = fs::read(fixture.root.join("old-keys/primary")).unwrap();
        for _ in 0..2 {
            assert!(!store
                .rotate_key(RotationRequest::new("cm90YXRpb24tdGVzdC1rZXk="))
                .is_committed());
            assert_eq!(
                fs::read(fixture.root.join("old-keys/primary")).unwrap(),
                primary
            );
            assert_eq!(
                held.query_row("SELECT x FROM relocation_value", [], |r| r.get::<_, i64>(0))
                    .unwrap(),
                41
            );
        }
        drop(held);
        assert!(store
            .rotate_key(RotationRequest::new("cm90YXRpb24tdGVzdC1rZXk="))
            .is_committed());
        drop(pool);
    }
}

#[test]
fn checked_out_reader_keeps_shared_exclusion_after_store_and_pool_drop() {
    let fixture = Fixture::new();
    let mut cfg = fixture.config();
    cfg.transition.acquire_timeout = Duration::from_millis(10);
    let store = EncryptedStore::open(cfg.clone()).unwrap();
    let pool = store
        .read_pool(1, rightkit_db::ReadPoolOptions::new().with_read_only(true))
        .unwrap();
    let revoked_clone = pool.clone();
    let held = pool.get().unwrap();
    drop(pool);
    drop(store);
    assert!(revoked_clone.get().is_err());
    assert!(rightkit_db::encrypted_lifecycle::StoreTransition::begin(
        rightkit_db::encrypted_lifecycle::TransitionKind::Restore,
        &[fixture.old().as_path()],
        &cfg.transition
    )
    .is_err());
    drop(held);
    assert!(rightkit_db::encrypted_lifecycle::StoreTransition::begin(
        rightkit_db::encrypted_lifecycle::TransitionKind::Restore,
        &[fixture.old().as_path()],
        &cfg.transition
    )
    .is_ok());
    assert!(
        revoked_clone.get().is_err(),
        "surviving revoked handle holds no drained generation guard"
    );
}

#[test]
fn abort_after_real_filesystem_commit_retains_copy_and_resumes_only_source() {
    let fixture = Fixture::new();
    let store = Arc::new(EncryptedStore::open(fixture.config()).unwrap());
    let mut session = store.prepare_relocation(fixture.plan()).unwrap();
    session.quiesce().unwrap();
    let source_bytes = fs::read(fixture.old()).unwrap();
    session.copy().unwrap();
    assert_eq!(fs::read(fixture.old()).unwrap(), source_bytes);
    assert_eq!(
        store.db_path(),
        fixture.old(),
        "copy cannot select destination"
    );
    assert!(
        store.with_writer(|_| ()).is_err(),
        "copy cannot resume old writer"
    );
    assert_eq!(value(&fixture.new_db()), 41);
    let report = session.settle(RelocationSettlement::Aborted).unwrap();
    assert_eq!(
        report.copy_retained.as_deref(),
        Some(fixture.new_db().as_path())
    );
    assert!(report.cleanup_retained.is_empty(), "{report:?}");
    assert_eq!(store.db_path(), fixture.old());
    assert!(store.with_writer(|_| ()).is_ok());
    assert_eq!(fs::read(fixture.old()).unwrap(), source_bytes);
    let repeated = session.settle(RelocationSettlement::Aborted).unwrap();
    assert_eq!(repeated.copy_retained, report.copy_retained);
    assert_eq!(repeated.key_store_residue, report.key_store_residue);
    assert_eq!(repeated.cleanup_retained, report.cleanup_retained);
    drop(session);
    drop(store);
    assert_eq!(value(&fixture.old()), 41);
    assert_eq!(
        value(&fixture.new_db()),
        41,
        "abort must retain already committed copy"
    );
}

#[test]
fn candidate_validation_refusal_never_publishes_and_explicit_abort_resumes_source() {
    let fixture = Fixture::new();
    let old = fixture.old();
    let allowed = old.clone();
    let cfg = fixture.config().with_validate(move |options| {
        if options.path == allowed {
            Ok(())
        } else {
            Err("requested candidate validation refusal".into())
        }
    });
    let store = Arc::new(EncryptedStore::open(cfg).unwrap());
    let mut session = store.prepare_relocation(fixture.plan()).unwrap();
    session.quiesce().unwrap();
    let source_bytes = fs::read(&old).unwrap();
    assert!(matches!(
        session.copy(),
        Err(LifecycleError::ValidationFailed(_))
    ));
    assert!(
        !fixture.new_db().exists(),
        "unvalidated candidate cannot become final database"
    );
    assert_eq!(store.db_path(), old);
    assert!(store.with_writer(|_| ()).is_err());
    assert_eq!(fs::read(fixture.old()).unwrap(), source_bytes);
    let report = session.settle(RelocationSettlement::Aborted).unwrap();
    assert!(report.copy_retained.is_none());
    assert!(report.cleanup_retained.is_empty(), "{report:?}");
    let served: i64 = store
        .with_writer(|conn| conn.query_row("SELECT x FROM relocation_value", [], |row| row.get(0)))
        .unwrap()
        .unwrap();
    assert_eq!(served, 41);
    assert!(!fixture.new_db().exists());
    assert_eq!(fs::read(fixture.old()).unwrap(), source_bytes);
    assert!(!journal(&fixture.old()).exists());
    assert!(!journal(&fixture.new_db()).exists());
}

#[test]
fn existing_destination_is_typed_refusal_preserving_foreign_bytes_and_live_source() {
    let fixture = Fixture::new();
    private_write(&fixture.new_db(), b"unrelated-existing-destination");
    let store = Arc::new(EncryptedStore::open(fixture.config()).unwrap());
    let source_bytes = fs::read(fixture.old()).unwrap();
    assert!(matches!(store.prepare_relocation(fixture.plan()),
        Err(LifecycleError::DestinationExists { path }) if path == fixture.new_db()));
    assert_eq!(
        fs::read(fixture.new_db()).unwrap(),
        b"unrelated-existing-destination"
    );
    assert_eq!(fs::read(fixture.old()).unwrap(), source_bytes);
    assert_eq!(store.db_path(), fixture.old());
    let served: i64 = store
        .with_writer(|conn| conn.query_row("SELECT x FROM relocation_value", [], |row| row.get(0)))
        .unwrap()
        .unwrap();
    assert_eq!(served, 41);
    assert!(!journal(&fixture.old()).exists());
    assert!(!journal(&fixture.new_db()).exists());
}

#[test]
fn canonical_key_readback_refusal_after_publication_retains_copy_and_key_on_abort() {
    let fixture = Fixture::new();
    private_write(
        &fixture.root.join("new-keys/deny-read-after-promote"),
        b"deny",
    );
    let store = Arc::new(EncryptedStore::open(fixture.config()).unwrap());
    let mut session = store.prepare_relocation(fixture.plan()).unwrap();
    session.quiesce().unwrap();
    let source_bytes = fs::read(fixture.old()).unwrap();
    assert!(
        matches!(session.copy(), Err(LifecycleError::KeyStore { .. })),
        "canonical readback must be required after promotion"
    );
    assert_eq!(
        value(&fixture.new_db()),
        41,
        "filesystem publication really committed"
    );
    assert_eq!(
        fs::read(fixture.root.join("new-keys/primary")).unwrap(),
        KEY.as_bytes()
    );
    assert_eq!(fs::read(fixture.old()).unwrap(), source_bytes);
    assert!(store.with_writer(|_| ()).is_err());
    let old_journal_path = journal(&fixture.old());
    let new_journal_path = journal(&fixture.new_db());
    let old_journal = fs::read(&old_journal_path).unwrap();
    let new_journal = fs::read(&new_journal_path).unwrap();
    let report = session.settle(RelocationSettlement::Aborted).unwrap();
    assert_eq!(
        report.copy_retained.as_deref(),
        Some(fixture.new_db().as_path())
    );
    assert!(
        report.key_store_residue,
        "promoted key must be reported as retained residue"
    );
    assert_eq!(
        report.cleanup_retained,
        vec![old_journal_path.clone(), new_journal_path.clone()]
    );
    let explained: Vec<&PathBuf> = report.cleanup_errors.iter().map(|(path, _)| path).collect();
    assert_eq!(
        explained,
        report.cleanup_retained.iter().collect::<Vec<_>>(),
        "every retained path says why: {report:?}"
    );
    assert!(
        report.cleanup_errors.iter().all(|(_, why)| !why.is_empty()),
        "{report:?}"
    );
    assert_eq!(fs::read(&old_journal_path).unwrap(), old_journal);
    assert_eq!(fs::read(&new_journal_path).unwrap(), new_journal);
    assert!(store.with_writer(|_| ()).is_ok());
    assert_eq!(value(&fixture.new_db()), 41);
    assert_eq!(
        fs::read(fixture.root.join("new-keys/primary")).unwrap(),
        KEY.as_bytes()
    );
    assert_eq!(fs::read(fixture.old()).unwrap(), source_bytes);
}

/// A cleanup that is given up reports the error that stopped it with the path it kept, so a
/// recurrence names its cause instead of only listing paths.
#[test]
fn retained_cleanup_carries_the_error_that_kept_it() {
    let fixture = Fixture::new();
    let store = Arc::new(EncryptedStore::open(fixture.config()).unwrap());
    let mut session = store.prepare_relocation(fixture.plan()).unwrap();
    session.quiesce().unwrap();
    session.copy().unwrap();
    session.reopen().unwrap();
    let workspace = fs::read_dir(fixture.root.join("new"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".rkdb-relocate-"))
        })
        .expect("the relocation workspace exists until committed settlement");
    // A stray file keeps the workspace from being removed: an OS error on every platform.
    fs::write(workspace.join("stray"), b"x").unwrap();
    let report = session.settle(RelocationSettlement::Committed).unwrap();
    assert_eq!(
        report.cleanup_retained,
        vec![workspace.clone()],
        "{report:?}"
    );
    assert_eq!(report.cleanup_errors.len(), 1, "{report:?}");
    let (path, why) = &report.cleanup_errors[0];
    assert_eq!(path, &workspace);
    assert!(why.contains("os error"), "the OS error is carried: {why}");
    assert!(
        journal(&fixture.old()).exists(),
        "the journals stay while cleanup is incomplete"
    );
    assert_eq!(value(&fixture.new_db()), 41);
}

#[test]
fn changed_destination_identity_refuses_recovery_without_destroying_foreign_or_source_bytes() {
    let fixture = Fixture::new();
    kill_after(&fixture.root, "selected");
    let saved = fixture.root.join("published-copy.db");
    fs::rename(fixture.new_db(), &saved).unwrap();
    private_write(&fixture.new_db(), b"foreign-writer-bytes");
    let old_journal = fs::read(journal(&fixture.old())).unwrap();
    let new_journal = fs::read(journal(&fixture.new_db())).unwrap();
    let outcome = recover_relocation(
        fixture.config(),
        fixture.plan(),
        RelocationSettlement::Committed,
    );
    assert!(
        outcome.is_err(),
        "changed file identity must block activation"
    );
    assert_eq!(fs::read(fixture.new_db()).unwrap(), b"foreign-writer-bytes");
    assert_eq!(value(&saved), 41, "published identity remains recoverable");
    assert_eq!(value(&fixture.old()), 41);
    assert_eq!(fs::read(journal(&fixture.old())).unwrap(), old_journal);
    assert_eq!(fs::read(journal(&fixture.new_db())).unwrap(), new_journal);
}

#[test]
fn changed_destination_directory_identity_keeps_pending_journal_and_both_copies() {
    let fixture = Fixture::new();
    kill_after(&fixture.root, "selected");
    let saved = fixture.root.join("original-destination");
    fs::rename(fixture.root.join("new"), &saved).unwrap();
    fs::create_dir(fixture.root.join("new")).unwrap();
    private_write(
        &fixture.root.join("new/foreign"),
        b"foreign-directory-entry",
    );
    let old_journal = fs::read(journal(&fixture.old())).unwrap();
    assert!(recover_relocation(
        fixture.config(),
        fixture.plan(),
        RelocationSettlement::Committed
    )
    .is_err());
    assert_eq!(
        fs::read(fixture.root.join("new/foreign")).unwrap(),
        b"foreign-directory-entry"
    );
    assert_eq!(value(&saved.join("store.db")), 41);
    assert_eq!(value(&fixture.old()), 41);
    assert_eq!(fs::read(journal(&fixture.old())).unwrap(), old_journal);
    assert!(journal(&saved.join("store.db")).exists());
}

#[test]
fn failed_activation_blocks_old_writer_and_rotation_cannot_clear_relocation_journal() {
    let fixture = Fixture::new();
    let store = Arc::new(EncryptedStore::open(fixture.config()).unwrap());
    let mut session = store.prepare_relocation(fixture.plan()).unwrap();
    session.quiesce().unwrap();
    session.copy().unwrap();
    private_write(&fixture.root.join("selected-root"), b"new");
    let destination_keys = DiskKeys::new(fixture.root.join("new-keys"));
    destination_keys.readback_failure(true);
    assert!(session.reopen().is_err());
    assert!(matches!(
        store.with_writer(|_| ()),
        Err(LifecycleError::RecoveryRequired { .. })
    ));
    assert_eq!(store.db_path(), fixture.old());
    drop(session);
    let old_journal = fs::read(journal(&fixture.old())).unwrap();
    let new_journal = fs::read(journal(&fixture.new_db())).unwrap();
    let rotation = store.rotate_key(RotationRequest::new("cm90YXRpb24tdGVzdC1rZXk="));
    assert!(
        !rotation.is_committed(),
        "rotation cannot settle a relocation"
    );
    let recovery = recover_rotation(&fixture.config());
    assert!(
        matches!(
            &recovery.outcome,
            RecoveryOutcome::Blocked(BlockedReason::Journal(_))
        ),
        "{recovery:?}"
    );
    assert_eq!(fs::read(journal(&fixture.old())).unwrap(), old_journal);
    assert_eq!(fs::read(journal(&fixture.new_db())).unwrap(), new_journal);
    assert!(recover_relocation(
        fixture.config(),
        fixture.plan(),
        RelocationSettlement::Committed
    )
    .is_err());
    assert!(journal(&fixture.old()).exists());
    assert!(journal(&fixture.new_db()).exists());
    assert_eq!(value(&fixture.old()), 41);
    assert_eq!(value(&fixture.new_db()), 41);
    destination_keys.readback_failure(false);
    drop(store);
    let recovered = recover_relocation(
        fixture.config(),
        fixture.plan(),
        RelocationSettlement::Committed,
    )
    .unwrap();
    let active = recovered
        .store
        .expect("retry must reopen destination after key service recovers");
    assert_eq!(active.db_path(), fixture.new_db());
    assert!(active.with_writer(|_| ()).is_ok());
    assert!(!journal(&fixture.old()).exists());
    assert!(!journal(&fixture.new_db()).exists());
}

#[test]
fn optional_old_root_recovery_is_terminal_and_never_adopts_retained_copy() {
    let fixture = Fixture::new();
    assert!(recover_aborted_relocation(fixture.config(), fixture.plan())
        .unwrap()
        .is_none());
    kill_after(&fixture.root, "copied");
    let original = fs::read(fixture.old()).unwrap();
    let published = fs::read(fixture.new_db()).unwrap();
    let recovered = recover_aborted_relocation(fixture.config(), fixture.plan())
        .unwrap()
        .unwrap();
    assert!(
        recovered.report.cleanup_retained.is_empty(),
        "{:?}",
        recovered.report
    );
    assert_eq!(
        recovered.report.copy_retained.as_deref(),
        Some(fixture.new_db().as_path())
    );
    let store = Arc::new(recovered.store.expect("verified old-root store"));
    assert_eq!(store.db_path(), fixture.old());
    assert_eq!(fs::read(fixture.old()).unwrap(), original);
    assert_eq!(fs::read(fixture.new_db()).unwrap(), published);
    assert!(!journal(&fixture.old()).exists());
    assert!(!journal(&fixture.new_db()).exists());
    store
        .with_writer(|conn| conn.execute_batch("UPDATE relocation_value SET x=42"))
        .unwrap()
        .unwrap();
    drop(store);
    assert!(recover_aborted_relocation(fixture.config(), fixture.plan())
        .unwrap()
        .is_none());
    let store = Arc::new(EncryptedStore::open(fixture.config()).unwrap());
    assert_eq!(store.db_path(), fixture.old());
    assert!(matches!(
        store.prepare_relocation(fixture.plan()),
        Err(LifecycleError::DestinationExists { .. })
    ));
    assert_eq!(value(&fixture.old()), 42);
    assert_eq!(value(&fixture.new_db()), 41);
}

#[test]
fn optional_old_root_recovery_handles_prepublication_and_refuses_selected_destination() {
    let fixture = Fixture::new();
    kill_after(&fixture.root, "quiesced");
    let recovered = recover_aborted_relocation(fixture.config(), fixture.plan())
        .unwrap()
        .unwrap();
    assert!(recovered.report.cleanup_retained.is_empty());
    assert!(recovered.report.copy_retained.is_none());
    let store = recovered.store.expect("old root remains selected");
    assert_eq!(store.db_path(), fixture.old());
    assert!(!fixture.new_db().exists());
    drop(store);
    assert!(recover_aborted_relocation(fixture.config(), fixture.plan())
        .unwrap()
        .is_none());

    let selected = Fixture::new();
    kill_after(&selected.root, "activated");
    let old_journal = fs::read(journal(&selected.old())).unwrap();
    let new_journal = fs::read(journal(&selected.new_db())).unwrap();
    assert!(recover_aborted_relocation(selected.config(), selected.plan()).is_err());
    assert_eq!(fs::read(journal(&selected.old())).unwrap(), old_journal);
    assert_eq!(fs::read(journal(&selected.new_db())).unwrap(), new_journal);
}

#[test]
fn optional_old_root_recovery_refuses_replaced_publication_without_changing_authority() {
    let fixture = Fixture::new();
    kill_after(&fixture.root, "copied");
    let saved = fixture.root.join("verified-copy.db");
    fs::rename(fixture.new_db(), &saved).unwrap();
    private_write(&fixture.new_db(), b"foreign");
    let old_journal = fs::read(journal(&fixture.old())).unwrap();
    let new_journal = fs::read(journal(&fixture.new_db())).unwrap();
    assert!(recover_aborted_relocation(fixture.config(), fixture.plan()).is_err());
    assert_eq!(fs::read(journal(&fixture.old())).unwrap(), old_journal);
    assert_eq!(fs::read(journal(&fixture.new_db())).unwrap(), new_journal);
    assert_eq!(fs::read(fixture.new_db()).unwrap(), b"foreign");
    assert_eq!(value(&fixture.old()), 41);
}

#[test]
fn optional_old_root_recovery_does_not_treat_unreadable_journal_as_absence() {
    let fixture = Fixture::new();
    private_write(&journal(&fixture.old()), b"invalid relocation authority");
    let authority = fs::read(journal(&fixture.old())).unwrap();
    assert!(recover_aborted_relocation(fixture.config(), fixture.plan()).is_err());
    assert_eq!(fs::read(journal(&fixture.old())).unwrap(), authority);
    assert!(!fixture.new_db().exists());
    assert_eq!(value(&fixture.old()), 41);
}

/// Windows refuses a rename over a file, or a delete of it, with `ERROR_ACCESS_DENIED` (5) or
/// `ERROR_SHARING_VIOLATION` (32) while another handle holds it without delete sharing (an indexer,
/// a virus scanner). The hold ends by itself, so a relocation must ride it out instead of failing
/// or retaining its journals. These cases cannot be reproduced off Windows.
#[cfg(windows)]
mod windows_transient_refusals {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    /// `FILE_SHARE_READ | FILE_SHARE_WRITE`: everything but `FILE_SHARE_DELETE`.
    const SHARE_READ_WRITE: u32 = 0x1 | 0x2;
    const HOLD: Duration = Duration::from_millis(250);

    /// Holds `path` open without delete sharing for `HOLD`, then lets go.
    fn hold_without_delete_sharing(path: &Path) -> std::thread::JoinHandle<()> {
        let file = fs::OpenOptions::new()
            .read(true)
            .share_mode(SHARE_READ_WRITE)
            .open(path)
            .unwrap();
        std::thread::spawn(move || {
            std::thread::sleep(HOLD);
            drop(file);
        })
    }

    /// The candidate file inside the relocation workspace under the destination's directory.
    fn candidate(fixture: &Fixture) -> PathBuf {
        let workspace = fs::read_dir(fixture.root.join("new"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with(".rkdb-relocate-"))
            })
            .expect("the relocation workspace exists until committed settlement");
        workspace.join("candidate.db")
    }

    #[test]
    fn journal_replace_rides_out_a_holder_without_delete_sharing() {
        let fixture = Fixture::new();
        let store = Arc::new(EncryptedStore::open(fixture.config()).unwrap());
        let mut session = store.prepare_relocation(fixture.plan()).unwrap();
        session.quiesce().unwrap();
        let holder = hold_without_delete_sharing(&journal(&fixture.old()));
        session.copy().unwrap();
        holder.join().unwrap();
        session.reopen().unwrap();
        let report = session.settle(RelocationSettlement::Committed).unwrap();
        assert!(report.cleanup_retained.is_empty(), "{report:?}");
        assert!(!journal(&fixture.old()).exists());
        assert!(!journal(&fixture.new_db()).exists());
        assert_eq!(value(&fixture.new_db()), 41);
    }

    /// A holder that excludes every other reader makes the open that reads a file's identity fail
    /// with `ERROR_SHARING_VIOLATION` until it lets go. That is not a changed identity.
    #[test]
    fn root_identity_rides_out_a_holder_that_blocks_reading() {
        use rightkit_db::encrypted_lifecycle::RootIdentity;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("root.db");
        fs::write(&path, b"identity").unwrap();
        let before = RootIdentity::capture(&path);
        assert!(before.file.is_some(), "{before:?}");
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        let holder = std::thread::spawn(move || {
            std::thread::sleep(HOLD);
            drop(file);
        });
        let during = RootIdentity::capture(&path);
        holder.join().unwrap();
        assert_eq!(during, before);
    }

    #[test]
    fn committed_cleanup_rides_out_a_holder_without_delete_sharing() {
        let fixture = Fixture::new();
        let store = Arc::new(EncryptedStore::open(fixture.config()).unwrap());
        let mut session = store.prepare_relocation(fixture.plan()).unwrap();
        session.quiesce().unwrap();
        session.copy().unwrap();
        session.reopen().unwrap();
        let candidate = candidate(&fixture);
        let holder = hold_without_delete_sharing(&candidate);
        let report = session.settle(RelocationSettlement::Committed).unwrap();
        holder.join().unwrap();
        assert!(report.cleanup_retained.is_empty(), "{report:?}");
        assert!(!candidate.exists());
        assert!(!candidate.parent().unwrap().exists());
        assert!(!journal(&fixture.old()).exists());
        assert!(!journal(&fixture.new_db()).exists());
        assert_eq!(value(&fixture.new_db()), 41);
    }
}
