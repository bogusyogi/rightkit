#![cfg(feature = "client-testing")]
//! The native client against a local mock licence server (no network): the activate, refresh,
//! stale, revoke and deactivate flows, the persisted envelope, sentinel secrecy across every
//! output, persistence failures and the lock refusals.

mod client_support;

use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

use client_support::*;
use rightkit_license::{
    ClockState, ClockStore, FileClockStore, LicenseError, LicenseMode, LicenseSetup, LicenseView,
    MessageCode, RefreshViewOutcome, RejectReason, RevisionState, SecretString, ViewKind,
    ViewStatus,
};
use serde_json::Value;

fn key() -> SecretString {
    SecretString::new(KEY)
}

/// Everything a flow produced, serialized and formatted, for the sentinel scan.
fn texts(view: &LicenseView) -> String {
    format!("{} {:?}", serde_json::to_string(view).unwrap(), view)
}

#[test]
fn a_disabled_client_has_no_network_and_every_mutation_is_unconfigured() {
    let f = fixture();
    let client = rightkit_license::LicenseClient::new(
        LicenseSetup::Disabled,
        std::sync::Arc::new(FixedDevice),
    )
    .unwrap();
    assert!(client.is_disabled());
    let view = client.status();
    assert_eq!(view.mode, LicenseMode::Disabled);
    assert_eq!(view.status, ViewStatus::None);
    assert_eq!(
        client.activate(key()).unwrap_err(),
        LicenseError::Unconfigured
    );
    assert_eq!(client.refresh().unwrap_err(), LicenseError::Unconfigured);
    assert_eq!(client.deactivate().unwrap_err(), LicenseError::Unconfigured);
    assert_eq!(
        client.request_trial("a@b.co").unwrap_err(),
        LicenseError::Unconfigured
    );
    assert_eq!(
        f.server.request_count(),
        0,
        "a disabled client uses no network"
    );
    assert!(f.backend.text(ACCOUNT).is_none());
}

