#![cfg(feature = "credentials")]

use rightkit_credentials::{
    Backend, Config, Error, Interaction, MemoryBackend, Namespace, Result, Secret, Store,
};
use rightkit_device::{credentials::CredentialStore, DeviceService, SecureStore};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Barrier,
};

const ACCOUNT: &str = "device-id";

fn store(backend: Arc<dyn Backend>) -> Store {
    Store::open(Config::new(Namespace::new("rightkit-device-test").unwrap()).backend(backend))
}

/// Uses the real MemoryBackend atomic map operation, with an initial-read barrier
/// that forces both independent services to observe absence before either creates.
struct RacingBackend {
    memory: MemoryBackend,
    initial_reads: Barrier,
    reads: AtomicUsize,
    adds: AtomicUsize,
    sets: AtomicUsize,
}

impl RacingBackend {
    fn new() -> Self {
        Self {
            memory: MemoryBackend::new(),
            initial_reads: Barrier::new(2),
            reads: AtomicUsize::new(0),
            adds: AtomicUsize::new(0),
            sets: AtomicUsize::new(0),
        }
    }
}

impl Backend for RacingBackend {
    fn name(&self) -> &'static str {
        "device-race"
    }

    fn get(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<Option<Secret>> {
        let value = self.memory.get(ns, account, mode)?;
        if self.reads.fetch_add(1, Ordering::SeqCst) < 2 {
            self.initial_reads.wait();
        }
        Ok(value)
    }

    fn exists(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool> {
        self.memory.exists(ns, account, mode)
    }

    fn set(&self, ns: &Namespace, account: &str, secret: &Secret, mode: Interaction) -> Result<()> {
        self.sets.fetch_add(1, Ordering::SeqCst);
        self.memory.set(ns, account, secret, mode)
    }

    fn add_if_absent(
        &self,
        ns: &Namespace,
        account: &str,
        secret: &Secret,
        mode: Interaction,
    ) -> Result<bool> {
        self.adds.fetch_add(1, Ordering::SeqCst);
        self.memory.add_if_absent(ns, account, secret, mode)
    }

    fn delete(&self, ns: &Namespace, account: &str, mode: Interaction) -> Result<bool> {
        self.memory.delete(ns, account, mode)
    }
}

#[test]
fn independent_services_race_through_atomic_add_without_overwrite() {
    let backend = Arc::new(RacingBackend::new());
    let first = DeviceService::new(CredentialStore::new(store(backend.clone()), ACCOUNT).unwrap());
    let second = DeviceService::new(CredentialStore::new(store(backend.clone()), ACCOUNT).unwrap());
    let first = std::thread::spawn(move || first.get_or_create().unwrap());
    let second = std::thread::spawn(move || second.get_or_create().unwrap());
    let (first, second) = (first.join().unwrap(), second.join().unwrap());

    assert_eq!(first, second);
    assert_eq!(backend.adds.load(Ordering::SeqCst), 2);
    assert_eq!(backend.sets.load(Ordering::SeqCst), 0);
    assert_eq!(
        store(backend)
            .get(ACCOUNT)
            .unwrap()
            .unwrap()
            .expose_str()
            .unwrap(),
        first
    );
}

#[test]
fn explicit_replace_delete_and_create_only_keep_distinct_semantics() {
    let raw = store(Arc::new(MemoryBackend::new()));
    let adapter = CredentialStore::new(raw.clone(), ACCOUNT).unwrap();
    assert_eq!(adapter.get().unwrap(), None);
    assert!(adapter.set_if_absent(&"winner".into()).unwrap());
    assert!(!adapter.set_if_absent(&"loser".into()).unwrap());
    assert_eq!(adapter.get().unwrap().as_deref(), Some("winner"));
    adapter.set(&"replacement".into()).unwrap();
    assert_eq!(adapter.get().unwrap().as_deref(), Some("replacement"));
    adapter.delete().unwrap();
    adapter.delete().unwrap();
    assert!(raw.get(ACCOUNT).unwrap().is_none());
}

#[test]
fn malformed_accounts_and_values_fail_without_echoing_inputs_or_replacing() {
    let backend = Arc::new(MemoryBackend::new());
    let raw = store(backend.clone());
    for account in ["".to_string(), "private\naccount".into(), "x".repeat(251)] {
        let error = CredentialStore::new(raw.clone(), account).err().unwrap();
        assert!(matches!(error, Error::Invalid { .. }));
        assert!(!error.to_string().contains("private"));
    }
    let adapter = CredentialStore::new(raw.clone(), ACCOUNT).unwrap();
    adapter.set(&"winner".into()).unwrap();
    for value in ["", "private\nvalue", "private\0value"] {
        for error in [
            adapter.set(&value.into()).unwrap_err(),
            adapter.set_if_absent(&value.into()).unwrap_err(),
        ] {
            assert!(!error.contains("private"));
        }
    }
    assert_eq!(adapter.get().unwrap().as_deref(), Some("winner"));
    for value in [vec![0xff], Vec::new(), b"private\nvalue".to_vec()] {
        // Simulate malformed legacy records, including empty bytes Store refuses to create.
        backend
            .set(
                raw.namespace(),
                ACCOUNT,
                &Secret::from_bytes(value),
                Interaction::NonInteractive,
            )
            .unwrap();
        let error = adapter.get().unwrap_err();
        assert!(!error.contains("private"));
        assert!(DeviceService::new(adapter.clone()).get_or_create().is_err());
    }
}

/// Changing the default after construction must not redirect any operation.
struct SwitchingBackend {
    first: Arc<MemoryBackend>,
    second: Arc<MemoryBackend>,
    pins: AtomicUsize,
}

struct FixedBackend(Arc<MemoryBackend>);

impl Backend for FixedBackend {
    fn name(&self) -> &'static str {
        "fixed-device-test"
    }
    fn exclusion_identity(&self) -> Option<String> {
        Some(format!("fixed-device-test:{:p}", Arc::as_ptr(&self.0)))
    }
    fn get(&self, ns: &Namespace, a: &str, m: Interaction) -> Result<Option<Secret>> {
        self.0.get(ns, a, m)
    }
    fn exists(&self, ns: &Namespace, a: &str, m: Interaction) -> Result<bool> {
        self.0.exists(ns, a, m)
    }
    fn set(&self, ns: &Namespace, a: &str, s: &Secret, m: Interaction) -> Result<()> {
        self.0.set(ns, a, s, m)
    }
    fn add_if_absent(&self, ns: &Namespace, a: &str, s: &Secret, m: Interaction) -> Result<bool> {
        self.0.add_if_absent(ns, a, s, m)
    }
    fn delete(&self, ns: &Namespace, a: &str, m: Interaction) -> Result<bool> {
        self.0.delete(ns, a, m)
    }
}

