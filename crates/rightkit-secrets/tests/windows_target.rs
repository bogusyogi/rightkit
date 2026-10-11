#![cfg(windows)]
//! Account isolation on Windows: a Credential Manager target name ends at its first NUL, so a
//! prefix containing one must never reach the OS. Uses uniquely named test credentials that the
//! test creates and removes.

use rightkit_secrets::{system_backend, Error, Interaction, Namespace, Secret, WindowsTarget};
use std::time::{SystemTime, UNIX_EPOCH};

struct Cleanup(Namespace, &'static str);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = system_backend().delete(&self.0, self.1, Interaction::NonInteractive);
    }
}

fn slash(service: &str, prefix: String) -> Namespace {
    Namespace::new(service)
        .unwrap()
        .windows_target(WindowsTarget::Slash {
            prefix: Some(prefix),
        })
}

#[test]
fn a_prefix_with_nul_cannot_reach_the_credential_it_would_truncate_to() {
    let base = format!(
        "rk-secrets-nul-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let ni = Interaction::NonInteractive;
    let backend = system_backend();

    // The victim is the credential `<base>/victim`.
    let victim = slash("dev.rightkit.secrets.nul.victim", base.clone());
    let _cleanup = Cleanup(victim.clone(), "victim");
    backend
        .set(&victim, "victim", &Secret::from_string("keep-me"), ni)
        .unwrap();

    // Windows would read `<base>/victim\0ignored/any` as `<base>/victim`.
    let attacker = slash(
        "dev.rightkit.secrets.nul.attacker",
        format!("{base}/victim\0ignored"),
    );
    assert!(matches!(
        backend.get(&attacker, "any", ni),
        Err(Error::Invalid { .. })
    ));
    assert!(matches!(
        backend.exists(&attacker, "any", ni),
        Err(Error::Invalid { .. })
    ));
    assert!(matches!(
        backend.set(&attacker, "any", &Secret::from_string("overwritten"), ni),
        Err(Error::Invalid { .. })
    ));
    assert!(matches!(
        backend.delete(&attacker, "any", ni),
        Err(Error::Invalid { .. })
    ));

    let still_there = backend.get(&victim, "victim", ni).unwrap().unwrap();
    assert_eq!(still_there.expose_str().unwrap(), "keep-me");
}

#[test]
fn services_sharing_a_prefix_see_one_credential_and_conditional_delete_is_atomic() {
    let base = format!(
        "rk-secrets-shared-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let ni = Interaction::NonInteractive;
    let backend = system_backend();
    let a = slash("dev.rightkit.secrets.shared.a", base.clone());
    let b = slash("dev.rightkit.secrets.shared.b", base);
    let _cleanup = Cleanup(a.clone(), "acct");

    assert!(backend
        .add_if_absent(&a, "acct", &Secret::from_string("first"), ni)
        .unwrap());
    // The same native target: the second service sees it as present.
    assert!(!backend
        .add_if_absent(&b, "acct", &Secret::from_string("second"), ni)
        .unwrap());
    // A conditional delete of a stale value removes nothing.
    assert_eq!(
        backend
            .delete_if_equals(&b, "acct", &Secret::from_string("stale"), ni)
            .unwrap(),
        rightkit_secrets::ConditionalDelete::Changed
    );
    assert_eq!(
        backend
            .delete_if_equals(&b, "acct", &Secret::from_string("first"), ni)
            .unwrap(),
        rightkit_secrets::ConditionalDelete::Deleted
    );
    assert!(backend.get(&a, "acct", ni).unwrap().is_none());
}
