//! The opt-in rebuild of a database its key cannot open: policy switches, the order mint, persist,
//! quarantine, create, and a reopen after a simulated death at every step.
#![cfg(all(feature = "encrypted-lifecycle", feature = "cipher"))]

use rightkit_db::encrypted_lifecycle::{
    EncryptedStore, EncryptedStoreConfig, Faults, Key, KeyStoreError, LifecycleError, PendingKey,
    PendingRevision, ProtectedKeySession, ProtectedKeyStore, RebuildPolicy, RebuildReason,
    RebuildStep,
};
use rightkit_db::DbOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const OLD_KEY: &str = "oldkeyoldkeyoldkeyoldkeyoldkey01";
const OTHER_KEY: &str = "otherkeyotherkeyotherkeyother002";
const NEW_KEY: &str = "newkeynewkeynewkeynewkeynewkey03";

// ----- an in-memory key store ---------------------------------------------------------------

#[derive(Default)]
struct Slots {
    primary: Option<String>,
    pending: Option<(String, String)>,
    recovery: Option<String>,
    next: u32,
}

#[derive(Clone, Default)]
struct MemKeys(Arc<Mutex<Slots>>);

impl MemKeys {
    fn primary(&self) -> Option<String> {
        self.0.lock().unwrap().primary.clone()
    }
    fn set_primary(&self, key: &str) {
        self.0.lock().unwrap().primary = Some(key.to_string());
    }
}

struct Session(Arc<Mutex<Slots>>);

impl ProtectedKeyStore for MemKeys {
    fn lock_rotation(
        &self,
        _timeout: Duration,
    ) -> Result<Box<dyn ProtectedKeySession + Send + 'static>, KeyStoreError> {
        Ok(Box::new(Session(self.0.clone())))
    }
}

impl ProtectedKeySession for Session {
    fn read_primary(&mut self) -> Result<Option<Key>, KeyStoreError> {
        Ok(self.0.lock().unwrap().primary.clone().map(Key::new))
    }
    fn read_pending(&mut self) -> Result<Option<PendingKey>, KeyStoreError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .pending
            .clone()
            .map(|(key, revision)| PendingKey {
                key: Key::new(key),
                revision: PendingRevision(revision),
            }))
    }
    fn write_pending(&mut self, key: &Key) -> Result<PendingRevision, KeyStoreError> {
        let mut slots = self.0.lock().unwrap();
        slots.next += 1;
        let revision = format!("r{}", slots.next);
        slots.pending = Some((key.to_string(), revision.clone()));
        Ok(PendingRevision(revision))
    }
    fn promote_pending(&mut self, revision: &PendingRevision) -> Result<(), KeyStoreError> {
        let mut slots = self.0.lock().unwrap();
        match slots.pending.take() {
            Some((key, current)) if current == revision.0 => {
                slots.primary = Some(key);
                Ok(())
            }
            other => {
                slots.pending = other;
                Err(KeyStoreError::other("no such pending revision"))
            }
        }
    }
    fn discard_pending(&mut self, revision: &PendingRevision) -> Result<(), KeyStoreError> {
        let mut slots = self.0.lock().unwrap();
        if slots
            .pending
            .as_ref()
            .is_some_and(|(_, current)| *current == revision.0)
        {
            slots.pending = None;
        }
        Ok(())
    }
    fn read_legacy_staging(&mut self, _alias: &str) -> Result<Option<Key>, KeyStoreError> {
        Ok(None)
    }
    fn delete_legacy_staging(&mut self, _alias: &str) -> Result<(), KeyStoreError> {
        Ok(())
    }
    fn read_recovery(&mut self) -> Result<Option<Key>, KeyStoreError> {
        Ok(self.0.lock().unwrap().recovery.clone().map(Key::new))
    }
    fn write_recovery(&mut self, key: &Key) -> Result<(), KeyStoreError> {
        self.0.lock().unwrap().recovery = Some(key.to_string());
        Ok(())
    }
    fn clear_recovery(&mut self) -> Result<(), KeyStoreError> {
        self.0.lock().unwrap().recovery = None;
        Ok(())
    }
}

// ----- fixtures -----------------------------------------------------------------------------

