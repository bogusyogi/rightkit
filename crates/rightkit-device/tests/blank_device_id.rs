//! A blank stored device id is absent, and a non-blank one is never replaced.
#![cfg(feature = "credentials")]

use rightkit_credentials::{
    Backend, Config, Interaction, Namespace, Result as CredResult, Secret, Store,
};
use rightkit_device::credentials::CredentialStore;
use rightkit_device::{DeviceService, SecureStore};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Memory(Mutex<BTreeMap<String, Vec<u8>>>);

impl Backend for Memory {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn get(&self, _: &Namespace, account: &str, _: Interaction) -> CredResult<Option<Secret>> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(account)
            .cloned()
            .map(Secret::from_bytes))
    }
    fn exists(&self, _: &Namespace, account: &str, _: Interaction) -> CredResult<bool> {
        Ok(self.0.lock().unwrap().contains_key(account))
    }
    fn set(&self, _: &Namespace, account: &str, s: &Secret, _: Interaction) -> CredResult<()> {
        self.0
            .lock()
            .unwrap()
            .insert(account.into(), s.expose_bytes().to_vec());
        Ok(())
    }
    fn add_if_absent(
        &self,
        _: &Namespace,
        account: &str,
        s: &Secret,
        _: Interaction,
    ) -> CredResult<bool> {
        let mut map = self.0.lock().unwrap();
        if map.contains_key(account) {
            return Ok(false);
        }
        map.insert(account.into(), s.expose_bytes().to_vec());
        Ok(true)
    }
    fn delete(&self, _: &Namespace, account: &str, _: Interaction) -> CredResult<bool> {
        Ok(self.0.lock().unwrap().remove(account).is_some())
    }
}

fn store_with(initial: Option<&str>) -> CredentialStore {
    let backend = Arc::new(Memory::default());
    if let Some(value) = initial {
        backend
            .0
            .lock()
            .unwrap()
            .insert("device".into(), value.as_bytes().to_vec());
    }
    let config = Config::new(Namespace::new("app.example.test").unwrap()).backend(backend);
    CredentialStore::new(Store::open(config), "device").unwrap()
}

#[test]
fn blank_values_read_as_absent_and_are_replaced() {
    for blank in ["", " ", "  \t\n "] {
        let store = store_with(Some(blank));
        assert_eq!(store.get().unwrap(), None, "{blank:?}");
        let service = DeviceService::new(store);
        let id = service.get_or_create().unwrap();
        assert!(!id.trim().is_empty());
        assert_eq!(
            service.get_or_create().unwrap(),
            id,
            "stable after replacement"
        );
    }
}

#[test]
fn a_non_blank_value_is_never_replaced() {
    let store = store_with(Some("  existing-id  "));
    assert!(!store.set_if_absent(&"new-id".to_string()).unwrap());
    assert_eq!(store.get().unwrap().as_deref(), Some("  existing-id  "));
    let service = DeviceService::new(store);
    assert_eq!(service.get_or_create().unwrap(), "  existing-id  ");
}

#[test]
fn control_characters_stay_an_error() {
    let store = store_with(Some("abc\u{7}def"));
    assert!(store.get().is_err());
    assert!(store.set_if_absent(&"new-id".to_string()).is_err());
    assert!(DeviceService::new(store).get_or_create().is_err());
}
