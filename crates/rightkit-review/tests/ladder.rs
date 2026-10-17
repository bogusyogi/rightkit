//! Scenario tests for the autonomy ladder (ported from CutRight #116, RightKit #236).
//! Each test drives a sequence of finals through a `Ladder` and asserts the level and
//! streak after every step.

use rightkit_review::ladder::{
    Final, Ladder, LadderError, LadderKey, LadderPolicy, LadderState, Level, LADDER_SCHEMA,
    LADDER_VERSION,
};

const OK: Final = Final::Approved {
    critic_failed: false,
};
const CRITIC_FAIL: Final = Final::Approved {
    critic_failed: true,
};
const REJECT: Final = Final::Rejected;

fn shorts() -> LadderKey {
    LadderKey::new("talking_head", "shorts").expect("valid key")
}

fn reels() -> LadderKey {
    LadderKey::new("talking_head", "reels").expect("valid key")
}

fn ladder() -> Ladder {
    Ladder::new(LadderPolicy::default()).expect("default policy is valid")
}

fn record_n(ladder: &mut Ladder, key: &LadderKey, result: Final, n: u32) {
    for _ in 0..n {
        ladder.record(key, result);
    }
}

fn at(level: Level, streak: u32) -> LadderState {
    LadderState { level, streak }
}

#[test]
fn default_policy_matches_cutright_numbers() {
    let policy = LadderPolicy::default();
    assert_eq!(policy.approvals_to_review_light, 5);
    assert_eq!(policy.approvals_to_autonomous, 10);
}

#[test]
fn a_new_pairing_starts_reviewed_with_no_streak() {
    let ladder = ladder();
    assert_eq!(ladder.state(&shorts()), at(Level::Reviewed, 0));
}

#[test]
fn five_clean_approvals_promote_to_review_light_on_the_fifth() {
    let mut ladder = ladder();
    let key = shorts();
    record_n(&mut ladder, &key, OK, 4);
    assert_eq!(ladder.state(&key), at(Level::Reviewed, 4));
    assert_eq!(ladder.record(&key, OK), at(Level::ReviewLight, 0));
}

#[test]
fn ten_more_clean_approvals_promote_review_light_to_autonomous() {
    let mut ladder = ladder();
    let key = shorts();
    record_n(&mut ladder, &key, OK, 5);
    record_n(&mut ladder, &key, OK, 9);
    assert_eq!(ladder.state(&key), at(Level::ReviewLight, 9));
    assert_eq!(ladder.record(&key, OK), at(Level::Autonomous, 0));
    record_n(&mut ladder, &key, OK, 3);
    assert_eq!(ladder.state(&key), at(Level::Autonomous, 0));
}

#[test]
fn a_rejection_at_autonomous_drops_to_reviewed_and_restarts_the_count() {
    let mut ladder = ladder();
    let key = shorts();
    record_n(&mut ladder, &key, OK, 15);
    assert_eq!(ladder.level(&key), Level::Autonomous);
    assert_eq!(ladder.record(&key, REJECT), at(Level::Reviewed, 0));
    record_n(&mut ladder, &key, OK, 4);
    assert_eq!(ladder.state(&key), at(Level::Reviewed, 4));
    assert_eq!(ladder.record(&key, OK), at(Level::ReviewLight, 0));
}

#[test]
fn a_rejection_mid_streak_at_review_light_restarts_from_reviewed() {
    let mut ladder = ladder();
    let key = shorts();
    record_n(&mut ladder, &key, OK, 5);
    record_n(&mut ladder, &key, OK, 3);
    assert_eq!(ladder.state(&key), at(Level::ReviewLight, 3));
    assert_eq!(ladder.record(&key, REJECT), at(Level::Reviewed, 0));
    // Four approvals are not enough: the full five are needed again.
    record_n(&mut ladder, &key, OK, 4);
    assert_eq!(ladder.state(&key), at(Level::Reviewed, 4));
}

#[test]
fn a_critic_failure_on_an_autonomous_final_drops_one_level_and_needs_ten_again() {
    let mut ladder = ladder();
    let key = shorts();
    record_n(&mut ladder, &key, OK, 15);
    assert_eq!(ladder.record(&key, CRITIC_FAIL), at(Level::ReviewLight, 0));
    record_n(&mut ladder, &key, OK, 9);
    assert_eq!(ladder.state(&key), at(Level::ReviewLight, 9));
    assert_eq!(ladder.record(&key, OK), at(Level::Autonomous, 0));
}

