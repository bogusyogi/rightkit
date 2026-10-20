#![cfg(all(feature = "db-adapter", debug_assertions))]

use std::sync::Arc;
use std::time::Duration;

use rightkit_credentials::db_adapter::CredentialsKeyStore;
use rightkit_credentials::{Config, Error, MemoryBackend, Namespace, Secret, Store};
use rightkit_db::encrypted_lifecycle::{Key, KeyStoreErrorKind, ProtectedKeyStore};

fn setup(name: &str) -> (Store, CredentialsKeyStore) {
    let store = Store::open(
        Config::new(Namespace::new(name).unwrap())
            .backend(Arc::new(MemoryBackend::new()))
            .timeout(Duration::from_millis(20)),
    );
    let adapter = CredentialsKeyStore::new(
        store.clone(),
        "db-key",
        "db-key.recovery",
        vec!["SQLCipher/DBKey.staging".into()],
    )
    .unwrap();
    (store, adapter)
}

#[test]
fn adapter_delegates_exact_pending_promotion_and_retains_recovery() {
    let (store, adapter) = setup("rightkit.test.db-adapter.rotation");
    store.set_str("db-key", "old-key").unwrap();
    let mut session = adapter.lock_rotation(Duration::from_millis(20)).unwrap();
    session.write_recovery(&Key::new("old-key".into())).unwrap();
    let revision = session.write_pending(&Key::new("new-key".into())).unwrap();
    assert!(!format!("{revision:?}").contains("new-key"));
    let pending = session.read_pending().unwrap().unwrap();
    assert_eq!(pending.revision, revision);
    assert_eq!(pending.key.as_str(), "new-key");
    assert!(matches!(
        store.begin_rotation("db-key", &Secret::from_string("racing-key")),
        Err(Error::InProgress { .. })
    ));
    session.promote_pending(&revision).unwrap();
    assert_eq!(session.read_primary().unwrap().unwrap().as_str(), "new-key");
    assert_eq!(
        session.read_recovery().unwrap().unwrap().as_str(),
        "old-key"
    );
    assert!(session.read_pending().unwrap().is_none());
    drop(session);
    assert!(store
        .begin_rotation("db-key", &Secret::from_string("later-key"))
        .unwrap());
    let mut session = adapter.lock_rotation(Duration::from_millis(20)).unwrap();
    assert!(session.promote_pending(&revision).is_err());
    assert!(session.discard_pending(&revision).is_err());
    assert_eq!(
        store
            .pending_rotation("db-key")
            .unwrap()
            .unwrap()
            .expose_str()
            .unwrap(),
        "later-key"
    );
}

#[test]
fn legacy_only_key_is_visible_and_unmapped_or_live_slots_cannot_be_deleted() {
    let (store, adapter) = setup("rightkit.test.db-adapter.legacy");
    store
        .set_str("SQLCipher/DBKey.staging", "legacy-key")
        .unwrap();
    let mut session = adapter.lock_rotation(Duration::from_millis(20)).unwrap();
    assert!(session.read_primary().unwrap().is_none());
    assert_eq!(
        session
            .read_legacy_staging("SQLCipher/DBKey.staging")
            .unwrap()
            .unwrap()
            .as_str(),
        "legacy-key"
    );
    assert!(session.delete_legacy_staging("db-key").is_err());
    assert!(CredentialsKeyStore::new(store.clone(), "db-key", "DB-KEY", vec![]).is_err());
    assert!(CredentialsKeyStore::new(
        store.clone(),
        "db-key",
        "recovery",
        vec!["DB-KEY.rotation-pending".into()]
    )
    .is_err());
    session
        .delete_legacy_staging("SQLCipher/DBKey.staging")
        .unwrap();
    assert!(session
        .read_legacy_staging("SQLCipher/DBKey.staging")
        .unwrap()
        .is_none());
}

#[test]
fn corrupt_slots_never_look_empty_and_rotation_lock_is_bounded() {
    let (store, adapter) = setup("rightkit.test.db-adapter.corrupt");
    store
        .set("db-key", &Secret::from_bytes(vec![0xff]))
        .unwrap();
    let mut session = adapter.lock_rotation(Duration::from_millis(20)).unwrap();
    assert_eq!(
        session.read_primary().unwrap_err().kind,
        KeyStoreErrorKind::Corrupt
    );
    store
        .set(
            "db-key.rotation-pending",
            &Secret::from_bytes(vec![0xff]),
        )
        .unwrap();
    assert_eq!(
        session.read_pending().unwrap_err().kind,
        KeyStoreErrorKind::Corrupt
    );
    assert!(adapter.lock_rotation(Duration::from_millis(1)).is_err());
    drop(session);
    assert!(adapter.lock_rotation(Duration::from_millis(20)).is_ok());
}

#[test]
fn owned_session_moves_threads_and_excludes_unrelated_callers_until_drop() {
    let (store, adapter) = setup("rightkit.test.db-adapter.capability");
    let adapter = Arc::new(adapter);
    store.set_str("db-key", "old-key").unwrap();
    let mut session = adapter.lock_rotation(Duration::from_millis(20)).unwrap();
    session.write_recovery(&Key::new("old-key".into())).unwrap();
    let revision = session.write_pending(&Key::new("new-key".into())).unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let moved = std::thread::spawn(move || {
        // No reacquisition or ambient identity lookup: capability itself crosses the thread.
        assert_eq!(session.read_pending().unwrap().unwrap().revision, revision);
        ready_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // Simulate interrupted lifecycle: preserve both viable keys & recovery on drop.
        assert_eq!(session.read_primary().unwrap().unwrap().as_str(), "old-key");
        assert_eq!(
            session.read_recovery().unwrap().unwrap().as_str(),
            "old-key"
        );
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let unrelated = Arc::clone(&adapter);
    std::thread::spawn(move || {
        // Without acquisition this caller has no API to read/delete the owner's pending slot.
        assert!(unrelated.lock_rotation(Duration::from_millis(1)).is_err());
    })
    .join()
    .unwrap();
    release_tx.send(()).unwrap();
    moved.join().unwrap();
    let mut recovered = adapter.lock_rotation(Duration::from_millis(20)).unwrap();
    assert_eq!(
        recovered.read_primary().unwrap().unwrap().as_str(),
        "old-key"
    );
    assert_eq!(
        recovered.read_pending().unwrap().unwrap().key.as_str(),
        "new-key"
    );
    assert_eq!(
        recovered.read_recovery().unwrap().unwrap().as_str(),
        "old-key"
    );
}
