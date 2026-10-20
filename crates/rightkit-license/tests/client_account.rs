#![cfg(feature = "client-testing")]
//! The native account-session client against the local mock service: the existing paths,
//! methods and payloads as compatibility vectors, tokens that never leave native code, a
//! sign-out that no late result can undo, deletion as a typed destructive action, and the
//! link-code pair that runs on the licence client.

mod client_support;

use std::sync::atomic::Ordering::SeqCst;
use std::sync::Arc;
use std::time::{Duration, Instant};

use client_support::*;
use rightkit_http::testing::RecordedRequest;
use rightkit_license::{
    AccountDeletion, AccountError, AccountRejectReason, AccountSessionClient, AccountStatusView,
    CrossProcessLock, DeleteAccountConfirmation, DeletionRefusal, LicenseClient, LicenseError,
    LicenseSetup, LinkRequestOutcome, RejectReason, SecretString, ViewStatus,
    ACCOUNT_SESSION_TTL_SECS,
};
use serde_json::json;

fn code() -> SecretString {
    SecretString::new(SESSION_CODE)
}

fn key() -> SecretString {
    SecretString::new(KEY)
}

fn confirm() -> DeleteAccountConfirmation {
    DeleteAccountConfirmation::confirmed_by_user()
}

/// Poll `condition` until it holds or `within` passes.
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

/// The recorded requests of one method and path.
fn sent(f: &Fixture, method: &str, path: &str) -> Vec<RecordedRequest> {
    f.server
        .requests()
        .into_iter()
        .filter(|r| r.method.eq_ignore_ascii_case(method) && r.path() == path)
        .collect()
}

fn signed_in(f: &Fixture, account: &AccountSessionClient) -> String {
    let view = account.complete_sign_in(code()).unwrap();
    assert!(view.signed_in, "{view:?}");
    f.state.sessions.lock().unwrap().last().unwrap().clone()
}