#[test]
fn a_critic_failure_below_autonomous_resets_the_streak_without_demotion() {
    let mut ladder = ladder();
    let key = shorts();
    record_n(&mut ladder, &key, OK, 3);
    assert_eq!(ladder.record(&key, CRITIC_FAIL), at(Level::Reviewed, 0));
    record_n(&mut ladder, &key, OK, 4);
    assert_eq!(ladder.state(&key), at(Level::Reviewed, 4));
    assert_eq!(ladder.record(&key, OK), at(Level::ReviewLight, 0));
    record_n(&mut ladder, &key, OK, 2);
    assert_eq!(ladder.record(&key, CRITIC_FAIL), at(Level::ReviewLight, 0));
}

#[test]
fn pairings_climb_and_fall_independently() {
    let mut ladder = ladder();
    let (a, b) = (shorts(), reels());
    let other_type = LadderKey::new("promo", "shorts").expect("valid key");
    record_n(&mut ladder, &a, OK, 5);
    assert_eq!(ladder.level(&a), Level::ReviewLight);
    assert_eq!(ladder.level(&b), Level::Reviewed);
    assert_eq!(ladder.level(&other_type), Level::Reviewed);
    record_n(&mut ladder, &b, OK, 2);
    ladder.record(&a, REJECT);
    assert_eq!(ladder.state(&a), at(Level::Reviewed, 0));
    assert_eq!(ladder.state(&b), at(Level::Reviewed, 2));
}

#[test]
fn ladder_state_survives_a_json_round_trip_and_keeps_climbing() {
    let mut ladder = ladder();
    let (a, b) = (shorts(), reels());
    record_n(&mut ladder, &a, OK, 8);
    record_n(&mut ladder, &b, OK, 15);
    let json = ladder.to_json().expect("serialise");
    assert!(json.contains("\"review_light\""), "{json}");
    assert!(json.contains("\"autonomous\""), "{json}");

    let mut restored = Ladder::from_json(&json).expect("restore");
    assert_eq!(restored, ladder);
    assert_eq!(restored.state(&a), at(Level::ReviewLight, 3));
    assert_eq!(restored.state(&b), at(Level::Autonomous, 0));
    record_n(&mut restored, &a, OK, 7);
    assert_eq!(restored.state(&a), at(Level::Autonomous, 0));
}

#[test]
fn serialised_state_carries_schema_and_version() {
    let json = ladder().to_json().expect("serialise");
    let value: serde_json::Value = serde_json::from_str(&json).expect("json");
    assert_eq!(value["schema"], LADDER_SCHEMA);
    assert_eq!(value["version"], LADDER_VERSION);
    assert_eq!(value["schema"], "rightkit-review.ladder");
    assert_eq!(value["version"], "1.0.0");
}

#[test]
fn round_trip_with_a_custom_policy_keeps_the_policy_and_version() {
    let policy = LadderPolicy {
        approvals_to_review_light: 2,
        approvals_to_autonomous: 3,
    };
    let mut ladder = Ladder::new(policy).expect("valid policy");
    let key = shorts();
    record_n(&mut ladder, &key, OK, 4);
    assert_eq!(ladder.state(&key), at(Level::ReviewLight, 2));
    let json = ladder.to_json().expect("serialise");
    let restored = Ladder::from_json(&json).expect("restore");
    assert_eq!(restored.policy(), policy);
    assert_eq!(restored, ladder);
    let again = restored.to_json().expect("serialise again");
    assert_eq!(again, json);
}

#[test]
fn an_untagged_cutright_snapshot_is_read_as_version_one() {
    let json = r#"{"policy":{"approvals_to_review_light":5,"approvals_to_autonomous":10},
        "pairings":[{"content_type":"talking_head","platform":"shorts","level":"review_light","streak":4}]}"#;
    let ladder = Ladder::from_json(json).expect("legacy snapshot");
    assert_eq!(ladder.state(&shorts()), at(Level::ReviewLight, 4));
    let rewritten = ladder.to_json().expect("serialise");
    assert!(rewritten.contains(LADDER_SCHEMA), "{rewritten}");
}

