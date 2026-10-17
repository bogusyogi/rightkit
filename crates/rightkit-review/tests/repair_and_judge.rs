//! The repair loop stops at the cap, never exceeds it, and returns the last
//! inspectable candidate. The judge is recorded and never changes a disposition.

mod support;

use std::cell::Cell;

use rightkit_review::{
    disposition, rule_ids, run_repair_loop, Disposition, Finding, Judge, JudgeScore, NoJudge,
    RepairCap, ReviewError, RuleSet, StopReason, Subject, VerdictRecord, HARD_MAX_REPAIR_ATTEMPTS,
};
use support::{clean_video, frame_mut};

struct FixedJudge(Result<Vec<JudgeScore>, String>);

impl Judge for FixedJudge {
    fn id(&self) -> &str {
        "fixed-test-judge"
    }
    fn score(&self, _subject: &Subject) -> Result<Vec<JudgeScore>, String> {
        self.0.clone()
    }
}

fn high_taste() -> FixedJudge {
    FixedJudge(Ok(vec![JudgeScore {
        dimension: "taste".into(),
        score: 1.0,
        rationale: "lovely".into(),
    }]))
}

fn black_defect() -> Subject {
    let mut subject = clean_video();
    frame_mut(&mut subject, 5000).black = true;
    subject
}

fn fix_black(candidate: &Subject, _findings: &[Finding], _attempt: u32) -> Option<Subject> {
    let mut next = candidate.clone();
    frame_mut(&mut next, 5000).black = false;
    Some(next)
}

#[test]
fn repair_on_first_retry_passes_and_records_both_attempts() {
    let rules = RuleSet::standard();
    let mut repairer = fix_black;
    let outcome = run_repair_loop(
        &rules,
        black_defect(),
        RepairCap::new(3).unwrap(),
        &mut repairer,
        &NoJudge,
    )
    .unwrap();
    assert_eq!(outcome.stop, StopReason::Passed);
    assert_eq!(outcome.record.attempts.len(), 2);
    assert_eq!(outcome.record.attempts[0].disposition, Disposition::Block);
    assert_eq!(outcome.record.attempts[1].disposition, Disposition::Pass);
    assert_eq!(outcome.record.disposition, Disposition::Pass);
}

#[test]
fn irreparable_candidate_stops_at_the_cap_and_is_preserved() {
    let rules = RuleSet::standard();
    let calls = Cell::new(0u32);
    let mut repairer = |candidate: &Subject, _: &[Finding], attempt: u32| {
        calls.set(calls.get() + 1);
        assert_eq!(
            attempt,
            calls.get(),
            "attempts are one-based and sequential"
        );
        Some(candidate.clone())
    };
    let cap = RepairCap::new(3).unwrap();
    let defect = black_defect();
    let outcome = run_repair_loop(&rules, defect.clone(), cap, &mut repairer, &NoJudge).unwrap();
    assert_eq!(outcome.stop, StopReason::RepairCapReached);
    assert_eq!(
        calls.get(),
        3,
        "repairer is called exactly the cap, never more"
    );
    assert_eq!(outcome.record.attempts.len(), 4);
    assert_eq!(outcome.record.disposition, Disposition::Block);
    assert_eq!(
        outcome.final_candidate, defect,
        "last inspectable candidate is kept"
    );
}

#[test]
fn zero_cap_means_no_repair_is_attempted() {
    let rules = RuleSet::standard();
    let calls = Cell::new(0u32);
    let mut repairer = |candidate: &Subject, _: &[Finding], _: u32| {
        calls.set(calls.get() + 1);
        Some(candidate.clone())
    };
    let outcome = run_repair_loop(
        &rules,
        black_defect(),
        RepairCap::new(0).unwrap(),
        &mut repairer,
        &NoJudge,
    )
    .unwrap();
    assert_eq!(calls.get(), 0);
    assert_eq!(outcome.stop, StopReason::RepairCapReached);
    assert_eq!(outcome.record.attempts.len(), 1);
}

#[test]
fn repair_cap_above_the_hard_limit_is_refused() {
    assert!(RepairCap::new(HARD_MAX_REPAIR_ATTEMPTS).is_ok());
    assert!(matches!(
        RepairCap::new(HARD_MAX_REPAIR_ATTEMPTS + 1),
        Err(ReviewError::RepairCapOutOfRange { .. })
    ));
}

#[test]
fn repairer_that_gives_up_stops_the_loop() {
    let rules = RuleSet::standard();
    let mut repairer = |_: &Subject, _: &[Finding], _: u32| None;
    let outcome = run_repair_loop(
        &rules,
        black_defect(),
        RepairCap::new(5).unwrap(),
        &mut repairer,
        &NoJudge,
    )
    .unwrap();
    assert_eq!(outcome.stop, StopReason::RepairUnavailable);
    assert_eq!(outcome.record.attempts.len(), 1);
}

#[test]
fn held_candidate_is_not_repaired() {
    let rules = RuleSet::standard();
    let calls = Cell::new(0u32);
    let mut repairer = |candidate: &Subject, _: &[Finding], _: u32| {
        calls.set(calls.get() + 1);
        Some(candidate.clone())
    };
    let mut held = clean_video();
    frame_mut(&mut held, 3000).ambiguous = Some("unclear".into());
    let outcome = run_repair_loop(
        &rules,
        held,
        RepairCap::new(5).unwrap(),
        &mut repairer,
        &NoJudge,
    )
    .unwrap();
    assert_eq!(calls.get(), 0);
    assert_eq!(outcome.stop, StopReason::Held);
    assert_eq!(outcome.record.disposition, Disposition::Hold);
}