#[test]
fn the_account_flow_uses_the_existing_paths_methods_and_payloads() {
    let f = fixture();
    let account = f.account_client();
    assert_eq!(account.status(), signed_out());

    // Sign-in request: POST {email}, trimmed.
    account.request_sign_in("  Owner@Example.com ").unwrap();
    assert_eq!(
        f.requests("/v1/shop/auth/magic-link/request"),
        vec![json!({"email": "Owner@Example.com"})]
    );

    // Completion: POST {token}.
    let view = account.complete_sign_in(code()).unwrap();
    assert!(view.signed_in);
    assert_eq!(view.email.as_deref(), Some(OWNER_EMAIL));
    assert_eq!(
        f.requests("/v1/shop/auth/magic-link/consume"),
        vec![json!({"token": SESSION_CODE})]
    );
    // The display estimate is one year past issue; it is not an authority.
    let estimate = view.expires_at.unwrap();
    assert!((estimate - (now() + ACCOUNT_SESSION_TTL_SECS) * 1000).abs() < 10_000);
    let token = f.state.sessions.lock().unwrap()[0].clone();

    // The stored item: an envelope with a revision, and the older field names at the top level.
    let item = f.session_item().unwrap();
    assert_eq!(item["rkv"], 1);
    assert_eq!(item["state"], "active");
    assert!(item["revision"].as_str().is_some());
    assert_eq!(item["token"], token.as_str());
    assert_eq!(item["email"], OWNER_EMAIL);
    assert!(item["issuedAtUnix"].as_i64().is_some());
    assert_eq!(account.status().email.as_deref(), Some(OWNER_EMAIL));

    // Devices: GET with the bearer; the licence key is reduced to its last four characters.
    let devices = account.list_devices().unwrap();
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].id, "row-1");
    assert_eq!(devices[0].license_suffix.as_deref(), Some("0002"));
    assert_eq!(devices[0].label.as_deref(), Some("Work Mac"));
    assert_eq!(devices[0].pool.as_deref(), Some("computer"));
    assert_eq!(devices[0].generation, 2);
    assert!(devices[0].last_seen_at.is_some() && devices[0].activated_at.is_some());
    let listed = sent(&f, "GET", "/v1/shop/account/devices");
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0].header("authorization"),
        Some(format!("Bearer {token}").as_str())
    );

    // Rename: PATCH {label} on the row id. Remove: DELETE the row id.
    account.rename_device("row-1", "Studio").unwrap();
    let renamed = sent(&f, "PATCH", "/v1/shop/account/devices/row-1");
    assert_eq!(renamed.len(), 1);
    assert_eq!(renamed[0].body_json(), json!({"label": "Studio"}));
    assert_eq!(
        renamed[0].header("authorization"),
        Some(format!("Bearer {token}").as_str())
    );
    account.remove_device("row-1").unwrap();
    let removed = sent(&f, "DELETE", "/v1/shop/account/devices/row-1");
    assert_eq!(removed.len(), 1);
    assert!(removed[0].body.is_empty());

    // Confirmation: GET /auth/me with the bearer.
    let refreshed = account.refresh().unwrap();
    assert!(refreshed.signed_in);
    let me = sent(&f, "GET", "/v1/shop/auth/me");
    assert_eq!(me.len(), 1);
    assert_eq!(
        me[0].header("authorization"),
        Some(format!("Bearer {token}").as_str())
    );
    // The refresh rewrote the item under a new revision with the same session.
    let rewritten = f.session_item().unwrap();
    assert_ne!(rewritten["revision"], item["revision"]);
    assert_eq!(rewritten["token"], token.as_str());

    // Sign-out: local tombstone, then POST /auth/logout {} with the bearer.
    let out = account.sign_out().unwrap();
    assert_eq!(out, signed_out());
    let logout = sent(&f, "POST", "/v1/shop/auth/logout");
    assert_eq!(logout.len(), 1);
    assert_eq!(logout[0].body_json(), json!({}));
    assert_eq!(
        logout[0].header("authorization"),
        Some(format!("Bearer {token}").as_str())
    );
    assert_eq!(*f.state.logged_out.lock().unwrap(), vec![token.clone()]);
    let tomb = f.session_item().unwrap();
    assert_eq!(tomb["state"], "tombstone");
    assert!(tomb.get("token").is_none() && tomb.get("email").is_none());
    assert_eq!(account.status(), signed_out());
}

fn signed_out() -> AccountStatusView {
    AccountStatusView {
        signed_in: false,
        email: None,
        expires_at: None,
        busy: false,
        unresolved: false,
    }
}

#[test]
fn views_errors_and_debug_output_never_hold_a_secret() {
    let f = fixture();
    let account = f.account_client();
    let mut texts = vec![
        format!("{account:?}"),
        format!("{:?}", f.account_client_with(|c| c)),
    ];
    let view = account.complete_sign_in(code()).unwrap();
    texts.push(serde_json::to_string(&view).unwrap());
    texts.push(serde_json::to_string(&account.status()).unwrap());
    texts.push(serde_json::to_string(&account.list_devices().unwrap()).unwrap());

    // A wrong code: a typed, fixed-text refusal that echoes neither the input nor the server.
    let wrong = account
        .complete_sign_in(SecretString::new("WRONG-CODE-SENTINEL"))
        .unwrap_err();
    assert_eq!(
        wrong,
        AccountError::Rejected {
            reason: AccountRejectReason::LinkInvalid
        }
    );
    for error in [
        wrong.clone(),
        AccountError::SessionExpired,
        AccountError::Store,
    ] {
        texts.push(format!("{error:?} {error}"));
        texts.push(serde_json::to_string(&error.to_command_error()).unwrap());
    }
    // A dead session.
    f.state.me_status.store(401, SeqCst);
    let expired = account.refresh().unwrap_err();
    texts.push(format!("{expired:?} {expired}"));

    for text in &texts {
        f.assert_no_secret("an account output", text);
        assert!(!text.contains("WRONG-CODE-SENTINEL"), "{text}");
        assert!(!text.contains("SERVER TEXT"), "{text}");
    }
    assert_eq!(expired, AccountError::SessionExpired);
}