struct World {
    dir: PathBuf,
    db: PathBuf,
    keys: MemKeys,
    mints: Arc<AtomicUsize>,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn options_for(path: &Path, key: &str) -> DbOptions {
    let mut options = DbOptions::new(path);
    options.key = Some(key.to_string().into());
    options
}

/// A database under `OLD_KEY` holding one row, and a key store holding `primary`.
fn world(tag: &str, primary: Option<&str>) -> World {
    let dir = std::env::temp_dir().join(format!("rk-db-rebuild-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("store.db");
    let conn = options_for(&db, OLD_KEY).open_writer().unwrap();
    conn.execute_batch("CREATE TABLE t(v TEXT); INSERT INTO t VALUES ('precious');")
        .unwrap();
    drop(conn);
    let keys = MemKeys::default();
    if let Some(primary) = primary {
        keys.set_primary(primary);
    }
    World {
        dir,
        db,
        keys,
        mints: Arc::new(AtomicUsize::new(0)),
    }
}

impl World {
    fn policy(&self, absent: bool, rejected: bool) -> RebuildPolicy {
        let mints = self.mints.clone();
        RebuildPolicy::new(move || {
            mints.fetch_add(1, Ordering::SeqCst);
            Ok(Key::new(NEW_KEY.to_string()))
        })
        .with_key_absent(absent)
        .with_key_rejected(rejected)
    }

    fn config(&self, policy: Option<RebuildPolicy>) -> EncryptedStoreConfig {
        let mut config =
            EncryptedStoreConfig::new(DbOptions::new(&self.db), Arc::new(self.keys.clone()));
        config.transition.acquire_timeout = Duration::from_millis(500);
        match policy {
            Some(policy) => config.with_rebuild(policy),
            None => config,
        }
    }

    fn quarantined_mains(&self) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                name.starts_with("store.db.corrupt-")
                    && !name.ends_with("-wal")
                    && !name.ends_with("-shm")
                    && !name.ends_with("-journal")
            })
            .collect();
        found.sort();
        found
    }

    fn leftovers(&self) -> Vec<String> {
        std::fs::read_dir(&self.dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with("-journal") && name.starts_with("store.db."))
            .filter(|name| name != "store.db-journal")
            .collect()
    }
}

fn row(path: &Path, key: &str) -> Option<String> {
    let conn = options_for(path, key).open_existing_writer().ok()?;
    conn.query_row("SELECT v FROM t", [], |row| row.get::<_, String>(0))
        .ok()
}

fn is_empty_new_db(path: &Path, key: &str) -> bool {
    match options_for(path, key).open_existing_writer() {
        Ok(conn) => conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| {
                row.get::<_, i64>(0)
            })
            .is_ok_and(|count| count == 0),
        Err(_) => false,
    }
}

/// The old data is reachable: at the original path, or in exactly one quarantined file.
fn old_data_survives(world: &World) -> bool {
    if row(&world.db, OLD_KEY).as_deref() == Some("precious") {
        return true;
    }
    let moved = world.quarantined_mains();
    moved.len() == 1 && row(&moved[0], OLD_KEY).as_deref() == Some("precious")
}

struct DieAfter(RebuildStep);

impl Faults for DieAfter {
    fn die_rebuild_after(&self, step: RebuildStep) -> bool {
        step == self.0
    }
}

const STEPS: [RebuildStep; 7] = [
    RebuildStep::Journaled,
    RebuildStep::Minted,
    RebuildStep::Persisted,
    RebuildStep::QuarantineRecorded,
    RebuildStep::MainMoved,
    RebuildStep::Quarantined,
    RebuildStep::Created,
];

// ----- without a policy ---------------------------------------------------------------------

#[test]
fn without_a_policy_nothing_is_created_quarantined_or_replaced() {
    let w = world("nopolicy", None);
    let error = EncryptedStore::open(w.config(None)).err().expect("fails");
    assert!(matches!(error, LifecycleError::KeyMissing), "{error:?}");
    w.keys.set_primary(OTHER_KEY);
    let error = EncryptedStore::open(w.config(None)).err().expect("fails");
    assert!(
        matches!(error, LifecycleError::NoKeyOpens { .. }),
        "{error:?}"
    );
    assert_eq!(row(&w.db, OLD_KEY).as_deref(), Some("precious"));
    assert!(w.quarantined_mains().is_empty() && w.leftovers().is_empty());
    assert_eq!(w.keys.primary().as_deref(), Some(OTHER_KEY));
}

