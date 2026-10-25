mod common;

use std::sync::Arc;

use common::{pubkey, Claims, APP, DEVICE};
use rightkit_license::{
    authenticate_token, evaluate_lifecycle, verify_token, AntiRollbackClock, ClockError,
    ClockState, ClockStore, Entitlement, EntitlementStage, LicenseKind, LicenseRequirement,
    Lifecycle, LifecycleContext, LifecycleMessageFormatter, LifecycleMessageKind,
    LifecycleMessages, LifecyclePolicy, MemoryClockStore, ProcessAnchor, RevokedIds,
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

// ------------------------------------------------------------------ message overrides

#[allow(clippy::field_reassign_with_default)]
fn custom_messages() -> LifecycleMessages {
    let mut messages = LifecycleMessages::default();
    messages.revoked = Some("m-revoked");
    messages.migration_required = Some("m-migration");
    messages.unsigned = Some("m-unsigned");
    messages.invalid_token = Some("m-invalid-token");
    messages.clock_repair_required = Some("m-clock");
    messages.issued_in_the_future = Some("m-future");
    messages.expired = Some("m-expired");
    messages.invalid_lifecycle = Some("m-invalid-lifecycle");
    messages
}

#[test]
fn default_messages_are_unchanged() {
    let key = pubkey();
    let plain = LifecycleContext::new(&key, APP, DEVICE);
    let explicit = plain.with_messages(LifecycleMessages::default());
    let store = MemoryClockStore::new();
    let c = clock_at(&store, "p", 5_000);
    for (credential, text) in [
        (
            StoredCredential::Revoked,
            "The server no longer considers this license active.",
        ),
        (
            StoredCredential::LegacyUnsigned,
            "This older paid record needs one signed online refresh.",
        ),
        (
            StoredCredential::Unsigned,
            "A current signed license is required.",
        ),
    ] {
        for ctx in [&plain, &explicit] {
            let report = evaluate_lifecycle(ctx, &c, credential, 5_000).unwrap();
            assert_eq!(report.message.as_deref(), Some(text));
        }
    }
}

#[test]
fn message_overrides_replace_every_user_facing_text() {
    let key = pubkey();
    let ctx = LifecycleContext::new(&key, APP, DEVICE).with_messages(custom_messages());
    let store = MemoryClockStore::new();
    let c = clock_at(&store, "p", 5_000);
    fn message_of(
        ctx: &LifecycleContext<'_>,
        c: &AntiRollbackClock<&MemoryClockStore>,
        credential: StoredCredential<'_>,
    ) -> Option<String> {
        evaluate_lifecycle(ctx, c, credential, 5_000)
            .unwrap()
            .message
    }

    assert_eq!(
        message_of(&ctx, &c, StoredCredential::Revoked).as_deref(),
        Some("m-revoked")
    );
    assert_eq!(
        message_of(&ctx, &c, StoredCredential::LegacyUnsigned).as_deref(),
        Some("m-migration")
    );
    assert_eq!(
        message_of(&ctx, &c, StoredCredential::Unsigned).as_deref(),
        Some("m-unsigned")
    );
    assert_eq!(
        message_of(
            &ctx,
            &c,
            StoredCredential::Signed {
                token: "garbage",
                clock_initialized: false
            }
        )
        .as_deref(),
        Some("m-invalid-token")
    );

    let expired = Claims::legacy(1_000, 2_000).token();
    assert_eq!(
        message_of(&ctx, &c, signed(&expired)).as_deref(),
        Some("m-expired")
    );

    let inconsistent = Claims {
        kind: Some("lifetime"),
        ..Claims::legacy(1_000, 9_000)
    }
    .token();
    assert_eq!(
        message_of(&ctx, &c, signed(&inconsistent)).as_deref(),
        Some("m-invalid-lifecycle")
    );

    let future = Claims::legacy(100_000, 200_000).token();
    assert_eq!(
        message_of(&ctx, &c, signed(&future)).as_deref(),
        Some("m-future")
    );
}

#[test]
fn clock_repair_message_override_applies_and_does_not_change_the_verdict() {
    let key = pubkey();
    let ctx = LifecycleContext::new(&key, APP, DEVICE).with_messages(custom_messages());
    let store = MemoryClockStore::new();
    let token = Claims::trial(9_000, 20_000).token();
    evaluate_lifecycle(&ctx, &clock_at(&store, "a", 10_000), signed(&token), 10_000).unwrap();

    let again = StoredCredential::Signed {
        token: &token,
        clock_initialized: true,
    };
    let report = evaluate_lifecycle(&ctx, &clock_at(&store, "b", 5_000), again, 5_000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::ClockRepairRequired);
    assert_eq!(report.message.as_deref(), Some("m-clock"));
}

// ------------------------------------------------------------------ message formatter

/// A formatter that renders every kind with the values it was given.
fn value_formatter() -> Arc<LifecycleMessageFormatter> {
    Arc::new(|c| {
        Some(format!(
            "{:?}|{}|nb={:?}|exp={:?}|iat={:?}|last={:?}|detail={}",
            c.kind,
            c.product_id,
            c.not_before,
            c.expiry,
            c.issued_at,
            c.last_trusted,
            c.detail.is_some()
        ))
    })
}

fn formatted(
    ctx: &LifecycleContext<'_>,
    credential: StoredCredential<'_>,
    wall: i64,
) -> Option<String> {
    let store = MemoryClockStore::new();
    let c = clock_at(&store, "p", wall);
    evaluate_lifecycle(ctx, &c, credential, wall)
        .unwrap()
        .message
}

#[test]
fn the_formatter_sees_the_values_of_every_kind() {
    let key = pubkey();
    let f = value_formatter();
    let ctx = LifecycleContext::new(&key, APP, DEVICE).with_formatter(&f);
    let p = APP;

    assert_eq!(
        formatted(&ctx, StoredCredential::Revoked, 5_000).unwrap(),
        format!("Revoked|{p}|nb=None|exp=None|iat=None|last=None|detail=false")
    );
    assert_eq!(
        formatted(&ctx, StoredCredential::LegacyUnsigned, 5_000).unwrap(),
        format!("MigrationRequired|{p}|nb=None|exp=None|iat=None|last=None|detail=false")
    );
    assert_eq!(
        formatted(&ctx, StoredCredential::Unsigned, 5_000).unwrap(),
        format!("Unsigned|{p}|nb=None|exp=None|iat=None|last=None|detail=false")
    );
    assert_eq!(
        formatted(&ctx, signed("garbage"), 5_000).unwrap(),
        format!("InvalidToken|{p}|nb=None|exp=None|iat=None|last=None|detail=true")
    );

    let expired = Claims::legacy(1_000, 2_000).token();
    assert_eq!(
        formatted(&ctx, signed(&expired), 5_000).unwrap(),
        format!("Expired|{p}|nb=None|exp=Some(2000)|iat=Some(1000)|last=None|detail=false")
    );

    let inconsistent = Claims {
        kind: Some("lifetime"),
        ..Claims::legacy(1_000, 9_000)
    }
    .token();
    assert_eq!(
        formatted(&ctx, signed(&inconsistent), 5_000).unwrap(),
        format!(
            "InvalidLifecycle|{p}|nb=None|exp=Some(9000)|iat=Some(1000)|last=None|detail=false"
        )
    );

    // not_before is iat minus the 300 s tolerance, the first trusted time the token is accepted.
    let future = Claims::legacy(100_000, 200_000).token();
    assert_eq!(
        formatted(&ctx, signed(&future), 5_000).unwrap(),
        format!(
            "IssuedInTheFuture|{p}|nb=Some(99700)|exp=Some(200000)|iat=Some(100000)|last=None|detail=false"
        )
    );
}

#[test]
fn the_formatter_sees_the_last_trusted_time_on_a_rollback() {
    let key = pubkey();
    let f = value_formatter();
    let ctx = LifecycleContext::new(&key, APP, DEVICE).with_formatter(&f);
    let store = MemoryClockStore::new();
    let token = Claims::trial(9_000, 20_000).token();
    evaluate_lifecycle(&ctx, &clock_at(&store, "a", 10_000), signed(&token), 10_000).unwrap();

    let again = StoredCredential::Signed {
        token: &token,
        clock_initialized: true,
    };
    let report = evaluate_lifecycle(&ctx, &clock_at(&store, "b", 5_000), again, 5_000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::ClockRepairRequired);
    assert_eq!(
        report.message.unwrap(),
        format!(
            "ClockRepairRequired|{APP}|nb=None|exp=Some(20000)|iat=Some(9000)|last=Some(10000)|detail=false"
        )
    );
}

#[test]
fn none_falls_back_to_the_static_override_then_the_default() {
    let key = pubkey();
    let none: Arc<LifecycleMessageFormatter> = Arc::new(|_| None);
    let with_override = LifecycleContext::new(&key, APP, DEVICE)
        .with_messages(custom_messages())
        .with_formatter(&none);
    assert_eq!(
        formatted(&with_override, StoredCredential::Revoked, 5_000).as_deref(),
        Some("m-revoked")
    );
    let expired = Claims::legacy(1_000, 2_000).token();
    assert_eq!(
        formatted(&with_override, signed(&expired), 5_000).as_deref(),
        Some("m-expired")
    );

    let defaults = LifecycleContext::new(&key, APP, DEVICE).with_formatter(&none);
    assert_eq!(
        formatted(&defaults, StoredCredential::Revoked, 5_000).as_deref(),
        Some("The server no longer considers this license active.")
    );
    assert_eq!(
        formatted(&defaults, signed(&expired), 5_000).as_deref(),
        Some("The signed license has expired.")
    );
}

#[test]
fn a_formatter_beats_the_static_override_for_its_kind_only() {
    let key = pubkey();
    let only_revoked: Arc<LifecycleMessageFormatter> =
        Arc::new(|c| (c.kind == LifecycleMessageKind::Revoked).then(|| "formatted".to_string()));
    let ctx = LifecycleContext::new(&key, APP, DEVICE)
        .with_messages(custom_messages())
        .with_formatter(&only_revoked);
    assert_eq!(
        formatted(&ctx, StoredCredential::Revoked, 5_000).as_deref(),
        Some("formatted")
    );
    assert_eq!(
        formatted(&ctx, StoredCredential::Unsigned, 5_000).as_deref(),
        Some("m-unsigned")
    );
}

#[test]
fn a_panicking_formatter_falls_back_to_the_default_text_and_keeps_the_verdict() {
    let key = pubkey();
    let boom: Arc<LifecycleMessageFormatter> = Arc::new(|_| panic!("formatter bug"));
    let ctx = LifecycleContext::new(&key, APP, DEVICE)
        .with_messages(custom_messages())
        .with_formatter(&boom);
    let store = MemoryClockStore::new();
    let c = clock_at(&store, "p", 5_000);
    let expired = Claims::legacy(1_000, 2_000).token();
    let report = evaluate_lifecycle(&ctx, &c, signed(&expired), 5_000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::Expired);
    assert_eq!(
        report.message.as_deref(),
        Some("The signed license has expired.")
    );
}

#[test]
fn an_unauthenticated_token_never_reaches_the_clock_messages() {
    let key = pubkey();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let f: Arc<LifecycleMessageFormatter> = Arc::new(move |c| {
        log.lock().unwrap().push(c.kind);
        None
    });
    let ctx = LifecycleContext::new(&key, APP, DEVICE).with_formatter(&f);
    let store = MemoryClockStore::new();
    let c = clock_at(&store, "p", 5_000);
    // Initialised install, empty ledger: the clock would report Missing, but the token fails
    // authentication first.
    let report = evaluate_lifecycle(
        &ctx,
        &c,
        StoredCredential::Signed {
            token: "garbage",
            clock_initialized: true,
        },
        5_000,
    )
    .unwrap();
    assert_eq!(report.lifecycle, Lifecycle::Invalid);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![LifecycleMessageKind::InvalidToken]
    );
}