#[test]
fn a_foreign_schema_or_newer_version_is_refused() {
    let body =
        r#""policy":{"approvals_to_review_light":5,"approvals_to_autonomous":10},"pairings":[]"#;
    for (schema, version) in [
        ("someone-else.ladder", "1.0.0"),
        (LADDER_SCHEMA, "2.0.0"),
        (LADDER_SCHEMA, "1.1.0"),
        (LADDER_SCHEMA, "1.0"),
        (LADDER_SCHEMA, "garbage"),
    ] {
        let json = format!(r#"{{"schema":"{schema}","version":"{version}",{body}}}"#);
        assert!(
            matches!(
                Ladder::from_json(&json),
                Err(LadderError::UnsupportedSnapshot { .. })
            ),
            "{schema} {version}"
        );
    }
    // A schema without a version (or the reverse) is refused, not guessed.
    let schema_only = format!(r#"{{"schema":"{LADDER_SCHEMA}",{body}}}"#);
    assert!(matches!(
        Ladder::from_json(&schema_only),
        Err(LadderError::UnsupportedSnapshot { .. })
    ));
    let version_only = format!(r#"{{"version":"1.0.0",{body}}}"#);
    assert!(matches!(
        Ladder::from_json(&version_only),
        Err(LadderError::UnsupportedSnapshot { .. })
    ));
}

#[test]
fn a_snapshot_listing_one_pairing_twice_is_refused() {
    let json = r#"{"policy":{"approvals_to_review_light":5,"approvals_to_autonomous":10},
        "pairings":[
          {"content_type":"a","platform":"b","level":"reviewed","streak":0},
          {"content_type":"a","platform":"b","level":"autonomous","streak":0}]}"#;
    assert!(matches!(
        Ladder::from_json(json),
        Err(LadderError::DuplicateKey(_))
    ));
}

#[test]
fn an_empty_pairing_part_is_refused() {
    assert!(matches!(
        LadderKey::new("  ", "shorts"),
        Err(LadderError::EmptyKey)
    ));
    assert!(matches!(
        LadderKey::new("talking_head", ""),
        Err(LadderError::EmptyKey)
    ));
}

#[test]
fn policy_overrides_apply_field_by_field_and_change_promotion() {
    let policy = LadderPolicy::from_json(r#"{"approvals_to_review_light":2}"#).expect("policy");
    assert_eq!(policy.approvals_to_review_light, 2);
    assert_eq!(policy.approvals_to_autonomous, 10);

    let mut ladder = Ladder::new(policy).expect("valid policy");
    let key = shorts();
    record_n(&mut ladder, &key, OK, 1);
    assert_eq!(ladder.level(&key), Level::Reviewed);
    assert_eq!(ladder.record(&key, OK), at(Level::ReviewLight, 0));
}

#[test]
fn an_overridden_autonomous_threshold_changes_the_second_promotion() {
    let policy = LadderPolicy::from_json(r#"{"approvals_to_autonomous":3}"#).expect("policy");
    assert_eq!(policy.approvals_to_review_light, 5);
    let mut ladder = Ladder::new(policy).expect("valid policy");
    let key = shorts();
    record_n(&mut ladder, &key, OK, 5);
    record_n(&mut ladder, &key, OK, 2);
    assert_eq!(ladder.state(&key), at(Level::ReviewLight, 2));
    assert_eq!(ladder.record(&key, OK), at(Level::Autonomous, 0));
}

#[test]
fn a_threshold_of_one_promotes_on_every_first_clean_approval() {
    let policy = LadderPolicy {
        approvals_to_review_light: 1,
        approvals_to_autonomous: 1,
    };
    let mut ladder = Ladder::new(policy).expect("valid policy");
    let key = shorts();
    assert_eq!(ladder.record(&key, OK), at(Level::ReviewLight, 0));
    assert_eq!(ladder.record(&key, OK), at(Level::Autonomous, 0));
    assert_eq!(ladder.record(&key, CRITIC_FAIL), at(Level::ReviewLight, 0));
}

#[test]
fn invalid_policies_are_refused() {
    assert!(matches!(
        LadderPolicy::from_json(r#"{"approvals_to_review_light":0}"#),
        Err(LadderError::InvalidPolicy("approvals_to_review_light"))
    ));
    assert!(matches!(
        LadderPolicy::from_json(r#"{"approvals_to_autonomus":10}"#),
        Err(LadderError::Json(_))
    ));
    assert!(matches!(
        Ladder::new(LadderPolicy {
            approvals_to_review_light: 5,
            approvals_to_autonomous: 0,
        }),
        Err(LadderError::InvalidPolicy("approvals_to_autonomous"))
    ));
}

#[test]
fn a_snapshot_with_an_invalid_policy_is_refused() {
    let json = format!(
        r#"{{"schema":"{LADDER_SCHEMA}","version":"{LADDER_VERSION}",
        "policy":{{"approvals_to_review_light":0,"approvals_to_autonomous":10}},"pairings":[]}}"#
    );
    assert!(matches!(
        Ladder::from_json(&json),
        Err(LadderError::InvalidPolicy("approvals_to_review_light"))
    ));
}