impl Backend for SwitchingBackend {
    fn name(&self) -> &'static str {
        "switching-device-test"
    }
    fn pinned(&self) -> Result<Option<Arc<dyn Backend>>> {
        let memory = if self.pins.fetch_add(1, Ordering::SeqCst) == 0 {
            self.first.clone()
        } else {
            self.second.clone()
        };
        Ok(Some(Arc::new(FixedBackend(memory))))
    }
    fn get(&self, _: &Namespace, _: &str, _: Interaction) -> Result<Option<Secret>> {
        panic!("adapter read an unpinned backend")
    }
    fn exists(&self, _: &Namespace, _: &str, _: Interaction) -> Result<bool> {
        panic!("adapter queried an unpinned backend")
    }
    fn set(&self, _: &Namespace, _: &str, _: &Secret, _: Interaction) -> Result<()> {
        panic!("adapter wrote an unpinned backend")
    }
    fn add_if_absent(&self, _: &Namespace, _: &str, _: &Secret, _: Interaction) -> Result<bool> {
        panic!("adapter created in an unpinned backend")
    }
    fn delete(&self, _: &Namespace, _: &str, _: Interaction) -> Result<bool> {
        panic!("adapter deleted from an unpinned backend")
    }
}

#[test]
fn backend_is_pinned_once_for_reads_creates_replacements_and_deletes() {
    let first = Arc::new(MemoryBackend::new());
    let second = Arc::new(MemoryBackend::new());
    let changing = Arc::new(SwitchingBackend {
        first: first.clone(),
        second: second.clone(),
        pins: AtomicUsize::new(0),
    });
    let adapter = CredentialStore::new(store(changing.clone()), ACCOUNT).unwrap();
    let device = DeviceService::new(adapter.clone());
    let id = device.get_or_create().unwrap();
    assert_eq!(device.get().unwrap(), Some(id));
    assert!(!adapter.set_if_absent(&"loser".into()).unwrap());
    adapter.set(&"replacement".into()).unwrap();
    assert_eq!(
        store(first.clone())
            .get(ACCOUNT)
            .unwrap()
            .unwrap()
            .expose_str()
            .unwrap(),
        "replacement"
    );
    adapter.delete().unwrap();
    assert!(!store(first).exists(ACCOUNT).unwrap());
    assert!(!store(second).exists(ACCOUNT).unwrap());
    assert_eq!(changing.pins.load(Ordering::SeqCst), 1);
}

