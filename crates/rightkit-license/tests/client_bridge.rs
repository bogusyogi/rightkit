#![cfg(feature = "client-testing")]
//! The Pro bridge as native typed operations against the local mock server: the same paths and
//! bodies the apps send today, the lease that comes back going through every check of a typed-key
//! lease, and no bridge token anywhere but in the one value that hands it to the caller.

mod client_support;

use std::sync::atomic::Ordering::SeqCst;
use std::time::{Duration, Instant};

use client_support::*;
use rightkit_license::{
    BridgePlatform, ClockRefusal, ClockState, ClockStore, FileClockStore, LicenseError,
    RejectReason, RevisionState, SecretString, ViewStatus,
};
use serde_json::json;

fn key() -> SecretString {
    SecretString::new(KEY)
}

fn bridge() -> SecretString {
    SecretString::new(BRIDGE_TOKEN)
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
fn a_bridge_token_is_minted_from_the_activation_and_the_licence_is_not_touched() {
    let f = fixture();
    let client = f.client();

    // No licence, nothing to publish, nothing sent.
    assert!(client.bridge_publish().unwrap().is_none());
    assert_eq!(f.state.bridge_calls.load(SeqCst), 0);

    client.activate(key()).unwrap();
    let stored = f.backend.text(ACCOUNT).unwrap();
    let token = client
        .bridge_publish()
        .unwrap()
        .expect("an active licence publishes a bridge");
    assert_eq!(token.expose(), BRIDGE_TOKEN);
    // The body the apps send today: the raw device id, never the hash, and the app version.
    assert_eq!(
        f.requests("/v1/licenses/bridge/from-activation"),
        vec![json!({
            "app": APP,
            "activationToken": ACTIVATION,
            "deviceId": DEVICE,
            "version": "1.0.0"
        })]
    );
    // Publishing reads the licence and writes nothing.
    assert_eq!(f.backend.text(ACCOUNT).unwrap(), stored);
    // The token never prints.
    assert!(!format!("{token:?}").contains("SENTINEL"));
    drop(token);
}

#[test]
fn nothing_is_published_from_a_licence_that_does_not_grant_access() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    // The service revokes it at the next refresh.
    *f.state.refresh_mode.lock().unwrap() = RefreshMode::Revoked;
    client.refresh().unwrap();
    assert!(client.bridge_publish().unwrap().is_none());
    client.deactivate().unwrap();
    assert!(client.bridge_publish().unwrap().is_none());
    assert_eq!(f.state.bridge_calls.load(SeqCst), 0);
}

#[test]
fn a_bridge_exchange_stores_a_verified_lease_on_a_fresh_device() {
    let f = fixture();
    let client = f.client();

    let view = client
        .bridge_exchange(bridge(), BridgePlatform::MacOs)
        .unwrap();
    assert_eq!(view.status, ViewStatus::Active, "{view:?}");
    assert_eq!(view.revision_state, RevisionState::Active);
    // The body the apps send today.
    assert_eq!(
        f.requests("/v1/licenses/bridge/exchange"),
        vec![json!({
            "app": APP,
            "bridgeToken": BRIDGE_TOKEN,
            "deviceId": DEVICE,
            "version": "1.0.0",
            "platform": "macos",
            "deviceClass": "mac"
        })]
    );
    // It is stored as a lease bound to the device hash, by the same fenced write, and the bridge
    // token is nowhere in the record or the view.
    let stored = f.backend.envelope().unwrap();
    assert_eq!(stored["record"]["deviceId"], DEVICE_HASH);
    assert_eq!(stored["record"]["lease"]["leaseId"], "lease-1");
    assert!(!f.backend.text(ACCOUNT).unwrap().contains(BRIDGE_TOKEN));
    f.assert_no_secret("the exchanged view", &serde_json::to_string(&view).unwrap());

    // A device that already holds a working licence is left alone: no network.
    let again = client
        .bridge_exchange(bridge(), BridgePlatform::MacOs)
        .unwrap();
    assert_eq!(again.status, ViewStatus::Active);
    assert_eq!(f.state.exchange_calls.load(SeqCst), 1);
}

