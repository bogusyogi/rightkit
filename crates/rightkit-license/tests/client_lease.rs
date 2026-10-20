#![cfg(feature = "client-testing")]
//! Device-lease flows (native only) against the local mock server: issue, renew, removal, and
//! the checks that run before anything is stored (envelope signature, device hash, generation
//! ordering, lease expiry).

mod client_support;

use std::sync::atomic::Ordering::SeqCst;

use client_support::*;
use rightkit_license::{
    LicenseError, MessageCode, RefreshViewOutcome, RejectReason, RevisionState, SecretString,
    ViewStatus,
};

fn key() -> SecretString {
    SecretString::new(KEY)
}

#[test]
fn a_lease_is_issued_renewed_and_removed() {
    let f = fixture();
    let client = f.client();

    let view = client.lease_activate(key(), Some("Work Mac")).unwrap();
    assert_eq!(view.status, ViewStatus::Active, "{view:?}");
    let issued = f.requests(&format!("/v1/pro/{APP}/devices/lease"));
    assert_eq!(issued.len(), 1);
    assert_eq!(issued[0]["licenseKey"], KEY);
    assert_eq!(
        issued[0]["deviceIdHash"], DEVICE_HASH,
        "the raw device id never goes on the wire"
    );
    assert_eq!(issued[0]["platform"], std::env::consts::OS);
    assert_eq!(issued[0]["deviceLabel"], "Work Mac");
    assert!(issued[0].get("deviceId").is_none());
    // The envelope is persisted inside the same single item, with the raw key nowhere.
    let stored = f.backend.envelope().unwrap();
    assert_eq!(stored["record"]["deviceId"], DEVICE_HASH);
    assert_eq!(stored["record"]["lease"]["leaseId"], "lease-1");
    assert_eq!(stored["record"]["lease"]["generation"], 1);
    assert!(!f.backend.text(ACCOUNT).unwrap().contains(KEY));

    // `refresh` on a lease record renews the lease, not the legacy route.
    let refreshed = client.refresh().unwrap();
    assert_eq!(refreshed.outcome, RefreshViewOutcome::Refreshed);
    assert_eq!(f.state.renew_calls.load(SeqCst), 1);
    assert_eq!(f.state.refresh_calls.load(SeqCst), 0);
    let renewed = f.requests(&format!("/v1/pro/{APP}/devices/lease/renew"));
    assert_eq!(renewed[0]["leaseId"], "lease-1");
    assert_eq!(renewed[0]["deviceIdHash"], DEVICE_HASH);
    assert_eq!(
        f.backend.envelope().unwrap()["record"]["lease"]["generation"],
        2
    );

    // Deactivating releases the lease and writes the tombstone.
    let gone = client.lease_deactivate().unwrap();
    assert_eq!(gone.revision_state, RevisionState::Tombstone);
    assert_eq!(f.state.remove_calls.load(SeqCst), 1);
    let removed = f.requests(&format!("/v1/pro/{APP}/devices/lease/remove"));
    assert_eq!(removed[0]["leaseId"], "lease-1");
    assert_eq!(
        f.state.deactivate_calls.load(SeqCst),
        0,
        "not the legacy route"
    );
    assert_eq!(f.backend.envelope().unwrap()["state"], "tombstone");

    // No sentinel in anything the views said.
    for view in [&view, &refreshed.view, &gone] {
        f.assert_no_secret("a lease view", &serde_json::to_string(view).unwrap());
    }
}

#[test]
fn server_refusals_map_to_native_reasons() {
    let f = fixture();
    let client = f.client();
    let wrong = client
        .lease_activate(SecretString::new("WRONG"), None)
        .unwrap_err();
    assert_eq!(
        wrong,
        LicenseError::Rejected {
            reason: RejectReason::KeyNotAccepted
        }
    );
    assert_eq!(wrong.to_string(), MessageCode::KeyNotAccepted.english());
    assert!(f.backend.text(ACCOUNT).is_none());
}