#[test]
fn activate_refresh_and_deactivate_run_against_the_server() {
    let f = fixture();
    let client = f.client();
    let mut outputs = Vec::new();

    let before = client.status();
    assert_eq!(before.mode, LicenseMode::Configured);
    assert_eq!(before.status, ViewStatus::None);
    assert_eq!(before.revision_state, RevisionState::None);

    // Activate.
    let active = client.activate(key()).unwrap();
    outputs.push(texts(&active));
    assert_eq!(active.status, ViewStatus::Active);
    assert_eq!(active.lifecycle, rightkit_license::Lifecycle::Active);
    assert_eq!(active.tier.as_deref(), Some("pro"));
    assert_eq!(active.kind, Some(ViewKind::Term));
    assert_eq!(active.license_id_suffix.as_deref(), Some("7d6c"));
    assert_eq!(active.revision_state, RevisionState::Active);
    assert!(!active.busy && !active.unresolved);
    let valid_until = active.valid_until.unwrap();
    assert!(valid_until > now() * 1000, "times are Unix milliseconds");

    // The wire: one POST, the documented body, no preflight.
    let activations = f.requests("/v1/licenses/activate");
    assert_eq!(activations.len(), 1);
    assert_eq!(activations[0]["app"], APP);
    assert_eq!(activations[0]["deviceId"], DEVICE);
    assert_eq!(activations[0]["licenseKey"], KEY);
    assert_eq!(activations[0]["version"], "1.0.0");
    assert!(f.server.requests().iter().all(|r| r.method == "POST"));

    // The stored item is an envelope with a revision, holding the tokens natively.
    let stored = f.backend.envelope().unwrap();
    assert_eq!(stored["rkv"], 1);
    assert_eq!(stored["state"], "active");
    let revision_1 = stored["revision"].as_str().unwrap().to_string();
    assert!(revision_1.len() >= 22);
    assert_eq!(stored["record"]["activationToken"], ACTIVATION);
    assert!(
        stored["record"].get("key").is_none(),
        "the key is never stored"
    );
    assert!(f.backend.text(MARKER).is_none(), "no marker is left behind");

    // Status reads it back.
    assert_eq!(client.status().status, ViewStatus::Active);

    // Refresh: a new token, a new revision.
    let refreshed = client.refresh().unwrap();
    outputs.push(texts(&refreshed.view));
    assert_eq!(refreshed.outcome, RefreshViewOutcome::Refreshed);
    assert_eq!(refreshed.view.status, ViewStatus::Active);
    let refreshes = f.requests("/v1/licenses/refresh");
    assert_eq!(refreshes.len(), 1);
    assert_eq!(refreshes[0]["activationToken"], ACTIVATION);
    assert_eq!(refreshes[0]["deviceId"], DEVICE);
    let revision_2 = f.backend.envelope().unwrap()["revision"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(revision_1, revision_2, "every write is a new revision");

    // Active with no token, cached token still valid: kept, no write needed.
    *f.state.refresh_mode.lock().unwrap() = RefreshMode::Omitted;
    let kept = client.refresh().unwrap();
    assert_eq!(kept.outcome, RefreshViewOutcome::Refreshed);
    assert_eq!(kept.view.status, ViewStatus::Active);

    // The server cannot be reached: stale, the record stands and notes the episode.
    *f.state.refresh_mode.lock().unwrap() = RefreshMode::ServerError;
    let stale = client.refresh().unwrap();
    outputs.push(texts(&stale.view));
    assert_eq!(stale.outcome, RefreshViewOutcome::Stale);
    assert_eq!(stale.view.status, ViewStatus::Active);
    let stale_since = stale.view.stale_since.expect("staleSince is set");
    assert!(stale_since > 0);
    // A second failure keeps the FIRST failure's time.
    let again = client.refresh().unwrap();
    assert_eq!(again.view.stale_since, Some(stale_since));
    // The next answer from the server ends the episode.
    *f.state.refresh_mode.lock().unwrap() = RefreshMode::NewToken;
    let recovered = client.refresh().unwrap();
    assert_eq!(recovered.outcome, RefreshViewOutcome::Refreshed);
    assert_eq!(recovered.view.stale_since, None);

    // The server says the licence is no longer active: revoked, tokens gone.
    *f.state.refresh_mode.lock().unwrap() = RefreshMode::Revoked;
    let revoked = client.refresh().unwrap();
    outputs.push(texts(&revoked.view));
    assert_eq!(revoked.outcome, RefreshViewOutcome::Revoked);
    assert_eq!(revoked.view.status, ViewStatus::Revoked);
    let stored = f.backend.text(ACCOUNT).unwrap();
    assert!(
        !stored.contains(ACTIVATION),
        "a revoked record holds no token"
    );
    for token in f.state.issued.lock().unwrap().iter() {
        assert!(!stored.contains(token.as_str()));
    }
    // Nothing to refresh with any more, and nothing is sent.
    let calls = f.state.refresh_calls.load(SeqCst);
    assert_eq!(
        client.refresh().unwrap().outcome,
        RefreshViewOutcome::NoCredential
    );
    assert_eq!(f.state.refresh_calls.load(SeqCst), calls);

    // Activate again, then deactivate: the seat is freed and a tombstone is written.
    let active = client.activate(key()).unwrap();
    assert_eq!(active.status, ViewStatus::Active);
    let gone = client.deactivate().unwrap();
    outputs.push(texts(&gone));
    assert_eq!(gone.status, ViewStatus::None);
    assert_eq!(gone.revision_state, RevisionState::Tombstone);
    assert_eq!(f.state.deactivate_calls.load(SeqCst), 1);
    let freed = f.requests("/v1/licenses/deactivate");
    assert_eq!(freed[0]["activationToken"], ACTIVATION);
    let stored = f.backend.envelope().unwrap();
    assert_eq!(stored["state"], "tombstone");
    assert!(stored.get("record").is_none());
    let status = client.status();
    assert_eq!(status.status, ViewStatus::None);
    assert_eq!(status.revision_state, RevisionState::Tombstone);
    // A deactivated install has no credential to refresh with.
    assert_eq!(
        client.refresh().unwrap().outcome,
        RefreshViewOutcome::NoCredential
    );
    // Deactivating again is a no-op that still succeeds.
    assert_eq!(
        client.deactivate().unwrap().revision_state,
        RevisionState::Tombstone
    );
    assert_eq!(f.state.deactivate_calls.load(SeqCst), 1);

    // No view of any step carries a sentinel (key, activation token, any issued token).
    for text in &outputs {
        f.assert_no_secret("a view", text);
    }
}

#[test]
fn error_paths_show_fixed_text_and_never_a_secret() {
    let f = fixture();
    let client = f.client();
    let mut errors = Vec::new();

    // A key the server does not accept: rejected, with a native reason and no server text.
    let wrong = client
        .activate(SecretString::new("WRONG-KEY-SENTINEL"))
        .unwrap_err();
    assert_eq!(
        wrong,
        LicenseError::Rejected {
            reason: RejectReason::KeyNotAccepted
        }
    );
    assert_eq!(wrong.to_string(), MessageCode::KeyNotAccepted.english());
    errors.push(format!("{wrong:?} {wrong}"));
    assert!(
        f.backend.text(ACCOUNT).is_none(),
        "nothing is stored for a rejection"
    );

    // A blank key never reaches the network.
    let calls = f.server.request_count();
    assert!(matches!(
        client.activate(SecretString::new("   ")).unwrap_err(),
        LicenseError::Rejected {
            reason: RejectReason::KeyNotAccepted
        }
    ));
    assert_eq!(f.server.request_count(), calls);

    // The server is down: Network.
    let down = f.client_with(|config| {
        let mut http = rightkit_http::testing::loopback_config();
        http.request_timeout = Duration::from_millis(500);
        // Nothing listens on port 1.
        config.with_test_origin("http://127.0.0.1:1", http)
    });
    let error = down.activate(key()).unwrap_err();
    assert_eq!(error, LicenseError::Network);
    errors.push(format!("{error:?} {error}"));

    // Server-text never appears in an error, in any form.
    for error in &errors {
        f.assert_no_secret("an error", error);
        assert!(!error.contains("SERVER TEXT"));
        assert!(!error.contains("WRONG-KEY-SENTINEL"));
    }
}

#[test]
fn a_clock_rolled_back_below_the_ledger_is_refused_and_a_repair_refresh_reanchors() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    let ledger = FileClockStore::new(f.clock_path());
    let state = ledger
        .load()
        .unwrap()
        .expect("activation starts the ledger");
    assert!(state.last_server_iat > 0);
    // The ledger was inflated by a clock that ran far ahead.
    ledger
        .save(&ClockState {
            highest_seen: now() + 10_000,
            trusted_process: state.trusted_process.clone(),
            last_server_iat: state.last_server_iat,
        })
        .unwrap();
    let view = client.status();
    assert_eq!(
        view.lifecycle,
        rightkit_license::Lifecycle::ClockRepairRequired
    );
    assert_eq!(view.status, ViewStatus::Unverifiable);
    assert_eq!(view.message, MessageCode::ClockRepairRequired);
    // One online refresh with a newer server time lowers the mark: the repair path.
    let repaired = client.refresh().unwrap();
    assert_eq!(repaired.outcome, RefreshViewOutcome::Reanchored);
    assert_eq!(repaired.view.status, ViewStatus::Active);
    assert_eq!(client.status().status, ViewStatus::Active);
}

