//! Each planted defect must be caught by exactly the rule that owns it, and
//! the clean subject must pass with no failure.

mod support;

use rightkit_review::{disposition, rule_ids, Disposition, RuleSet};
use support::{clean_video, failed_ids, frame_mut};

fn evaluate(subject: &rightkit_review::Subject) -> Vec<rightkit_review::Finding> {
    RuleSet::standard()
        .evaluate(subject)
        .expect("valid subject")
}

#[test]
fn clean_subject_passes_every_standard_rule() {
    let findings = evaluate(&clean_video());
    assert!(failed_ids(&findings).is_empty(), "{findings:?}");
    assert_eq!(disposition(&findings), Disposition::Pass);
}

#[test]
fn planted_black_frame_is_caught_by_black_frame_rule() {
    let mut subject = clean_video();
    frame_mut(&mut subject, 5000).black = true;
    let findings = evaluate(&subject);
    assert_eq!(failed_ids(&findings), vec![rule_ids::BLACK_FRAME]);
    assert_eq!(disposition(&findings), Disposition::Block);
}

#[test]
fn planted_frozen_run_is_caught_by_frozen_frame_rule() {
    let mut subject = clean_video();
    for time in [2000u64, 3000, 4000, 5000, 6000] {
        frame_mut(&mut subject, time).frame_hash = Some("stuck".to_string());
    }
    let findings = evaluate(&subject);
    assert_eq!(failed_ids(&findings), vec![rule_ids::FROZEN_FRAME]);
    assert_eq!(disposition(&findings), Disposition::Block);
}

#[test]
fn planted_text_overflow_is_caught_by_overflow_rule() {
    let mut subject = clean_video();
    subject.artifact.text_regions.as_mut().unwrap()[0].overflow = Some(true);
    assert_eq!(
        failed_ids(&evaluate(&subject)),
        vec![rule_ids::TEXT_OVERFLOW]
    );
}

#[test]
fn planted_low_contrast_is_caught_by_contrast_rule() {
    let mut subject = clean_video();
    subject.artifact.text_regions.as_mut().unwrap()[0].contrast_ratio = Some(3.2);
    assert_eq!(failed_ids(&evaluate(&subject)), vec![rule_ids::CONTRAST]);
}

#[test]
fn planted_fast_flashing_is_caught_by_flash_rule() {
    let mut subject = clean_video();
    subject.artifact.max_flashes_per_second = Some(5.0);
    assert_eq!(failed_ids(&evaluate(&subject)), vec![rule_ids::FLASH_RATE]);
}

#[test]
fn planted_nondeterministic_render_is_caught_by_determinism_rule() {
    let mut subject = clean_video();
    subject.artifact.determinism.as_mut().unwrap().output_hashes[1] = "blake3:other".to_string();
    assert_eq!(failed_ids(&evaluate(&subject)), vec![rule_ids::DETERMINISM]);
}

#[test]
fn planted_out_of_scope_licence_is_caught_by_licence_rule() {
    let mut subject = clean_video();
    subject.artifact.licence = Some("all-rights-reserved".to_string());
    assert_eq!(failed_ids(&evaluate(&subject)), vec![rule_ids::LICENCE]);
}

#[test]
fn planted_overlong_video_is_caught_by_duration_rule() {
    let mut subject = clean_video();
    subject.declarations.max_duration_ms = Some(15_000);
    assert_eq!(failed_ids(&evaluate(&subject)), vec![rule_ids::DURATION]);
}

#[test]
fn planted_wrong_resolution_is_caught_by_resolution_rule() {
    let mut subject = clean_video();
    subject.artifact.width = Some(1080);
    subject.artifact.height = Some(1080);
    assert_eq!(failed_ids(&evaluate(&subject)), vec![rule_ids::RESOLUTION]);
}

#[test]
fn every_planted_defect_is_caught_by_its_own_rule_only() {
    // Guards against one rule masking another: each defect above fires one id.
    type Plant = fn(&mut rightkit_review::Subject);
    let cases: Vec<(&str, Plant, &str)> = vec![
        (
            "black",
            |s| frame_mut(s, 5000).black = true,
            rule_ids::BLACK_FRAME,
        ),
        (
            "flash",
            |s| s.artifact.max_flashes_per_second = Some(9.0),
            rule_ids::FLASH_RATE,
        ),
        (
            "licence",
            |s| s.artifact.licence = Some("x".into()),
            rule_ids::LICENCE,
        ),
    ];
    for (name, plant, expected) in cases {
        let mut subject = clean_video();
        plant(&mut subject);
        assert_eq!(
            failed_ids(&evaluate(&subject)),
            vec![expected],
            "case {name}"
        );
    }
}