#[test]
fn a_bridge_exchange_that_is_refused_or_does_not_verify_stores_nothing() {
    let f = fixture();
    let client = f.client();
    let rejected = |reason| LicenseError::Rejected { reason };

    // Refused by the service.
    f.state.exchange_mode.store(201, SeqCst);
    assert_eq!(
        client
            .bridge_exchange(bridge(), BridgePlatform::MacOs)
            .unwrap_err(),
        rejected(RejectReason::Revoked)
    );
    f.state.exchange_mode.store(500, SeqCst);
    assert_eq!(
        client
            .bridge_exchange(bridge(), BridgePlatform::MacOs)
            .unwrap_err(),
        LicenseError::Network
    );
    f.state.exchange_mode.store(200, SeqCst);
    assert_eq!(
        client
            .bridge_exchange(SecretString::new("OTHER-BRIDGE"), BridgePlatform::MacOs)
            .unwrap_err(),
        rejected(RejectReason::KeyNotAccepted)
    );

    // A lease for another device, signed by the wrong key, or carrying a token for another app is
    // never stored, whoever issued it.
    f.state.wrong_hash.store(true, SeqCst);
    assert_eq!(
        client
            .bridge_exchange(bridge(), BridgePlatform::MacOs)
            .unwrap_err(),
        LicenseError::Verify
    );
    f.state.wrong_hash.store(false, SeqCst);
    f.state.bad_signature.store(true, SeqCst);
    assert_eq!(
        client
            .bridge_exchange(bridge(), BridgePlatform::MacOs)
            .unwrap_err(),
        LicenseError::Verify
    );
    f.state.bad_signature.store(false, SeqCst);
    f.state.wrong_app.store(true, SeqCst);
    assert_eq!(
        client
            .bridge_exchange(bridge(), BridgePlatform::MacOs)
            .unwrap_err(),
        LicenseError::Verify
    );
    f.state.wrong_app.store(false, SeqCst);
    assert!(f.backend.text(ACCOUNT).is_none());
    assert_eq!(client.status().status, ViewStatus::None);

    // A token the service could not have issued is refused unread and unsent.
    let calls = f.state.exchange_calls.load(SeqCst);
    for bad in [String::new(), "   ".to_string(), "x".repeat(16 * 1024 + 1)] {
        assert_eq!(
            client
                .bridge_exchange(SecretString::new(bad), BridgePlatform::MacOs)
                .unwrap_err(),
            rejected(RejectReason::KeyNotAccepted)
        );
    }
    assert_eq!(f.state.exchange_calls.load(SeqCst), calls);

    // And once none of that applies, the same client succeeds.
    assert_eq!(
        client
            .bridge_exchange(bridge(), BridgePlatform::MacOs)
            .unwrap()
            .status,
        ViewStatus::Active
    );
}

#[test]
fn a_replayed_server_clock_is_refused_by_an_exchange() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    // The ledger holds a server time later than anything the service will issue next.
    let ledger = FileClockStore::new(f.clock_path());
    let mut state: ClockState = ledger.load().unwrap().unwrap();
    state.last_server_iat = f.state.next_iat.load(SeqCst) + 1_000;
    ledger.save(&state).unwrap();
    client.deactivate().unwrap();

    let error = client
        .bridge_exchange(bridge(), BridgePlatform::MacOs)
        .unwrap_err();
    assert!(
        matches!(error, LicenseError::Clock(ClockRefusal::Rollback { .. })),
        "{error:?}"
    );
    // Nothing was stored over the tombstone.
    assert_eq!(f.backend.envelope().unwrap()["state"], "tombstone");
}

#[test]
fn a_deactivate_during_a_slow_exchange_is_busy_and_cannot_be_overwritten_by_it() {
    let f = fixture();
    f.state.exchange_delay_ms.store(700, SeqCst);
    let exchanging = f.client();
    let impatient =
        f.client_with(|c| c.with_waits(Duration::from_millis(150), Duration::from_millis(100)));

    let handle =
        std::thread::spawn(move || exchanging.bridge_exchange(bridge(), BridgePlatform::MacOs));
    assert!(wait_for(Duration::from_secs(5), || {
        f.state.exchange_calls.load(SeqCst) == 1
    }));
    // The exchange holds the operation lock for its whole workflow: the deactivate cannot slip in
    // between the answer and the write.
    assert_eq!(impatient.deactivate().unwrap_err(), LicenseError::Busy);
    assert!(handle.join().unwrap().is_ok());
    assert_eq!(f.backend.envelope().unwrap()["state"], "active");
}
