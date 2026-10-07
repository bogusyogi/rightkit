//! Store behaviour on every platform (memory backend and guards) and a real Windows Credential
//! Manager round trip under a unique, cleaned-up test namespace.

use std::sync::Arc;

use rightkit_secrets::{Config, Error, GuardMode, MemoryBackend, Namespace, Secret, Store};

fn memory_store(service: &str) -> Store {
    Store::open(
        Config::new(Namespace::new(service).unwrap()).backend(Arc::new(MemoryBackend::new())),
    )
}

#[test]
fn set_get_exists_delete_round_trip() {
    let store = memory_store("dev.rightkit.secrets.test.roundtrip");
    assert!(store.get("acct").unwrap().is_none());
    store.set_str("acct", "value-1").unwrap();
    assert!(store.exists("acct").unwrap());
    assert_eq!(
        store.get("acct").unwrap().unwrap().expose_str().unwrap(),
        "value-1"
    );
    assert!(store.delete("acct").unwrap());
    assert!(!store.delete("acct").unwrap());
    assert!(store.get("acct").unwrap().is_none());
}

#[test]
fn add_if_absent_and_get_or_create_key_never_overwrite() {
    let store = memory_store("dev.rightkit.secrets.test.create");
    assert!(store
        .add_if_absent("a", &Secret::from_string("first"))
        .unwrap());
    assert!(!store
        .add_if_absent("a", &Secret::from_string("second"))
        .unwrap());
    assert_eq!(
        store.get("a").unwrap().unwrap().expose_str().unwrap(),
        "first"
    );
    let (k1, created1) = store.get_or_create_key("k", 32).unwrap();
    let (k2, created2) = store.get_or_create_key("k", 32).unwrap();
    assert!(created1 && !created2);
    assert_eq!(k1.len(), 44, "32 random bytes, stored base64");
    assert!(k1.ct_eq(&k2));
}

#[test]
fn a_deny_guard_refuses_without_touching_the_os_store() {
    let guard = format!("RK_SECRETS_TEST_DENY_{}", std::process::id());
    std::env::set_var(&guard, "1");
    let store = Store::open(
        Config::new(Namespace::new("dev.rightkit.secrets.test.guard").unwrap())
            .guard_env(&guard)
            .guard_mode(GuardMode::Deny),
    );
    assert!(store.guard_active());
    assert!(matches!(store.get("x"), Err(Error::Guarded { .. })));
    assert!(matches!(
        store.set_str("x", "v"),
        Err(Error::Guarded { .. })
    ));
    std::env::remove_var(&guard);
}

#[test]
fn secret_debug_output_is_redacted() {
    let s = Secret::from_string("do-not-print-me");
    assert!(!format!("{s:?}").contains("do-not-print-me"));
}

#[cfg(windows)]
#[test]
fn windows_credential_manager_real_round_trip() {
    let service = format!("dev.rightkit.secrets.e2e.{}", std::process::id());
    let store = Store::open(
        Config::new(Namespace::new(&service).unwrap()).backend(rightkit_secrets::system_backend()),
    );
    let _ = store.delete("probe");
    store.set_str("probe", "credential-manager-value").unwrap();
    assert_eq!(
        store.get("probe").unwrap().unwrap().expose_str().unwrap(),
        "credential-manager-value"
    );
    assert!(!store
        .add_if_absent("probe", &Secret::from_string("other"))
        .unwrap());
    store.set_str("probe", "replaced").unwrap();
    assert_eq!(
        store.get("probe").unwrap().unwrap().expose_str().unwrap(),
        "replaced"
    );
    assert!(store.delete("probe").unwrap());
    assert!(store.get("probe").unwrap().is_none());
}