#[test]
fn a_replayed_server_time_is_refused_as_a_clock_refusal() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    // The server hands back a token whose issue time is not newer than the last anchor.
    let ledger = FileClockStore::new(f.clock_path());
    let mut state = ledger.load().unwrap().unwrap();
    state.last_server_iat = f.state.next_iat.load(SeqCst) + 1_000;
    ledger.save(&state).unwrap();
    let refused = client.refresh().unwrap();
    assert_eq!(refused.outcome, RefreshViewOutcome::ClockRefused);
}

#[test]
fn a_failed_replacement_keeps_the_previous_verified_record() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    let before = f.backend.text(ACCOUNT).unwrap();
    // Every write of the licence item now fails; the marker is writable.
    f.backend.fail_primary_set.store(true, SeqCst);
    let error = client.refresh().unwrap_err();
    assert_eq!(error, LicenseError::Store, "a failed save is a store error");
    // The previous record stands, no marker is left, and status is unchanged.
    assert_eq!(f.backend.text(ACCOUNT).unwrap(), before);
    assert!(f.backend.text(MARKER).is_none());
    f.backend.fail_primary_set.store(false, SeqCst);
    let view = client.status();
    assert_eq!(view.status, ViewStatus::Active);
    assert!(!view.unresolved);
}

#[test]
fn a_failed_clear_is_a_store_error_never_success() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    f.backend.fail_primary_set.store(true, SeqCst);
    assert_eq!(client.deactivate().unwrap_err(), LicenseError::Store);
    // The licence is still there: a clear failure is not a deactivation.
    f.backend.fail_primary_set.store(false, SeqCst);
    assert_eq!(client.status().status, ViewStatus::Active);
    assert_eq!(
        client.deactivate().unwrap().revision_state,
        RevisionState::Tombstone
    );
}

