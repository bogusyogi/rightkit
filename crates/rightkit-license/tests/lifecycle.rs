mod common;

use common::{pubkey, Claims, APP, DEVICE};
use rightkit_license::{
    authenticate_token, evaluate_lifecycle, verify_token, AntiRollbackClock, ClockError,
    ClockState, ClockStore, Entitlement, EntitlementStage, LicenseKind, LicenseRequirement,
    Lifecycle, LifecycleContext, LifecyclePolicy, MemoryClockStore, ProcessAnchor,
    StoredCredential, VerifyError,
};

fn ent(
    kind: Option<LicenseKind>,
    stage: Option<EntitlementStage>,
    issued: Option<i64>,
    due: Option<i64>,
    iat: i64,
    exp: i64,
) -> Entitlement {
    Entitlement {
        v: 2,
        id: "lic_1".into(),
        app: APP.into(),
        tier: "pro".into(),
        features: vec![],
        device_id: DEVICE.into(),
        iat,
        exp,
        license_kind: kind,
        entitlement_stage: stage,
        license_issued_at: issued,
        confirmation_due_at: due,
    }
}

fn legacy(iat: i64, exp: i64) -> Entitlement {
    ent(None, None, None, None, iat, exp)
}
fn trial(iat: i64, exp: i64) -> Entitlement {
    ent(
        Some(LicenseKind::Trial),
        Some(EntitlementStage::TimeBound),
        Some(iat),
        None,
        iat,
        exp,
    )
}
fn term(iat: i64, exp: i64) -> Entitlement {
    ent(
        Some(LicenseKind::Term),
        Some(EntitlementStage::TimeBound),
        Some(iat),
        None,
        iat,
        exp,
    )
}
fn provisional(iat: i64, due: i64) -> Entitlement {
    ent(
        Some(LicenseKind::Lifetime),
        Some(EntitlementStage::Provisional),
        Some(iat),
        Some(due),
        iat,
        due,
    )
}
fn final_lifetime(issued: i64, due: i64, iat: i64, exp: i64) -> Entitlement {
    ent(
        Some(LicenseKind::Lifetime),
        Some(EntitlementStage::Final),
        Some(issued),
        Some(due),
        iat,
        exp,
    )
}

const NO_GRACE: LifecyclePolicy = LifecyclePolicy {
    grace_secs: 0,
    lifetime_survives_rollback: false,
};
const GRACE_100: LifecyclePolicy = LifecyclePolicy {
    grace_secs: 100,
    lifetime_survives_rollback: false,
};