#[test]
fn a_dead_session_is_cleared_on_401() {
    let f = fixture();
    let account = f.account_client();
    signed_in(&f, &account);
    f.state.me_status.store(401, SeqCst);
    assert_eq!(account.refresh().unwrap_err(), AccountError::SessionExpired);
    assert_eq!(account.status(), signed_out());
    assert_eq!(f.session_item().unwrap()["state"], "tombstone");
    // Nothing more is sent for a session that is gone.
    assert_eq!(
        account.list_devices().unwrap_err(),
        AccountError::NotSignedIn
    );
    assert_eq!(f.state.devices_calls.load(SeqCst), 0);
}

#[test]
fn a_401_about_an_old_session_does_not_clear_a_newer_one() {
    let f = fixture();
    let account = f.account_client();
    signed_in(&f, &account);
    f.state.me_status.store(401, SeqCst);
    f.state.me_delay_ms.store(500, SeqCst);

    let refreshing = {
        let account = account.clone();
        std::thread::spawn(move || account.refresh())
    };
    assert!(wait_for(Duration::from_secs(5), || {
        f.state.me_calls.load(SeqCst) == 1
    }));
    // While the old session's confirmation is on the wire, the person signs out and in again.
    account.sign_out().unwrap();
    let newer = account.complete_sign_in(code()).unwrap();
    assert!(newer.signed_in);
    let newer_token = f.state.sessions.lock().unwrap()[1].clone();

    assert_eq!(
        refreshing.join().unwrap().unwrap_err(),
        AccountError::Superseded
    );
    // The newer session stands.
    assert!(account.status().signed_in);
    assert_eq!(f.session_item().unwrap()["token"], newer_token.as_str());
}

#[test]
fn a_sign_out_during_a_slow_refresh_wins_and_nothing_resurrects_the_session() {
    let f = fixture();
    let account = f.account_client();
    let token = signed_in(&f, &account);
    f.state.me_delay_ms.store(600, SeqCst);

    let refreshing = {
        let account = account.clone();
        std::thread::spawn(move || account.refresh())
    };
    assert!(wait_for(Duration::from_secs(5), || {
        f.state.me_calls.load(SeqCst) == 1
    }));
    // The sign-out does not wait for the server call: the lock is not held across it.
    let started = Instant::now();
    account.sign_out().unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));

    // The late 200 is discarded: the item stays a tombstone.
    assert_eq!(
        refreshing.join().unwrap().unwrap_err(),
        AccountError::Superseded
    );
    assert_eq!(f.session_item().unwrap()["state"], "tombstone");
    assert_eq!(account.status(), signed_out());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(f.session_item().unwrap()["state"], "tombstone");
    assert_eq!(*f.state.logged_out.lock().unwrap(), vec![token]);
}

#[test]
fn a_sign_in_that_finishes_after_a_sign_out_is_discarded_and_logged_out() {
    let f = fixture();
    let account = f.account_client();
    let first = signed_in(&f, &account);
    f.state.consume_delay_ms.store(600, SeqCst);

    let signing_in = {
        let account = account.clone();
        std::thread::spawn(move || account.complete_sign_in(code()))
    };
    assert!(wait_for(Duration::from_secs(5), || {
        f.state.consume_calls.load(SeqCst) == 2
    }));
    account.sign_out().unwrap();

    assert_eq!(
        signing_in.join().unwrap().unwrap_err(),
        AccountError::Superseded
    );
    // The old token after the tombstone is not back, and the session the late sign-in was
    // issued is not left alive at the service.
    assert_eq!(f.session_item().unwrap()["state"], "tombstone");
    assert_eq!(account.status(), signed_out());
    let second = f.state.sessions.lock().unwrap()[1].clone();
    assert_eq!(*f.state.logged_out.lock().unwrap(), vec![first, second]);
}