#[test]
fn high_judge_score_does_not_override_a_deterministic_failure() {
    let rules = RuleSet::standard();
    let mut no_repair = |_: &Subject, _: &[Finding], _: u32| None;
    let outcome = run_repair_loop(
        &rules,
        black_defect(),
        RepairCap::new(0).unwrap(),
        &mut no_repair,
        &high_taste(),
    )
    .unwrap();
    assert_eq!(outcome.record.disposition, Disposition::Block);
    assert_eq!(outcome.record.advisory.len(), 1);
    assert_eq!(outcome.record.advisory[0].scores.len(), 1);
    assert!(outcome.record.advisory[0].advisory);
}

#[test]
fn low_judge_score_does_not_block_a_clean_candidate() {
    let rules = RuleSet::standard();
    let harsh = FixedJudge(Ok(vec![JudgeScore {
        dimension: "taste".into(),
        score: 0.0,
        rationale: "flat".into(),
    }]));
    let mut no_repair = |_: &Subject, _: &[Finding], _: u32| None;
    let outcome = run_repair_loop(
        &rules,
        clean_video(),
        RepairCap::new(2).unwrap(),
        &mut no_repair,
        &harsh,
    )
    .unwrap();
    assert_eq!(outcome.record.disposition, Disposition::Pass);
    assert_eq!(outcome.stop, StopReason::Passed);
}

#[test]
fn judge_failure_is_recorded_and_leaves_the_disposition_alone() {
    let rules = RuleSet::standard();
    let broken = FixedJudge(Err("model timed out".into()));
    let mut no_repair = |_: &Subject, _: &[Finding], _: u32| None;
    let outcome = run_repair_loop(
        &rules,
        clean_video(),
        RepairCap::new(0).unwrap(),
        &mut no_repair,
        &broken,
    )
    .unwrap();
    assert_eq!(outcome.record.disposition, Disposition::Pass);
    assert_eq!(
        outcome.record.advisory[0].error.as_deref(),
        Some("model timed out")
    );
}

#[test]
fn out_of_range_judge_score_is_dropped_not_clamped() {
    let rules = RuleSet::standard();
    let wild = FixedJudge(Ok(vec![JudgeScore {
        dimension: "taste".into(),
        score: 7.0,
        rationale: "off the scale".into(),
    }]));
    let mut no_repair = |_: &Subject, _: &[Finding], _: u32| None;
    let outcome = run_repair_loop(
        &rules,
        clean_video(),
        RepairCap::new(0).unwrap(),
        &mut no_repair,
        &wild,
    )
    .unwrap();
    let record = &outcome.record.advisory[0];
    assert!(record.scores.is_empty());
    assert!(record.error.is_some());
}

#[test]
fn planted_defect_is_caught_by_black_rule_inside_the_loop() {
    let rules = RuleSet::standard();
    let mut no_repair = |_: &Subject, _: &[Finding], _: u32| None;
    let outcome = run_repair_loop(
        &rules,
        black_defect(),
        RepairCap::new(0).unwrap(),
        &mut no_repair,
        &NoJudge,
    )
    .unwrap();
    let first = &outcome.record.attempts[0].findings;
    assert_eq!(
        first
            .iter()
            .filter(|finding| matches!(finding.verdict, rightkit_review::Verdict::Failed { .. }))
            .map(|finding| finding.rule_id.as_str())
            .collect::<Vec<_>>(),
        vec![rule_ids::BLACK_FRAME]
    );
}

#[test]
fn verdict_record_round_trips_and_refuses_foreign_or_newer_versions() {
    let rules = RuleSet::standard();
    let mut no_repair = |_: &Subject, _: &[Finding], _: u32| None;
    let outcome = run_repair_loop(
        &rules,
        black_defect(),
        RepairCap::new(1).unwrap(),
        &mut no_repair,
        &high_taste(),
    )
    .unwrap();
    let json = outcome.record.to_json().unwrap();
    let parsed = VerdictRecord::from_json(&json).unwrap();
    assert_eq!(parsed, outcome.record);
    assert_eq!(parsed.schema, "rightkit-review.verdict");
    assert_eq!(parsed.version, "1.1.0");

    let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
    value["version"] = "1.2.0".into();
    assert!(matches!(
        VerdictRecord::from_json(&value.to_string()),
        Err(ReviewError::UnsupportedRecord { .. })
    ));
    value["version"] = "2.0.0".into();
    assert!(matches!(
        VerdictRecord::from_json(&value.to_string()),
        Err(ReviewError::UnsupportedRecord { .. })
    ));
    value["version"] = "1.0.0".into();
    value["schema"] = "some-other-record".into();
    assert!(matches!(
        VerdictRecord::from_json(&value.to_string()),
        Err(ReviewError::UnsupportedRecord { .. })
    ));
}

#[test]
fn record_is_independent_of_the_judge_for_the_disposition() {
    let rules = RuleSet::standard();
    let run = |judge: &dyn Judge| {
        let mut no_repair = |_: &Subject, _: &[Finding], _: u32| None;
        run_repair_loop(
            &rules,
            black_defect(),
            RepairCap::new(0).unwrap(),
            &mut no_repair,
            judge,
        )
        .unwrap()
        .record
        .disposition
    };
    assert_eq!(run(&NoJudge), run(&high_taste()));
    assert_eq!(
        disposition(&rules.evaluate(&black_defect()).unwrap()),
        Disposition::Block
    );
}