#[test]
fn a_marker_left_after_the_commit_is_cleanup_pending_and_fails_closed_until_resolved() {
    let f = fixture();
    let client = f.client();
    // The primary write lands but removing the marker keeps failing.
    f.backend.fail_marker_delete.store(true, SeqCst);
    let view = client.activate(key()).unwrap();
    // The new envelope IS stored, but the state is unresolved and shows no entitlement.
    assert!(view.unresolved);
    assert_eq!(view.status, ViewStatus::Unverifiable);
    assert_eq!(view.message, MessageCode::Unresolved);
    assert_eq!(f.backend.envelope().unwrap()["state"], "active");
    assert!(f.backend.text(MARKER).is_some());
    // Still failing: status stays unresolved.
    assert!(client.status().unresolved);
    // A mutation refuses rather than build on an unresolved write.
    assert_eq!(client.refresh().unwrap_err(), LicenseError::Unresolved);
    // The store recovers: the next status reconciles under the lock and the licence is live.
    f.backend.fail_marker_delete.store(false, SeqCst);
    let resolved = client.status();
    assert!(!resolved.unresolved);
    assert_eq!(resolved.status, ViewStatus::Active);
    assert!(f.backend.text(MARKER).is_none());
}

#[test]
fn a_leftover_active_marker_rolls_forward_but_never_over_a_tombstone() {
    let f = fixture();
    let client = f.client();
    client.activate(key()).unwrap();
    client.deactivate().unwrap();
    let tombstone = f.backend.text(ACCOUNT).unwrap();

    // An activation began after the deactivation and crashed before its commit: the active
    // marker must not resurrect the tombstone.
    let envelope = |state: &str, revision: &str| {
        let record = if state == "active" {
            format!(
                r#","record":{{"status":"active","deviceId":"{DEVICE}","signedToken":"{}"}}"#,
                f.state.issued.lock().unwrap().last().cloned().unwrap()
            )
        } else {
            String::new()
        };
        format!(r#"{{"rkv":1,"revision":"{revision}","state":"{state}"{record}}}"#)
    };
    f.leave_marker(&envelope("active", "stale-active-marker"));
    let view = client.status();
    assert_eq!(view.status, ViewStatus::None);
    assert_eq!(view.revision_state, RevisionState::Tombstone);
    assert!(
        f.backend.text(MARKER).is_none(),
        "the stale marker is discarded"
    );
    assert_eq!(f.backend.text(ACCOUNT).unwrap(), tombstone);

    // A tombstone marker rolls forward: finishing a deactivation grants nothing.
    client.activate(key()).unwrap();
    f.leave_marker(&envelope("tombstone", "tombstone-marker"));
    let view = client.status();
    assert_eq!(view.status, ViewStatus::None);
    assert_eq!(view.revision_state, RevisionState::Tombstone);
    assert_eq!(
        f.backend.envelope().unwrap()["revision"],
        "tombstone-marker"
    );
    assert!(f.backend.text(MARKER).is_none());
}

#[test]
fn an_older_bare_record_is_read_as_active_and_upgraded_in_place() {
    let f = fixture();
    let client = f.client();
    // The record an older @rightkit/license client stored.
    let token = {
        let mut claims = common::Claims::term(now() - 100, now() + 3600);
        claims.id = LICENSE_ID;
        claims.token()
    };
    let bare = format!(
        r#"{{"status":"active","deviceId":"{DEVICE}","activationToken":"{ACTIVATION}","signedToken":"{token}","licenseId":"{LICENSE_ID}"}}"#
    );
    f.backend.put_text(ACCOUNT, &bare);
    let view = client.status();
    assert_eq!(view.status, ViewStatus::Active, "{view:?}");
    assert_eq!(view.revision_state, RevisionState::Active);

    // The source IS the licence item: import upgrades it in place and removes nothing.
    let source = rightkit_license::LegacySource {
        store: f.store.clone(),
        account: ACCOUNT.to_string(),
    };
    let upgraded = client.import_legacy(&source).unwrap();
    assert_eq!(upgraded.status, ViewStatus::Active);
    let stored = f.backend.envelope().unwrap();
    assert_eq!(stored["rkv"], 1);
    assert!(stored["revision"].as_str().is_some());
    // Deactivating then importing again must not bring it back.
    client.deactivate().unwrap();
    f.backend.put_text("legacy-copy", &bare);
    let legacy = rightkit_license::LegacySource {
        store: f.store.clone(),
        account: "legacy-copy".to_string(),
    };
    let after = client.import_legacy(&legacy).unwrap();
    assert_eq!(after.status, ViewStatus::None);
    assert_eq!(after.revision_state, RevisionState::Tombstone);
    assert!(
        f.backend.text("legacy-copy").is_none(),
        "the legacy copy is cleaned up"
    );
}

#[test]
fn a_legacy_record_imports_as_found_and_removes_the_legacy_item_after_the_commit() {
    let f = fixture();
    let client = f.client();
    // An EXPIRED legacy token: it imports as expired and grants nothing.
    let expired = {
        let mut claims = common::Claims::term(now() - 7200, now() - 3600);
        claims.id = LICENSE_ID;
        claims.token()
    };
    let bare = format!(
        r#"{{"status":"active","deviceId":"{DEVICE}","activationToken":"{ACTIVATION}","signedToken":"{expired}"}}"#
    );
    f.backend.put_text("legacy-record", &bare);
    let legacy = rightkit_license::LegacySource {
        store: f.store.clone(),
        account: "legacy-record".to_string(),
    };
    let view = client.import_legacy(&legacy).unwrap();
    assert_eq!(view.status, ViewStatus::Expired, "{view:?}");
    assert!(f.backend.text("legacy-record").is_none());
    assert_eq!(f.backend.envelope().unwrap()["state"], "active");
    // A token for another app does not verify and imports nothing.
    client.deactivate().unwrap();
    f.backend.remove(ACCOUNT);
    let mut other = common::Claims::term(now() - 100, now() + 3600);
    other.app = "someone-else";
    let wrong_app = format!(
        r#"{{"status":"active","deviceId":"{DEVICE}","signedToken":"{}"}}"#,
        other.token()
    );
    f.backend.put_text("legacy-record", &wrong_app);
    assert_eq!(
        client.import_legacy(&legacy).unwrap_err(),
        LicenseError::Verify
    );
    assert!(
        f.backend.text("legacy-record").is_some(),
        "an import that did not commit keeps the legacy item"
    );
}

#[test]
fn trial_requests_issue_no_credential() {
    let f = fixture();
    let client = f.client();
    let accepted = client.request_trial("someone@example.com").unwrap();
    assert!(accepted.accepted);
    assert_eq!(accepted.message, MessageCode::TrialRequested);
    let sent = f.requests("/v1/licenses/trial");
    assert_eq!(sent[0]["app"], APP);
    assert_eq!(sent[0]["email"], "someone@example.com");
    f.state.trial_ok.store(false, SeqCst);
    let refused = client.request_trial("someone@example.com").unwrap();
    assert!(!refused.accepted);
    assert_eq!(refused.message, MessageCode::TrialRefused);
    // A malformed address never reaches the network.
    let calls = f.state.trial_calls.load(SeqCst);
    assert!(!client.request_trial("not an email").unwrap().accepted);
    assert_eq!(f.state.trial_calls.load(SeqCst), calls);
    assert!(
        f.backend.text(ACCOUNT).is_none(),
        "a trial request stores nothing"
    );
}

#[test]
fn a_held_operation_lock_makes_status_busy_and_a_mutation_busy() {
    let f = fixture();
    let client =
        f.client_with(|c| c.with_waits(Duration::from_millis(200), Duration::from_millis(100)));
    let active = client.activate(key()).unwrap();
    assert_eq!(active.status, ViewStatus::Active);
    // Another process holds the licence operation lock.
    let held = rightkit_license::CrossProcessLock::try_acquire(f.license_dir().join(".lock"))
        .unwrap()
        .unwrap();
    let started = std::time::Instant::now();
    let busy = client.status();
    assert!(
        busy.busy,
        "status shows the last committed view, marked busy"
    );
    assert_eq!(
        busy.status,
        ViewStatus::Active,
        "the display-only view is the last one"
    );
    assert_eq!(busy.message, MessageCode::Busy);
    assert_eq!(client.refresh().unwrap_err(), LicenseError::Busy);
    assert_eq!(client.deactivate().unwrap_err(), LicenseError::Busy);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "waits are bounded"
    );
    drop(held);
    assert!(!client.status().busy);
}

#[test]
fn serialized_views_never_hold_a_sentinel_even_when_the_store_does() {
    let f = fixture();
    let client = f.client();
    let view = client.activate(key()).unwrap();
    let refreshed = client.refresh().unwrap();
    let all = format!(
        "{} {} {}",
        serde_json::to_string(&view).unwrap(),
        serde_json::to_string(&refreshed).unwrap(),
        serde_json::to_string(&client.status()).unwrap()
    );
    f.assert_no_secret("serialized views", &all);
    let parsed: Value = serde_json::from_str(&serde_json::to_string(&refreshed).unwrap()).unwrap();
    assert_eq!(parsed["outcome"], "refreshed");
    assert_eq!(parsed["view"]["lifecycle"], "active");
    // And the store DOES hold them: the secrecy is the view's, not the store's absence.
    assert!(f.backend.text(ACCOUNT).unwrap().contains(ACTIVATION));
}
