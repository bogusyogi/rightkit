#![cfg(target_os = "linux")]
//! Live round trip against the user's Secret Service (fake secrets only). Runs only where real
//! user stores are allowed (CI or `RIGHTKIT_TEST_REAL_USER_STORES=1`) and skips, passing, when no
//! Secret Service is reachable or its default collection is locked.

use rightkit_credentials::{system_backend, Error, Interaction, Namespace, Secret};

mod common;

const ACCOUNT: &str = "linux-probe";
const FAKE: &str = "fake-secret caf\u{e9} \u{1f511}";
const NI: Interaction = Interaction::NonInteractive;

struct Cleanup(Namespace);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = system_backend().delete(&self.0, ACCOUNT, NI);
    }
}

/// `None` (with a printed reason) when this machine has no usable Secret Service.
fn setup(test: &str) -> Option<(Namespace, Cleanup)> {
    if !common::real_user_stores_allowed(test) {
        return None;
    }
    let service = format!("dev.rightkit.secrets.linux.{test}.{}", std::process::id());
    common::assert_test_name(&service, "dev.rightkit.secrets.linux.");
    let ns = Namespace::new(service).unwrap();
    match system_backend().exists(&ns, ACCOUNT, NI) {
        Ok(_) => {}
        Err(Error::Unavailable(_)) | Err(Error::InteractionRequired) => {
            println!("skipped {test}: no usable unlocked Secret Service");
            return None;
        }
        Err(e) => panic!("unexpected error probing the Secret Service: {e:?}"),
    }
    Some((ns.clone(), Cleanup(ns)))
}

#[test]
fn set_get_exists_delete_round_trip() {
    let Some((ns, _c)) = setup("round_trip") else {
        return;
    };
    let b = system_backend();
    assert!(!b.exists(&ns, ACCOUNT, NI).unwrap());
    b.set(&ns, ACCOUNT, &Secret::from_string(FAKE), NI).unwrap();
    assert!(b.exists(&ns, ACCOUNT, NI).unwrap());
    let got = b.get(&ns, ACCOUNT, NI).unwrap().expect("present");
    assert_eq!(got.expose_str().unwrap(), FAKE);
    b.set(&ns, ACCOUNT, &Secret::from_string("second"), NI)
        .unwrap();
    assert_eq!(
        b.get(&ns, ACCOUNT, NI)
            .unwrap()
            .unwrap()
            .expose_str()
            .unwrap(),
        "second"
    );
    assert!(b.delete(&ns, ACCOUNT, NI).unwrap());
    assert!(!b.delete(&ns, ACCOUNT, NI).unwrap());
    assert!(b.get(&ns, ACCOUNT, NI).unwrap().is_none());
}

#[test]
fn add_if_absent_does_not_overwrite() {
    let Some((ns, _c)) = setup("add_if_absent") else {
        return;
    };
    let b = system_backend();
    assert!(b
        .add_if_absent(&ns, ACCOUNT, &Secret::from_string("first"), NI)
        .unwrap());
    assert!(!b
        .add_if_absent(&ns, ACCOUNT, &Secret::from_string("other"), NI)
        .unwrap());
    assert_eq!(
        b.get(&ns, ACCOUNT, NI)
            .unwrap()
            .unwrap()
            .expose_str()
            .unwrap(),
        "first"
    );
}
