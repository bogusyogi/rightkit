//! Consumers register rules without editing the crate. An unregistered kind
//! blocks when required. Existing media rule ids and their JSON are unchanged.

mod support;

use rightkit_review::{
    disposition, run_repair_loop_with, Disposition, Finding, NoJudge, RepairCap, ReviewError, Rule,
    RuleKind, RuleRegistry, RuleSet, StopReason, Subject, Verdict,
};
use serde_json::json;
use support::{clean_video, text_subject};

fn word_limit_registry() -> RuleRegistry {
    let mut registry = RuleRegistry::new();
    registry
        .register(
            "acme.word_limit",
            |subject: &Subject, params: &serde_json::Value| {
                let max = params["max"].as_u64().unwrap_or(0) as usize;
                let words = subject
                    .artifact
                    .text
                    .as_ref()
                    .map_or(0, |text| text.body.split_whitespace().count());
                if words <= max {
                    Verdict::Verified
                } else {
                    Verdict::Failed {
                        reason: format!("{words} words exceeds the limit of {max}"),
                    }
                }
            },
        )
        .expect("register");
    registry
}

fn word_limit_set(max: u64) -> RuleSet {
    RuleSet::new(
        "acme.text",
        1,
        vec![Rule {
            id: "acme.word_limit".to_string(),
            required: true,
            kind: RuleKind::Extension {
                kind: "acme.word_limit".to_string(),
                params: json!({ "max": max }),
            },
        }],
    )
    .expect("valid rule set")
}

#[test]
fn registered_extension_rule_receives_its_params_and_subject() {
    let registry = word_limit_registry();
    let set = word_limit_set(5);
    let pass = set
        .evaluate_with(&text_subject("one two three"), &registry)
        .unwrap();
    assert_eq!(pass[0].verdict, Verdict::Verified);
    let fail = set
        .evaluate_with(&text_subject("one two three four five six"), &registry)
        .unwrap();
    assert!(matches!(fail[0].verdict, Verdict::Failed { .. }));
    assert_eq!(disposition(&fail), Disposition::Block);
}

#[test]
fn extension_rule_at_its_limit_passes() {
    let registry = word_limit_registry();
    let findings = word_limit_set(5)
        .evaluate_with(&text_subject("one two three four five"), &registry)
        .unwrap();
    assert_eq!(findings[0].verdict, Verdict::Verified);
}

#[test]
fn unregistered_extension_kind_is_inconclusive_and_blocks_when_required() {
    let set = word_limit_set(5);
    let findings: Vec<Finding> = set.evaluate(&text_subject("one two")).unwrap();
    match &findings[0].verdict {
        Verdict::Inconclusive { reason } => assert!(reason.contains("acme.word_limit")),
        other => panic!("expected inconclusive, got {other:?}"),
    }
    assert_eq!(disposition(&findings), Disposition::Block);
}

#[test]
fn registry_refuses_empty_reserved_and_duplicate_kinds() {
    let mut registry = RuleRegistry::new();
    let rule = |_: &Subject, _: &serde_json::Value| Verdict::Verified;
    assert!(matches!(
        registry.register("  ", rule),
        Err(ReviewError::InvalidRuleSet(_))
    ));
    assert!(matches!(
        registry.register("det.black_frame", rule),
        Err(ReviewError::InvalidRuleSet(_))
    ));
    registry
        .register("acme.check", rule)
        .expect("first register");
    assert!(registry.contains("acme.check"));
    assert!(matches!(
        registry.register("acme.check", rule),
        Err(ReviewError::InvalidRuleSet(_))
    ));
}

#[test]
fn rule_set_refuses_an_empty_extension_kind() {
    let refused = RuleSet::new(
        "acme.text",
        1,
        vec![Rule {
            id: "acme.empty".to_string(),
            required: true,
            kind: RuleKind::Extension {
                kind: " ".to_string(),
                params: json!({}),
            },
        }],
    );
    assert!(matches!(refused, Err(ReviewError::InvalidRuleSet(_))));
}

