mod common;

use std::cell::{Cell, RefCell};

use common::{pubkey, Claims, APP, DEVICE};
use ed25519_dalek::SigningKey;
use rightkit_license::{
    judge_refresh_response, refresh_and_reanchor, AntiRollbackClock, ClockError, ClockState,
    ClockStore, LicenseRequirement, Lifecycle, MemoryClockStore, ProcessAnchor, RefreshJudgement,
    RefreshOutcome, RefreshParams, RefreshRejection, RefreshRequest, RefreshResponse,
    RefreshTransport, TokenDropReason, TokenKeptReason, TransportError, VerifyError,
};

/// A scripted server.
struct FakeServer {
    reply: RefCell<Result<RefreshResponse, TransportError>>,
    calls: Cell<u32>,
    last_request: RefCell<Option<String>>,
}

impl FakeServer {
    fn replying(body: &str) -> Self {
        Self::with(RefreshResponse::from_json(body))
    }

    fn token(status: &str, token: &str) -> Self {
        Self::replying(&format!(
            r#"{{"status":"{status}","signedToken":"{token}"}}"#
        ))
    }

    fn offline() -> Self {
        Self::with(Err(TransportError::new("network down")))
    }

    fn with(reply: Result<RefreshResponse, TransportError>) -> Self {
        Self {
            reply: RefCell::new(reply),
            calls: Cell::new(0),
            last_request: RefCell::new(None),
        }
    }
}

impl RefreshTransport for FakeServer {
    fn refresh(&self, request: &RefreshRequest<'_>) -> Result<RefreshResponse, TransportError> {
        self.calls.set(self.calls.get() + 1);
        *self.last_request.borrow_mut() = Some(serde_json::to_string(request).unwrap());
        self.reply.borrow().clone()
    }
}

fn state(highest_seen: i64, process: &str, last_server_iat: i64) -> ClockState {
    ClockState {
        highest_seen,
        trusted_process: process.to_string(),
        last_server_iat,
    }
}

fn clock(store: &MemoryClockStore, wall: i64) -> AntiRollbackClock<&MemoryClockStore> {
    AntiRollbackClock::new(store, ProcessAnchor::with_id("proc", wall))
}

fn params(key: &[u8; 32], wall: i64) -> RefreshParams<'_> {
    RefreshParams::new(key, APP, DEVICE, "act_secret", wall)
}

#[test]
fn request_body_matches_the_backend_contract() {
    let key = pubkey();
    let server = FakeServer::replying(r#"{"status":"not_found"}"#);
    let store = MemoryClockStore::new();
    refresh_and_reanchor(&params(&key, 10_000), &server, &clock(&store, 10_000)).unwrap();
    assert_eq!(
        server.last_request.borrow().as_deref(),
        Some(r#"{"app":"heardright","activationToken":"act_secret","deviceId":"dev-1"}"#)
    );
}

#[test]
fn network_failure_is_stale_with_the_first_failure_time_and_touches_nothing() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    store.save(&state(9_000, "old", 8_000)).unwrap();
    let before = store.load().unwrap();

    let mut p = params(&key, 10_000);
    let outcome = refresh_and_reanchor(&p, &FakeServer::offline(), &clock(&store, 10_000)).unwrap();
    assert_eq!(
        outcome,
        RefreshOutcome::Stale {
            stale_since: 10_000,
            error: "network down".to_string()
        }
    );

    // A later failure keeps the earlier stamp.
    p.stale_since = Some(10_000);
    p.wall_now = 99_999;
    let outcome = refresh_and_reanchor(&p, &FakeServer::offline(), &clock(&store, 99_999)).unwrap();
    assert!(matches!(
        outcome,
        RefreshOutcome::Stale {
            stale_since: 10_000,
            ..
        }
    ));
    assert_eq!(outcome.as_str(), "stale");
    assert_eq!(store.load().unwrap(), before);
}

#[test]
fn unparsable_response_is_a_transport_failure_not_a_revocation() {
    assert!(RefreshResponse::from_json("<html>bad gateway</html>").is_err());
    assert!(
        RefreshResponse::from_json("{}").is_err(),
        "status is required"
    );
}

#[test]
fn non_active_status_is_revoked_and_leaves_the_clock_alone() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    store.save(&state(9_000, "old", 8_000)).unwrap();
    let before = store.load().unwrap();
    let server =
        FakeServer::replying(r#"{"ok":false,"status":"not_found","message":"License inactive"}"#);
    let outcome =
        refresh_and_reanchor(&params(&key, 10_000), &server, &clock(&store, 10_000)).unwrap();
    assert_eq!(
        outcome,
        RefreshOutcome::Revoked {
            message: Some("License inactive".to_string())
        }
    );
    assert_eq!(outcome.as_str(), "revoked");
    assert_eq!(store.load().unwrap(), before);
}

#[test]
fn fresh_valid_token_replaces_and_reanchors_before_returning() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    store.save(&state(10_000, "old", 8_000)).unwrap();
    let old = Claims::trial(8_000, 20_000).token();
    let fresh = Claims::trial(10_020, 30_000).token();
    let server = FakeServer::replying(&format!(
        r#"{{"status":"active","signedToken":"{fresh}","validUntil":"2026-12-01T00:00:00.000Z","entitlements":{{"v":1,"tier":"pro","features":[]}}}}"#
    ));
    let mut p = params(&key, 10_030);
    p.cached_token = Some(&old);
    p.clock_initialized = true;
    let outcome = refresh_and_reanchor(&p, &server, &clock(&store, 10_030)).unwrap();
    let RefreshOutcome::TokenReplaced {
        token,
        entitlement,
        lifecycle,
        reanchored,
        response,
    } = outcome
    else {
        panic!("expected TokenReplaced");
    };
    assert_eq!(token, fresh);
    assert_eq!(entitlement.exp, 30_000);
    assert_eq!(lifecycle, Lifecycle::Trial);
    assert!(!reanchored.lowered);
    assert_eq!(reanchored.state, state(10_020, "proc", 10_020));
    assert_eq!(response.extra["validUntil"], "2026-12-01T00:00:00.000Z");
    assert_eq!(
        store.load().unwrap(),
        Some(reanchored.state),
        "clock persisted before the caller writes the record"
    );
}