#[test]
fn a_session_write_that_outlives_its_caller_is_unresolved_and_the_session_is_not_logged_out() {
    // The credential store gives up on a call after 300 ms; the backend takes 1.4 s for the
    // first write of the session. The caller stops waiting after 250 ms.
    let f = fixture_with(Duration::from_millis(300));
    let account = f.account_client_with(|c| c.with_persist_wait(Duration::from_millis(250)));
    f.backend.primary_set_delay_ms.store(1400, SeqCst);

    let error = account.complete_sign_in(code()).unwrap_err();
    assert_eq!(error, AccountError::Unresolved);
    // The write may still land: logging the session out now could strand a stored one.
    assert_eq!(f.state.logout_calls.load(SeqCst), 0);
    // The worker keeps the lock until the backend call has really finished.
    let during = account.status();
    assert!(during.busy || during.unresolved, "{during:?}");

    // Once it has, what is stored is the session, and the status says so without a flag.
    assert!(wait_for(Duration::from_secs(10), || {
        let view = account.status();
        !view.busy && view.signed_in
    }));
    let view = account.status();
    assert!(view.signed_in && !view.unresolved, "{view:?}");
    assert_eq!(f.session_item().unwrap()["state"], "active");
}

#[test]
fn a_denied_secret_store_burns_no_code_and_a_denied_write_logs_the_session_out() {
    // Reads denied: the code is not spent.
    let f = fixture();
    let account = f.account_client();
    f.backend.deny_get.store(true, SeqCst);
    assert_eq!(
        account.complete_sign_in(code()).unwrap_err(),
        AccountError::Store
    );
    assert_eq!(f.state.consume_calls.load(SeqCst), 0);
    assert_eq!(account.sign_out().unwrap_err(), AccountError::Store);
    let status = account.status();
    assert!(status.unresolved && !status.signed_in, "{status:?}");
    f.backend.deny_get.store(false, SeqCst);

    // Writes denied: the session the spent code bought is not kept, so it is logged out.
    f.backend.deny_set.store(true, SeqCst);
    assert_eq!(
        account.complete_sign_in(code()).unwrap_err(),
        AccountError::Store
    );
    assert_eq!(f.state.consume_calls.load(SeqCst), 1);
    let issued = f.state.sessions.lock().unwrap()[0].clone();
    assert_eq!(*f.state.logged_out.lock().unwrap(), vec![issued]);
    assert!(f.backend.text(SESSION_ACCOUNT).is_none());
    assert_eq!(account.status(), signed_out());
    f.backend.deny_set.store(false, SeqCst);

    // Recovery: the same client signs in once the store answers.
    assert!(account.complete_sign_in(code()).unwrap().signed_in);
}

#[test]
fn a_sign_out_whose_tombstone_cannot_be_written_tells_the_service_nothing() {
    let f = fixture();
    let account = f.account_client();
    signed_in(&f, &account);
    f.backend.deny_set.store(true, SeqCst);
    assert_eq!(account.sign_out().unwrap_err(), AccountError::Store);
    // Local first: nothing was changed, so the service was not told.
    assert_eq!(f.state.logout_calls.load(SeqCst), 0);
    f.backend.deny_set.store(false, SeqCst);
    assert!(account.status().signed_in);
    // And a sign-out that can write works.
    assert_eq!(account.sign_out().unwrap(), signed_out());
    assert_eq!(f.state.logout_calls.load(SeqCst), 1);
}

#[test]
fn deletion_keeps_every_server_refusal_and_clears_the_session_only_on_200() {
    let f = fixture();
    let account = f.account_client();
    signed_in(&f, &account);

    for (status, expected) in [
        (
            409,
            AccountDeletion::Refused {
                reason: DeletionRefusal::ActiveSubscription,
            },
        ),
        (
            429,
            AccountDeletion::Refused {
                reason: DeletionRefusal::RateLimited,
            },
        ),
        (
            404,
            AccountDeletion::Refused {
                reason: DeletionRefusal::Other,
            },
        ),
        (500, AccountDeletion::Indeterminate),
    ] {
        f.state.delete_status.store(status, SeqCst);
        assert_eq!(
            account.delete_account(confirm()).unwrap(),
            expected,
            "{status}"
        );
        // The session is kept and nothing is signed out.
        assert!(account.status().signed_in, "{status}");
        assert_eq!(f.session_item().unwrap()["state"], "active");
    }
    assert_eq!(f.state.delete_calls.load(SeqCst), 4);
    let sent = sent(&f, "DELETE", "/v1/shop/account");
    assert!(sent.iter().all(|r| r.body.is_empty()));

    f.state.delete_status.store(200, SeqCst);
    assert_eq!(
        account.delete_account(confirm()).unwrap(),
        AccountDeletion::Deleted {
            local_session_cleared: true
        }
    );
    assert_eq!(account.status(), signed_out());
    assert_eq!(f.session_item().unwrap()["state"], "tombstone");
    // Nothing left to delete with.
    assert_eq!(
        account.delete_account(confirm()).unwrap_err(),
        AccountError::NotSignedIn
    );
    assert_eq!(f.state.delete_calls.load(SeqCst), 5);
}

