use rightkit_license::{Entitlement, LicenseRequirement};

fn entitlement(tier: &str) -> Entitlement {
    Entitlement {
        v: 2,
        id: "lic-1".into(),
        app: "heardright".into(),
        tier: tier.into(),
        features: vec!["wake_word".into(), "voice_commands".into()],
        device_id: "dev-1".into(),
        iat: 1_000,
        exp: 9_999_999_999,
        license_kind: None,
        entitlement_stage: None,
        license_issued_at: None,
        confirmation_due_at: None,
    }
}

#[test]
fn empty_requirements_fail_closed() {
    let pro = entitlement("pro");

    assert!(!pro.satisfies(&LicenseRequirement::default()));
    assert!(!pro.satisfies(&LicenseRequirement::all_features(Vec::<String>::new())));
    assert!(!pro.satisfies(&LicenseRequirement::any_feature(Vec::<String>::new())));
}

#[test]
fn malformed_feature_requirements_fail_closed() {
    let pro = entitlement("pro");

    assert!(!pro.satisfies(&LicenseRequirement::feature("")));
    assert!(!pro.satisfies(&LicenseRequirement::feature("   ")));
    assert!(!pro.satisfies(&LicenseRequirement::all_features(["wake_word", ""])));
    assert!(!pro.satisfies(&LicenseRequirement::any_feature(["wake_word", " "])));
}

#[test]
fn unknown_required_tiers_fail_closed() {
    let pro = entitlement("pro");

    assert!(!pro.satisfies(&LicenseRequirement::tier("por")));
    assert!(!pro.satisfies(&LicenseRequirement::tier("enterprise")));
}

#[test]
fn malformed_entitlement_tier_cannot_authorize_by_feature() {
    let malformed = entitlement("enterprise");

    assert!(!malformed.satisfies(&LicenseRequirement::feature("wake_word")));
    assert!(!malformed.satisfies(&LicenseRequirement::tier("free")));
}

#[test]
fn recognized_tier_order_remains_free_plus_pro() {
    let free = entitlement("free");
    let plus = entitlement("plus");
    let pro = entitlement("pro");

    assert!(free.satisfies(&LicenseRequirement::tier("free")));
    assert!(!free.satisfies(&LicenseRequirement::tier("plus")));
    assert!(plus.satisfies(&LicenseRequirement::tier("free")));
    assert!(plus.satisfies(&LicenseRequirement::tier("plus")));
    assert!(!plus.satisfies(&LicenseRequirement::tier("pro")));
    assert!(pro.satisfies(&LicenseRequirement::tier("free")));
    assert!(pro.satisfies(&LicenseRequirement::tier("plus")));
    assert!(pro.satisfies(&LicenseRequirement::tier("pro")));
}