#[test]
fn first_ever_refresh_seeds_the_ledger() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    let fresh = Claims::legacy(10_020, 30_000).token();
    let server = FakeServer::token("active", &fresh);
    let outcome =
        refresh_and_reanchor(&params(&key, 10_030), &server, &clock(&store, 10_030)).unwrap();
    assert_eq!(outcome.as_str(), "tokenReplaced");
    assert_eq!(store.load().unwrap(), Some(state(10_030, "proc", 10_020)));
}

#[test]
fn repair_path_lowers_an_inflated_mark_and_says_so() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    store.save(&state(900_000, "old", 8_000)).unwrap();
    let fresh = Claims::trial(10_020, 30_000).token();
    let server = FakeServer::token("active", &fresh);
    let mut p = params(&key, 10_030);
    p.clock_initialized = true;
    let outcome = refresh_and_reanchor(&p, &server, &clock(&store, 10_030)).unwrap();
    let RefreshOutcome::TokenReplaced { reanchored, .. } = outcome else {
        panic!("expected TokenReplaced");
    };
    assert!(reanchored.lowered);
    assert_eq!(store.load().unwrap(), Some(state(10_020, "proc", 10_020)));
}

#[test]
fn replayed_server_time_is_rejected_and_the_ledger_is_untouched() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    store.save(&state(10_000, "old", 10_020)).unwrap();
    let before = store.load().unwrap();
    for iat in [10_020, 10_019] {
        let fresh = Claims::trial(iat, 30_000).token();
        let server = FakeServer::token("active", &fresh);
        let mut p = params(&key, 10_030);
        p.clock_initialized = true;
        let outcome = refresh_and_reanchor(&p, &server, &clock(&store, 10_030)).unwrap();
        assert_eq!(
            outcome,
            RefreshOutcome::Rejected(RefreshRejection::ReplayedServerTime {
                last_server_iat: 10_020,
                got: iat
            })
        );
        assert_eq!(store.load().unwrap(), before);
    }
}

#[test]
fn forged_token_is_rejected() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    let forged = Claims::trial(10_020, 30_000).token_signed_by(&SigningKey::from_bytes(&[9; 32]));
    let outcome = refresh_and_reanchor(
        &params(&key, 10_030),
        &FakeServer::token("active", &forged),
        &clock(&store, 10_030),
    )
    .unwrap();
    assert_eq!(
        outcome,
        RefreshOutcome::Rejected(RefreshRejection::InvalidToken(VerifyError::BadSignature))
    );
    assert_eq!(store.load().unwrap(), None);
}