#[test]
fn unavailable_store_never_becomes_absence() {
    let raw = store(Arc::new(rightkit_credentials::UnavailableBackend(
        "offline",
    )));
    let adapter = CredentialStore::new(raw, ACCOUNT).unwrap();
    assert!(adapter.get().is_err());
    assert!(adapter.set(&"value".into()).is_err());
    assert!(adapter.set_if_absent(&"value".into()).is_err());
    assert!(adapter.delete().is_err());
    assert!(DeviceService::new(adapter).get_or_create().is_err());
}

#[cfg(feature = "license-client")]
mod license_source {
    use super::*;
    use rightkit_license::{DeviceIdError, DeviceIdSource};

    fn source(store: Store) -> Arc<dyn DeviceIdSource> {
        // The licence client takes `Arc<dyn DeviceIdSource>`: the service must be Send + Sync.
        Arc::new(DeviceService::new(
            CredentialStore::new(store, ACCOUNT).unwrap(),
        ))
    }

    #[test]
    fn device_service_is_a_device_id_source() {
        let raw = store(Arc::new(MemoryBackend::new()));
        let source = source(raw.clone());
        let id = source.device_id().unwrap();
        assert_eq!(source.device_id().unwrap(), id, "one id, created once");
        // The hash is the lowercase hex SHA-256 of that id, the only form the lease routes get.
        let hash = source.device_id_hash().unwrap();
        assert_eq!(hash, rightkit_device::device_id_hash(&id));
        assert_eq!(hash.len(), 64);
        assert!(hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        assert_ne!(hash, id);
        // The id is the one the credential store holds, and a second service over the same store agrees.
        let stored = CredentialStore::new(raw.clone(), ACCOUNT)
            .unwrap()
            .get()
            .unwrap();
        assert_eq!(stored.as_deref(), Some(id.as_str()));
        assert_eq!(self::source(raw).device_id().unwrap(), id);
    }

    #[test]
    fn an_unavailable_store_is_an_error_and_never_a_fresh_device_id() {
        let raw = store(Arc::new(rightkit_credentials::UnavailableBackend(
            "offline",
        )));
        let source = source(raw);
        assert_eq!(source.device_id(), Err(DeviceIdError));
        assert_eq!(source.device_id_hash(), Err(DeviceIdError));
    }

    #[test]
    fn a_stored_value_that_is_not_a_device_id_fails_closed_instead_of_being_replaced() {
        let raw = store(Arc::new(MemoryBackend::new()));
        raw.set(ACCOUNT, &Secret::from_string("bad\nvalue"))
            .unwrap();
        let source = source(raw.clone());
        assert_eq!(source.device_id(), Err(DeviceIdError));
        let kept = raw.get(ACCOUNT).unwrap().unwrap();
        assert_eq!(
            kept.expose_str().unwrap(),
            "bad\nvalue",
            "the malformed value is left alone"
        );
    }
}