#[test]
fn a_deletion_that_times_out_is_indeterminate_not_a_failure_or_a_success() {
    let f = fixture();
    let account = f.account_client_with(|c| c.with_request_timeout(Duration::from_millis(300)));
    signed_in(&f, &account);
    f.state.delete_delay_ms.store(1500, SeqCst);

    assert_eq!(
        account.delete_account(confirm()).unwrap(),
        AccountDeletion::Indeterminate
    );
    // The session is kept: the next authenticated call finds out what happened.
    assert!(account.status().signed_in);
    assert_eq!(f.session_item().unwrap()["state"], "active");
}

#[test]
fn a_dead_session_makes_deletion_session_expired_and_clears_it() {
    let f = fixture();
    let account = f.account_client();
    let token = signed_in(&f, &account);
    // The service forgot the session.
    f.state.logged_out.lock().unwrap().push(token);
    assert_eq!(
        account.delete_account(confirm()).unwrap_err(),
        AccountError::SessionExpired
    );
    assert_eq!(account.status(), signed_out());
}

#[test]
fn input_that_could_reach_a_path_or_hurt_is_refused_before_anything_is_sent() {
    let f = fixture();
    let account = f.account_client();
    signed_in(&f, &account);
    let before = f.server.request_count();
    let invalid = AccountError::Rejected {
        reason: AccountRejectReason::InvalidInput,
    };
    for bad in [
        "",
        "../account",
        "a/b",
        "a b",
        "a?x=1",
        "a#b",
        "a%2fb",
        "a.b",
    ] {
        assert_eq!(
            account.rename_device(bad, "x").unwrap_err(),
            invalid,
            "{bad:?}"
        );
        assert_eq!(account.remove_device(bad).unwrap_err(), invalid, "{bad:?}");
    }
    assert_eq!(
        account.rename_device("row-1", "bad\u{7}label").unwrap_err(),
        invalid
    );
    assert_eq!(
        account
            .rename_device("row-1", &"x".repeat(300))
            .unwrap_err(),
        invalid
    );
    assert_eq!(
        account.request_sign_in("not-an-email").unwrap_err(),
        invalid
    );
    assert_eq!(account.request_sign_in("").unwrap_err(), invalid);
    assert_eq!(f.server.request_count(), before, "nothing was sent");

    // An empty or oversized sign-in code is a link that is not usable, also unsent.
    let link = AccountError::Rejected {
        reason: AccountRejectReason::LinkInvalid,
    };
    assert_eq!(
        account
            .complete_sign_in(SecretString::new("  "))
            .unwrap_err(),
        link
    );
    assert_eq!(
        account
            .complete_sign_in(SecretString::new("x".repeat(16 * 1024 + 1)))
            .unwrap_err(),
        link
    );
    assert_eq!(f.server.request_count(), before);

    // An unknown device is a typed refusal, and the session survives it.
    assert_eq!(
        account.remove_device("gone").unwrap_err(),
        AccountError::Rejected {
            reason: AccountRejectReason::DeviceNotFound
        }
    );
    assert!(account.status().signed_in);
}