#[test]
fn classification_table() {
    use Lifecycle::*;
    let cases = [
        ("legacy live", legacy(1000, 2000), 1999, NO_GRACE, Active),
        ("legacy at exp", legacy(1000, 2000), 2000, NO_GRACE, Expired),
        (
            "legacy past exp",
            legacy(1000, 2000),
            9000,
            NO_GRACE,
            Expired,
        ),
        (
            "legacy in grace",
            legacy(1000, 2000),
            2099,
            GRACE_100,
            Grace,
        ),
        (
            "legacy past grace",
            legacy(1000, 2000),
            2100,
            GRACE_100,
            Expired,
        ),
        ("term live", term(1000, 2000), 1500, NO_GRACE, Active),
        ("term expired", term(1000, 2000), 2000, NO_GRACE, Expired),
        ("term in grace", term(1000, 2000), 2050, GRACE_100, Grace),
        ("trial live", trial(1000, 2000), 1999, NO_GRACE, Trial),
        (
            "trial at exp",
            trial(1000, 2000),
            2000,
            NO_GRACE,
            ExpiredTrial,
        ),
        (
            "trial never graced",
            trial(1000, 2000),
            2050,
            GRACE_100,
            ExpiredTrial,
        ),
        (
            "provisional live",
            provisional(1000, 2000),
            1999,
            NO_GRACE,
            LifetimeProvisional,
        ),
        (
            "provisional at due",
            provisional(1000, 2000),
            2000,
            NO_GRACE,
            VerificationRequired,
        ),
        (
            "provisional never graced",
            provisional(1000, 2000),
            2050,
            GRACE_100,
            VerificationRequired,
        ),
        (
            "final live",
            final_lifetime(1000, 2000, 2000, 5000),
            4999,
            NO_GRACE,
            Lifetime,
        ),
        (
            "final expired",
            final_lifetime(1000, 2000, 2000, 5000),
            5000,
            NO_GRACE,
            Expired,
        ),
        (
            "final in grace",
            final_lifetime(1000, 2000, 2000, 5000),
            5050,
            GRACE_100,
            Grace,
        ),
        (
            "negative grace is zero",
            legacy(1000, 2000),
            2000,
            LifecyclePolicy {
                grace_secs: -500,
                lifetime_survives_rollback: false,
            },
            Expired,
        ),
        (
            "partial claims (kind only)",
            ent(Some(LicenseKind::Lifetime), None, None, None, 1000, 2000),
            1500,
            NO_GRACE,
            Invalid,
        ),
        (
            "trial carrying a due date",
            ent(
                Some(LicenseKind::Trial),
                Some(EntitlementStage::TimeBound),
                Some(1000),
                Some(1500),
                1000,
                2000,
            ),
            1500,
            NO_GRACE,
            Invalid,
        ),
        (
            "provisional exp != due",
            ent(
                Some(LicenseKind::Lifetime),
                Some(EntitlementStage::Provisional),
                Some(1000),
                Some(2000),
                1000,
                3000,
            ),
            1500,
            NO_GRACE,
            Invalid,
        ),
        (
            "trial with lifetime stage",
            ent(
                Some(LicenseKind::Trial),
                Some(EntitlementStage::Final),
                Some(1000),
                None,
                1000,
                2000,
            ),
            1500,
            NO_GRACE,
            Invalid,
        ),
    ];
    for (name, entitlement, now, policy, expected) in cases {
        assert_eq!(entitlement.lifecycle(now, &policy), expected, "{name}");
    }
}

#[test]
fn has_valid_lifecycle_is_public_and_matches_verify_token() {
    assert!(legacy(1000, 2000).has_valid_lifecycle());
    assert!(trial(1000, 2000).has_valid_lifecycle());
    assert!(!ent(Some(LicenseKind::Term), None, None, None, 1000, 2000).has_valid_lifecycle());

    let bad = Claims {
        kind: Some("lifetime"),
        ..Claims::legacy(1000, 2000)
    }
    .token();
    assert_eq!(
        verify_token(&bad, &pubkey(), APP, DEVICE, 1500),
        Err(VerifyError::InvalidLifecycle)
    );
    // authenticate_token accepts it (no shape check); the entitlement then self-reports.
    let authenticated = authenticate_token(&bad, &pubkey(), APP, DEVICE).expect("authentic");
    assert!(!authenticated.has_valid_lifecycle());
}

#[test]
fn authenticate_token_ignores_time_but_not_identity() {
    let expired = Claims::trial(1000, 2000).token();
    assert!(authenticate_token(&expired, &pubkey(), APP, DEVICE).is_ok());
    assert!(matches!(
        verify_token(&expired, &pubkey(), APP, DEVICE, 5000),
        Err(VerifyError::Expired { .. })
    ));
    assert_eq!(
        authenticate_token(&expired, &pubkey(), APP, "other-device"),
        Err(VerifyError::DeviceMismatch)
    );
    assert!(matches!(
        authenticate_token(&expired, &pubkey(), "coderight", DEVICE),
        Err(VerifyError::AppMismatch { .. })
    ));
    assert_eq!(
        authenticate_token(&expired, &common::other_pubkey(), APP, DEVICE),
        Err(VerifyError::BadSignature)
    );
}