#[test]
fn token_for_another_device_is_rejected() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    let other = Claims {
        device: "someone-else",
        ..Claims::trial(10_020, 30_000)
    }
    .token();
    let outcome = refresh_and_reanchor(
        &params(&key, 10_030),
        &FakeServer::token("active", &other),
        &clock(&store, 10_030),
    )
    .unwrap();
    assert_eq!(
        outcome,
        RefreshOutcome::Rejected(RefreshRejection::InvalidToken(VerifyError::DeviceMismatch))
    );
}

#[test]
fn token_that_is_not_active_on_arrival_is_rejected() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    for (token, expected) in [
        (Claims::legacy(10_020, 10_020).token(), Lifecycle::Expired),
        (
            Claims::trial(10_020, 10_020).token(),
            Lifecycle::ExpiredTrial,
        ),
        (
            Claims {
                kind: Some("lifetime"),
                ..Claims::legacy(10_020, 30_000)
            }
            .token(),
            Lifecycle::Invalid,
        ),
    ] {
        let outcome = refresh_and_reanchor(
            &params(&key, 10_030),
            &FakeServer::token("active", &token),
            &clock(&store, 10_030),
        )
        .unwrap();
        assert_eq!(
            outcome,
            RefreshOutcome::Rejected(RefreshRejection::NotActive(expected))
        );
    }
    assert_eq!(
        store.load().unwrap(),
        None,
        "no anchor from an unusable token"
    );
}

#[test]
fn requirement_is_enforced_on_the_fresh_token() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    let pro = LicenseRequirement::pro();
    let plus = Claims {
        tier: "plus",
        ..Claims::trial(10_020, 30_000)
    }
    .token();
    let mut p = params(&key, 10_030);
    p.requirement = Some(&pro);
    let outcome = refresh_and_reanchor(
        &p,
        &FakeServer::token("active", &plus),
        &clock(&store, 10_030),
    )
    .unwrap();
    assert_eq!(
        outcome,
        RefreshOutcome::Rejected(RefreshRejection::RequirementNotMet)
    );
    assert_eq!(store.load().unwrap(), None);
}

#[test]
fn omitted_token_keeps_a_cached_token_that_is_still_valid() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    store.save(&state(10_000, "proc", 8_000)).unwrap();
    let cached = Claims::trial(8_000, 20_000).token();
    for body in [
        r#"{"status":"active"}"#,
        r#"{"status":"active","signedToken":null}"#,
        r#"{"status":"active","signedToken":"   "}"#,
    ] {
        let mut p = params(&key, 10_030);
        p.cached_token = Some(&cached);
        p.clock_initialized = true;
        let outcome =
            refresh_and_reanchor(&p, &FakeServer::replying(body), &clock(&store, 10_030)).unwrap();
        let RefreshOutcome::TokenKept {
            reason,
            lifecycle,
            valid_until,
            ..
        } = outcome
        else {
            panic!("expected TokenKept for {body}");
        };
        assert_eq!(reason, TokenKeptReason::ServerOmittedToken);
        assert_eq!(lifecycle, Lifecycle::Trial);
        assert_eq!(valid_until, 20_000);
    }
    assert_eq!(
        store.load().unwrap().unwrap().last_server_iat,
        8_000,
        "keeping a token never re-anchors"
    );
}

#[test]
fn same_token_echoed_back_is_kept_not_replaced() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    store.save(&state(10_000, "proc", 8_000)).unwrap();
    let cached = Claims::trial(8_000, 20_000).token();
    let mut p = params(&key, 10_030);
    p.cached_token = Some(&cached);
    p.clock_initialized = true;
    let outcome = refresh_and_reanchor(
        &p,
        &FakeServer::token("active", &cached),
        &clock(&store, 10_030),
    )
    .unwrap();
    assert!(matches!(
        outcome,
        RefreshOutcome::TokenKept {
            reason: TokenKeptReason::ServerReturnedSameToken,
            ..
        }
    ));
}