#[test]
fn a_listed_entitlement_id_is_revoked_before_expiry_and_before_the_clock_is_read() {
    let key = pubkey();
    let revoked = RevokedIds::new().with("lic_1");
    let other = RevokedIds::new().with("lic_2");
    let token = Claims::term(9_000, 20_000).token();

    // Not listed: an ordinary live term, which initialises the clock.
    let store = MemoryClockStore::new();
    let clock = clock_at(&store, "p1", 10_000);
    let ctx = LifecycleContext::new(&key, APP, DEVICE).with_revoked_ids(&other);
    let live = evaluate_lifecycle(&ctx, &clock, signed(&token), 10_000).unwrap();
    assert_eq!(
        (live.lifecycle, live.grants_access()),
        (Lifecycle::Active, true)
    );

    // Listed: revoked, with the authenticated entitlement and a message, and the clock untouched.
    let store = MemoryClockStore::new();
    let clock = clock_at(&store, "p1", 10_000);
    let ctx = LifecycleContext::new(&key, APP, DEVICE).with_revoked_ids(&revoked);
    let report = evaluate_lifecycle(&ctx, &clock, signed(&token), 10_000).unwrap();
    assert_eq!(report.lifecycle, Lifecycle::Revoked);
    assert!(!report.grants_access());
    assert_eq!(report.entitlement.as_ref().unwrap().id, "lic_1");
    assert!(report.message.is_some());
    assert_eq!(
        store.load().unwrap(),
        None,
        "a revoked token never writes the ledger"
    );

    // Still revoked, not expired, once the token's time has passed.
    let late = evaluate_lifecycle(&ctx, &clock, signed(&token), 99_000).unwrap();
    assert_eq!(late.lifecycle, Lifecycle::Revoked);

    // Still revoked, not "clock repair required", when the ledger says time ran backwards.
    let store = MemoryClockStore::new();
    store
        .save(&ClockState {
            highest_seen: 500_000,
            trusted_process: "p0".into(),
            last_server_iat: 0,
        })
        .unwrap();
    let clock = clock_at(&store, "p1", 10_000);
    let rolled_back = evaluate_lifecycle(&ctx, &clock, signed(&token), 10_000).unwrap();
    assert_eq!(rolled_back.lifecycle, Lifecycle::Revoked);

    // A token that does not authenticate is never matched against the list: its id is unverified.
    let forged = Claims::term(9_000, 20_000).token_signed_by(&common::other_signing_key());
    let forged = evaluate_lifecycle(&ctx, &clock, signed(&forged), 10_000).unwrap();
    assert_eq!(forged.lifecycle, Lifecycle::Invalid);
    assert!(forged.entitlement.is_none());
}
