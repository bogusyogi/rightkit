#![cfg(all(windows, feature = "db-adapter", debug_assertions))]
//! A real Credential Manager promotion completes only after its Store caller timed out.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rightkit_credentials::db_adapter::CredentialsKeyStore;
use rightkit_credentials::{
    system_backend, Backend, Config, Interaction, Namespace, Result, Secret, Store,
};
use rightkit_db::encrypted_lifecycle::{Key, KeyStoreErrorKind, ProtectedKeyStore};

mod common;

/// Delay one native primary write inside the backend call, after Store owns its mutation
/// permit/file hold. Reads deliberately bypass this gate, matching Credential Manager.
struct DelayedNativeWrite {
    inner: Arc<dyn Backend>,
    armed: AtomicBool,
    release: Mutex<mpsc::Receiver<()>>,
    entered: mpsc::Sender<()>,
    finished: mpsc::Sender<()>,
}

impl Backend for DelayedNativeWrite {
    fn name(&self) -> &'static str {
        "delayed-native-credential-manager"
    }

    fn exclusion_identity(&self) -> Option<String> {
        self.inner.exclusion_identity()
    }

    fn credential_identity(&self, ns: &Namespace, account: &str) -> Result<String> {
        self.inner.credential_identity(ns, account)
    }

    fn durable(&self) -> bool {
        self.inner.durable()
    }

    fn get(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<Option<Secret>> {
        self.inner.get(ns, account, mode)
    }

    fn exists(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool> {
        self.inner.exists(ns, account, mode)
    }

    fn set(&self, ns: &Namespace, account: &str, value: &Secret, mode: Interaction) -> Result<()> {
        let delayed = account == "db-key" && self.armed.swap(false, Ordering::SeqCst);
        if delayed {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }
        let result = self.inner.set(ns, account, value, mode);
        if delayed {
            self.finished.send(()).unwrap();
        }
        result
    }

    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        value: &Secret,
        mode: Interaction,
    ) -> Result<bool> {
        self.inner.add_if_absent(ns, account, value, mode)
    }

    fn delete(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool> {
        self.inner.delete(ns, account, mode)
    }
}

struct Cleanup(Namespace);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for account in ["db-key", "db-key.rotation-pending", "db-key.recovery"] {
            let _ = system_backend().delete(&self.0, account, Interaction::NonInteractive);
        }
    }
}

#[test]
fn late_native_promotion_is_unsettled_and_blocks_cleanup_and_new_sessions() {
    if !common::real_user_stores_allowed(
        "late_native_promotion_is_unsettled_and_blocks_cleanup_and_new_sessions",
    ) {
        return;
    }
    let service = format!(
        "rk-secrets-db-late-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    common::assert_test_name(&service, "rk-secrets-");
    let ns = Namespace::new(service).unwrap();
    let _cleanup = Cleanup(ns.clone());
    let (release, release_rx) = mpsc::channel();
    let (entered_tx, entered) = mpsc::channel();
    let (finished_tx, finished) = mpsc::channel();
    let backend = Arc::new(DelayedNativeWrite {
        inner: system_backend(),
        armed: AtomicBool::new(false),
        release: Mutex::new(release_rx),
        entered: entered_tx,
        finished: finished_tx,
    });
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(
        Config::new(ns)
            .backend(backend.clone())
            .rotation_lock_dir(dir.path())
            .timeout(Duration::from_secs(1)),
    );
    store.set_str("db-key", "old-key").unwrap();
    let adapter =
        CredentialsKeyStore::new(store.clone(), "db-key", "db-key.recovery", vec![]).unwrap();
    let mut session = adapter.lock_rotation(Duration::from_secs(1)).unwrap();
    session.write_recovery(&Key::new("old-key".into())).unwrap();
    let revision = session.write_pending(&Key::new("new-key".into())).unwrap();
    backend.armed.store(true, Ordering::SeqCst);
    assert_eq!(
        session.promote_pending(&revision).unwrap_err().kind,
        KeyStoreErrorKind::Unsettled
    );
    entered.recv_timeout(Duration::from_secs(1)).unwrap();

    // This is the dangerous native observation: the old primary remains readable while the
    // write is still running. Adapter must reject it as settlement evidence.
    assert_eq!(
        store.get("db-key").unwrap().unwrap().expose_str().unwrap(),
        "old-key"
    );
    assert_eq!(
        session.read_primary().unwrap_err().kind,
        KeyStoreErrorKind::Unsettled
    );
    assert_eq!(
        session.clear_recovery().unwrap_err().kind,
        KeyStoreErrorKind::Unsettled
    );
    assert_eq!(
        session.discard_pending(&revision).unwrap_err().kind,
        KeyStoreErrorKind::Unsettled
    );
    drop(session);
    assert!(adapter.lock_rotation(Duration::from_millis(20)).is_err());

    release.send(()).unwrap();
    finished.recv_timeout(Duration::from_secs(5)).unwrap();
    // Acquiring the native rotation file fence waits until Store drops the actual worker's
    // permit/hold, rather than assuming the backend's finished notification settled ownership.
    let mut recovered = adapter.lock_rotation(Duration::from_secs(2)).unwrap();
    assert_eq!(
        recovered.read_primary().unwrap().unwrap().as_str(),
        "new-key"
    );
    assert_eq!(
        recovered.read_pending().unwrap().unwrap().key.as_str(),
        "new-key"
    );
    assert_eq!(
        recovered.read_recovery().unwrap().unwrap().as_str(),
        "old-key"
    );
    recovered.promote_pending(&revision).unwrap();
    assert!(recovered.read_pending().unwrap().is_none());
    assert_eq!(
        recovered.read_recovery().unwrap().unwrap().as_str(),
        "old-key"
    );
}