#[test]
fn a_policy_with_every_reason_off_changes_nothing() {
    let w = world("alloff", None);
    let error = EncryptedStore::open(w.config(Some(w.policy(false, false))))
        .err()
        .expect("fails");
    assert!(matches!(error, LifecycleError::KeyMissing), "{error:?}");
    assert!(w.quarantined_mains().is_empty() && w.leftovers().is_empty());
    assert_eq!(w.mints.load(Ordering::SeqCst), 0);
    assert_eq!(w.keys.primary(), None);
}

// ----- the rebuild --------------------------------------------------------------------------

#[test]
fn key_absent_mints_persists_quarantines_and_creates() {
    let w = world("absent", None);
    let store = EncryptedStore::open(w.config(Some(w.policy(true, false)))).unwrap();
    let report = store.rebuilt().expect("rebuilt").clone();
    assert_eq!(report.reason, RebuildReason::KeyAbsent);
    assert!(report.minted_key);
    let moved = report.quarantined.expect("path");
    assert_eq!(row(&moved, OLD_KEY).as_deref(), Some("precious"));
    assert_eq!(w.keys.primary().as_deref(), Some(NEW_KEY));
    drop(store);
    assert!(is_empty_new_db(&w.db, NEW_KEY));
    assert!(w.leftovers().is_empty(), "{:?}", w.leftovers());
    // The next open is an ordinary one.
    let store = EncryptedStore::open(w.config(Some(w.policy(true, false)))).unwrap();
    assert!(store.rebuilt().is_none());
}

#[test]
fn key_rejected_reuses_the_stored_key() {
    let w = world("rejected", Some(OTHER_KEY));
    let store = EncryptedStore::open(w.config(Some(w.policy(false, true)))).unwrap();
    let report = store.rebuilt().expect("rebuilt").clone();
    assert_eq!(report.reason, RebuildReason::KeyRejected);
    assert!(!report.minted_key);
    assert_eq!(w.mints.load(Ordering::SeqCst), 0);
    assert_eq!(w.keys.primary().as_deref(), Some(OTHER_KEY));
    assert_eq!(
        row(&report.quarantined.unwrap(), OLD_KEY).as_deref(),
        Some("precious")
    );
    drop(store);
    assert!(is_empty_new_db(&w.db, OTHER_KEY));
}

#[test]
fn each_reason_has_its_own_switch() {
    let w = world("switches", None);
    let error = EncryptedStore::open(w.config(Some(w.policy(false, true))))
        .err()
        .expect("absent is off");
    assert!(matches!(error, LifecycleError::KeyMissing), "{error:?}");
    w.keys.set_primary(OTHER_KEY);
    let error = EncryptedStore::open(w.config(Some(w.policy(true, false))))
        .err()
        .expect("rejected is off");
    assert!(
        matches!(error, LifecycleError::NoKeyOpens { .. }),
        "{error:?}"
    );
    assert_eq!(row(&w.db, OLD_KEY).as_deref(), Some("precious"));
    assert!(w.quarantined_mains().is_empty() && w.leftovers().is_empty());
}

#[test]
fn a_failing_mint_changes_nothing() {
    let w = world("mintfail", None);
    let policy = RebuildPolicy::new(|| Err("no entropy".into())).with_key_absent(true);
    let error = EncryptedStore::open(w.config(Some(policy)))
        .err()
        .expect("fails");
    assert!(
        matches!(error, LifecycleError::InvalidConfig(_)),
        "{error:?}"
    );
    assert_eq!(row(&w.db, OLD_KEY).as_deref(), Some("precious"));
    assert_eq!(w.keys.primary(), None);
}

