//! Critic states and repair cap, exercised through the out-of-process `rightkit-render-critic`
//! binary. Each case spawns the real child process; nothing is judged in this test.

use std::time::Duration;

use rightkit_review::render_qa::critic::{
    checks_from_findings, critic_unavailable_check, run_critic_loop, CriticInput, CriticRunner,
    Span, CHECK_RUN, RULE_BLACK, RULE_CAPTION_FACE, RULE_CAPTION_STATE, RULE_CONTRAST, RULE_FLASH,
    RULE_LOUDNESS, RULE_SILENCE,
};
use rightkit_review::render_qa::report::{CheckStatus, QaCheck, Severity};
use rightkit_review::{disposition, Disposition, Finding, RepairCap, StopReason, Verdict};

fn runner() -> CriticRunner {
    CriticRunner::new(
        env!("CARGO_BIN_EXE_rightkit-render-critic"),
        Duration::from_secs(30),
    )
}

fn span(start_ms: u64, end_ms: u64) -> Span {
    Span { start_ms, end_ms }
}

fn base(duration_ms: Option<u64>) -> CriticInput {
    CriticInput {
        duration_ms,
        silence: None,
        black: None,
        holds: Vec::new(),
        silence_holds: Vec::new(),
        black_holds: Vec::new(),
        max_silence_ms: 1_000,
        max_black_ms: 40,
    }
}

fn check_of<'a>(checks: &'a [QaCheck], rule: &str) -> &'a QaCheck {
    checks
        .iter()
        .find(|c| c.id == rule)
        .unwrap_or_else(|| panic!("no check {rule}"))
}

fn verdict_of<'a>(findings: &'a [Finding], rule: &str) -> &'a Verdict {
    &findings
        .iter()
        .find(|f| f.rule_id == rule)
        .unwrap_or_else(|| panic!("no finding for {rule}"))
        .verdict
}

#[test]
fn out_of_process_critic_returns_every_state() {
    let r = runner();

    // Unverified: silence was never measured.
    let findings = r.run(&base(Some(10_000))).expect("critic runs");
    assert!(
        matches!(
            verdict_of(&findings, RULE_SILENCE),
            Verdict::Unverified { .. }
        ),
        "unmeasured silence must be unverified"
    );
    assert_eq!(
        disposition(&findings),
        Disposition::Block,
        "required unverified blocks"
    );

    // Inconclusive: spans measured but the duration is unknown.
    let mut inc = base(None);
    inc.silence = Some(vec![span(0, 500)]);
    let findings = r.run(&inc).expect("critic runs");
    assert!(
        matches!(
            verdict_of(&findings, RULE_SILENCE),
            Verdict::Inconclusive { .. }
        ),
        "measured spans without a duration must be inconclusive"
    );

    // Failed: a planted two-second silence over the one-second limit.
    let mut failed = base(Some(10_000));
    failed.silence = Some(vec![span(3_000, 5_000)]);
    let findings = r.run(&failed).expect("critic runs");
    assert!(
        matches!(verdict_of(&findings, RULE_SILENCE), Verdict::Failed { .. }),
        "two-second silence must fail"
    );

    // Verified: the same silence inside a declared two-second hold passes.
    let mut held = failed.clone();
    held.holds = vec![span(3_000, 5_000)];
    let findings = r.run(&held).expect("critic runs");
    assert!(
        matches!(verdict_of(&findings, RULE_SILENCE), Verdict::Verified),
        "declared hold must pass"
    );
}

#[test]
fn planted_silence_and_black_are_caught_then_repaired_without_a_model() {
    let r = runner();
    let mut input = base(Some(10_000));
    input.silence = Some(vec![span(2_000, 4_000)]);
    input.black = Some(vec![span(7_000, 7_120)]);

    let mut repairs = 0u32;
    let outcome = run_critic_loop(&r, input, RepairCap::new(2).expect("cap"), |c, f, _| {
        repairs += 1;
        if !f
            .iter()
            .any(|x| matches!(x.verdict, Verdict::Failed { .. }))
        {
            return None;
        }
        let mut next = c.clone();
        next.silence = Some(Vec::new());
        next.black = Some(Vec::new());
        Some(next)
    })
    .expect("loop runs");

    assert_eq!(repairs, 1, "one repair removes both planted defects");
    assert_eq!(outcome.attempts.len(), 2);
    let first = &outcome.attempts[0].findings;
    assert!(matches!(
        verdict_of(first, RULE_SILENCE),
        Verdict::Failed { .. }
    ));
    assert!(matches!(
        verdict_of(first, RULE_BLACK),
        Verdict::Failed { .. }
    ));
    let last = &outcome.attempts[1].findings;
    assert!(matches!(verdict_of(last, RULE_SILENCE), Verdict::Verified));
    assert!(matches!(verdict_of(last, RULE_BLACK), Verdict::Verified));
    // The not-yet-implemented checks are required = false, so the repaired input passes.
    assert_eq!(outcome.disposition, Disposition::Pass);
    assert_eq!(outcome.stop, StopReason::Passed);
}

