//! Verdicts are bound to the policy hash and the check versions (Orthic-Labs/rightkit#179):
//! tightening a policy under an unchanged rule set name or version invalidates a stored pass.

mod support;

use rightkit_review::{
    run_repair_loop, Finding, NoJudge, RepairCap, ReviewError, Rule, RuleKind, RuleSet, Subject,
    VerdictRecord, BUILTIN_CHECK_VERSION,
};
use support::clean_video;

fn flash_set(max_per_second: f32) -> RuleSet {
    RuleSet::new(
        "preset.social",
        1,
        vec![Rule {
            id: "det.flash_rate".to_string(),
            required: true,
            kind: RuleKind::FlashRate { max_per_second },
        }],
    )
    .unwrap()
}

fn record_for(rules: &RuleSet) -> VerdictRecord {
    let mut no_repair = |_: &Subject, _: &[Finding], _: u32| None;
    run_repair_loop(
        rules,
        clean_video(),
        RepairCap::new(0).unwrap(),
        &mut no_repair,
        &NoJudge,
    )
    .unwrap()
    .record
}

#[test]
fn a_record_carries_the_binding_of_its_rule_set() {
    let rules = flash_set(3.0);
    let record = record_for(&rules);
    assert_eq!(record.binding, Some(rules.policy_binding().unwrap()));
    let binding = rules.policy_binding().unwrap();
    assert_eq!(binding.policy_hash.len(), 64);
    assert_eq!(binding.check_versions["det.flash_rate"], BUILTIN_CHECK_VERSION);
    assert!(record.verify_current(&rules).is_ok());
}

#[test]
fn the_hash_is_stable_and_follows_every_policy_change() {
    let a = flash_set(3.0);
    assert_eq!(a.policy_hash().unwrap(), flash_set(3.0).policy_hash().unwrap());
    assert_ne!(a.policy_hash().unwrap(), flash_set(2.0).policy_hash().unwrap());
    let mut optional = flash_set(3.0);
    optional.rules[0].required = false;
    assert_ne!(a.policy_hash().unwrap(), optional.policy_hash().unwrap());
}

#[test]
fn tightening_a_policy_under_the_same_name_and_version_makes_a_pass_stale() {
    let loose = flash_set(3.0);
    let record = record_for(&loose);
    let tight = flash_set(0.5);
    assert_eq!((tight.id.as_str(), tight.version), (loose.id.as_str(), loose.version));
    assert!(matches!(
        record.verify_current(&tight),
        Err(ReviewError::StaleVerdict(_))
    ));
}

#[test]
fn a_changed_check_version_or_rule_set_version_is_stale() {
    let rules = flash_set(3.0);
    let mut record = record_for(&rules);
    record
        .binding
        .as_mut()
        .unwrap()
        .check_versions
        .insert("det.flash_rate".to_string(), BUILTIN_CHECK_VERSION + 1);
    assert!(matches!(
        record.verify_current(&rules),
        Err(ReviewError::StaleVerdict(_))
    ));
    let mut bumped = flash_set(3.0);
    bumped.version = 2;
    assert!(matches!(
        record_for(&rules).verify_current(&bumped),
        Err(ReviewError::StaleVerdict(_))
    ));
}

#[test]
fn a_record_without_a_binding_is_never_current_and_round_trips() {
    let rules = flash_set(3.0);
    let mut record = record_for(&rules);
    let json = record.to_json().unwrap();
    let parsed = VerdictRecord::from_json(&json).unwrap();
    assert_eq!(parsed, record);
    assert!(parsed.verify_current(&rules).is_ok());

    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value.as_object_mut().unwrap().remove("binding");
    value["version"] = "1.0.0".into();
    let legacy = VerdictRecord::from_json(&value.to_string()).unwrap();
    assert!(legacy.binding.is_none());
    assert!(matches!(
        legacy.verify_current(&rules),
        Err(ReviewError::StaleVerdict(_))
    ));
    record.binding = None;
    assert!(record.verify_current(&rules).is_err());
}
