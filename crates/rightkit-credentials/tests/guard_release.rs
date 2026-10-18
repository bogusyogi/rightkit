//! Guard behaviour that must hold in every profile, in particular release: the volatile
//! in-memory store (`MemoryBackend`) is not compiled into a release build, so a tripped guard
//! that asks for `GuardMode::Memory` resolves to `GuardMode::Deny` there. (`MemoryBackend`
//! tests live in `store.rs`, a debug-profile suite.)

use rightkit_credentials::{Config, Error, GuardMode, Namespace, Store};

#[test]
fn a_deny_guard_refuses_every_operation_without_touching_the_os_store() {
    let guard = format!("RK_SECRETS_TEST_DENY_PROFILE_{}", std::process::id());
    std::env::set_var(&guard, "1");
    let store = Store::open(
        Config::new(Namespace::new("dev.rightkit.secrets.test.guard-profile").unwrap())
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

#[cfg(not(debug_assertions))]
#[test]
fn an_env_memory_guard_denies_instead_of_using_volatile_memory_in_release_builds() {
    let guard = format!("RK_SECRETS_TEST_MEMORY_REL_{}", std::process::id());
    std::env::set_var(&guard, "1");
    let store = Store::open(
        Config::new(Namespace::new("dev.rightkit.secrets.test.guard-memory-rel").unwrap())
            .guard_env(&guard)
            .guard_mode(GuardMode::Memory),
    );
    assert!(store.guard_active());
    assert!(matches!(store.get("x"), Err(Error::Guarded { .. })));
    assert!(matches!(
        store.set_str("x", "v"),
        Err(Error::Guarded { .. })
    ));
    std::env::remove_var(&guard);
}

#[cfg(not(debug_assertions))]
#[test]
fn a_guard_forced_from_code_to_memory_denies_in_release_builds() {
    let store = Store::open(
        Config::new(Namespace::new("dev.rightkit.secrets.test.guard-forced-rel").unwrap())
            .guard_when(true, GuardMode::Memory),
    );
    assert!(store.guard_active());
    assert!(matches!(store.get("x"), Err(Error::Guarded { .. })));
    assert!(matches!(
        store.set_str("x", "v"),
        Err(Error::Guarded { .. })
    ));
}