#[test]
fn wire_names_and_helpers() {
    let all = [
        (Lifecycle::Unlicensed, "unlicensed", "free", false, false),
        (
            Lifecycle::MigrationRequired,
            "migration_required",
            "migration_required",
            false,
            true,
        ),
        (Lifecycle::Trial, "trial", "active_trial", true, false),
        (
            Lifecycle::ExpiredTrial,
            "expired_trial",
            "expired_trial",
            false,
            false,
        ),
        (Lifecycle::Active, "active", "active_legacy", true, false),
        (Lifecycle::Grace, "grace", "active_legacy", true, false),
        (Lifecycle::Expired, "expired", "invalid", false, false),
        (
            Lifecycle::LifetimeProvisional,
            "lifetime_provisional",
            "active_lifetime_provisional",
            true,
            false,
        ),
        (
            Lifecycle::VerificationRequired,
            "verification_required",
            "verification_required",
            false,
            true,
        ),
        (
            Lifecycle::Lifetime,
            "lifetime",
            "active_lifetime",
            true,
            false,
        ),
        (
            Lifecycle::ClockRepairRequired,
            "clock_repair_required",
            "clock_repair_required",
            false,
            true,
        ),
        (Lifecycle::Revoked, "revoked", "invalid", false, false),
        (Lifecycle::Invalid, "invalid", "invalid", false, false),
    ];
    for (lifecycle, name, snapshot, grants, online) in all {
        assert_eq!(lifecycle.as_str(), name);
        assert_eq!(lifecycle.snapshot_kind(), snapshot, "{name}");
        assert_eq!(lifecycle.grants_access(), grants, "{name}");
        assert_eq!(lifecycle.needs_online_refresh(), online, "{name}");
        let json = serde_json::to_string(&lifecycle).unwrap();
        assert_eq!(json, format!("\"{name}\""));
        assert_eq!(serde_json::from_str::<Lifecycle>(&json).unwrap(), lifecycle);
    }
}

// ---------------------------------------------------------------- evaluate_lifecycle

fn clock_at<'a>(
    store: &'a MemoryClockStore,
    process: &str,
    wall: i64,
) -> AntiRollbackClock<&'a MemoryClockStore> {
    AntiRollbackClock::new(store, ProcessAnchor::with_id(process, wall))
}

fn signed(token: &str) -> StoredCredential<'_> {
    StoredCredential::Signed {
        token,
        clock_initialized: false,
    }
}

#[test]
fn non_token_credentials_never_touch_the_clock() {
    let key = pubkey();
    let ctx = LifecycleContext::new(&key, APP, DEVICE);
    let store = MemoryClockStore::new();
    let clock = clock_at(&store, "p1", 10_000);
    for (credential, expected) in [
        (StoredCredential::Absent, Lifecycle::Unlicensed),
        (StoredCredential::Revoked, Lifecycle::Revoked),
        (
            StoredCredential::LegacyUnsigned,
            Lifecycle::MigrationRequired,
        ),
        (StoredCredential::Unsigned, Lifecycle::Invalid),
    ] {
        let report = evaluate_lifecycle(&ctx, &clock, credential, 10_000).unwrap();
        assert_eq!(report.lifecycle, expected);
        assert!(!report.grants_access());
        assert!(!report.clock_initialized);
        assert!(report.entitlement.is_none());
    }
    assert_eq!(store.load().unwrap(), None);
}

#[test]
fn trusted_token_classifies_and_initialises_the_clock() {
    let key = pubkey();
    let ctx = LifecycleContext::new(&key, APP, DEVICE);
    let store = MemoryClockStore::new();
    let clock = clock_at(&store, "p1", 10_000);
    let token = Claims::trial(9_000, 20_000).token();
    let report = evaluate_lifecycle(&ctx, &clock, signed(&token), 10_000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::Trial);
    assert!(report.grants_access());
    assert_eq!(report.expires_at, Some(20_000));
    assert_eq!(report.effective_now, Some(10_000));
    assert!(report.clock_initialized);
    assert_eq!(report.entitlement.as_ref().unwrap().id, "lic_1");
    assert_eq!(store.load().unwrap().unwrap().highest_seen, 10_000);
}

