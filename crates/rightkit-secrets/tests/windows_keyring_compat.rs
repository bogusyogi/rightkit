#![cfg(windows)]
//! Byte-compat with the `keyring` crate's windows-native store (fake secrets only).

use keyring_core::api::CredentialStoreApi;
use keyring_core::Entry;
use rightkit_secrets::{system_backend, Interaction, Namespace, Secret, WindowsTarget};

const ACCOUNT: &str = "compat-probe";
const FAKE: &str = "fake-secret caf\u{e9} \u{1f511}";

struct Cleanup(Namespace);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = system_backend().delete(&self.0, ACCOUNT, Interaction::NonInteractive);
    }
}

fn namespace(tag: &str) -> Namespace {
    let service = format!("dev.rightkit.secrets.compat.{tag}.{}", std::process::id());
    Namespace::new(service)
        .unwrap()
        .windows_target(WindowsTarget::KeyringCompat)
}

fn keyring_entry(ns: &Namespace) -> Entry {
    let store = windows_native_keyring_store::Store::new().unwrap();
    store.build(ns.service(), ACCOUNT, None).unwrap()
}

#[test]
fn keyring_writes_rightkit_reads() {
    let ns = namespace("k2r");
    let _c = Cleanup(ns.clone());
    keyring_entry(&ns).set_password(FAKE).unwrap();
    let got = system_backend()
        .get(&ns, ACCOUNT, Interaction::NonInteractive)
        .unwrap()
        .expect("present");
    assert_eq!(got.expose_str().unwrap(), FAKE);
}

#[test]
fn rightkit_writes_keyring_reads() {
    let ns = namespace("r2k");
    let _c = Cleanup(ns.clone());
    system_backend()
        .set(
            &ns,
            ACCOUNT,
            &Secret::from_string(FAKE),
            Interaction::NonInteractive,
        )
        .unwrap();
    assert_eq!(keyring_entry(&ns).get_password().unwrap(), FAKE);
}
