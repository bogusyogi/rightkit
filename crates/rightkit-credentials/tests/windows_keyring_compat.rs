#![cfg(windows)]
//! Byte-compat with the `keyring` crate's windows-native store (fake secrets only).

use keyring_core::api::CredentialStoreApi;
use keyring_core::Entry;
use rightkit_credentials::{system_backend, Error, Interaction, Namespace, Secret, WindowsTarget};

mod common;

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
    common::assert_test_name(&service, "dev.rightkit.secrets.compat.");
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
    if !common::real_user_stores_allowed("keyring_writes_rightkit_reads") {
        return;
    }
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
    if !common::real_user_stores_allowed("rightkit_writes_keyring_reads") {
        return;
    }
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

/// U+4142 is written as the two bytes `42 41`, which the old reader took for the UTF-8 text
/// "BA". The namespace selects the encoding now, so text is never guessed.
const AMBIGUOUS: &str = "\u{4142}\u{4344}";

#[test]
fn utf16_text_whose_bytes_are_also_valid_utf8_round_trips_both_ways() {
    if !common::real_user_stores_allowed(
        "utf16_text_whose_bytes_are_also_valid_utf8_round_trips_both_ways",
    ) {
        return;
    }
    let ns = namespace("ambiguous");
    let _c = Cleanup(ns.clone());
    let ni = Interaction::NonInteractive;
    system_backend()
        .set(&ns, ACCOUNT, &Secret::from_string(AMBIGUOUS), ni)
        .unwrap();
    let got = system_backend().get(&ns, ACCOUNT, ni).unwrap().unwrap();
    assert_eq!(got.expose_str().unwrap(), AMBIGUOUS);
    assert_eq!(keyring_entry(&ns).get_password().unwrap(), AMBIGUOUS);

    keyring_entry(&ns).set_password(AMBIGUOUS).unwrap();
    let got = system_backend().get(&ns, ACCOUNT, ni).unwrap().unwrap();
    assert_eq!(got.expose_str().unwrap(), AMBIGUOUS);
}

#[test]
fn a_secret_that_is_not_text_cannot_be_stored_in_the_keyring_layout() {
    if !common::real_user_stores_allowed(
        "a_secret_that_is_not_text_cannot_be_stored_in_the_keyring_layout",
    ) {
        return;
    }
    let ns = namespace("binary");
    let _c = Cleanup(ns.clone());
    let ni = Interaction::NonInteractive;
    let result = system_backend().set(
        &ns,
        ACCOUNT,
        &Secret::from_bytes(vec![0xff, 0xfe, 0xfd]),
        ni,
    );
    assert_eq!(result, Err(Error::NotUtf8));
    assert!(system_backend().get(&ns, ACCOUNT, ni).unwrap().is_none());
}

#[test]
fn legacy_utf8_blobs_are_read_through_the_explicit_migration_namespace_only() {
    if !common::real_user_stores_allowed(
        "legacy_utf8_blobs_are_read_through_the_explicit_migration_namespace_only",
    ) {
        return;
    }
    let compat = namespace("legacy");
    let _c = Cleanup(compat.clone());
    let legacy = Namespace::new(compat.service())
        .unwrap()
        .windows_target(WindowsTarget::KeyringCompatLegacyUtf8);
    let ni = Interaction::NonInteractive;
    // A raw UTF-8 blob of odd length, as rightkit-credentials 0.1.1 and 0.1.2 stored text.
    keyring_entry(&compat)
        .set_secret(b"legacy-utf8-key")
        .unwrap();

    // The keyring layout does not guess: a blob that is not UTF-16LE is an error.
    assert_eq!(
        system_backend().get(&compat, ACCOUNT, ni).map(|_| ()),
        Err(Error::NotUtf8)
    );
    // The explicit legacy view returns the raw bytes and cannot write.
    let raw = system_backend().get(&legacy, ACCOUNT, ni).unwrap().unwrap();
    assert_eq!(raw.expose_bytes(), b"legacy-utf8-key");
    assert!(matches!(
        system_backend().set(&legacy, ACCOUNT, &Secret::from_string("x"), ni),
        Err(Error::Invalid { .. })
    ));
    // Migration is an explicit read through the legacy view and a write through the real one.
    system_backend().set(&compat, ACCOUNT, &raw, ni).unwrap();
    let migrated = system_backend().get(&compat, ACCOUNT, ni).unwrap().unwrap();
    assert_eq!(migrated.expose_str().unwrap(), "legacy-utf8-key");
    assert_eq!(
        keyring_entry(&compat).get_password().unwrap(),
        "legacy-utf8-key"
    );
}