#[test]
fn moving_the_clock_forward_expires_the_license() {
    let key = pubkey();
    let ctx = LifecycleContext::new(&key, APP, DEVICE);
    let store = MemoryClockStore::new();
    let clock = clock_at(&store, "p1", 10_000);
    let token = Claims::trial(9_000, 13_000).token();
    let first = evaluate_lifecycle(&ctx, &clock, signed(&token), 10_000).unwrap();
    assert_eq!(first.lifecycle, Lifecycle::Trial);
    let later = StoredCredential::Signed {
        token: &token,
        clock_initialized: true,
    };
    let second = evaluate_lifecycle(&ctx, &clock, later, 13_600).unwrap();
    assert_eq!(second.lifecycle, Lifecycle::ExpiredTrial);
    assert_eq!(second.effective_now, Some(13_600));
    assert!(!second.grants_access());
}

#[test]
fn small_backward_step_cannot_extend_validity() {
    let key = pubkey();
    let ctx = LifecycleContext::new(&key, APP, DEVICE);
    let store = MemoryClockStore::new();
    let clock = clock_at(&store, "p1", 10_000);
    let token = Claims::trial(9_000, 10_100).token();
    evaluate_lifecycle(&ctx, &clock, signed(&token), 10_000).unwrap();
    // Wall clock wound back 200s (inside the 300s tolerance), same process: time must not
    // move backwards, so the effective time stays at the high-water mark.
    let again = StoredCredential::Signed {
        token: &token,
        clock_initialized: true,
    };
    let report = evaluate_lifecycle(&ctx, &clock, again, 9_800).unwrap();
    assert_eq!(report.effective_now, Some(10_000));
    assert_eq!(report.lifecycle, Lifecycle::Trial);
}

#[test]
fn large_rollback_after_restart_requires_repair_and_is_not_persisted() {
    let key = pubkey();
    let ctx = LifecycleContext::new(&key, APP, DEVICE);
    let store = MemoryClockStore::new();
    let token = Claims::trial(9_000, 20_000).token();
    let first = clock_at(&store, "process-a", 10_000);
    evaluate_lifecycle(&ctx, &first, signed(&token), 10_000).unwrap();
    let before = store.load().unwrap();

    let restarted = clock_at(&store, "process-b", 5_000);
    let again = StoredCredential::Signed {
        token: &token,
        clock_initialized: true,
    };
    let report = evaluate_lifecycle(&ctx, &restarted, again, 5_000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::ClockRepairRequired);
    assert!(!report.grants_access());
    assert!(report.lifecycle.needs_online_refresh());
    assert!(report.message.as_deref().unwrap().contains("10000"));
    assert_eq!(report.expires_at, Some(20_000));
    assert_eq!(
        store.load().unwrap(),
        before,
        "a rollback must not rewrite the ledger"
    );
}

