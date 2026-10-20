#![cfg(feature = "client-testing")]
//! The engine reader against the native client's record: the envelope (a tombstone is no
//! licence), the older bare record, and the fencing (the operation lock, a rotation marker, a
//! write still in flight) all fail closed.

mod client_support;

use std::sync::atomic::Ordering::SeqCst;
use std::time::{Duration, Instant};

use client_support::*;
use rightkit_license::{
    CredentialTokenSource, EngineEntitlementError, EngineEntitlements, EngineEntitlementsConfig,
    SecretString, TokenSource, ENGINE_BUSY_TEXT, ENGINE_UNRESOLVED_TEXT,
};

fn key() -> SecretString {
    SecretString::new(KEY)
}

fn source(f: &Fixture) -> CredentialTokenSource {
    CredentialTokenSource::from_record_store(f.store.clone(), ACCOUNT, f.license_dir())
}

fn engine(f: &Fixture) -> EngineEntitlements {
    let config = EngineEntitlementsConfig::new(common::pubkey(), APP, f.dir.path(), || {
        Ok(DEVICE.to_string())
    })
    .with_token_source(source(f))
    .with_cache_ttl_secs(0);
    EngineEntitlements::new(config)
}

fn unreadable(error: EngineEntitlementError) -> String {
    match error {
        EngineEntitlementError::Unreadable(text) => text,
        other => panic!("expected Unreadable, got {other:?}"),
    }
}

#[test]
fn the_engine_reads_the_envelope_and_a_tombstone_is_no_licence() {
    let f = fixture();
    let client = f.client();
    let engine = engine(&f);
    // Nothing stored: no licence.
    assert_eq!(
        engine.require_pro().unwrap_err(),
        EngineEntitlementError::NoLicense
    );

    client.activate(key()).unwrap();
    let entitlement = engine
        .require_pro()
        .expect("an active envelope verifies in the engine");
    assert_eq!(entitlement.id, LICENSE_ID);

    client.deactivate().unwrap();
    assert_eq!(
        engine.require_pro().unwrap_err(),
        EngineEntitlementError::NoLicense
    );
    assert_eq!(f.backend.envelope().unwrap()["state"], "tombstone");
}

#[test]
fn an_older_bare_record_still_reads_as_active() {
    let f = fixture();
    let token = {
        let mut claims = common::Claims::term(now() - 100, now() + 3600);
        claims.id = LICENSE_ID;
        claims.token()
    };
    f.backend.put_text(
        ACCOUNT,
        &format!(r#"{{"status":"active","deviceId":"{DEVICE}","signedToken":"{token}"}}"#),
    );
    assert!(engine(&f).require_pro().is_ok());
    // A record the server marked invalid grants nothing, token or not.
    f.backend.put_text(
        ACCOUNT,
        &format!(r#"{{"status":"invalid","deviceId":"{DEVICE}","signedToken":"{token}"}}"#),
    );
    assert_eq!(
        engine(&f).require_pro().unwrap_err(),
        EngineEntitlementError::NoLicense
    );
    // A record-shaped text that does not parse is unreadable, never "no licence".
    f.backend.put_text(ACCOUNT, "{not json");
    assert!(unreadable(engine(&f).require_pro().unwrap_err()).starts_with("stored licence record"));
    // Text that is not a record is the older contract: a bare token. This one is not a valid
    // token, so it is refused as malformed (fail closed), still never an entitlement.
    f.backend.put_text(ACCOUNT, "not json");
    assert!(matches!(
        engine(&f).require_pro().unwrap_err(),
        EngineEntitlementError::Invalid(_)
    ));
}

#[test]
fn a_host_reader_closure_returning_the_record_text_is_understood_too() {
    // The existing public API: a reader closure. It may now return the whole stored record.
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    let backend = f.backend.clone();
    let config = EngineEntitlementsConfig::new(common::pubkey(), APP, f.dir.path(), || {
        Ok(DEVICE.to_string())
    })
    .with_credential_reader(move || Ok(backend.text(ACCOUNT)))
    .with_cache_ttl_secs(0);
    let engine = EngineEntitlements::new(config);
    assert!(engine.require_pro().is_ok());
    client.deactivate().unwrap();
    assert_eq!(
        engine.require_pro().unwrap_err(),
        EngineEntitlementError::NoLicense
    );
}

#[test]
fn a_rotation_marker_makes_the_engine_busy_unresolved_and_never_entitled() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    let engine = engine(&f);
    assert!(engine.require_pro().is_ok());

    // A write began and never finished: the marker is visible.
    f.leave_marker("a-marker-no-client-wrote");
    let error = engine.require_pro().unwrap_err();
    assert_eq!(unreadable(error), ENGINE_UNRESOLVED_TEXT);
    assert!(
        source(&f).load_token().is_err(),
        "the source itself fails closed"
    );

    // The marker goes (an abort by the owner): the engine reads again.
    let store = f
        .store
        .with_rotation_lock_dir(f.license_dir().join("rotation"));
    assert!(store.abort_rotation(ACCOUNT).unwrap());
    assert!(engine.require_pro().is_ok());
}

#[test]
fn a_held_operation_lock_is_busy_after_a_bounded_try() {
    let f = fixture();
    f.client().activate(key()).unwrap();
    let engine = engine(&f);
    let held = rightkit_license::CrossProcessLock::try_acquire(f.license_dir().join(".lock"))
        .unwrap()
        .unwrap();
    let started = Instant::now();
    let error = engine.require_pro().unwrap_err();
    assert_eq!(unreadable(error), ENGINE_BUSY_TEXT);
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(held);
    assert!(engine.require_pro().is_ok());
}

#[test]
fn the_engine_is_busy_while_a_slow_write_is_in_flight_and_sees_the_new_licence_after() {
    // The credential store gives up on a call after 300 ms; the next write of the licence item
    // takes 1.2 s. The writer stops waiting after 250 ms, but its worker keeps the lock.
    let f = fixture_with(Duration::from_millis(300));
    f.backend.primary_set_delay_ms.store(1200, SeqCst);
    let slow = f.client_with(|c| c.with_persist_wait(Duration::from_millis(250)));
    let writer = std::thread::spawn(move || slow.activate(key()));
    // The write is inside the backend now.
    let deadline = Instant::now() + Duration::from_secs(5);
    while f.backend.primary_sets.load(SeqCst) < 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let engine = engine(&f);
    let text = unreadable(engine.require_pro().unwrap_err());
    assert!(
        text == ENGINE_BUSY_TEXT || text == ENGINE_UNRESOLVED_TEXT,
        "{text}"
    );
    // The caller gave up; the worker finishes the write and reconciles on its own.
    assert!(writer.join().unwrap().is_err());
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut result = engine.require_pro();
    while result.is_err() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
        result = engine.require_pro();
    }
    assert!(result.is_ok(), "{result:?}");
}