#[test]
fn repair_loop_resolves_extension_rules_through_the_registry() {
    let registry = word_limit_registry();
    let rules = word_limit_set(5);
    let mut repairer = |candidate: &Subject, _findings: &[Finding], _attempt: u32| {
        let mut next = candidate.clone();
        if let Some(text) = next.artifact.text.as_mut() {
            text.body = "one two three four five".to_string();
        }
        Some(next)
    };
    let outcome = run_repair_loop_with(
        &rules,
        text_subject("one two three four five six"),
        RepairCap::new(2).unwrap(),
        &mut repairer,
        &NoJudge,
        &registry,
    )
    .unwrap();
    assert_eq!(outcome.stop, StopReason::Passed);
    assert_eq!(outcome.record.attempts.len(), 2);
    assert_eq!(outcome.record.attempts[0].disposition, Disposition::Block);
    assert_eq!(outcome.record.attempts[1].disposition, Disposition::Pass);
}

// Wire compatibility: existing ids and shapes serialise exactly as before.

#[test]
fn existing_media_rule_kinds_keep_their_wire_shape() {
    assert_eq!(
        serde_json::to_value(RuleKind::NoUnexpectedBlackFrames).unwrap(),
        json!({ "type": "no_unexpected_black_frames" })
    );
    assert_eq!(
        serde_json::to_value(RuleKind::FlashRate {
            max_per_second: 3.0
        })
        .unwrap(),
        json!({ "type": "flash_rate", "max_per_second": 3.0 })
    );
    assert_eq!(
        serde_json::to_value(RuleKind::NoUnexpectedFrozenRun { min_run_ms: 1000 }).unwrap(),
        json!({ "type": "no_unexpected_frozen_run", "min_run_ms": 1000 })
    );
}

#[test]
fn legacy_rule_json_still_parses_with_required_defaulting_to_true() {
    let rule: Rule = serde_json::from_str(
        r#"{"id":"det.black_frame","kind":{"type":"no_unexpected_black_frames"}}"#,
    )
    .expect("legacy rule parses");
    assert!(rule.required);
    assert_eq!(rule.kind, RuleKind::NoUnexpectedBlackFrames);
}

#[test]
fn standard_rule_set_round_trips_through_json() {
    let standard = RuleSet::standard();
    let text = serde_json::to_string(&standard).unwrap();
    let back: RuleSet = serde_json::from_str(&text).unwrap();
    assert_eq!(back, standard);
    let findings = back.evaluate(&clean_video()).unwrap();
    assert_eq!(disposition(&findings), Disposition::Pass);
}

#[test]
fn extension_and_text_kinds_round_trip_through_json() {
    let extension = RuleKind::Extension {
        kind: "acme.x".to_string(),
        params: json!({ "a": 1 }),
    };
    assert_eq!(
        serde_json::to_value(&extension).unwrap(),
        json!({ "type": "extension", "kind": "acme.x", "params": { "a": 1 } })
    );
    let banned = RuleKind::BannedPhrase {
        phrases: vec!["cure".to_string()],
        min_words: 3,
    };
    let text = serde_json::to_string(&banned).unwrap();
    assert_eq!(serde_json::from_str::<RuleKind>(&text).unwrap(), banned);
}

#[test]
fn subject_without_text_still_parses_and_text_round_trips() {
    let legacy: Subject =
        serde_json::from_str(r#"{"artifact":{"artifactId":"clip-1","kind":"video"}}"#)
            .expect("legacy subject parses");
    assert!(legacy.artifact.text.is_none());

    let subject = text_subject("We shipped.");
    let text = serde_json::to_string(&subject).unwrap();
    let back: Subject = serde_json::from_str(&text).unwrap();
    assert_eq!(back, subject);
}