#[test]
fn final_lifetime_survives_rollback_only_when_the_policy_allows_it() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    let token = Claims::final_lifetime(1_000, 2_000, 2_000, 9_999_999_999).token();
    let first = clock_at(&store, "process-a", 10_000);
    let default_ctx = LifecycleContext::new(&key, APP, DEVICE);
    evaluate_lifecycle(&default_ctx, &first, signed(&token), 10_000).unwrap();
    let before = store.load().unwrap();

    let restarted = clock_at(&store, "process-b", 5_000);
    let credential = StoredCredential::Signed {
        token: &token,
        clock_initialized: true,
    };

    let strict = evaluate_lifecycle(&default_ctx, &restarted, credential, 5_000).unwrap();
    assert_eq!(strict.lifecycle, Lifecycle::ClockRepairRequired);

    let lenient_ctx = LifecycleContext::new(&key, APP, DEVICE).with_policy(LifecyclePolicy {
        grace_secs: 0,
        lifetime_survives_rollback: true,
    });
    let lenient = evaluate_lifecycle(&lenient_ctx, &restarted, credential, 5_000).unwrap();
    assert_eq!(lenient.lifecycle, Lifecycle::Lifetime);
    assert_eq!(
        lenient.effective_now,
        Some(10_000),
        "evaluated at the high-water mark"
    );
    assert_eq!(store.load().unwrap(), before);

    // A non-final lifetime token gets no such exemption.
    let provisional = Claims::provisional(2_000, 99_999).token();
    let provisional_credential = StoredCredential::Signed {
        token: &provisional,
        clock_initialized: true,
    };
    let report =
        evaluate_lifecycle(&lenient_ctx, &restarted, provisional_credential, 5_000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::ClockRepairRequired);
}

#[test]
fn missing_ledger_on_an_initialised_install_is_an_error() {
    let key = pubkey();
    let ctx = LifecycleContext::new(&key, APP, DEVICE);
    let store = MemoryClockStore::new();
    let clock = clock_at(&store, "p1", 10_000);
    let token = Claims::trial(9_000, 20_000).token();
    let credential = StoredCredential::Signed {
        token: &token,
        clock_initialized: true,
    };
    assert_eq!(
        evaluate_lifecycle(&ctx, &clock, credential, 10_000),
        Err(ClockError::Missing)
    );
    assert_eq!(
        store.load().unwrap(),
        None,
        "must not silently restart the ledger"
    );
}

#[test]
fn issue_time_skew_is_checked_on_the_expired_path() {
    // Regression guard (ViewRight license.rs ~478-524 / 627-662): an already-expired
    // token used to be re-verified at `exp - 1`, which bypassed verify_token's own skew
    // test, so the explicit `effective_now + 300 < iat` check was the only iat check on
    // that path. Legacy tokens carry no iat<=exp constraint, so this shape is reachable:
    // exp long past, iat far in the future relative to the trusted clock.
    let key = pubkey();
    let ctx = LifecycleContext::new(&key, APP, DEVICE);
    let store = MemoryClockStore::new();
    let clock = clock_at(&store, "p1", 50_000);
    let token = Claims::legacy(100_000, 3_000).token();
    let report = evaluate_lifecycle(&ctx, &clock, signed(&token), 50_000).unwrap();
    assert_eq!(
        report.lifecycle,
        Lifecycle::Invalid,
        "skew wins over Expired"
    );
    assert!(report.message.as_deref().unwrap().contains("issue time"));
    assert!(report.entitlement.is_some());
    assert_eq!(
        store.load().unwrap(),
        None,
        "an invalid verdict must not write the ledger"
    );

    // Boundary: exactly `iat - 300` is tolerated, one second earlier is not.
    let skewed = Claims::legacy(50_300, 3_000).token();
    let tolerated = evaluate_lifecycle(&ctx, &clock, signed(&skewed), 50_000).unwrap();
    assert_eq!(tolerated.lifecycle, Lifecycle::Expired);
    let too_far = Claims::legacy(50_301, 3_000).token();
    let rejected = evaluate_lifecycle(&ctx, &clock, signed(&too_far), 50_000).unwrap();
    assert_eq!(rejected.lifecycle, Lifecycle::Invalid);
}

#[test]
fn skew_is_also_checked_on_the_unexpired_path() {
    let key = pubkey();
    let ctx = LifecycleContext::new(&key, APP, DEVICE);
    let store = MemoryClockStore::new();
    let clock = clock_at(&store, "p1", 1_000);
    let token = Claims::trial(5_000, 9_000).token();
    let report = evaluate_lifecycle(&ctx, &clock, signed(&token), 1_000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::Invalid);
}