#[test]
fn an_unresolved_transition_journal_blocks_the_rebuild() {
    let w = world("transition", None);
    std::fs::write(
        PathBuf::from(format!("{}.transition-journal", w.db.display())),
        b"not a journal we can read",
    )
    .unwrap();
    let _ = EncryptedStore::open(w.config(Some(w.policy(true, true))));
    assert_eq!(row(&w.db, OLD_KEY).as_deref(), Some("precious"));
    assert!(w.quarantined_mains().is_empty());
    assert_eq!(w.keys.primary(), None);
}

// ----- a crash at every step, then reopen ---------------------------------------------------

fn crash_then_reopen(tag: &str, primary: Option<&str>, absent: bool, rejected: bool) {
    for step in STEPS {
        // A stored key that is reused is never minted.
        if primary.is_some() && step == RebuildStep::Minted {
            continue;
        }
        let w = world(&format!("{tag}-{step:?}"), primary);
        let died = EncryptedStore::open_with_faults(
            w.config(Some(w.policy(absent, rejected))),
            &DieAfter(step),
        )
        .err()
        .unwrap_or_else(|| panic!("{step:?}: the simulated death must stop the open"));
        assert!(
            matches!(
                died,
                LifecycleError::Precondition(_) | LifecycleError::Db(_)
            ),
            "{step:?}: {died:?}"
        );
        assert!(
            old_data_survives(&w),
            "{step:?}: the old data is lost after the crash"
        );
        // The process restarts with the same configuration.
        let store = EncryptedStore::open(w.config(Some(w.policy(absent, rejected))))
            .unwrap_or_else(|error| panic!("{step:?}: reopen failed: {error:?}"));
        drop(store);
        let moved = w.quarantined_mains();
        assert_eq!(moved.len(), 1, "{step:?}: {moved:?}");
        assert_eq!(
            row(&moved[0], OLD_KEY).as_deref(),
            Some("precious"),
            "{step:?}"
        );
        let key = w.keys.primary().expect("a primary key is stored");
        assert!(is_empty_new_db(&w.db, &key), "{step:?}: no new database");
        assert!(w.leftovers().is_empty(), "{step:?}: {:?}", w.leftovers());
        // And it stays settled.
        let store = EncryptedStore::open(w.config(Some(w.policy(absent, rejected)))).unwrap();
        assert!(store.rebuilt().is_none(), "{step:?}");
    }
}

#[test]
fn a_crash_at_any_step_of_a_key_absent_rebuild_settles_on_reopen() {
    crash_then_reopen("crash-absent", None, true, true);
}

#[test]
fn a_crash_at_any_step_of_a_key_rejected_rebuild_settles_on_reopen() {
    crash_then_reopen("crash-rejected", Some(OTHER_KEY), true, true);
}

#[test]
fn the_intent_record_finishes_a_rebuild_whose_reason_switch_is_off() {
    // Only key-absent is enabled; the crash after the key was stored leaves a key that does not
    // open the file. The record the rebuild kept lets the next open finish it anyway.
    crash_then_reopen("crash-absent-only", None, true, false);
}

#[test]
fn a_crash_with_wal_sidecars_moves_them_all_aside() {
    let w = world("wal", None);
    // A live writer leaves `-wal` and `-shm` beside the file while the "process" dies.
    let conn = options_for(&w.db, OLD_KEY).open_writer().unwrap();
    conn.execute_batch("INSERT INTO t VALUES ('in the wal');")
        .unwrap();
    std::mem::forget(conn);
    for step in [RebuildStep::MainMoved, RebuildStep::QuarantineRecorded] {
        let _ =
            EncryptedStore::open_with_faults(w.config(Some(w.policy(true, true))), &DieAfter(step));
        let store = EncryptedStore::open(w.config(Some(w.policy(true, true))));
        if let Ok(store) = store {
            drop(store);
            break;
        }
    }
    assert!(is_empty_new_db(&w.db, w.keys.primary().unwrap().as_str()));
    let names: Vec<String> = std::fs::read_dir(&w.dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    // No old sidecar is left to be replayed into the new database, and none was deleted.
    let moved = w.quarantined_mains();
    assert_eq!(moved.len(), 1, "{names:?}");
    assert!(row(&moved[0], OLD_KEY).is_some(), "{names:?}");
}