#[test]
fn omitted_token_drops_a_cached_token_that_has_lapsed() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    store.save(&state(10_000, "proc", 8_000)).unwrap();
    for (cached, expected) in [
        (
            Claims::trial(8_000, 10_000).token(),
            Lifecycle::ExpiredTrial,
        ),
        (Claims::term(8_000, 10_000).token(), Lifecycle::Expired),
        (
            Claims::provisional(8_000, 10_000).token(),
            Lifecycle::VerificationRequired,
        ),
    ] {
        let mut p = params(&key, 10_030);
        p.cached_token = Some(&cached);
        p.clock_initialized = true;
        let outcome = refresh_and_reanchor(
            &p,
            &FakeServer::replying(r#"{"status":"active"}"#),
            &clock(&store, 10_030),
        )
        .unwrap();
        let RefreshOutcome::TokenDropped { reason, .. } = outcome else {
            panic!("expected TokenDropped");
        };
        assert_eq!(reason, TokenDropReason::CachedTokenLapsed(expected));
    }
}

#[test]
fn omitted_token_does_not_keep_a_cached_token_on_a_rolled_back_clock() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    store.save(&state(900_000, "other", 8_000)).unwrap();
    let cached = Claims::trial(8_000, 5_000_000).token();
    let mut p = params(&key, 10_030);
    p.cached_token = Some(&cached);
    p.clock_initialized = true;
    let outcome = refresh_and_reanchor(
        &p,
        &FakeServer::replying(r#"{"status":"active"}"#),
        &clock(&store, 10_030),
    )
    .unwrap();
    assert!(matches!(
        outcome,
        RefreshOutcome::TokenDropped {
            reason: TokenDropReason::CachedTokenLapsed(Lifecycle::ClockRepairRequired),
            ..
        }
    ));
}

#[test]
fn omitted_token_with_nothing_cached_is_dropped() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    let outcome = refresh_and_reanchor(
        &params(&key, 10_030),
        &FakeServer::replying(r#"{"status":"active"}"#),
        &clock(&store, 10_030),
    )
    .unwrap();
    assert!(matches!(
        outcome,
        RefreshOutcome::TokenDropped {
            reason: TokenDropReason::NoCachedToken,
            ..
        }
    ));
    assert_eq!(outcome.as_str(), "tokenDropped");
}

#[test]
fn missing_ledger_on_an_initialised_install_fails_before_any_network_call() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    let server = FakeServer::token("active", "irrelevant");
    let mut p = params(&key, 10_030);
    p.clock_initialized = true;
    assert_eq!(
        refresh_and_reanchor(&p, &server, &clock(&store, 10_030)),
        Err(ClockError::Missing)
    );
    assert_eq!(server.calls.get(), 0);
}

#[test]
fn outcome_names_match_the_js_client() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    let fresh = Claims::trial(10_020, 30_000).token();
    let replaced = refresh_and_reanchor(
        &params(&key, 10_030),
        &FakeServer::token("active", &fresh),
        &clock(&store, 10_030),
    )
    .unwrap();
    assert_eq!(replaced.as_str(), "tokenReplaced");
    let rejected = RefreshOutcome::Rejected(RefreshRejection::RequirementNotMet);
    assert_eq!(rejected.as_str(), "rejected");
}

// ------------------------------------------------------------------ pure judgement