#[test]
fn expired_tokens_still_report_their_true_kind() {
    let key = pubkey();
    let ctx = LifecycleContext::new(&key, APP, DEVICE);
    let store = MemoryClockStore::new();
    let clock = clock_at(&store, "p1", 30_000);
    for (claims, expected) in [
        (Claims::trial(1_000, 2_000), Lifecycle::ExpiredTrial),
        (Claims::term(1_000, 2_000), Lifecycle::Expired),
        (Claims::legacy(1_000, 2_000), Lifecycle::Expired),
        (
            Claims::provisional(1_000, 2_000),
            Lifecycle::VerificationRequired,
        ),
        (
            Claims::final_lifetime(500, 1_000, 1_000, 2_000),
            Lifecycle::Expired,
        ),
    ] {
        let token = claims.token();
        let report = evaluate_lifecycle(&ctx, &clock, signed(&token), 30_000).unwrap();
        assert_eq!(report.lifecycle, expected);
        assert!(!report.grants_access());
    }
}

#[test]
fn authentication_failures_are_invalid_not_errors() {
    let key = pubkey();
    let store = MemoryClockStore::new();
    let clock = clock_at(&store, "p1", 10_000);
    let token = Claims::trial(9_000, 20_000).token();

    let wrong_device = LifecycleContext::new(&key, APP, "someone-else");
    let report = evaluate_lifecycle(&wrong_device, &clock, signed(&token), 10_000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::Invalid);
    assert!(report.entitlement.is_none());

    let other = common::other_pubkey();
    let wrong_key = LifecycleContext::new(&other, APP, DEVICE);
    let report = evaluate_lifecycle(&wrong_key, &clock, signed(&token), 10_000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::Invalid);

    let ctx = LifecycleContext::new(&key, APP, DEVICE);
    let report = evaluate_lifecycle(&ctx, &clock, signed("not-a-token"), 10_000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::Invalid);
    assert_eq!(store.load().unwrap(), None);
}

#[test]
fn inconsistent_lifecycle_claims_are_invalid() {
    let key = pubkey();
    let ctx = LifecycleContext::new(&key, APP, DEVICE);
    let store = MemoryClockStore::new();
    let clock = clock_at(&store, "p1", 10_000);
    let token = Claims {
        kind: Some("lifetime"),
        ..Claims::legacy(9_000, 20_000)
    }
    .token();
    let report = evaluate_lifecycle(&ctx, &clock, signed(&token), 10_000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::Invalid);
}

#[test]
fn requirement_not_met_is_unlicensed() {
    let key = pubkey();
    let pro = LicenseRequirement::pro();
    let ctx = LifecycleContext::new(&key, APP, DEVICE).with_requirement(&pro);
    let store = MemoryClockStore::new();
    let clock = clock_at(&store, "p1", 10_000);

    let plus = Claims {
        tier: "plus",
        ..Claims::trial(9_000, 20_000)
    }
    .token();
    let report = evaluate_lifecycle(&ctx, &clock, signed(&plus), 10_000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::Unlicensed);
    assert!(!report.grants_access());
    assert!(report.entitlement.is_some());

    let pro_token = Claims::trial(9_000, 20_000).token();
    let report = evaluate_lifecycle(&ctx, &clock, signed(&pro_token), 10_000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::Trial);
}

#[test]
fn legacy_scalar_ledger_fails_closed_on_a_backward_clock() {
    let key = pubkey();
    let ctx = LifecycleContext::new(&key, APP, DEVICE);
    let store = MemoryClockStore::new();
    store
        .save(&ClockState::decode("10000").expect("scalar ledger"))
        .unwrap();
    let clock = clock_at(&store, "process-new", 9_000);
    let token = Claims::trial(5_000, 90_000).token();
    let credential = StoredCredential::Signed {
        token: &token,
        clock_initialized: true,
    };
    let report = evaluate_lifecycle(&ctx, &clock, credential, 9_000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::ClockRepairRequired);
}