#[test]
fn sign_in_refusals_are_typed_and_a_bad_code_changes_nothing() {
    let f = fixture();
    let account = f.account_client();
    f.state.signin_request_status.store(429, SeqCst);
    assert_eq!(
        account.request_sign_in("a@b.co").unwrap_err(),
        AccountError::Rejected {
            reason: AccountRejectReason::RateLimited
        }
    );
    f.state.signin_request_status.store(500, SeqCst);
    assert_eq!(
        account.request_sign_in("a@b.co").unwrap_err(),
        AccountError::Network
    );

    let bad = account
        .complete_sign_in(SecretString::new("NOT-THE-CODE"))
        .unwrap_err();
    assert_eq!(
        bad,
        AccountError::Rejected {
            reason: AccountRejectReason::LinkInvalid
        }
    );
    assert_eq!(account.status(), signed_out());
    assert!(f.backend.text(SESSION_ACCOUNT).is_none());
}

#[test]
fn nothing_is_sent_without_a_session() {
    let f = fixture();
    let account = f.account_client();
    assert_eq!(
        account.list_devices().unwrap_err(),
        AccountError::NotSignedIn
    );
    assert_eq!(
        account.rename_device("row-1", "x").unwrap_err(),
        AccountError::NotSignedIn
    );
    assert_eq!(
        account.remove_device("row-1").unwrap_err(),
        AccountError::NotSignedIn
    );
    assert_eq!(account.refresh().unwrap(), signed_out());
    assert_eq!(account.sign_out().unwrap(), signed_out());
    assert_eq!(f.server.request_count(), 0);
}

#[test]
fn a_session_stored_by_an_older_build_is_read_in_place_and_upgraded_on_the_next_write() {
    let f = fixture();
    let account = f.account_client();
    let token = "LEGACY-SESSION-SENTINEL";
    f.state.sessions.lock().unwrap().push(token.to_string());
    f.backend.put_text(
        SESSION_ACCOUNT,
        &format!(
            r#"{{"token":"{token}","email":"{OWNER_EMAIL}","issuedAtUnix":{}}}"#,
            now() - 100
        ),
    );
    let view = account.status();
    assert!(view.signed_in, "{view:?}");
    assert_eq!(view.email.as_deref(), Some(OWNER_EMAIL));
    // The older item is used as it is.
    assert_eq!(account.list_devices().unwrap().len(), 1);
    assert_eq!(
        sent(&f, "GET", "/v1/shop/account/devices")[0].header("authorization"),
        Some(format!("Bearer {token}").as_str())
    );
    assert!(f.session_item().unwrap().get("rkv").is_none());

    // The next write upgrades it to the envelope and keeps the session and its field names.
    account.refresh().unwrap();
    let upgraded = f.session_item().unwrap();
    assert_eq!(upgraded["rkv"], 1);
    assert!(upgraded["revision"].as_str().is_some());
    assert_eq!(upgraded["token"], token);
    assert_eq!(upgraded["email"], OWNER_EMAIL);
    f.assert_no_secret(
        "the status",
        &serde_json::to_string(&account.status()).unwrap(),
    );
    assert!(!serde_json::to_string(&account.status())
        .unwrap()
        .contains(token));
}