#[test]
fn planted_silence_is_a_failed_critic_check_in_the_report() {
    let r = runner();
    let mut input = base(Some(10_000));
    input.silence = Some(vec![span(3_000, 5_000)]);
    input.black = Some(Vec::new());
    let findings = r.run(&input).expect("critic runs");
    let checks = checks_from_findings(&findings);

    let silence = check_of(&checks, RULE_SILENCE);
    assert_eq!(
        silence.status,
        CheckStatus::Fail,
        "planted silence must fail"
    );
    assert_eq!(silence.severity, Severity::Error);
    assert!(
        silence.blocking(),
        "a failed required critic check blocks packaging"
    );
    assert_eq!(silence.evidence["critic"]["verdict"], "failed");
    assert!(
        silence.message.contains("2000ms"),
        "message names the planted gap: {}",
        silence.message
    );

    // Measured and clean: black passes and stays a pass.
    let black = check_of(&checks, RULE_BLACK);
    assert_eq!(black.status, CheckStatus::Pass);
    assert!(!black.blocking());

    // Not-yet-implemented checks never block.
    for rule in [
        RULE_LOUDNESS,
        RULE_FLASH,
        RULE_CONTRAST,
        RULE_CAPTION_FACE,
        RULE_CAPTION_STATE,
    ] {
        let c = check_of(&checks, rule);
        assert_eq!(c.status, CheckStatus::NotApplicable, "{rule} is not judged");
        assert!(!c.blocking(), "{rule} must not block packaging");
        assert_eq!(c.evidence["critic"]["required"], false);
        assert!(
            c.evidence["critic"]["reason"]
                .as_str()
                .is_some_and(|s| s.contains("not implemented")),
            "{rule} carries its reason"
        );
    }
}

#[test]
fn required_unverified_and_inconclusive_block_while_optional_ones_do_not() {
    let finding = |required, verdict| Finding {
        rule_id: "critic.test".into(),
        required,
        verdict,
    };
    let blocking = |f: Finding| checks_from_findings(&[f])[0].blocking();

    assert!(blocking(finding(
        true,
        Verdict::Unverified {
            reason: "not measured".into()
        }
    )));
    assert!(blocking(finding(
        true,
        Verdict::Inconclusive {
            reason: "no duration".into()
        }
    )));
    assert!(!blocking(finding(
        false,
        Verdict::Unverified {
            reason: "not implemented".into()
        }
    )));
    assert!(!blocking(finding(
        false,
        Verdict::Inconclusive {
            reason: "no duration".into()
        }
    )));
    assert!(blocking(finding(
        true,
        Verdict::Failed {
            reason: "too long".into()
        }
    )));
    assert!(!blocking(finding(
        false,
        Verdict::Failed {
            reason: "advisory".into()
        }
    )));
    assert!(!blocking(finding(true, Verdict::Verified)));
}

#[test]
fn unavailable_critic_blocks_with_its_reason() {
    let check = critic_unavailable_check("no rightkit-render-critic binary found");
    assert_eq!(check.id, CHECK_RUN);
    assert!(
        check.blocking(),
        "a render the critic never judged cannot pass"
    );
}

#[test]
fn irreparable_input_stops_at_the_repair_cap_with_last_revision_inspectable() {
    let r = runner();
    let mut input = base(Some(10_000));
    input.silence = Some(vec![span(2_000, 4_000)]);
    input.black = Some(Vec::new());

    let mut calls = 0u32;
    let outcome = run_critic_loop(
        &r,
        input.clone(),
        RepairCap::new(2).expect("cap"),
        |c, _, _| {
            calls += 1;
            Some(c.clone())
        },
    )
    .expect("loop runs");

    assert_eq!(calls, 2, "repairer is called exactly the cap");
    assert_eq!(outcome.stop, StopReason::RepairCapReached);
    assert_eq!(
        outcome.attempts.len(),
        3,
        "original plus two repairs, all recorded"
    );
    assert_eq!(outcome.attempts.last().map(|a| a.attempt), Some(2));
    assert_eq!(outcome.disposition, Disposition::Block);
    assert_eq!(
        outcome.final_input, input,
        "last revision is returned for inspection"
    );
    assert!(matches!(
        verdict_of(&outcome.attempts[2].findings, RULE_SILENCE),
        Verdict::Failed { .. }
    ));
    assert!(
        RepairCap::new(6).is_err(),
        "caps above the shared hard limit are refused"
    );
}
