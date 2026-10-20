#![cfg(feature = "client-testing")]
//! The remaining K05 negative cases of the native client: wrong app and wrong device, the
//! offline (grace) policies, an interrupted deactivation, a denied secret store, and the types that
//! hold a credential never being serialisable.

mod client_support;

use std::marker::PhantomData;
use std::sync::atomic::Ordering::SeqCst;
use std::time::{Duration, Instant};

use client_support::*;
use rightkit_license::{
    AccountSessionClient, CredentialTokenSource, EngineEntitlementError, EngineEntitlements,
    EngineEntitlementsConfig, LicenseClient, LicenseError, LicenseInvalidation, LifecyclePolicy,
    MessageCode, RevisionState, SecretString, ViewStatus, ENGINE_STORE_TEXT,
};

fn key() -> SecretString {
    SecretString::new(KEY)
}

fn wait_for(within: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    condition()
}

#[test]
fn a_token_for_another_app_or_device_is_refused_and_nothing_is_stored() {
    let f = fixture();
    let client = f.client();

    f.state.wrong_app.store(true, SeqCst);
    assert_eq!(client.activate(key()).unwrap_err(), LicenseError::Verify);
    f.state.wrong_app.store(false, SeqCst);
    assert!(f.backend.text(ACCOUNT).is_none());

    f.state.wrong_device.store(true, SeqCst);
    assert_eq!(client.activate(key()).unwrap_err(), LicenseError::Verify);
    f.state.wrong_device.store(false, SeqCst);
    assert!(f.backend.text(ACCOUNT).is_none());
    assert_eq!(client.status().status, ViewStatus::None);

    // The same client activates once the service answers for this app and device.
    assert_eq!(client.activate(key()).unwrap().status, ViewStatus::Active);
}

/// The record an older client stored for a token that expired 100 s ago.
fn expired_record() -> String {
    let token = {
        let mut claims = common::Claims::term(now() - 5_000, now() - 100);
        claims.id = LICENSE_ID;
        claims.token()
    };
    format!(
        r#"{{"status":"active","deviceId":"{DEVICE}","activationToken":"{ACTIVATION}","signedToken":"{token}","licenseId":"{LICENSE_ID}"}}"#
    )
}

#[test]
fn offline_policy_decides_whether_an_expired_licence_has_grace() {
    // The default is hard expiry, as in every shipping app.
    let f = fixture();
    let hard = f.client();
    let view = hard
        .import_legacy_json(SecretString::new(expired_record()))
        .unwrap();
    assert_eq!(view.status, ViewStatus::Expired, "{view:?}");
    assert_eq!(view.message, MessageCode::Expired);

    // An app that opts into grace keeps it working inside the window...
    let f = fixture();
    let graceful = f.client_with(|c| {
        c.with_policy(LifecyclePolicy {
            grace_secs: 3_600,
            ..LifecyclePolicy::default()
        })
    });
    let view = graceful
        .import_legacy_json(SecretString::new(expired_record()))
        .unwrap();
    assert_eq!(view.status, ViewStatus::Grace, "{view:?}");

    // ... and not past it.
    let f = fixture();
    let short = f.client_with(|c| {
        c.with_policy(LifecyclePolicy {
            grace_secs: 10,
            ..LifecyclePolicy::default()
        })
    });
    let view = short
        .import_legacy_json(SecretString::new(expired_record()))
        .unwrap();
    assert_eq!(view.status, ViewStatus::Expired, "{view:?}");
}

#[test]
fn an_interrupted_deactivation_never_reports_success_and_settles_to_the_tombstone() {
    // The credential store gives up on a call after 300 ms; the backend takes 1.4 s for the next
    // write of the licence item (the tombstone). The caller stops waiting after 250 ms.
    let f = fixture_with(Duration::from_millis(300));
    let patient = f.client_with(|c| c.with_persist_wait(Duration::from_millis(250)));
    patient.activate(key()).unwrap();
    f.backend.primary_set_delay_ms.store(1400, SeqCst);

    let error = patient.deactivate().unwrap_err();
    assert_eq!(error, LicenseError::Unresolved);
    // The clear is local first: a server call is never made for a clear that did not finish, so
    // the two never disagree about a licence that is still here.
    assert_eq!(f.state.deactivate_calls.load(SeqCst), 0);
    // While it is unfinished the answer is display-only busy or unresolved, never an entitlement.
    let during = patient.status();
    assert!(during.busy || during.unresolved, "{during:?}");

    // The worker finishes on its own and the store ends at the tombstone.
    assert!(wait_for(Duration::from_secs(10), || {
        f.backend
            .envelope()
            .is_some_and(|envelope| envelope["state"] == "tombstone")
    }));
    assert!(wait_for(Duration::from_secs(10), || {
        let view = patient.status();
        !view.busy && !view.unresolved
    }));
    let view = patient.status();
    assert_eq!(view.status, ViewStatus::None);
    assert_eq!(view.revision_state, RevisionState::Tombstone);
    assert!(f.backend.text(MARKER).is_none());
}