#[test]
fn an_unreadable_session_item_is_signed_out_and_a_sign_in_replaces_it() {
    let f = fixture();
    let account = f.account_client();
    for junk in [
        "not json",
        "[]",
        r#"{"rkv":9,"revision":"r","state":"active","token":"T"}"#,
    ] {
        f.backend.put_text(SESSION_ACCOUNT, junk);
        assert_eq!(account.status(), signed_out(), "{junk}");
        assert_eq!(
            account.list_devices().unwrap_err(),
            AccountError::NotSignedIn
        );
    }
    // A stored token over 16 KiB is refused unread: signed out, and nothing is sent with it.
    let oversized = format!(r#"{{"token":"{}"}}"#, "x".repeat(16 * 1024 + 1));
    f.backend.put_text(SESSION_ACCOUNT, &oversized);
    assert_eq!(account.status(), signed_out());
    assert_eq!(
        account.list_devices().unwrap_err(),
        AccountError::NotSignedIn
    );
    assert_eq!(f.state.devices_calls.load(SeqCst), 0);

    assert!(account.complete_sign_in(code()).unwrap().signed_in);
    assert_eq!(f.session_item().unwrap()["state"], "active");
    // A sign-out clears junk as well.
    f.backend.put_text(SESSION_ACCOUNT, "not json");
    account.sign_out().unwrap();
    assert_eq!(f.session_item().unwrap()["state"], "tombstone");
}

#[test]
fn a_held_account_lock_is_busy_and_bounded_not_a_hang() {
    let f = fixture();
    let account = f.account_client_with(|c| {
        c.with_waits(Duration::from_millis(80), Duration::from_millis(40))
    });
    signed_in(&f, &account);
    assert!(account.status().signed_in);

    let held = CrossProcessLock::try_acquire(f.account_lock_path())
        .unwrap()
        .unwrap();
    let started = Instant::now();
    // Status shows the last view this process saw, marked busy.
    let during = account.status();
    assert!(during.busy && during.signed_in, "{during:?}");
    assert_eq!(account.sign_out().unwrap_err(), AccountError::Busy);
    assert_eq!(
        account.complete_sign_in(code()).unwrap_err(),
        AccountError::Busy
    );
    assert_eq!(account.list_devices().unwrap_err(), AccountError::Busy);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the waits are bounded"
    );
    // The busy refusals changed nothing and the server was not spent.
    assert_eq!(f.state.consume_calls.load(SeqCst), 1);
    assert_eq!(f.state.logout_calls.load(SeqCst), 0);
    drop(held);
    assert!(!account.status().busy);
    assert!(account.status().signed_in);
}

#[test]
fn a_disabled_account_client_has_no_network_and_every_operation_is_unconfigured() {
    let disabled_license =
        LicenseClient::new(LicenseSetup::Disabled, Arc::new(FixedDevice)).unwrap();
    let account = AccountSessionClient::disabled(disabled_license);
    assert!(account.is_disabled());
    assert_eq!(account.status(), signed_out());
    assert_eq!(account.refresh().unwrap_err(), AccountError::Unconfigured);
    assert_eq!(
        account.request_sign_in("a@b.co").unwrap_err(),
        AccountError::Unconfigured
    );
    assert_eq!(
        account.complete_sign_in(code()).unwrap_err(),
        AccountError::Unconfigured
    );
    assert_eq!(account.sign_out().unwrap_err(), AccountError::Unconfigured);
    assert_eq!(
        account.list_devices().unwrap_err(),
        AccountError::Unconfigured
    );
    assert_eq!(
        account.delete_account(confirm()).unwrap_err(),
        AccountError::Unconfigured
    );
    // The link codes are licence operations: a disabled licence says so.
    assert_eq!(
        account.request_link().unwrap_err(),
        LicenseError::Unconfigured
    );
    assert_eq!(
        account.redeem_link(LINK_CODE, None).unwrap_err(),
        LicenseError::Unconfigured
    );
}

// ---- link codes ---------------------------------------------------------------------

#[test]
fn a_link_code_is_requested_with_the_lease_and_never_without_one() {
    let f = fixture();
    let client = f.client();
    let account = f.account_client();

    // No licence: nothing to prove ownership with, and nothing is sent.
    assert_eq!(
        account.request_link().unwrap(),
        LinkRequestOutcome::NotLeased
    );
    // A typed-key activation holds no lease either.
    client.activate(key()).unwrap();
    assert_eq!(
        account.request_link().unwrap(),
        LinkRequestOutcome::NotLeased
    );
    assert_eq!(f.state.link_request_calls.load(SeqCst), 0);

    client.deactivate().unwrap();
    client.lease_activate(key(), None).unwrap();
    let outcome = account.request_link().unwrap();
    let LinkRequestOutcome::Issued(issued) = &outcome else {
        panic!("{outcome:?}")
    };
    assert_eq!(issued.code, LINK_CODE);
    assert!((issued.expires_at - (now() + 600) * 1000).abs() < 10_000);
    assert_eq!(
        f.requests(&format!("/v1/pro/{APP}/devices/windows-link/request")),
        vec![json!({
            "activationToken": ACTIVATION,
            "leaseId": "lease-1",
            "deviceIdHash": DEVICE_HASH
        })]
    );
    // The view carries the code (it is typed by a person) and nothing the lease proves it with.
    let text = serde_json::to_string(&outcome).unwrap();
    assert!(
        text.contains(LINK_CODE) && !text.contains(ACTIVATION),
        "{text}"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["outcome"],
        "issued"
    );

    f.state.link_request_status.store(404, SeqCst);
    assert_eq!(
        account.request_link().unwrap(),
        LinkRequestOutcome::NotActiveLeaseHolder
    );
    f.state.link_request_status.store(429, SeqCst);
    assert_eq!(
        account.request_link().unwrap_err(),
        LicenseError::Rejected {
            reason: RejectReason::RateLimited
        }
    );
    f.state.link_request_status.store(500, SeqCst);
    assert_eq!(account.request_link().unwrap_err(), LicenseError::Network);
}

