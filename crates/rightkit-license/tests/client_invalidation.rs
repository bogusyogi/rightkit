#![cfg(feature = "client-testing")]
//! Native entitlement invalidation: every licence change reaches live engine verifiers at once,
//! through the attached handle, instead of after the cache TTL.

mod client_support;

use std::sync::{Arc, Mutex};

use client_support::*;
use rightkit_license::{
    CredentialTokenSource, EngineEntitlementError, EngineEntitlements, EngineEntitlementsConfig,
    LicenseClient, LicenseInvalidation, LicenseSetup, SecretString, ViewStatus,
};
use std::sync::atomic::Ordering::SeqCst;

fn key() -> SecretString {
    SecretString::new(KEY)
}

/// An engine verifier over the client's record that caches a verdict for five minutes.
fn engine(f: &Fixture, invalidation: Option<&LicenseInvalidation>) -> EngineEntitlements {
    let config = EngineEntitlementsConfig::new(common::pubkey(), APP, f.dir.path(), || {
        Ok(DEVICE.to_string())
    })
    .with_token_source(CredentialTokenSource::from_record_store(
        f.store.clone(),
        ACCOUNT,
        f.license_dir(),
    ))
    .with_cache_ttl_secs(300);
    let engine = EngineEntitlements::new(config);
    match invalidation {
        Some(invalidation) => engine.with_invalidation(invalidation),
        None => engine,
    }
}

#[test]
fn a_deactivation_reaches_a_cached_engine_verdict_at_once() {
    let f = fixture();
    let client = f.client();
    let attached = engine(&f, Some(&client.invalidation()));
    let detached = engine(&f, None);

    client.activate(key()).unwrap();
    attached.require_pro().expect("an active licence");
    detached.require_pro().expect("an active licence");

    client.deactivate().unwrap();
    // The attached verifier never serves a verdict from before the change.
    assert_eq!(
        attached.require_pro().unwrap_err(),
        EngineEntitlementError::NoLicense
    );
    // Without the handle the cached verdict stands until its TTL: the gap the handle closes.
    detached.require_pro().expect("the cache has not been told");

    // A new activation is seen at once as well.
    client.activate(key()).unwrap();
    attached.require_pro().expect("the new licence");
}

#[test]
fn a_rebuilt_engine_attaches_the_same_handle_and_every_change_is_seen() {
    let f = fixture();
    let client = f.client();
    let first = engine(&f, Some(&client.invalidation()));
    client.activate(key()).unwrap();
    first.require_pro().unwrap();

    // The engine restarts: a new verifier, the same handle, nothing carried over.
    drop(first);
    let second = engine(&f, Some(&client.invalidation()));
    second.require_pro().unwrap();
    client.deactivate().unwrap();
    assert_eq!(
        second.require_pro().unwrap_err(),
        EngineEntitlementError::NoLicense
    );
}

#[test]
fn every_committed_change_signals_once_and_reads_signal_nothing() {
    let f = fixture();
    let client = f.client();
    let invalidation = client.invalidation();
    let seen = Arc::new(Mutex::new(Vec::new()));
    {
        let (seen, client) = (seen.clone(), client.clone());
        // A listener may call back into the client: no lock is held when it runs.
        invalidation.subscribe(move |generation| {
            let status = client.status().status;
            seen.lock().unwrap().push((generation, status));
        });
    }
    assert_eq!(invalidation.generation(), 0);

    // Reads and refusals before any commit signal nothing.
    client.status();
    client.request_trial("a@b.co").unwrap();
    assert!(client.activate(SecretString::new("WRONG-KEY")).is_err());
    assert_eq!(invalidation.generation(), 0);

    client.activate(key()).unwrap();
    assert_eq!(invalidation.generation(), 1);
    client.status();
    assert_eq!(invalidation.generation(), 1);
    client.refresh().unwrap();
    assert_eq!(invalidation.generation(), 2);
    // An attempt that fails to write still signals: the stored record may not be the one the
    // verifiers last read, and a re-read only ever makes a check stricter.
    f.backend.fail_primary_set.store(true, SeqCst);
    assert!(client.refresh().is_err());
    f.backend.fail_primary_set.store(false, SeqCst);
    assert_eq!(invalidation.generation(), 3);
    client.deactivate().unwrap();
    assert_eq!(invalidation.generation(), 4);

    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen.iter()
            .map(|(generation, _)| *generation)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    // The listener saw the committed state, not a half-written one.
    assert_eq!(seen[0].1, ViewStatus::Active);
    assert_eq!(seen[3].1, ViewStatus::None);
}

#[test]
fn a_leftover_write_resolved_by_a_status_read_signals_too() {
    let f = fixture();
    let client = f.client();
    let invalidation = client.invalidation();
    // A crashed deactivation left its tombstone marker behind.
    f.leave_marker(r#"{"rkv":1,"revision":"r1","state":"tombstone"}"#);
    assert_eq!(invalidation.generation(), 0);
    let view = client.status();
    assert!(!view.unresolved, "{view:?}");
    assert_eq!(invalidation.generation(), 1);
    assert!(f.backend.text(MARKER).is_none());
}

#[test]
fn a_config_handle_is_the_one_the_client_signals() {
    let f = fixture();
    let handle = LicenseInvalidation::new();
    let client = f.client_with(|c| c.with_invalidation(handle.clone()));
    client.activate(key()).unwrap();
    assert_eq!(handle.generation(), 1);
    assert_eq!(client.invalidation().generation(), 1);
}

#[test]
fn a_disabled_client_never_signals() {
    let client = LicenseClient::new(LicenseSetup::Disabled, Arc::new(FixedDevice)).unwrap();
    assert!(client.activate(key()).is_err());
    assert!(client.deactivate().is_err());
    assert_eq!(client.status().status, ViewStatus::None);
    assert_eq!(client.invalidation().generation(), 0);
}
