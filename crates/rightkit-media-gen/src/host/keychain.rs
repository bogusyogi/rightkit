//! The host OS credential store configuration, shared by the engine (provider keys, licence
//! record) and the shell (the per-install device id through `rightkit-device`).

/// A store for `service`: `<PREFIX>_NO_KEYCHAIN` denies every operation. There is no in-memory store
/// (rightkit-credentials 0.4.6): QA runs on the real store under the rightkit-qa snapshot and restore.
pub fn store_for(service: &str) -> rightkit_credentials::Store {
    let ns = rightkit_credentials::Namespace::new(service)
        .expect("keychain service is a fixed valid identifier");
    let cfg = rightkit_credentials::Config::new(ns)
        .guard_env(rightkit_local_gen::host::env_name("NO_KEYCHAIN"))
        .guard_mode(rightkit_credentials::GuardMode::Deny);
    rightkit_credentials::Store::open(cfg)
}

/// The host store (service = the host app identifier).
pub fn keychain_store() -> rightkit_credentials::Store {
    store_for(&rightkit_local_gen::host::config().app_identifier)
}

/// Keychain account holding the per-install device id (`rightkit-device`).
pub const DEVICE_ACCOUNT: &str = "license-device-id";