#[test]
fn a_link_code_is_redeemed_into_a_verified_lease() {
    let f = fixture();
    let account = f.account_client();

    let view = account
        .redeem_link("  LINK-1234 ", Some(" Work Mac "))
        .unwrap();
    assert_eq!(view.status, ViewStatus::Active, "{view:?}");
    assert_eq!(
        f.requests(&format!("/v1/pro/{APP}/devices/windows-link/redeem")),
        vec![json!({
            "code": LINK_CODE,
            "deviceIdHash": DEVICE_HASH,
            "deviceLabel": "Work Mac"
        })]
    );
    // The lease is stored by the licence client's own write, bound to the device hash, and the
    // code is nowhere in the record.
    let stored = f.backend.envelope().unwrap();
    assert_eq!(stored["record"]["deviceId"], DEVICE_HASH);
    assert_eq!(stored["record"]["lease"]["leaseId"], "lease-1");
    assert!(!f.backend.text(ACCOUNT).unwrap().contains(LINK_CODE));
    f.assert_no_secret("the redeemed view", &serde_json::to_string(&view).unwrap());
}

#[test]
fn link_redemption_refusals_are_typed_and_store_nothing() {
    let f = fixture();
    let account = f.account_client();
    let reject = |reason| LicenseError::Rejected { reason };

    // Unknown, expired or used.
    assert_eq!(
        account.redeem_link("OTHER-CODE", None).unwrap_err(),
        reject(RejectReason::KeyNotAccepted)
    );
    f.state.link_redeem_status.store(409, SeqCst);
    assert_eq!(
        account.redeem_link(LINK_CODE, None).unwrap_err(),
        reject(RejectReason::DeviceLimit)
    );
    f.state.link_redeem_status.store(404, SeqCst);
    assert_eq!(
        account.redeem_link(LINK_CODE, None).unwrap_err(),
        reject(RejectReason::KeyNotAccepted)
    );
    assert!(f.backend.text(ACCOUNT).is_none());
    let calls = f.state.link_redeem_calls.load(SeqCst);

    // Refused before anything is sent.
    let long = "x".repeat(200);
    for bad in ["", "   ", "AB CD", "AB\nCD", long.as_str()] {
        assert_eq!(
            account.redeem_link(bad, None).unwrap_err(),
            reject(RejectReason::KeyNotAccepted),
            "{bad:?}"
        );
    }
    assert_eq!(
        account
            .redeem_link(LINK_CODE, Some("bad\u{7}label"))
            .unwrap_err(),
        reject(RejectReason::Other)
    );
    assert_eq!(f.state.link_redeem_calls.load(SeqCst), calls);

    // A lease that fails its checks is never stored, whoever issued the code.
    f.state.link_redeem_status.store(200, SeqCst);
    f.state.wrong_hash.store(true, SeqCst);
    assert_eq!(
        account.redeem_link(LINK_CODE, None).unwrap_err(),
        LicenseError::Verify
    );
    f.state.wrong_hash.store(false, SeqCst);
    f.state.bad_signature.store(true, SeqCst);
    assert_eq!(
        account.redeem_link(LINK_CODE, None).unwrap_err(),
        LicenseError::Verify
    );
    assert!(f.backend.text(ACCOUNT).is_none());
}