#[test]
fn an_envelope_that_does_not_verify_is_never_stored() {
    let f = fixture();
    let client = f.client();

    f.state.bad_signature.store(true, SeqCst);
    assert_eq!(
        client.lease_activate(key(), None).unwrap_err(),
        LicenseError::Verify
    );
    assert!(f.backend.text(ACCOUNT).is_none());
    f.state.bad_signature.store(false, SeqCst);

    // A lease issued for another device is refused even though its signature is good.
    f.state.wrong_hash.store(true, SeqCst);
    assert_eq!(
        client.lease_activate(key(), None).unwrap_err(),
        LicenseError::Verify
    );
    assert!(f.backend.text(ACCOUNT).is_none());
    f.state.wrong_hash.store(false, SeqCst);

    assert!(client.lease_activate(key(), None).is_ok());
}

#[test]
fn an_older_lease_generation_is_refused_and_the_record_stands() {
    let f = fixture();
    let client = f.client();
    client.lease_activate(key(), None).unwrap();
    let before = f.backend.text(ACCOUNT).unwrap();
    f.state.renew_generation.store(0, SeqCst);
    assert_eq!(client.lease_renew().unwrap_err(), LicenseError::Verify);
    assert_eq!(f.backend.text(ACCOUNT).unwrap(), before);
}

#[test]
fn a_removed_device_is_revoked_at_the_next_renew() {
    let f = fixture();
    let client = f.client();
    client.lease_activate(key(), None).unwrap();
    f.state.renew_status.store(410, SeqCst);
    let revoked = client.lease_renew().unwrap();
    assert_eq!(revoked.outcome, RefreshViewOutcome::Revoked);
    assert_eq!(revoked.view.status, ViewStatus::Revoked);
    let stored = f.backend.text(ACCOUNT).unwrap();
    assert!(
        !stored.contains(ACTIVATION),
        "a revoked record holds no token"
    );
    // An unreachable server is stale, not revoked.
    let f2 = fixture();
    let client = f2.client();
    client.lease_activate(key(), None).unwrap();
    f2.state.renew_status.store(500, SeqCst);
    let stale = client.lease_renew().unwrap();
    assert_eq!(stale.outcome, RefreshViewOutcome::Stale);
    assert_eq!(stale.view.status, ViewStatus::Active);
    assert!(stale.view.stale_since.is_some());
}

#[test]
fn a_lease_past_its_expiry_and_grace_is_expired_whatever_the_token_says() {
    let f = fixture();
    let client = f.client();
    f.state.lease_ttl.store(-1000, SeqCst);
    let view = client.lease_activate(key(), None).unwrap();
    assert_eq!(view.status, ViewStatus::Expired, "{view:?}");
    assert_eq!(view.message, MessageCode::Expired);
}

#[test]
fn lease_operations_on_a_record_with_no_lease_do_nothing() {
    let f = fixture();
    let client = f.client();
    assert_eq!(
        client.lease_renew().unwrap().outcome,
        RefreshViewOutcome::NoCredential
    );
    client.activate(key()).unwrap();
    assert_eq!(
        client.lease_renew().unwrap().outcome,
        RefreshViewOutcome::NoCredential,
        "a key-typed record has no lease to renew"
    );
    assert_eq!(f.state.renew_calls.load(SeqCst), 0);
}

fn import_binding_only(f: &Fixture, activation: bool) {
    let mut claims = common::Claims::term(now() - 100, now() + 3600);
    claims.device = DEVICE_HASH;
    let mut raw = serde_json::json!({
        "status": "active", "signedToken": claims.token(), "leaseId": "legacy-incomplete-lease",
    });
    if activation {
        raw["activationToken"] = serde_json::json!(ACTIVATION);
    }
    f.client()
        .import_legacy_json(SecretString::new(raw.to_string()))
        .unwrap();
    assert!(f.backend.envelope().unwrap()["record"]
        .get("lease")
        .is_none());
}

#[test]
fn incomplete_imported_lease_dispatches_with_or_without_activation_credential() {
    for activation in [false, true] {
        let f = fixture();
        import_binding_only(&f, activation);
        let before = f.backend.text(ACCOUNT).unwrap();
        let result = f.client().refresh();
        if activation {
            assert_eq!(result.unwrap().outcome, RefreshViewOutcome::Refreshed);
            assert_eq!(
                f.backend.envelope().unwrap()["record"]["lease"]["generation"],
                2
            );
        } else {
            // Renewal dispatch needs only the binding; persistence still requires a credential.
            assert_eq!(result.unwrap_err(), LicenseError::Verify);
            assert_eq!(f.backend.text(ACCOUNT).unwrap(), before);
        }
        let requests = f.requests(&format!("/v1/pro/{APP}/devices/lease/renew"));
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["leaseId"], "legacy-incomplete-lease");
        assert_eq!(requests[0]["deviceIdHash"], DEVICE_HASH);
        assert!(requests[0].get("activationToken").is_none());
        assert_eq!(f.state.refresh_calls.load(SeqCst), 0);
    }
}

