//! Declared holds exempt deliberate pauses; undeclared or partly declared
//! defects still fail; ambiguous measurements hold; unknowns block only when required.

mod support;

use rightkit_review::{
    disposition, rule_ids, Disposition, ReviewError, Rule, RuleKind, RuleSet, Verdict, WindowKind,
};
use support::{clean_video, declare, failed_ids, frame_mut, verdict_of};

fn evaluate(subject: &rightkit_review::Subject) -> Vec<rightkit_review::Finding> {
    RuleSet::standard()
        .evaluate(subject)
        .expect("valid subject")
}

#[test]
fn declared_static_region_passes_a_frozen_run() {
    let mut subject = clean_video();
    for time in [2000u64, 3000, 4000, 5000, 6000] {
        frame_mut(&mut subject, time).frame_hash = Some("title-card".to_string());
    }
    declare(&mut subject, WindowKind::Static, 1500, 6500);
    let findings = evaluate(&subject);
    assert!(failed_ids(&findings).is_empty(), "{findings:?}");
    assert_eq!(disposition(&findings), Disposition::Pass);
}

#[test]
fn frozen_run_only_partly_inside_a_declared_window_still_fails() {
    let mut subject = clean_video();
    for time in [2000u64, 3000, 4000, 5000, 6000] {
        frame_mut(&mut subject, time).frame_hash = Some("stuck".to_string());
    }
    declare(&mut subject, WindowKind::Static, 1500, 5500);
    assert_eq!(
        failed_ids(&evaluate(&subject)),
        vec![rule_ids::FROZEN_FRAME]
    );
}

#[test]
fn declared_hold_passes_a_frozen_run() {
    let mut subject = clean_video();
    for time in [2000u64, 3000, 4000, 5000, 6000] {
        frame_mut(&mut subject, time).frame_hash = Some("held".to_string());
    }
    declare(&mut subject, WindowKind::Hold, 1500, 6500);
    assert!(failed_ids(&evaluate(&subject)).is_empty());
}

#[test]
fn declared_silence_passes_a_black_frame() {
    let mut subject = clean_video();
    frame_mut(&mut subject, 5000).black = true;
    declare(&mut subject, WindowKind::Silence, 4500, 5500);
    let findings = evaluate(&subject);
    assert!(failed_ids(&findings).is_empty(), "{findings:?}");
    assert_eq!(disposition(&findings), Disposition::Pass);
}

#[test]
fn static_window_does_not_exempt_a_black_frame() {
    let mut subject = clean_video();
    frame_mut(&mut subject, 5000).black = true;
    declare(&mut subject, WindowKind::Static, 4500, 5500);
    assert_eq!(failed_ids(&evaluate(&subject)), vec![rule_ids::BLACK_FRAME]);
}

#[test]
fn ambiguous_frame_holds_for_a_human_without_failing() {
    let mut subject = clean_video();
    frame_mut(&mut subject, 3000).ambiguous = Some("encoder artefact vs real flash".to_string());
    let findings = evaluate(&subject);
    assert!(failed_ids(&findings).is_empty());
    assert!(matches!(
        verdict_of(&findings, rule_ids::BLACK_FRAME),
        Verdict::Held { .. }
    ));
    assert_eq!(disposition(&findings), Disposition::Hold);
}

#[test]
fn a_failure_outranks_a_hold() {
    let mut subject = clean_video();
    frame_mut(&mut subject, 3000).ambiguous = Some("unclear".to_string());
    subject.artifact.max_flashes_per_second = Some(6.0);
    let findings = evaluate(&subject);
    assert_eq!(disposition(&findings), Disposition::Block);
}

#[test]
fn required_unmeasured_overflow_blocks() {
    let mut subject = clean_video();
    subject.artifact.text_regions.as_mut().unwrap()[0].overflow = None;
    let findings = evaluate(&subject);
    assert!(matches!(
        verdict_of(&findings, rule_ids::TEXT_OVERFLOW),
        Verdict::Unverified { .. }
    ));
    assert_eq!(disposition(&findings), Disposition::Block);
}