fn response(status: &str, token: Option<&str>) -> RefreshResponse {
    match token {
        Some(token) => RefreshResponse::from_json(&format!(
            r#"{{"status":"{status}","signedToken":"{token}"}}"#
        )),
        None => RefreshResponse::from_json(&format!(r#"{{"status":"{status}"}}"#)),
    }
    .unwrap()
}

#[test]
fn judge_covers_every_server_answer_without_touching_a_store() {
    let key = pubkey();
    let p = params(&key, 10_030);
    let fresh = Claims::trial(10_020, 30_000).token();

    assert_eq!(
        judge_refresh_response(&p, &response("not_found", None), None),
        RefreshJudgement::Revoked { message: None }
    );
    assert_eq!(
        judge_refresh_response(&p, &response("active", None), None),
        RefreshJudgement::NoNewToken {
            reason: TokenKeptReason::ServerOmittedToken
        }
    );
    let mut cached = params(&key, 10_030);
    cached.cached_token = Some(&fresh);
    assert_eq!(
        judge_refresh_response(&cached, &response("active", Some(&fresh)), None),
        RefreshJudgement::NoNewToken {
            reason: TokenKeptReason::ServerReturnedSameToken
        }
    );

    let RefreshJudgement::ReplaceToken {
        token,
        lifecycle,
        anchor,
        ..
    } = judge_refresh_response(&p, &response("active", Some(&fresh)), None)
    else {
        panic!("expected ReplaceToken");
    };
    assert_eq!(token, fresh);
    assert_eq!(lifecycle, Lifecycle::Trial);
    assert_eq!(anchor.into_state("proc"), state(10_030, "proc", 10_020));
}

#[test]
fn judge_reports_exp_and_iat_of_a_token_that_is_not_active_on_arrival() {
    let key = pubkey();
    let p = params(&key, 10_030);
    let expired = Claims::legacy(10_020, 10_020).token();
    let RefreshJudgement::Rejected { rejection, token } =
        judge_refresh_response(&p, &response("active", Some(&expired)), None)
    else {
        panic!("expected Rejected");
    };
    assert_eq!(rejection, RefreshRejection::NotActive(Lifecycle::Expired));
    let claims = token.expect("an authentic token reports its claims");
    assert_eq!((claims.iat, claims.exp), (10_020, 10_020));
    assert_eq!(claims.lifecycle, Lifecycle::Expired);

    // A token that does not authenticate has no trustworthy claims.
    let forged = Claims::trial(10_020, 30_000).token_signed_by(&SigningKey::from_bytes(&[9; 32]));
    assert!(matches!(
        judge_refresh_response(&p, &response("active", Some(&forged)), None),
        RefreshJudgement::Rejected {
            rejection: RefreshRejection::InvalidToken(_),
            token: None
        }
    ));
}

#[test]
fn judge_applies_the_ledger_rules_to_the_prior_it_is_given() {
    let key = pubkey();
    let fresh = Claims::trial(10_020, 30_000).token();
    let mut p = params(&key, 10_030);
    p.clock_initialized = true;

    assert_eq!(
        judge_refresh_response(&p, &response("active", Some(&fresh)), None),
        RefreshJudgement::LedgerMissing
    );
    let replayed = state(10_000, "old", 10_020);
    let RefreshJudgement::Rejected { rejection, token } =
        judge_refresh_response(&p, &response("active", Some(&fresh)), Some(&replayed))
    else {
        panic!("expected Rejected");
    };
    assert_eq!(
        rejection,
        RefreshRejection::ReplayedServerTime {
            last_server_iat: 10_020,
            got: 10_020
        }
    );
    assert_eq!(token.unwrap().exp, 30_000);

    let inflated = state(900_000, "old", 8_000);
    let RefreshJudgement::ReplaceToken { anchor, .. } =
        judge_refresh_response(&p, &response("active", Some(&fresh)), Some(&inflated))
    else {
        panic!("expected ReplaceToken");
    };
    assert!(anchor.lowered);
}

/// The outcome and the pure judgement agree on every scenario, because the first is built
/// on the second: judge against the ledger as it was before, then compare with the run.
#[test]
fn refresh_and_reanchor_agrees_with_the_pure_judgement() {
    let key = pubkey();
    let fresh = Claims::trial(10_020, 30_000).token();
    let expired = Claims::legacy(10_020, 10_020).token();
    let ledgers = [
        None,
        Some(state(900_000, "old", 8_000)),
        Some(state(10_000, "old", 10_020)),
        Some(state(10_000, "old", 8_000)),
    ];
    for ledger in ledgers {
        for token in [&fresh, &expired] {
            let store = MemoryClockStore::new();
            if let Some(ledger) = &ledger {
                store.save(ledger).unwrap();
            }
            let mut p = params(&key, 10_030);
            p.clock_initialized = ledger.is_some();
            let body = response("active", Some(token));
            let judged = judge_refresh_response(&p, &body, ledger.as_ref());
            let outcome = refresh_and_reanchor(
                &p,
                &FakeServer::token("active", token),
                &clock(&store, 10_030),
            )
            .unwrap();
            match (judged, outcome) {
                (
                    RefreshJudgement::Rejected { rejection, .. },
                    RefreshOutcome::Rejected(actual),
                ) => assert_eq!(rejection, actual),
                (
                    RefreshJudgement::ReplaceToken { anchor, .. },
                    RefreshOutcome::TokenReplaced { reanchored, .. },
                ) => {
                    assert_eq!(reanchored.state, anchor.into_state("proc"));
                    assert_eq!(reanchored.lowered, anchor.lowered);
                }
                (judged, outcome) => panic!("diverged: {judged:?} vs {outcome:?}"),
            }
        }
    }
}