#[test]
fn incomplete_imported_lease_uses_the_same_direct_renew_path() {
    let f = fixture();
    import_binding_only(&f, false);
    let before = f.backend.text(ACCOUNT).unwrap();
    assert_eq!(f.client().lease_renew().unwrap_err(), LicenseError::Verify);
    assert_eq!(f.backend.text(ACCOUNT).unwrap(), before);
    assert_eq!(
        f.requests(&format!("/v1/pro/{APP}/devices/lease/renew"))[0]["leaseId"],
        "legacy-incomplete-lease"
    );
    assert_eq!(f.state.refresh_calls.load(SeqCst), 0);
}

#[test]
fn inactive_or_invalid_imported_lease_never_dispatches_refresh_or_renew() {
    for status in ["revoked", "invalid"] {
        let f = fixture();
        import_binding_only(&f, true);
        let mut stored = f.backend.envelope().unwrap();
        stored["record"]["status"] = serde_json::json!(status);
        f.backend.put_text(ACCOUNT, &stored.to_string());
        let before = f.backend.text(ACCOUNT).unwrap();
        let client = f.client();
        assert_eq!(
            client.refresh().unwrap().outcome,
            RefreshViewOutcome::NoCredential
        );
        assert_eq!(
            client.lease_renew().unwrap().outcome,
            RefreshViewOutcome::NoCredential
        );
        assert_eq!(f.state.renew_calls.load(SeqCst), 0);
        assert_eq!(f.state.refresh_calls.load(SeqCst), 0);
        assert_eq!(f.backend.text(ACCOUNT).unwrap(), before);
    }
}

#[test]
fn activation_only_refresh_still_uses_legacy_endpoint() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    assert_eq!(
        client.refresh().unwrap().outcome,
        RefreshViewOutcome::Refreshed
    );
    assert_eq!(f.state.refresh_calls.load(SeqCst), 1);
    assert_eq!(f.state.renew_calls.load(SeqCst), 0);
}

#[test]
fn incomplete_import_renewal_still_refuses_unverified_server_metadata() {
    let f = fixture();
    import_binding_only(&f, true);
    let before = f.backend.text(ACCOUNT).unwrap();
    f.state.bad_signature.store(true, SeqCst);
    assert_eq!(f.client().refresh().unwrap_err(), LicenseError::Verify);
    assert_eq!(f.backend.text(ACCOUNT).unwrap(), before);
    assert_eq!(f.state.refresh_calls.load(SeqCst), 0);
}

#[test]
fn refused_credential_store_never_dispatches_imported_lease() {
    let f = fixture();
    import_binding_only(&f, false);
    f.backend.deny_get.store(true, SeqCst);
    let client = f.client();
    assert!(client.refresh().is_err());
    assert!(client.lease_renew().is_err());
    assert_eq!(f.state.renew_calls.load(SeqCst), 0);
    assert_eq!(f.state.refresh_calls.load(SeqCst), 0);
}

#[test]
fn complete_lease_id_takes_precedence_over_preserved_import_binding() {
    let f = fixture();
    let client = f.client();
    client.lease_activate(key(), None).unwrap();
    let mut stored = f.backend.envelope().unwrap();
    stored["record"]["leaseId"] = serde_json::json!("obsolete-import-binding");
    f.backend.put_text(ACCOUNT, &stored.to_string());
    assert_eq!(
        client.refresh().unwrap().outcome,
        RefreshViewOutcome::Refreshed
    );
    assert_eq!(
        f.requests(&format!("/v1/pro/{APP}/devices/lease/renew"))[0]["leaseId"],
        "lease-1"
    );
    assert_eq!(f.state.refresh_calls.load(SeqCst), 0);
}