#[test]
fn missing_determinism_probe_is_inconclusive_and_blocks_when_required() {
    let mut subject = clean_video();
    subject.artifact.determinism = None;
    let findings = evaluate(&subject);
    assert!(matches!(
        verdict_of(&findings, rule_ids::DETERMINISM),
        Verdict::Inconclusive { .. }
    ));
    assert_eq!(disposition(&findings), Disposition::Block);
}

#[test]
fn advisory_unknown_does_not_block() {
    let mut rules = RuleSet::standard();
    for rule in rules.rules.iter_mut() {
        if rule.id == rule_ids::DETERMINISM {
            rule.required = false;
        }
    }
    let mut subject = clean_video();
    subject.artifact.determinism = None;
    let findings = rules.evaluate(&subject).unwrap();
    assert!(matches!(
        verdict_of(&findings, rule_ids::DETERMINISM),
        Verdict::Inconclusive { .. }
    ));
    assert_eq!(disposition(&findings), Disposition::Pass);
}

#[test]
fn still_artifact_is_not_applicable_to_video_rules() {
    let mut subject = clean_video();
    subject.artifact.kind = rightkit_review::ArtifactKind::Still;
    subject.artifact.frames = None;
    subject.artifact.max_flashes_per_second = None;
    subject.artifact.duration_ms = None;
    let findings = evaluate(&subject);
    assert!(matches!(
        verdict_of(&findings, rule_ids::BLACK_FRAME),
        Verdict::NotApplicable { .. }
    ));
    assert!(matches!(
        verdict_of(&findings, rule_ids::FLASH_RATE),
        Verdict::NotApplicable { .. }
    ));
    assert_eq!(disposition(&findings), Disposition::Pass);
}

#[test]
fn empty_licence_scope_is_inconclusive() {
    let mut subject = clean_video();
    subject.declarations.allowed_licences.clear();
    let findings = evaluate(&subject);
    assert!(matches!(
        verdict_of(&findings, rule_ids::LICENCE),
        Verdict::Inconclusive { .. }
    ));
    assert_eq!(disposition(&findings), Disposition::Block);
}

#[test]
fn declared_window_without_reason_is_refused() {
    let mut subject = clean_video();
    declare(&mut subject, WindowKind::Hold, 1000, 2000);
    subject.declarations.windows[0].reason = "  ".to_string();
    assert!(matches!(
        RuleSet::standard().evaluate(&subject),
        Err(ReviewError::InvalidSubject(_))
    ));
}

#[test]
fn reversed_declared_window_is_refused() {
    let mut subject = clean_video();
    declare(&mut subject, WindowKind::Hold, 2000, 1000);
    assert!(matches!(
        RuleSet::standard().evaluate(&subject),
        Err(ReviewError::InvalidSubject(_))
    ));
}

#[test]
fn rule_set_rejects_duplicate_ids_and_zero_thresholds() {
    let duplicate = vec![
        Rule {
            id: "a".into(),
            required: true,
            kind: RuleKind::RenderDeterminism,
        },
        Rule {
            id: "a".into(),
            required: true,
            kind: RuleKind::LicenceAllowed,
        },
    ];
    assert!(matches!(
        RuleSet::new("s", 1, duplicate),
        Err(ReviewError::InvalidRuleSet(_))
    ));
    let zero_run = vec![Rule {
        id: "frozen".into(),
        required: true,
        kind: RuleKind::NoUnexpectedFrozenRun { min_run_ms: 0 },
    }];
    assert!(matches!(
        RuleSet::new("s", 1, zero_run),
        Err(ReviewError::InvalidRuleSet(_))
    ));
    assert!(RuleSet::new("s", 1, RuleSet::standard().rules).is_ok());
}