fn engine(f: &Fixture) -> EngineEntitlements {
    let config = EngineEntitlementsConfig::new(common::pubkey(), APP, f.dir.path(), || {
        Ok(DEVICE.to_string())
    })
    .with_token_source(CredentialTokenSource::from_record_store(
        f.store.clone(),
        ACCOUNT,
        f.license_dir(),
    ))
    .with_cache_ttl_secs(0);
    EngineEntitlements::new(config)
}

#[test]
fn a_denied_secret_store_is_unreadable_never_no_licence_and_nothing_reaches_the_network() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    f.backend.deny_get.store(true, SeqCst);

    // Every authorizer fails closed with a fixed, non-secret reason.
    let view = client.status();
    assert_eq!(view.status, ViewStatus::Unverifiable, "{view:?}");
    assert_eq!(view.message, MessageCode::StoreFailed);
    assert_eq!(
        engine(&f).require_pro().unwrap_err(),
        EngineEntitlementError::Unreadable(ENGINE_STORE_TEXT.to_string())
    );
    // A mutation cannot read what it would replace, so it does nothing and uses no network.
    let activations = f.state.activate_calls.load(SeqCst);
    let refreshes = f.state.refresh_calls.load(SeqCst);
    assert_eq!(client.activate(key()).unwrap_err(), LicenseError::Store);
    assert_eq!(client.refresh().unwrap_err(), LicenseError::Store);
    assert_eq!(client.deactivate().unwrap_err(), LicenseError::Store);
    assert_eq!(f.state.activate_calls.load(SeqCst), activations);
    assert_eq!(f.state.refresh_calls.load(SeqCst), refreshes);
    f.assert_no_secret("a denied status", &serde_json::to_string(&view).unwrap());

    // Reads allowed again: the licence was never touched.
    f.backend.deny_get.store(false, SeqCst);
    assert_eq!(client.status().status, ViewStatus::Active);
}

#[test]
fn a_denied_write_stores_no_licence() {
    let f = fixture();
    let client = f.client();
    f.backend.deny_set.store(true, SeqCst);
    let error = client.activate(key()).unwrap_err();
    assert!(
        matches!(error, LicenseError::Store | LicenseError::Unresolved),
        "{error:?}"
    );
    assert_ne!(client.status().status, ViewStatus::Active);
    assert_ne!(
        f.backend
            .envelope()
            .map(|envelope| envelope["state"].clone()),
        Some(serde_json::json!("active"))
    );
    f.assert_no_secret("the error", &format!("{error:?} {error}"));
}

/// Whether `T` implements `Serialize`, decided at compile time (an inherent constant that exists
/// only when the bound holds takes precedence over the trait's default).
struct Probe<T>(PhantomData<T>);

trait NotSerialize {
    const SERIALIZE: bool = false;
}

impl<T> NotSerialize for Probe<T> {}

impl<T: serde::Serialize> Probe<T> {
    const SERIALIZE: bool = true;
}

#[test]
fn nothing_that_holds_a_credential_can_be_serialised() {
    assert!(!Probe::<SecretString>::SERIALIZE);
    assert!(!Probe::<LicenseClient>::SERIALIZE);
    assert!(!Probe::<AccountSessionClient>::SERIALIZE);
    assert!(!Probe::<LicenseInvalidation>::SERIALIZE);
    // The views a renderer receives are serialisable, so the probe itself works.
    assert!(Probe::<rightkit_license::LicenseView>::SERIALIZE);
    assert!(Probe::<rightkit_license::AccountStatusView>::SERIALIZE);
    assert!(Probe::<rightkit_license::AccountDeviceView>::SERIALIZE);
}
